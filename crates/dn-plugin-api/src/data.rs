//! Plain, serialisable values that cross the boundary between an add-on and the host.

use crate::Id;
use serde::{Deserialize, Serialize};

/// A conversation the add-on may read.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// The conversation's id.
    pub id: Id,
    /// The project it belongs to, if any.
    pub project_id: Option<Id>,
    /// Its title.
    pub title: String,
    /// When it last changed, Unix seconds.
    pub updated_at: i64,
}

impl ConversationSummary {
    /// A conversation (built by the host).
    pub fn new(id: Id, project_id: Option<Id>, title: impl Into<String>, updated_at: i64) -> Self {
        Self {
            id,
            project_id,
            title: title.into(),
            updated_at,
        }
    }
}

/// Who wrote a message.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The person using Demios.
    User,
    /// The model.
    Assistant,
}

/// A copy of one message.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// The message's id.
    pub id: Id,
    /// Who wrote it.
    pub role: Role,
    /// Its text.
    pub text: String,
    /// When it was written, Unix seconds.
    pub created_at: i64,
}

impl ChatMessage {
    /// A message copy (built by the host).
    pub fn new(id: Id, role: Role, text: impl Into<String>, created_at: i64) -> Self {
        Self {
            id,
            role,
            text: text.into(),
            created_at,
        }
    }
}

/// An item in the add-on's memory. The user sees every item and can delete it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryItem {
    /// The item's id.
    pub id: Id,
    /// What the add-on remembers, as plain text.
    pub text: String,
    /// The chats it came from. When all of them are purged, the host deletes the item.
    pub sources: Vec<Id>,
    /// When it was added, Unix seconds.
    pub created_at: i64,
    /// When it last changed, Unix seconds.
    pub updated_at: i64,
}

impl MemoryItem {
    /// A memory item (built by the host).
    pub fn new(
        id: Id,
        text: impl Into<String>,
        sources: Vec<Id>,
        created_at: i64,
        updated_at: i64,
    ) -> Self {
        Self {
            id,
            text: text.into(),
            sources,
            created_at,
            updated_at,
        }
    }
}

/// A new memory item.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMemory {
    /// What to remember.
    pub text: String,
    /// The chats it came from (may be empty).
    pub sources: Vec<Id>,
}

impl NewMemory {
    /// An item with these sources.
    pub fn new(text: impl Into<String>, sources: Vec<Id>) -> Self {
        Self {
            text: text.into(),
            sources,
        }
    }
}

/// A passage found by project search.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeHit {
    /// The project the file belongs to.
    pub project_id: Id,
    /// The file's name as shown to the user.
    pub file: String,
    /// The first line of the passage.
    pub start_line: u32,
    /// The last line of the passage.
    pub end_line: u32,
    /// The passage.
    pub text: String,
}

impl KnowledgeHit {
    /// A search hit (built by the host).
    pub fn new(
        project_id: Id,
        file: impl Into<String>,
        start_line: u32,
        end_line: u32,
        text: impl Into<String>,
    ) -> Self {
        Self {
            project_id,
            file: file.into(),
            start_line,
            end_line,
            text: text.into(),
        }
    }
}

/// Which of the user's model choices a prompt runs on.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSlot {
    /// The model that answers in chats.
    Chat,
    /// The model for quick background tasks.
    Background,
}

/// A prompt for the user's chosen model.
///
/// The instructions and the request are plain strings the add-on wrote. Untrusted text can
/// only come in as [`Quoted`](crate::Quoted) blocks, which the host marks as data.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrompt {
    /// What the model should do.
    pub instructions: String,
    /// The request itself.
    pub request: String,
    /// Material to work from, quoted and cited.
    pub context: Vec<crate::Quoted>,
}

impl ModelPrompt {
    /// A prompt with no quoted material.
    pub fn new(instructions: impl Into<String>, request: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
            request: request.into(),
            context: Vec::new(),
        }
    }

    /// Adds a quoted block.
    pub fn with_context(mut self, q: crate::Quoted) -> Self {
        self.context.push(q);
        self
    }
}

/// One of the add-on's own calls waiting in the held queue for the user's answer
/// (ADR-0013 §9.2), as [`Host::held`](crate::Host::held) lists it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldCall {
    /// The held call's id, for [`Held::withdraw`](crate::host::Held::withdraw) and
    /// [`Held::replace`](crate::host::Held::replace).
    pub id: Id,
    /// The tool's capability id.
    pub tool: String,
    /// The exact arguments it will run with if the user allows it.
    pub arguments: serde_json::Value,
    /// What the call does, in plain words.
    pub summary: String,
    /// When it was held (Unix seconds).
    pub created_at: i64,
    /// The call this one replaced, if any.
    pub replaces: Option<Id>,
}

impl HeldCall {
    /// A held call, as the host reports it.
    pub fn new(
        id: Id,
        tool: impl Into<String>,
        arguments: serde_json::Value,
        summary: impl Into<String>,
        created_at: i64,
        replaces: Option<Id>,
    ) -> Self {
        Self {
            id,
            tool: tool.into(),
            arguments,
            summary: summary.into(),
            created_at,
            replaces,
        }
    }
}

/// What a tool returns.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Text for the model.
    pub text: String,
    /// Structured output, when the tool declares an output schema.
    pub structured: Option<serde_json::Value>,
    /// True when the tool failed in a way the model should hear about.
    pub is_error: bool,
}

impl ToolResult {
    /// A successful result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            structured: None,
            is_error: false,
        }
    }

    /// A failure the model should hear about.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            structured: None,
            is_error: true,
        }
    }

    /// Adds structured output.
    pub fn with_structured(mut self, v: serde_json::Value) -> Self {
        self.structured = Some(v);
        self
    }
}

/// An HTTP method.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET: the only method a read-tier handler may use.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

/// An outbound HTTPS request.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    /// The method.
    pub method: HttpMethod,
    /// An `https://` URL on one of the hosts in the manifest, with no port or user part.
    pub url: String,
    /// Header names and values.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// A GET request with no headers.
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Get, url)
    }

    /// A request with no headers and no body.
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Adds a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets the body.
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

/// An HTTP response.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// Header names and values.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }
}

/// A value in an add-on's own database.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SqlValue {
    /// NULL.
    Null,
    /// An integer.
    Integer(i64),
    /// A float.
    Real(f64),
    /// Text.
    Text(String),
    /// A blob.
    Blob(Vec<u8>),
}

/// One row of a query result, in column order.
pub type Row = Vec<SqlValue>;
