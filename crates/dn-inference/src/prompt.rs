//! Turning a [`ChatRequest`] into the plain role/content turns a GGUF chat template accepts,
//! plus the tool-calling convention local models are asked to follow.
//!
//! llama.cpp's built-in template engine has no tool support, so tools are described in the
//! system turn using the Hermes/Qwen convention (`<tool_call>{"name":…,"arguments":…}</tool_call>`)
//! and, once the model opens a `<tool_call>` tag, a lazy grammar built from the tools' JSON
//! Schemas forces the call to be valid JSON naming a real tool.

use dn_core::chat::{ChatMessage, ChatRequest, ContentPart, Role, ToolSpec};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub role: &'static str,
    pub content: String,
}

pub fn tool_system_prompt(tools: &[ToolSpec]) -> String {
    let mut s = String::from(
        "# Tools\n\nYou may call one or more functions to assist with the user query.\n\n\
         You are provided with function signatures within <tools></tools> XML tags:\n<tools>\n",
    );
    for t in tools {
        let sig = json!({
            "type": "function",
            "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}
        });
        s.push_str(&sig.to_string());
        s.push('\n');
    }
    s.push_str(
        "</tools>\n\nFor each function call, return a json object with function name and \
         arguments within <tool_call></tool_call> XML tags:\n<tool_call>\n\
         {\"name\": <function-name>, \"arguments\": <args-json-object>}\n</tool_call>",
    );
    s
}

fn text_of(m: &ChatMessage) -> String {
    m.content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.as_str(),
            ContentPart::Image { .. } => "[image omitted: this model has no vision support]",
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Flatten messages into template turns. Tool results become user turns wrapped in
/// `<tool_response>` (the role most built-in templates understand).
pub fn turns(req: &ChatRequest) -> Vec<Turn> {
    let mut out = vec![];
    let mut system: Vec<String> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(text_of)
        .collect();
    if !req.tools.is_empty() {
        system.push(tool_system_prompt(&req.tools));
    }
    if !system.is_empty() {
        out.push(Turn {
            role: "system",
            content: system.join("\n\n"),
        });
    }
    for m in &req.messages {
        let (role, content) = match m.role {
            Role::System => continue,
            Role::User => ("user", text_of(m)),
            Role::Assistant => {
                let mut c = text_of(m);
                for call in &m.tool_calls {
                    if !c.is_empty() {
                        c.push('\n');
                    }
                    c.push_str(&format!(
                        "<tool_call>\n{}\n</tool_call>",
                        json!({"name": call.name, "arguments": call.arguments})
                    ));
                }
                ("assistant", c)
            }
            Role::Tool => (
                "user",
                format!("<tool_response>\n{}\n</tool_response>", text_of(m)),
            ),
        };
        // Merge consecutive same-role turns (several tool results in a row).
        match out.last_mut() {
            Some(last) if last.role == role && m.role == Role::Tool => {
                last.content.push('\n');
                last.content.push_str(&content);
            }
            _ => out.push(Turn { role, content }),
        }
    }
    out
}

/// JSON Schema of one tool call: `{"name": <one of the tools>, "arguments": <its schema>}`.
pub fn tool_call_schema(tools: &[ToolSpec]) -> Value {
    let variants: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "object",
                "properties": {
                    "name": {"const": t.name},
                    "arguments": t.input_schema,
                },
                "required": ["name", "arguments"],
            })
        })
        .collect();
    if variants.len() == 1 {
        variants.into_iter().next().unwrap_or_default()
    } else {
        json!({ "oneOf": variants })
    }
}

/// Wrap a GBNF grammar for one tool-call JSON object (root rule `root`) so it matches
/// `<tool_call>\n{json}\n</tool_call>`, one or more times.
pub fn wrap_tool_grammar(json_gbnf: &str) -> String {
    let renamed: String = json_gbnf
        .lines()
        .map(|l| {
            l.strip_prefix("root ::=")
                .map(|r| format!("tool-json ::={r}"))
                .unwrap_or_else(|| l.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "root ::= tool-call (\"\\n\" tool-call)*\n\
         tool-call ::= \"<tool_call>\" \"\\n\"? tool-json \"\\n\"? \"</tool_call>\"\n{renamed}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::chat::ToolCall;

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "get_weather".into(),
            description: "Weather for a city".into(),
            input_schema: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
        }
    }

    fn req(messages: Vec<ChatMessage>, tools: Vec<ToolSpec>) -> ChatRequest {
        ChatRequest {
            model: "local:x".parse().unwrap(),
            messages,
            tools,
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Default::default(),
            caller: "test".into(),
        }
    }

    #[test]
    fn tools_go_into_system_turn() {
        let t = turns(&req(
            vec![
                ChatMessage::text(Role::System, "Be brief."),
                ChatMessage::text(Role::User, "Weather?"),
            ],
            vec![tool()],
        ));
        assert_eq!(t.len(), 2);
        assert!(t[0].content.starts_with("Be brief."));
        assert!(t[0].content.contains("\"name\":\"get_weather\""));
        assert_eq!(t[1].role, "user");
    }

    #[test]
    fn tool_round_trip_rendering() {
        let mut a = ChatMessage::text(Role::Assistant, "");
        a.tool_calls.push(ToolCall {
            id: "c1".into(),
            name: "get_weather".into(),
            arguments: json!({"city":"Oslo"}),
        });
        let mut r1 = ChatMessage::text(Role::Tool, "rain");
        r1.tool_call_id = Some("c1".into());
        let r2 = ChatMessage::text(Role::Tool, "cold");
        let t = turns(&req(
            vec![ChatMessage::text(Role::User, "w?"), a, r1, r2],
            vec![],
        ));
        assert_eq!(t.len(), 3);
        assert!(t[1].content.starts_with("<tool_call>\n{"));
        assert_eq!(
            t[2].content,
            "<tool_response>\nrain\n</tool_response>\n<tool_response>\ncold\n</tool_response>"
        );
    }

    #[test]
    fn schema_variants() {
        assert_eq!(
            tool_call_schema(&[tool()])["properties"]["name"]["const"],
            "get_weather"
        );
        assert_eq!(
            tool_call_schema(&[tool(), tool()])["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn grammar_root_renamed() {
        let g = wrap_tool_grammar("root ::= \"{\" x \"}\"\nx ::= [a-z]+");
        assert!(g.starts_with("root ::= tool-call"));
        assert!(g.contains("tool-json ::= \"{\" x \"}\""));
        assert_eq!(g.matches("root ::=").count(), 1);
    }
}
