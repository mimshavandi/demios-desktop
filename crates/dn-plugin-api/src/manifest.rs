//! The manifest: who an add-on is and which permissions it asks for (ADR-0013 §3).

use crate::error::{ErrorKind, PluginError};
use crate::id::PluginId;
use crate::pattern::check_pattern;
use crate::{API_MAJOR, RiskTier};
use serde::{Deserialize, Serialize};

/// A permission an add-on can ask for. The user consents to each one, and can turn each one
/// off again in Settings (ADR-0013 §3).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Read conversations and messages, in the chats the user chose.
    ChatsRead,
    /// The add-on's own memory store.
    Memory,
    /// Search the local index, in the projects the user chose.
    KnowledgeSearch,
    /// Call tools that aren't its own, from the list in the manifest. Each call is still
    /// approved per the user's choices.
    ToolsCall,
    /// Show OS notifications.
    Notify,
    /// Outbound HTTPS to the hosts listed in the manifest.
    Network,
    /// Register background jobs. Each job is still off until the user turns it on.
    Jobs,
}

impl Permission {
    /// Every permission, in the order the consent screen lists them.
    pub const ALL: [Permission; 7] = [
        Self::ChatsRead,
        Self::Memory,
        Self::KnowledgeSearch,
        Self::ToolsCall,
        Self::Notify,
        Self::Network,
        Self::Jobs,
    ];

    /// The permission's name in the manifest and in error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatsRead => "chats.read",
            Self::Memory => "memory",
            Self::KnowledgeSearch => "knowledge.search",
            Self::ToolsCall => "tools.call",
            Self::Notify => "notify",
            Self::Network => "network",
            Self::Jobs => "jobs",
        }
    }
}

/// A permission as the manifest asks for it, with the scope only the add-on can state.
///
/// The scopes the user chooses (which chats, which projects) are part of the consent, not of
/// the request.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "permission", rename_all = "snake_case")]
pub enum PermissionRequest {
    /// `chats.read`.
    ChatsRead,
    /// `memory`.
    Memory,
    /// `knowledge.search`.
    KnowledgeSearch,
    /// `tools.call`, for these capability ids or prefixes ending in `*` (`mcp.files.*`). The
    /// consent screen shows the list. The add-on's own `plugin.<id>.*` tools need no
    /// permission.
    ToolsCall {
        /// Exact capability ids or prefixes ending in `*`.
        tools: Vec<String>,
    },
    /// `notify`.
    Notify,
    /// `network`, to these hosts only (`api.example.com`; no scheme, port or path).
    Network {
        /// Host names, lowercase.
        hosts: Vec<String>,
    },
    /// `jobs`.
    Jobs,
}

impl PermissionRequest {
    /// Which permission this asks for.
    pub fn permission(&self) -> Permission {
        match self {
            Self::ChatsRead => Permission::ChatsRead,
            Self::Memory => Permission::Memory,
            Self::KnowledgeSearch => Permission::KnowledgeSearch,
            Self::ToolsCall { .. } => Permission::ToolsCall,
            Self::Notify => Permission::Notify,
            Self::Network { .. } => Permission::Network,
            Self::Jobs => Permission::Jobs,
        }
    }
}

/// Who an add-on is and what it asks for. Returned by `Plugin::manifest`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// The add-on's id.
    pub id: PluginId,
    /// The add-on's own version, shown in Settings.
    pub version: String,
    /// The `dn-plugin-api` major version it was built for ([`API_MAJOR`] when it was built).
    pub api_major: u32,
    /// Its name, shown as plain text.
    pub name: String,
    /// The permissions it asks for, each at most once.
    pub permissions: Vec<PermissionRequest>,
}

impl PluginManifest {
    /// A manifest for an add-on built against this version of the API, with no permissions.
    pub fn new(id: PluginId, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id,
            version: version.into(),
            api_major: API_MAJOR,
            name: name.into(),
            permissions: Vec::new(),
        }
    }

    /// Adds a permission request.
    pub fn with_permission(mut self, p: PermissionRequest) -> Self {
        self.permissions.push(p);
        self
    }

    /// True when the manifest asks for `p`.
    pub fn asks_for(&self, p: Permission) -> bool {
        self.permissions.iter().any(|r| r.permission() == p)
    }

    /// The tool patterns of the `tools.call` request, if any.
    pub fn tool_patterns(&self) -> &[String] {
        self.permissions
            .iter()
            .find_map(|r| match r {
                PermissionRequest::ToolsCall { tools } => Some(tools.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }

    /// The hosts of the `network` request, if any.
    pub fn network_hosts(&self) -> &[String] {
        self.permissions
            .iter()
            .find_map(|r| match r {
                PermissionRequest::Network { hosts } => Some(hosts.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }

    /// Checks the manifest: the API major matches ([`check_compatible`]), the name and
    /// version aren't empty, each permission is asked for once, tool patterns and hosts are
    /// well formed and not empty.
    pub fn validate(&self) -> Result<(), PluginError> {
        check_compatible(self)?;
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(PluginError::invalid(format!(
                "The add-on `{}` has no name or version",
                self.id
            )));
        }
        for (i, r) in self.permissions.iter().enumerate() {
            if self.permissions[..i]
                .iter()
                .any(|o| o.permission() == r.permission())
            {
                return Err(PluginError::invalid(format!(
                    "The add-on `{}` asks for `{}` twice",
                    self.id,
                    r.permission().as_str()
                )));
            }
            match r {
                PermissionRequest::ToolsCall { tools } => {
                    if tools.is_empty() {
                        return Err(PluginError::invalid(format!(
                            "The add-on `{}` asks for `tools.call` without listing any tools",
                            self.id
                        )));
                    }
                    for t in tools {
                        check_pattern(t)?;
                    }
                }
                PermissionRequest::Network { hosts } => {
                    if hosts.is_empty() {
                        return Err(PluginError::invalid(format!(
                            "The add-on `{}` asks for `network` without listing any hosts",
                            self.id
                        )));
                    }
                    for h in hosts {
                        check_host(h)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Refuses an add-on built for another `dn-plugin-api` major version (ADR-0013 §7). There is
/// no compatibility shim and no partial load.
pub fn check_compatible(m: &PluginManifest) -> Result<(), PluginError> {
    if m.api_major == API_MAJOR {
        return Ok(());
    }
    let fix = if m.api_major > API_MAJOR {
        "update Demios"
    } else {
        "update the add-on"
    };
    Err(PluginError::new(
        ErrorKind::Incompatible,
        format!(
            "{} needs add-on interface version {}, and this Demios has version {API_MAJOR}: \
             {fix}.",
            m.name, m.api_major
        ),
    ))
}

/// Checks a host name for the `network` list: lowercase letters, digits, `-` and `.`, with
/// at least one dot, and no scheme, port, path or user part.
pub fn check_host(h: &str) -> Result<(), PluginError> {
    let ok = !h.is_empty()
        && h.len() <= 253
        && h.contains('.')
        && h.split('.').all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if ok {
        Ok(())
    } else {
        Err(PluginError::invalid(format!(
            "`{h}` isn't a host name: give it without scheme, port or path, such as \
             `api.example.com`"
        )))
    }
}

/// The lowest risk tier a tool of this add-on may declare (ADR-0013 §9.1): [`RiskTier::Read`]
/// for every tool.
///
/// The add-on-wide floor is gone because the host now limits every handler to its own
/// declared tier: a `read` handler can't write or reach the network, a `low_write` handler
/// can write only the add-on's own data and has no `http()`, and a handler can't call a tool
/// above its tier ([`Access`](crate::Access)). So a tool's tier is what it declares.
///
/// The function stays the one place the rule lives (the [`Registrar`](crate::Registrar) is
/// its only caller), so a later permission that can't be limited per handler has one place
/// to put a floor back.
pub fn risk_floor(_m: &PluginManifest) -> RiskTier {
    RiskTier::Read
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> PluginManifest {
        PluginManifest::new(PluginId::new("brief").unwrap(), "Daily brief", "1.0.0")
    }

    #[test]
    fn a_plain_manifest_is_valid_and_compatible() {
        assert!(m().validate().is_ok());
    }

    #[test]
    fn another_major_is_refused_with_the_fix() {
        let mut newer = m();
        newer.api_major = API_MAJOR + 1;
        let e = check_compatible(&newer).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Incompatible);
        assert!(e.message.contains("update Demios"), "{}", e.message);
        let mut older = m();
        older.api_major = API_MAJOR.wrapping_sub(1);
        assert!(
            check_compatible(&older)
                .unwrap_err()
                .message
                .contains("update the add-on")
        );
        assert!(newer.validate().is_err());
    }

    #[test]
    fn permissions_are_asked_once_with_well_formed_scopes() {
        assert!(
            m().with_permission(PermissionRequest::Notify)
                .with_permission(PermissionRequest::Notify)
                .validate()
                .is_err()
        );
        let tools = |t: &[&str]| PermissionRequest::ToolsCall {
            tools: t.iter().map(|s| s.to_string()).collect(),
        };
        assert!(m().with_permission(tools(&[])).validate().is_err());
        assert!(m().with_permission(tools(&["mcp.*.x"])).validate().is_err());
        assert!(
            m().with_permission(tools(&["mcp.files.*"]))
                .validate()
                .is_ok()
        );
        let net = |h: &str| PermissionRequest::Network {
            hosts: vec![h.into()],
        };
        for bad in [
            "https://a.com",
            "a.com:443",
            "a.com/x",
            "localhost",
            "A.com",
            "u@a.com",
        ] {
            assert!(m().with_permission(net(bad)).validate().is_err(), "{bad}");
        }
        assert!(
            m().with_permission(net("api.example.com"))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn the_floor_is_read_whatever_the_permissions() {
        // ADR-0013 §9.1: each handler's Host is limited to its own tier instead.
        for manifest in [
            m(),
            m().with_permission(PermissionRequest::Memory),
            m().with_permission(PermissionRequest::Network {
                hosts: vec!["a.example.com".into()],
            }),
            m().with_permission(PermissionRequest::Memory)
                .with_permission(PermissionRequest::ToolsCall {
                    tools: vec!["mcp.files.read".into()],
                }),
        ] {
            assert_eq!(risk_floor(&manifest), RiskTier::Read);
        }
    }
}
