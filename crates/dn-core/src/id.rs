use serde::{Deserialize, Serialize};
use std::fmt;

/// Time-ordered UUIDv7 identifier. Stable across devices so records can sync (ADR-0007/0008).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub uuid::Uuid);

impl Id {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
