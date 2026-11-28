//! Reading memory into a prompt (ADR-0020 §4.1, #346, R3 PR 3): the core profile, the items
//! retrieved per answer, and which ones the answer used (decision (b) on #346).
//!
//! Agreed with the runtime's owner (#346):
//! - **The profile** goes in `Parts.profile`, which is rendered unquoted, so it holds only
//!   what the person said or confirmed (trust ≥ 2), never a claim from someone else's
//!   content. It's built to fit the policy's `profile_cap()` in the runtime's model-independent
//!   units (the caller passes `dn_runtime::estimate::units`), whole items in a fixed order, so
//!   the cached prefix only changes when the list does.
//! - **Retrieved items** go in `Parts.retrieved` as untrusted blocks named `M<n>`, so a
//!   citation `[M3]` matches its block. Claims from someone else's content come only this way.
//! - **Used:** every retrieved item counts; a profile item counts when the answer cites it.
//!   The citation instruction ([`CITE_INSTRUCTION`]) is the same on every turn while memory
//!   is on for the chat, so the prompt prefix stays stable.

use super::store::{self, Item, Kind, Result, Scope, Status, Trust};
use dn_core::Id;
use dn_core::run::{MemoryUse, UsedMemory};
use dn_core::untrusted::SourceRef;
use rusqlite::Connection;

/// Added to the system part whenever memory is on for the chat (fixed, so caching holds).
/// Meant to join the shared citation rule (answer.rules, #371).
pub const CITE_INSTRUCTION: &str = "When a statement relies on something you remember about \
     the person, put its label after it, like [M2]. Use only labels you were given.";

/// The core profile and the labels of its items.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub text: String,
    /// `M<n>` → item id, in profile order.
    pub labels: Vec<(String, String)>,
}

/// An item retrieved for this answer, with its label.
#[derive(Clone, Debug, PartialEq)]
pub struct Retrieved {
    pub label: String,
    pub item: Item,
}

impl Retrieved {
    /// The untrusted block's source and text (for `dn_runtime::Item`).
    pub fn block(&self) -> (SourceRef, String) {
        let mut s = SourceRef::new(&self.label)
            .attr("kind", self.item.kind.as_str())
            .attr("about", &self.item.subject);
        if self.item.third_party {
            s = s.attr("from", "someone else's content");
        }
        (s, self.item.text.clone())
    }
}

fn kind_rank(k: Kind) -> usize {
    [
        Kind::Preference,
        Kind::Style,
        Kind::Correction,
        Kind::Fact,
        Kind::Person,
        Kind::Project,
        Kind::Commitment,
    ]
    .iter()
    .position(|x| *x == k)
    .unwrap_or(usize::MAX)
}

fn in_scope(conn: &Connection, scopes: &[Scope]) -> Result<Vec<Item>> {
    let mut all = Vec::new();
    for s in scopes {
        all.extend(store::list(
            conn,
            &store::Filter {
                status: Some(Status::Active),
                scope: Some(s.clone()),
                ..Default::default()
            },
        )?);
    }
    Ok(all.into_iter().filter(|i| i.valid_to.is_none()).collect())
}

/// The core profile for `scopes` (from [`store::read_scopes`]; empty with memory off or in
/// incognito), within `cap` as measured by `measure`. `None` when nothing qualifies.
pub fn profile(
    conn: &Connection,
    scopes: &[Scope],
    cap: f64,
    measure: impl Fn(&str) -> f64,
) -> Result<Option<Profile>> {
    let mut items: Vec<Item> = in_scope(conn, scopes)?
        .into_iter()
        .filter(|i| i.trust >= Trust::UserConfirmed && !i.third_party)
        .collect();
    items.sort_by(|a, b| {
        (kind_rank(a.kind), &a.subject, &a.id).cmp(&(kind_rank(b.kind), &b.subject, &b.id))
    });
    let head = "What you remember about the person:";
    let mut text = head.to_owned();
    let mut labels = Vec::new();
    for it in items {
        let line = format!(
            "\n[M{}] ({}, {}) {}",
            labels.len() + 1,
            it.kind.as_str(),
            it.subject.replace('\n', " "),
            it.text.replace('\n', " ")
        );
        // Whole items only; one that doesn't fit is left to retrieval.
        if measure(&text) + measure(&line) > cap {
            continue;
        }
        text.push_str(&line);
        labels.push((format!("M{}", labels.len() + 1), it.id));
    }
    Ok((!labels.is_empty()).then_some(Profile { text, labels }))
}

/// Up to `k` items matching `input`, in scope and not in the profile, best first, labelled
/// after the profile's (`M<n+1>`…).
pub fn retrieve(
    conn: &Connection,
    scopes: &[Scope],
    input: &str,
    profile: Option<&Profile>,
    k: usize,
) -> Result<Vec<Retrieved>> {
    if scopes.is_empty() || k == 0 {
        return Ok(Vec::new());
    }
    let in_profile: Vec<&str> = profile
        .map(|p| p.labels.iter().map(|(_, id)| id.as_str()).collect())
        .unwrap_or_default();
    let allowed: Vec<Item> = in_scope(conn, scopes)?;
    let hits = store::search_ranked(conn, input, 4 * k)?;
    let first = profile.map_or(0, |p| p.labels.len());
    let mut out = Vec::new();
    for id in hits {
        if in_profile.contains(&id.as_str()) {
            continue;
        }
        if let Some(it) = allowed.iter().find(|i| i.id == id) {
            out.push(Retrieved {
                label: format!("M{}", first + out.len() + 1),
                item: it.clone(),
            });
            if out.len() == k {
                break;
            }
        }
    }
    Ok(out)
}

/// The answer without its memory labels, and the labels it cited (known ones only, in
/// order, each once).
pub fn cited(answer: &str, known: &[&str]) -> (String, Vec<String>) {
    let mut text = String::with_capacity(answer.len());
    let mut found: Vec<String> = Vec::new();
    let mut rest = answer;
    while let Some(i) = rest.find("[M") {
        let after = &rest[i + 2..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let label = format!("M{digits}");
        if !digits.is_empty()
            && after[digits.len()..].starts_with(']')
            && known.contains(&label.as_str())
        {
            text.push_str(rest[..i].trim_end_matches(' '));
            if !found.contains(&label) {
                found.push(label);
            }
            rest = &after[digits.len() + 1..];
        } else {
            text.push_str(&rest[..i + 2]);
            rest = after;
        }
    }
    text.push_str(rest);
    (text, found)
}

fn id_of(s: &str) -> Option<Id> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

/// What the answer used: every retrieved item, and profile items it cited.
pub fn used(
    profile: Option<&Profile>,
    retrieved: &[Retrieved],
    cited: &[String],
) -> Vec<UsedMemory> {
    let mut out = Vec::new();
    for r in retrieved {
        if let Some(id) = id_of(&r.item.id) {
            out.push(UsedMemory {
                id,
                how: MemoryUse::Retrieved,
            });
        }
    }
    if let Some(p) = profile {
        for (label, item) in &p.labels {
            if cited.contains(label)
                && let Some(id) = id_of(item)
            {
                out.push(UsedMemory {
                    id,
                    how: MemoryUse::Profile,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Settings, Source, SourceKind, Suggestion, migrations};

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for m in migrations() {
            c.execute_batch(m).unwrap();
        }
        c
    }

    fn chars(s: &str) -> f64 {
        s.chars().count() as f64
    }

    fn claim(c: &Connection, text: &str) -> Item {
        let it = store::suggest(
            c,
            Suggestion {
                scope: Scope::Account,
                kind: Kind::Commitment,
                subject: "Review".into(),
                text: text.into(),
                third_party: true,
                created_by: "model:qwen@memory-extract-v1".into(),
                sources: vec![Source {
                    kind: SourceKind::Message,
                    ref_id: "m1".into(),
                    conversation_id: Some("c1".into()),
                    title: "Mail".into(),
                    at: 1,
                }],
            },
        )
        .unwrap();
        store::keep(c, &it.id).unwrap()
    }

    #[test]
    fn the_profile_holds_only_the_persons_items_in_a_fixed_order_within_the_cap() {
        let c = db();
        store::add_user(
            &c,
            Scope::Account,
            Kind::Fact,
            "you",
            "Lives in Lisbon",
            None,
        )
        .unwrap();
        store::add_user(
            &c,
            Scope::Account,
            Kind::Preference,
            "you",
            "Prefers metric units",
            None,
        )
        .unwrap();
        store::add_user(
            &c,
            Scope::Project("p1".into()),
            Kind::Fact,
            "trip",
            "Flies on 3 May",
            None,
        )
        .unwrap();
        let kept = claim(&c, "Alice says the review moved to Friday");
        assert_eq!(kept.trust, Trust::UserConfirmed, "kept, yet still a claim");
        let p = profile(&c, &[Scope::Account], 10_000.0, chars)
            .unwrap()
            .unwrap();
        assert_eq!(
            p.text,
            "What you remember about the person:\n[M1] (preference, you) Prefers metric units\n[M2] (fact, you) Lives in Lisbon"
        );
        assert!(
            !p.text.contains("Alice"),
            "a claim never goes in the unquoted profile"
        );
        // The same list gives the same bytes.
        assert_eq!(
            profile(&c, &[Scope::Account], 10_000.0, chars)
                .unwrap()
                .unwrap(),
            p
        );
        // Whole items only, within the cap.
        let small = profile(&c, &[Scope::Account], 80.0, chars)
            .unwrap()
            .unwrap();
        assert_eq!(small.labels.len(), 1);
        assert!(chars(&small.text) <= 80.0);
        assert!(
            profile(&c, &[], 10_000.0, chars).unwrap().is_none(),
            "memory off or incognito"
        );
    }

    #[test]
    fn retrieval_skips_the_profile_quotes_claims_and_labels_after_the_profile() {
        let c = db();
        store::add_user(
            &c,
            Scope::Account,
            Kind::Preference,
            "you",
            "Prefers metric units",
            None,
        )
        .unwrap();
        let claim = claim(&c, "Alice says the review moved to Friday");
        let p = profile(&c, &[Scope::Account], 10_000.0, chars)
            .unwrap()
            .unwrap();
        let r = retrieve(
            &c,
            &[Scope::Account],
            "When is the review? metric?",
            Some(&p),
            3,
        )
        .unwrap();
        assert_eq!(r.len(), 1, "the profile's item isn't retrieved again");
        assert_eq!(
            (r[0].label.as_str(), r[0].item.id.as_str()),
            ("M2", claim.id.as_str())
        );
        let (src, text) = r[0].block();
        assert_eq!(src.id, "M2");
        assert!(src.attrs.iter().any(|(k, _)| k == "from"));
        assert!(text.contains("Alice says"));
        // Out of scope: a project chat with use_account off.
        store::set_settings(
            &c,
            &Scope::Project("p1".into()),
            Settings {
                use_account: false,
                ..Default::default()
            },
        )
        .unwrap();
        let scopes = store::read_scopes(&c, Some("p1")).unwrap();
        assert!(retrieve(&c, &scopes, "review", None, 3).unwrap().is_empty());
    }

    #[test]
    fn citations_are_stripped_and_only_known_labels_count() {
        let (t, c) = cited(
            "You prefer metric [M1]. Lisbon [M2][M1], see [M9] and [Mx].",
            &["M1", "M2"],
        );
        assert_eq!(t, "You prefer metric. Lisbon, see [M9] and [Mx].");
        assert_eq!(c, ["M1", "M2"]);
        let (t, c) = cited("Nothing cited.", &["M1"]);
        assert_eq!((t.as_str(), c.len()), ("Nothing cited.", 0));
    }

    #[test]
    fn used_lists_every_retrieved_item_and_only_cited_profile_items() {
        let c = db();
        let a = store::add_user(
            &c,
            Scope::Account,
            Kind::Preference,
            "you",
            "Prefers metric units",
            None,
        )
        .unwrap();
        store::add_user(
            &c,
            Scope::Account,
            Kind::Fact,
            "you",
            "Lives in Lisbon",
            None,
        )
        .unwrap();
        let x = claim(&c, "Alice says the review moved to Friday");
        let p = profile(&c, &[Scope::Account], 10_000.0, chars)
            .unwrap()
            .unwrap();
        let r = retrieve(&c, &[Scope::Account], "review", Some(&p), 3).unwrap();
        let u = used(Some(&p), &r, &["M1".into()]);
        let ids: Vec<(String, MemoryUse)> = u.iter().map(|m| (m.id.to_string(), m.how)).collect();
        assert_eq!(
            ids,
            [(x.id, MemoryUse::Retrieved), (a.id, MemoryUse::Profile)]
        );
    }
}
