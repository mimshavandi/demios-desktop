//! Declarative settings panels (ADR-0013 §2).
//!
//! An add-on describes its settings as values; the host renders them with the design
//! system's controls. An add-on ships no HTML or JavaScript, and every string here is shown
//! as plain text. Values are stored by the host in the add-on's own settings namespace;
//! [`FieldKind::Secret`] values go to the keychain under ids the host owns for the add-on.
//!
//! The panel is a closed set of field kinds rather than free-form JSON Schema, so everything
//! an add-on can declare is something the host knows how to show. A new kind is an API minor
//! version.

use crate::error::PluginError;
use crate::id::check_name;
use serde::{Deserialize, Serialize};

/// The most fields one panel can have.
pub const MAX_FIELDS: usize = 50;
/// The longest title or help text, in characters.
pub const MAX_TEXT: usize = 500;

/// An add-on's settings panel.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsPanel {
    /// The fields, in the order they are shown.
    pub fields: Vec<Field>,
}

impl SettingsPanel {
    /// A panel with these fields.
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// Checks the panel: at most [`MAX_FIELDS`] fields with unique, valid keys, titles that
    /// aren't empty, texts no longer than [`MAX_TEXT`], and kinds whose limits make sense.
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.fields.len() > MAX_FIELDS {
            return Err(PluginError::invalid(format!(
                "A settings panel can have at most {MAX_FIELDS} fields"
            )));
        }
        for (i, f) in self.fields.iter().enumerate() {
            check_name("setting key", &f.key)?;
            if self.fields[..i].iter().any(|o| o.key == f.key) {
                return Err(PluginError::invalid(format!(
                    "The setting `{}` is listed twice",
                    f.key
                )));
            }
            if f.title.trim().is_empty() {
                return Err(PluginError::invalid(format!(
                    "The setting `{}` has no title",
                    f.key
                )));
            }
            let long = [Some(&f.title), f.help.as_ref()]
                .into_iter()
                .flatten()
                .any(|t| t.chars().count() > MAX_TEXT);
            if long {
                return Err(PluginError::invalid(format!(
                    "The title or help of `{}` is longer than {MAX_TEXT} characters",
                    f.key
                )));
            }
            f.kind.validate(&f.key)?;
        }
        Ok(())
    }
}

/// One setting.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// The key the value is stored under in the add-on's namespace.
    pub key: String,
    /// The label, in sentence case.
    pub title: String,
    /// Help shown under the control.
    pub help: Option<String>,
    /// The control and its limits.
    pub kind: FieldKind,
}

impl Field {
    /// A field with no help text.
    pub fn new(key: impl Into<String>, title: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            help: None,
            kind,
        }
    }

    /// Adds help text.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// The control a setting uses.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    /// On or off. Saved at once.
    Toggle {
        /// The value before the user changes it.
        default: bool,
    },
    /// One line of text.
    Text {
        /// The value before the user changes it.
        default: String,
        /// The most characters allowed.
        max_len: u32,
    },
    /// A whole number in a range.
    Number {
        /// The value before the user changes it.
        default: i64,
        /// The smallest allowed value.
        min: i64,
        /// The largest allowed value.
        max: i64,
    },
    /// One of a few options.
    Choice {
        /// The option chosen before the user changes it (one of `options`' values).
        default: String,
        /// The options, in the order shown.
        options: Vec<ChoiceOption>,
    },
    /// A secret, such as an API key: kept in the keychain, never shown again after saving.
    Secret,
}

impl FieldKind {
    fn validate(&self, key: &str) -> Result<(), PluginError> {
        let bad = |m: &str| Err(PluginError::invalid(format!("The setting `{key}` {m}")));
        match self {
            Self::Text { default, max_len } => {
                if *max_len == 0 || default.chars().count() > *max_len as usize {
                    return bad("has a default longer than its limit, or a limit of zero");
                }
            }
            Self::Number { default, min, max } => {
                if min > max || default < min || default > max {
                    return bad("has a default outside its range");
                }
            }
            Self::Choice { default, options } => {
                if options.is_empty() || !options.iter().any(|o| &o.value == default) {
                    return bad("has no options, or a default that isn't one of them");
                }
                for (i, o) in options.iter().enumerate() {
                    check_name("option value", &o.value)?;
                    if options[..i].iter().any(|p| p.value == o.value) {
                        return bad("lists an option twice");
                    }
                }
            }
            Self::Toggle { .. } | Self::Secret => {}
        }
        Ok(())
    }
}

/// One option of a [`FieldKind::Choice`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    /// The stored value.
    pub value: String,
    /// The label shown.
    pub label: String,
}

impl ChoiceOption {
    /// An option.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_good_panel_validates() {
        let p = SettingsPanel::new(vec![
            Field::new(
                "enabled",
                "Send a summary",
                FieldKind::Toggle { default: true },
            ),
            Field::new(
                "tone",
                "Tone",
                FieldKind::Choice {
                    default: "short".into(),
                    options: vec![
                        ChoiceOption::new("short", "Short"),
                        ChoiceOption::new("full", "Full"),
                    ],
                },
            )
            .with_help("How long the summary is"),
            Field::new("api_key", "API key", FieldKind::Secret),
        ]);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn bad_panels_are_refused() {
        let t = |k: &str, kind| SettingsPanel::new(vec![Field::new(k, "Title", kind)]);
        assert!(
            t("Bad-Key", FieldKind::Toggle { default: true })
                .validate()
                .is_err()
        );
        let dup = SettingsPanel::new(vec![
            Field::new("a", "A", FieldKind::Secret),
            Field::new("a", "A", FieldKind::Secret),
        ]);
        assert!(dup.validate().is_err());
        assert!(
            t(
                "n",
                FieldKind::Number {
                    default: 5,
                    min: 1,
                    max: 3
                }
            )
            .validate()
            .is_err()
        );
        assert!(
            t(
                "c",
                FieldKind::Choice {
                    default: "x".into(),
                    options: vec![ChoiceOption::new("y", "Y")]
                }
            )
            .validate()
            .is_err()
        );
        assert!(
            t(
                "s",
                FieldKind::Text {
                    default: "toolong".into(),
                    max_len: 3
                }
            )
            .validate()
            .is_err()
        );
        let untitled = SettingsPanel::new(vec![Field::new("a", " ", FieldKind::Secret)]);
        assert!(untitled.validate().is_err());
    }
}
