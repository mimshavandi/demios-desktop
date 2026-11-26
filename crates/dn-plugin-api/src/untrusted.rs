//! Untrusted content (ADR-0013 §4).
//!
//! Every host result that carries text someone else wrote or a model produced is an
//! [`Untrusted<T>`]: chat messages, tool outputs, memory items, knowledge hits, model output
//! and HTTP responses. The model service takes such text only inside a [`Quoted`] block,
//! and its instructions are plain `String`s, so untrusted text can't become the instructions
//! of that call by accident.
//!
//! [`Untrusted::into_inner`] gives the value out. It is a named call so reviews can find
//! every place an add-on does that. What the add-on does with the text afterwards (for
//! example passing it on as instructions, or as tool or HTTP arguments) is a review rule:
//! the types don't follow it.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A value that came from outside the add-on and must be treated as data, never as
/// instructions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Untrusted<T>(T);

impl<T> Untrusted<T> {
    /// Marks `value` as untrusted. The host calls this for every result it hands out.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Takes the value out. Review every call: from here on the types no longer mark it.
    pub fn into_inner(self) -> T {
        self.0
    }

    /// Changes the value inside without taking it out (for example to pick a field).
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Untrusted<U> {
        Untrusted(f(self.0))
    }

    /// A reference to the value, still marked.
    pub fn as_ref(&self) -> Untrusted<&T> {
        Untrusted(&self.0)
    }
}

impl<T> fmt::Debug for Untrusted<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Untrusted").field(&self.0).finish()
    }
}

/// Untrusted text with where it came from, for the model service. The host renders each
/// block as a quoted, cited excerpt with a note that it is data, not instructions (the same
/// format as project search uses), and neutralises anything in the text that would close the
/// quote early.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quoted {
    /// Where the text came from, shown in the citation ("chat: Weekly plan", "example.com").
    pub source: String,
    /// The text.
    pub text: Untrusted<String>,
}

impl Quoted {
    /// A block of `text` from `source`.
    pub fn new(source: impl Into<String>, text: Untrusted<String>) -> Self {
        Self {
            source: source.into(),
            text,
        }
    }
}
