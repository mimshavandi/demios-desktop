//! Model-facing protocol (ADR-0004): the three artifact tools, turning tool calls into
//! [`ArtifactOp`]s, tool results, and applying fenced-fallback blocks.

use crate::error::{ArtifactError, Result};
use crate::fenced::FencedArtifact;
use crate::store::{Applied, ArtifactStore, OpContext, check_content, check_title};
use dn_core::Id;
use dn_core::artifact::{ArtifactOp, ArtifactType};
use dn_core::chat::{ToolCall, ToolSpec};
use serde_json::{Value, json};

pub const TOOL_CREATE: &str = "artifact_create";
pub const TOOL_UPDATE: &str = "artifact_update";
pub const TOOL_REWRITE: &str = "artifact_rewrite";

/// Type names the tools accept (`type` field).
pub const TYPE_NAMES: [&str; 6] = ["html", "react", "svg", "mermaid", "markdown", "code"];

/// Tool specs to expose to every model.
pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: TOOL_CREATE.into(),
            description: "Create an artifact: a self-contained document shown next to the chat \
                (web page, React component, SVG, Mermaid diagram, Markdown document or code file). \
                Use it for substantial content the user may reuse or iterate on, not for short \
                answers. HTML and React run in a sandbox without network access, so inline all \
                CSS and JS; React artifacts are one JSX file that `export default`s a component \
                and may import only `react` and `react-dom`. Returns the artifact id."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": TYPE_NAMES,
                        "description": "Kind of artifact."
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language, required when type is `code` (e.g. `python`)."
                    },
                    "title": { "type": "string", "description": "Short human-readable title." },
                    "content": { "type": "string", "description": "Full content of the artifact." }
                },
                "required": ["type", "title", "content"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: TOOL_UPDATE.into(),
            description: "Edit an existing artifact by replacing one exact piece of text. \
                `old_str` must appear exactly once in the latest version, character for \
                character including whitespace; include enough surrounding text to make it \
                unique. Prefer this for small changes; use artifact_rewrite for large ones or \
                when an update fails twice."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Artifact id returned by artifact_create." },
                    "old_str": { "type": "string", "description": "Exact text to replace (must occur once)." },
                    "new_str": { "type": "string", "description": "Replacement text (may be empty to delete)." }
                },
                "required": ["id", "old_str", "new_str"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: TOOL_REWRITE.into(),
            description: "Replace the whole content of an existing artifact with a new version."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Artifact id returned by artifact_create." },
                    "content": { "type": "string", "description": "New full content." }
                },
                "required": ["id", "content"],
                "additionalProperties": false
            }),
        },
    ]
}

/// System-prompt text for models **without tool calling** (parsed by [`crate::fenced`]). Models
/// that support tools get [`tool_specs`] instead, and their text is not run through the fenced
/// parser, so an echoed `<artifact>` tag in their prose or in tool output can't create anything.
pub const FENCED_INSTRUCTIONS: &str = "\
To create an artifact (a self-contained web page, React component, SVG, Mermaid diagram, \
Markdown document or code file shown next to the chat), write it as:
<artifact id=\"short-id\" type=\"html|react|svg|mermaid|markdown|code\" title=\"Title\" language=\"(code only)\">
...full content...
</artifact>
Start the opening tag on its own line, outside any ``` code block. The content must not contain \
the text </artifact>. To change an artifact, write the block again with the same id, the same \
type and the complete new content. \
HTML and React run without network access: inline all CSS and JS. React artifacts are one JSX \
file that `export default`s a component and may import only `react` and `react-dom`.";

pub fn is_artifact_tool(name: &str) -> bool {
    matches!(name, TOOL_CREATE | TOOL_UPDATE | TOOL_REWRITE)
}

/// Map a `type` value (ours, a MIME type, or Claude's `application/vnd.ant.*`) to a type.
pub fn parse_artifact_type(ty: &str, language: Option<&str>) -> Option<ArtifactType> {
    let ty = ty.trim().to_ascii_lowercase();
    Some(match ty.as_str() {
        "html" | "text/html" => ArtifactType::Html,
        "react" | "jsx" | "tsx" | "application/vnd.ant.react" => ArtifactType::React,
        "svg" | "image/svg+xml" => ArtifactType::Svg,
        "mermaid" | "application/vnd.ant.mermaid" => ArtifactType::Mermaid,
        "markdown" | "md" | "text/markdown" => ArtifactType::Markdown,
        "code" | "application/vnd.ant.code" => ArtifactType::Code {
            language: language
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .unwrap_or("text")
                .to_owned(),
        },
        _ => return None,
    })
}

/// Turn a model tool call into an operation. Errors are phrased for the model.
pub fn op_from_tool_call(call: &ToolCall) -> Result<ArtifactOp> {
    let tool = call.name.as_str();
    let args = &call.arguments;
    let bad = |reason: String| ArtifactError::InvalidToolInput {
        tool: tool.to_owned(),
        reason,
    };
    let field = |name: &'static str| -> Result<String> {
        match args.get(name) {
            Some(Value::String(s)) => {
                check_content(name, s)?;
                Ok(s.clone())
            }
            Some(_) => Err(bad(format!("`{name}` must be a string"))),
            None => Err(bad(format!("missing `{name}`"))),
        }
    };
    let id = || -> Result<Id> {
        let raw = field("id")?;
        uuid::Uuid::parse_str(raw.trim()).map(Id).map_err(|_| {
            bad(format!(
                "`id` `{raw}` is not an artifact id returned by {TOOL_CREATE}"
            ))
        })
    };
    if !args.is_object() {
        return Err(bad("arguments must be a JSON object".into()));
    }
    match tool {
        TOOL_CREATE => {
            let ty = field("type")?;
            let language = args.get("language").and_then(Value::as_str);
            let artifact_type = parse_artifact_type(&ty, language).ok_or_else(|| {
                bad(format!(
                    "unknown type `{ty}`; use one of {}",
                    TYPE_NAMES.join(", ")
                ))
            })?;
            let title = field("title")?;
            if title.trim().is_empty() {
                return Err(bad("`title` must not be empty".into()));
            }
            check_title(title.trim())?;
            Ok(ArtifactOp::Create {
                artifact_type,
                title: title.trim().to_owned(),
                content: field("content")?,
            })
        }
        TOOL_UPDATE => Ok(ArtifactOp::Update {
            id: id()?,
            old_str: field("old_str")?,
            new_str: field("new_str")?,
        }),
        TOOL_REWRITE => Ok(ArtifactOp::Rewrite {
            id: id()?,
            content: field("content")?,
        }),
        other => Err(ArtifactError::UnknownTool(other.to_owned())),
    }
}

/// Run an artifact tool call against the store.
pub fn execute_tool_call(
    store: &ArtifactStore<'_>,
    conversation_id: Id,
    call: &ToolCall,
    ctx: &OpContext,
) -> Result<Applied> {
    let op = op_from_tool_call(call)?;
    store.apply(conversation_id, &op, ctx)
}

/// JSON to send back to the model as the tool result.
pub fn tool_result(result: &Result<Applied>) -> Value {
    match result {
        Ok(a) => json!({
            "ok": true,
            "id": a.artifact_id.to_string(),
            "title": a.title,
            "version": a.version.n,
        }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

/// Apply a fenced block from [`FenceEvent::Close`](crate::fenced::FenceEvent::Close). A block
/// whose `id` names an artifact of this conversation (its UUID, or an alias from an earlier
/// block) rewrites it, and its type must match; otherwise a new artifact is created and the
/// `id` is remembered as its alias. Unterminated blocks are refused, never saved.
pub fn apply_fenced(
    store: &ArtifactStore<'_>,
    conversation_id: Id,
    block: &FencedArtifact,
    ctx: &OpContext,
) -> Result<Applied> {
    if !block.complete {
        return Err(ArtifactError::Unterminated);
    }
    let h = &block.header;
    if let Some(key) = h.id.as_deref() {
        let existing = match uuid::Uuid::parse_str(key).map(Id) {
            Ok(id) if store.exists_in(id, conversation_id)? => Some(id),
            _ => store.find_by_alias(conversation_id, key)?,
        };
        if let Some(id) = existing {
            let current = store.get(id)?.artifact_type;
            if current != h.artifact_type {
                return Err(ArtifactError::TypeMismatch {
                    id,
                    existing: type_name(&current),
                    requested: type_name(&h.artifact_type),
                });
            }
            return store.rewrite(conversation_id, id, &block.content, ctx);
        }
    }
    store.create(
        conversation_id,
        &h.artifact_type,
        &h.title,
        &block.content,
        h.id.as_deref(),
        ctx,
    )
}

fn type_name(t: &ArtifactType) -> String {
    match t {
        ArtifactType::Html => "html".into(),
        ArtifactType::React => "react".into(),
        ArtifactType::Svg => "svg".into(),
        ArtifactType::Mermaid => "mermaid".into(),
        ArtifactType::Markdown => "markdown".into(),
        ArtifactType::Code { language } => format!("code ({language})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenced::{FenceEvent, FencedParser};
    use crate::store::tests::db;

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[test]
    fn specs_are_valid_and_named() {
        let specs = tool_specs();
        let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, [TOOL_CREATE, TOOL_UPDATE, TOOL_REWRITE]);
        for s in &specs {
            assert_eq!(s.input_schema["type"], "object");
            assert!(s.input_schema["required"].is_array());
            assert!(is_artifact_tool(&s.name));
        }
    }

    #[test]
    fn tool_calls_drive_the_store() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let ctx = OpContext::model(None);
        let created = execute_tool_call(
            &store,
            conv,
            &call(
                TOOL_CREATE,
                json!({"type": "code", "language": "rust", "title": " main ", "content": "fn a() {}"}),
            ),
            &ctx,
        );
        let out = tool_result(&created);
        assert_eq!(out["ok"], true);
        assert_eq!(out["version"], 1);
        assert_eq!(out["title"], "main");
        let id = out["id"].as_str().unwrap().to_owned();

        let upd = execute_tool_call(
            &store,
            conv,
            &call(
                TOOL_UPDATE,
                json!({"id": id, "old_str": "a()", "new_str": "b()"}),
            ),
            &ctx,
        );
        assert_eq!(tool_result(&upd)["version"], 2);

        let miss = execute_tool_call(
            &store,
            conv,
            &call(
                TOOL_UPDATE,
                json!({"id": id, "old_str": "zzz", "new_str": ""}),
            ),
            &ctx,
        );
        let out = tool_result(&miss);
        assert_eq!(out["ok"], false);
        assert!(out["error"].as_str().unwrap().contains("not found"));

        let rw = execute_tool_call(
            &store,
            conv,
            &call(TOOL_REWRITE, json!({"id": id, "content": "fn c() {}"})),
            &ctx,
        )
        .unwrap();
        assert_eq!(rw.version.n, 3);
    }

    #[test]
    fn bad_tool_input_is_explained() {
        let err = op_from_tool_call(&call(
            TOOL_CREATE,
            json!({"type": "pdf", "title": "x", "content": ""}),
        ))
        .unwrap_err();
        assert!(err.to_string().contains("unknown type `pdf`"));
        let err = op_from_tool_call(&call(
            TOOL_UPDATE,
            json!({"id": "todo", "old_str": "a", "new_str": "b"}),
        ))
        .unwrap_err();
        assert!(err.to_string().contains("not an artifact id"));
        let err = op_from_tool_call(&call(TOOL_REWRITE, json!({"id": Id::new().to_string()})))
            .unwrap_err();
        assert!(err.to_string().contains("missing `content`"));
        let err = op_from_tool_call(&call(TOOL_CREATE, json!("nope"))).unwrap_err();
        assert!(err.to_string().contains("JSON object"));
        assert!(matches!(
            op_from_tool_call(&call("web_search", json!({}))).unwrap_err(),
            ArtifactError::UnknownTool(_)
        ));
    }

    #[test]
    fn fenced_blocks_create_then_rewrite_by_alias() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let ctx = OpContext::model(None);
        let mut parser = FencedParser::new();
        let mut blocks = Vec::new();
        for chunk in [
            "First:\n<artifact id=\"page\" type=\"html\" title=\"Page\">\n<p>1</p>\n</arti",
            "fact>\nThen:\n<artifact id=\"page\" type=\"html\" title=\"Page\">\n<p>2</p>\n</artifact>",
        ] {
            for ev in parser.push(chunk) {
                if let FenceEvent::Close(b) = ev {
                    blocks.push(b);
                }
            }
        }
        let a = apply_fenced(&store, conv, &blocks[0], &ctx).unwrap();
        let b = apply_fenced(&store, conv, &blocks[1], &ctx).unwrap();
        assert_eq!(a.artifact_id, b.artifact_id);
        assert_eq!(b.version.n, 2);
        assert_eq!(b.version.content, "<p>2</p>");
        // Using the real id works too.
        let mut by_uuid = blocks[1].clone();
        by_uuid.header.id = Some(a.artifact_id.to_string());
        by_uuid.content = "<p>3</p>".into();
        assert_eq!(
            apply_fenced(&store, conv, &by_uuid, &ctx)
                .unwrap()
                .version
                .n,
            3
        );
        // Same alias in another conversation is a different artifact.
        let other = apply_fenced(&store, Id::new(), &blocks[0], &ctx).unwrap();
        assert_ne!(other.artifact_id, a.artifact_id);
    }

    fn block(id: &str, ty: ArtifactType, content: &str, complete: bool) -> FencedArtifact {
        FencedArtifact {
            header: crate::fenced::FenceHeader {
                id: Some(id.into()),
                artifact_type: ty,
                title: "T".into(),
            },
            content: content.into(),
            complete,
        }
    }

    #[test]
    fn fenced_type_change_and_unterminated_blocks_are_refused() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let ctx = OpContext::model(None);
        apply_fenced(
            &store,
            conv,
            &block("p", ArtifactType::Html, "<p>1</p>", true),
            &ctx,
        )
        .unwrap();
        let err = apply_fenced(
            &store,
            conv,
            &block("p", ArtifactType::Svg, "<svg/>", true),
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, ArtifactError::TypeMismatch { .. }), "{err:?}");
        assert!(err.to_string().contains("not svg"));
        let err = apply_fenced(
            &store,
            conv,
            &block("q", ArtifactType::Html, "<p>", false),
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, ArtifactError::Unterminated));
        assert_eq!(store.list(conv).unwrap().len(), 1);
    }

    #[test]
    fn oversized_tool_arguments_are_refused_before_the_store() {
        let big = "x".repeat(crate::store::MAX_CONTENT_BYTES + 1);
        let err = op_from_tool_call(&call(
            TOOL_REWRITE,
            json!({"id": Id::new().to_string(), "content": big}),
        ))
        .unwrap_err();
        assert!(matches!(
            err,
            ArtifactError::TooLarge {
                what: "content",
                ..
            }
        ));
        let err = op_from_tool_call(&call(
            TOOL_CREATE,
            json!({"type": "html", "title": "t".repeat(500), "content": "x"}),
        ))
        .unwrap_err();
        assert!(matches!(err, ArtifactError::TitleTooLong { .. }));
    }
}
