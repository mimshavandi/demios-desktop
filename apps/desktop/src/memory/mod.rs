//! Memory store (ADR-0020 §4.1, #346, R3 PR 1): the explicit memory layer, one host-owned
//! list for chats and add-ons.
//!
//! Self-contained so it can become its own crate (`dn-memory`, pending the owner's OK): it
//! uses only `dn-core`, `rusqlite`, `serde` and `serde_json`, and owns its `memory_*` tables
//! through [`migrations`]. Nothing here talks to a model or the network; extraction (PR 2),
//! the core profile and retrieval (PR 3) build on this API.
//!
//! Rules enforced here, not left to callers:
//! - **Trust.** 3 user-stated, 2 user-confirmed, 1 from the person's own content, 0 from
//!   third-party content. A third-party item is a claim about its source (`third_party`),
//!   trust 0, and only a fact, person or commitment.
//! - **Pending.** An inferred item waits for the person's Keep, except one from the person's
//!   own content in a scope with "Remember without asking" on. Third-party items always wait.
//! - **Memory off.** With memory off for the account, nothing is added and nothing is read
//!   ([`read_scopes`] is empty); the list stays editable.
//! - **Scopes.** A project chat reads its project and, with "Use account memory" on, the
//!   account; with it off, the project neither reads nor adds to the account.
//! - **Purge.** Forgetting a source deletes every item whose last source it was (ADR-0013 §5,
//!   #280). Deletes leave tombstones for sync later.

mod store;

pub use store::*;

/// The `memory_*` tables (composed by dn-store's migration runner).
pub fn migrations() -> &'static [&'static str] {
    &[r#"
CREATE TABLE memory_items (
    id            TEXT PRIMARY KEY,
    scope         TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('preference','fact','person','commitment','project','style','correction')),
    subject       TEXT NOT NULL,
    text          TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('pending','active','superseded','retracted')),
    trust         INTEGER NOT NULL CHECK (trust BETWEEN 0 AND 3),
    third_party   INTEGER NOT NULL CHECK (third_party IN (0, 1)),
    created_by    TEXT NOT NULL,
    valid_from    INTEGER NOT NULL,
    valid_to      INTEGER,
    superseded_by TEXT,
    last_used_at  INTEGER,
    last_used_in  TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
CREATE INDEX memory_items_scope ON memory_items (scope, status);
CREATE TABLE memory_versions (
    item_id    TEXT NOT NULL REFERENCES memory_items(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    subject    TEXT NOT NULL,
    scope      TEXT NOT NULL,
    valid_from INTEGER NOT NULL,
    valid_to   INTEGER NOT NULL
);
CREATE INDEX memory_versions_item ON memory_versions (item_id, valid_to);
CREATE TABLE memory_sources (
    item_id         TEXT NOT NULL REFERENCES memory_items(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('message','file','item','addon','settings')),
    ref_id          TEXT NOT NULL,
    conversation_id TEXT,
    title           TEXT NOT NULL,
    at              INTEGER NOT NULL,
    PRIMARY KEY (item_id, kind, ref_id)
);
CREATE INDEX memory_sources_ref ON memory_sources (kind, ref_id);
CREATE INDEX memory_sources_conversation ON memory_sources (conversation_id);
CREATE TABLE memory_used (
    message_id TEXT NOT NULL,
    item_id    TEXT NOT NULL,
    how        TEXT NOT NULL CHECK (how IN ('retrieved','profile','saved')),
    PRIMARY KEY (message_id, item_id)
);
CREATE TABLE memory_settings (
    scope                  TEXT PRIMARY KEY,
    enabled                INTEGER NOT NULL,
    remember_automatically INTEGER NOT NULL,
    use_account            INTEGER NOT NULL
);
CREATE VIRTUAL TABLE memory_fts USING fts5(item_id UNINDEXED, subject, text);
CREATE TABLE memory_tombstones (
    id         TEXT PRIMARY KEY,
    deleted_at INTEGER NOT NULL
);
"#]
}

#[cfg(test)]
mod tests;
