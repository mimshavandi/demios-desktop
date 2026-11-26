//! Add-on ids and the names an add-on gives the things it registers.

use crate::error::PluginError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The longest add-on id, in bytes.
pub const MAX_ID_LEN: usize = 64;
/// The longest name of a tool, job, setting, secret or file segment, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// An add-on's id: dot-separated segments, each `[a-z][a-z0-9]*` (no `_` or `-`), for example
/// `brief` or `acme.notes` (ADR-0013 §3).
///
/// The id names the add-on's tools (`plugin.<id>.<name>`), its data folder and its settings.
/// Ownership is checked against the host's registry, never by comparing strings, and two
/// add-ons whose ids are equal or where one is a leading part of the other
/// ([`PluginId::conflicts_with`]) can't both be loaded.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PluginId(String);

impl PluginId {
    /// Checks `id` and makes a `PluginId` of it.
    pub fn new(id: impl Into<String>) -> Result<Self, PluginError> {
        let id = id.into();
        let ok = !id.is_empty()
            && id.len() <= MAX_ID_LEN
            && id.split('.').all(|s| {
                let mut c = s.chars();
                c.next().is_some_and(|f| f.is_ascii_lowercase())
                    && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            });
        if ok {
            Ok(Self(id))
        } else {
            Err(PluginError::invalid(format!(
                "`{id}` isn't an add-on id: use dot-separated parts of lowercase letters and \
                 digits, each starting with a letter, such as `brief` or `acme.notes`"
            )))
        }
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the two ids are equal, or when one is the other followed by more segments
    /// (`foo` and `foo.bar`). Such add-ons are refused at load, so no add-on's tools, files
    /// or settings can look like another's.
    pub fn conflicts_with(&self, other: &PluginId) -> bool {
        let (a, b) = (self.as_str(), other.as_str());
        a == b
            || b.strip_prefix(a).is_some_and(|r| r.starts_with('.'))
            || a.strip_prefix(b).is_some_and(|r| r.starts_with('.'))
    }

    /// The capability id of this add-on's tool `name`: `plugin.<id>.<name>`.
    pub fn tool_capability_id(&self, name: &str) -> String {
        format!("plugin.{}.{name}", self.0)
    }

    /// The approval scope for one run of this add-on: `plugin:<id>:<run>`. It never equals a
    /// chat's scope, so "allow for this chat" grants are never shared with a chat
    /// (ADR-0013 §4).
    pub fn approval_scope(&self, run: dn_core::Id) -> String {
        format!("plugin:{}:{run}", self.0)
    }
}

impl TryFrom<String> for PluginId {
    type Error = PluginError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<PluginId> for String {
    fn from(id: PluginId) -> Self {
        id.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Refuses a set of add-ons in which two ids conflict ([`PluginId::conflicts_with`]).
pub fn check_unique_ids<'a>(
    ids: impl IntoIterator<Item = &'a PluginId>,
) -> Result<(), PluginError> {
    let ids: Vec<&PluginId> = ids.into_iter().collect();
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            if a.conflicts_with(b) {
                return Err(PluginError::invalid(format!(
                    "The add-ons `{a}` and `{b}` can't both be installed: their ids overlap"
                )));
            }
        }
    }
    Ok(())
}

/// Checks the name of a tool, job, setting or secret: `[a-z][a-z0-9_]*`, at most
/// [`MAX_NAME_LEN`] bytes. `what` names the thing in the error ("tool name").
pub fn check_name(what: &str, name: &str) -> Result<(), PluginError> {
    let mut c = name.chars();
    let ok = name.len() <= MAX_NAME_LEN
        && c.next().is_some_and(|f| f.is_ascii_lowercase())
        && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(PluginError::invalid(format!(
            "`{name}` isn't a valid {what}: use lowercase letters, digits and `_`, starting \
             with a letter, at most {MAX_NAME_LEN} characters"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_dotted_lowercase_segments() {
        for ok in ["brief", "acme.notes", "a1.b2c3"] {
            assert!(PluginId::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "Brief", "my_addon", "my-addon", "1brief", ".brief", "brief.", "a..b", "a.1b",
            "plugin/x",
        ] {
            assert!(PluginId::new(bad).is_err(), "{bad:?}");
        }
        assert!(PluginId::new("a".repeat(MAX_ID_LEN + 1)).is_err());
    }

    #[test]
    fn equal_or_prefix_segment_ids_conflict() {
        let id = |s: &str| PluginId::new(s).unwrap();
        assert!(id("foo").conflicts_with(&id("foo")));
        assert!(id("foo").conflicts_with(&id("foo.bar")));
        assert!(id("foo.bar").conflicts_with(&id("foo")));
        assert!(!id("foo").conflicts_with(&id("foobar")));
        assert!(!id("foo.bar").conflicts_with(&id("foo.baz")));
        assert!(check_unique_ids(&[id("a"), id("b"), id("a.c")]).is_err());
        assert!(check_unique_ids(&[id("a"), id("b"), id("ab")]).is_ok());
    }

    #[test]
    fn ids_deserialise_only_when_valid() {
        assert!(serde_json::from_str::<PluginId>("\"acme.notes\"").is_ok());
        assert!(serde_json::from_str::<PluginId>("\"Acme\"").is_err());
    }

    #[test]
    fn plugin_scopes_never_look_like_chat_scopes() {
        let s = PluginId::new("brief")
            .unwrap()
            .approval_scope(dn_core::Id::new());
        assert!(s.starts_with("plugin:brief:"));
        assert!(!s.starts_with("conv:") && !s.starts_with("run:"));
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("tool name", "daily_brief").is_ok());
        for bad in ["", "Daily", "_x", "a-b", "a.b", "1a"] {
            assert!(check_name("tool name", bad).is_err(), "{bad:?}");
        }
    }
}
