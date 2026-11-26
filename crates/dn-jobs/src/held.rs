//! The held-approvals queue (ADR-0013 §2, ADR-0014 §6).
//!
//! A job's tool call that isn't set to "run without asking" is **held**: stored with the
//! add-on, the job and its exact arguments, and shown on whichever device the user picks up.
//!
//! - **Never approved by itself.** Only [`answer`] moves a call out of `held`, and only
//!   with an answer the host's [`AnswerVerifier`] accepts. On a Node, that's a signature
//!   from a current member of the device group.
//! - **The answer names the exact arguments** (SHA-256 of the stored argument text), so an
//!   answer can't approve a different call.
//! - **It applies once.** `held → approved` happens once, and [`take_approved`] hands an
//!   approved call to the runner once. A replayed or second answer finds nothing held.
//! - **Expiry is a refusal, never a run.** A call past its expiry becomes `expired`
//!   ("expired, not run"), and a late answer is refused.
//! - **"Delete all" empties it** ([`delete_all`]).

use crate::{JobKey, JobsError, Result, parse_id, parse_plugin};
use dn_core::Id;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) const MIGRATION: &str = "
CREATE TABLE held_approvals (
    id               TEXT    PRIMARY KEY NOT NULL,
    plugin_id        TEXT    NOT NULL,
    job_id           TEXT    NOT NULL,
    run_id           TEXT    NOT NULL,
    tool             TEXT    NOT NULL,
    -- The arguments exactly as they will run, and the SHA-256 (hex) of that text.
    arguments        TEXT    NOT NULL,
    arguments_sha256 TEXT    NOT NULL,
    summary          TEXT    NOT NULL,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER,
    state            TEXT    NOT NULL CHECK (state IN
        ('held', 'approved', 'denied', 'dismissed', 'expired', 'running', 'ran', 'failed')),
    answered_by      TEXT,
    answered_at      INTEGER,
    signature        BLOB,
    error            TEXT
);
CREATE INDEX held_approvals_state ON held_approvals (state, created_at);
";

/// Where a held call is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldState {
    Held,
    Approved,
    Denied,
    Dismissed,
    /// "Expired, not run."
    Expired,
    Running,
    Ran,
    Failed,
}

impl HeldState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Dismissed => "dismissed",
            Self::Expired => "expired",
            Self::Running => "running",
            Self::Ran => "ran",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "held" => Self::Held,
            "approved" => Self::Approved,
            "denied" => Self::Denied,
            "dismissed" => Self::Dismissed,
            "expired" => Self::Expired,
            "running" => Self::Running,
            "ran" => Self::Ran,
            "failed" => Self::Failed,
            other => return Err(JobsError::Damaged(format!("held state `{other}`"))),
        })
    }
}

/// A held call, as every device shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HeldApproval {
    pub id: Id,
    pub job: JobKey,
    pub run: Id,
    /// The capability id, e.g. `plugin.brief.send_mail`.
    pub tool: String,
    /// The exact arguments, as stored text (JSON).
    pub arguments: String,
    pub arguments_sha256: String,
    /// What the call does, in plain words (shown as plain text, never markup).
    pub summary: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub state: HeldState,
    pub answered_by: Option<String>,
    pub answered_at: Option<i64>,
    pub error: Option<String>,
}

/// A call to hold.
#[derive(Clone, Debug)]
pub struct HoldRequest {
    pub job: JobKey,
    pub run: Id,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub summary: String,
}

/// An answer from a device.
#[derive(Clone, Debug)]
pub struct Answer {
    pub approval: Id,
    pub approve: bool,
    /// SHA-256 (hex) of the arguments the device showed.
    pub arguments_sha256: String,
    /// Who answered, as the host names devices (a device id; see [`AnswerVerifier`]).
    pub answered_by: String,
    /// Signature over [`answer_message`], when the host requires one.
    pub signature: Option<Vec<u8>>,
}

/// The exact bytes a device signs to answer a held call.
pub fn answer_message(approval: Id, approve: bool, arguments_sha256: &str) -> Vec<u8> {
    format!(
        "dn-held-answer v1\n{approval}\n{}\n{arguments_sha256}",
        if approve { "approve" } else { "deny" }
    )
    .into_bytes()
}

/// Decides whether an answer really comes from someone allowed to answer.
///
/// - A Node checks the signature against the device group, refusing removed devices.
/// - The desktop app with no Node accepts answers from its own window only.
pub trait AnswerVerifier {
    fn verify(
        &self,
        answered_by: &str,
        message: &[u8],
        signature: Option<&[u8]>,
    ) -> std::result::Result<(), String>;
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const COLUMNS: &str = "id, plugin_id, job_id, run_id, tool, arguments, arguments_sha256, summary,
                       created_at, expires_at, state, answered_by, answered_at, error";

/// The columns as stored; [`RawRow::parse`] checks them.
struct RawRow {
    id: String,
    plugin: String,
    job: String,
    run: String,
    tool: String,
    arguments: String,
    arguments_sha256: String,
    summary: String,
    created_at: i64,
    expires_at: Option<i64>,
    state: String,
    answered_by: Option<String>,
    answered_at: Option<i64>,
    error: Option<String>,
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        id: r.get(0)?,
        plugin: r.get(1)?,
        job: r.get(2)?,
        run: r.get(3)?,
        tool: r.get(4)?,
        arguments: r.get(5)?,
        arguments_sha256: r.get(6)?,
        summary: r.get(7)?,
        created_at: r.get(8)?,
        expires_at: r.get(9)?,
        state: r.get(10)?,
        answered_by: r.get(11)?,
        answered_at: r.get(12)?,
        error: r.get(13)?,
    })
}

impl RawRow {
    fn parse(self) -> Result<HeldApproval> {
        Ok(HeldApproval {
            id: parse_id(&self.id)?,
            job: JobKey::new(parse_plugin(&self.plugin)?, self.job),
            run: parse_id(&self.run)?,
            tool: self.tool,
            arguments: self.arguments,
            arguments_sha256: self.arguments_sha256,
            summary: self.summary,
            created_at: self.created_at,
            expires_at: self.expires_at,
            state: HeldState::parse(&self.state)?,
            answered_by: self.answered_by,
            answered_at: self.answered_at,
            error: self.error,
        })
    }
}

/// Holds a call. `expires_at` is the user's choice; `None` means it waits until answered
/// or dismissed.
pub fn hold(
    conn: &Connection,
    request: &HoldRequest,
    expires_at: Option<i64>,
    now: i64,
) -> Result<HeldApproval> {
    let id = Id::new();
    let arguments =
        serde_json::to_string(&request.arguments).map_err(|e| JobsError::Damaged(e.to_string()))?;
    conn.execute(
        "INSERT INTO held_approvals (id, plugin_id, job_id, run_id, tool, arguments,
                                     arguments_sha256, summary, created_at, expires_at, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'held')",
        params![
            id.to_string(),
            request.job.plugin.as_str(),
            request.job.job,
            request.run.to_string(),
            request.tool,
            arguments,
            sha256_hex(&arguments),
            request.summary,
            now,
            expires_at
        ],
    )?;
    get(conn, id)
}

pub fn get(conn: &Connection, id: Id) -> Result<HeldApproval> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM held_approvals WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(JobsError::UnknownApproval)?
    .parse()
}

/// Marks every held call past its expiry as `expired` ("expired, not run").
pub fn expire_due(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE held_approvals SET state = 'expired'
         WHERE state = 'held' AND expires_at IS NOT NULL AND expires_at <= ?1",
        params![now],
    )?)
}

/// The calls waiting for an answer, oldest first. Expired ones are moved out first.
pub fn waiting(conn: &Connection, now: i64) -> Result<Vec<HeldApproval>> {
    expire_due(conn, now)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM held_approvals WHERE state = 'held' ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    rows.map(|row| row?.parse()).collect()
}

/// Applies an answer once. Refuses an expired, already settled or unknown call, an answer
/// for other arguments, and anything the verifier doesn't accept.
pub fn answer(
    conn: &Connection,
    verifier: &dyn AnswerVerifier,
    answer: &Answer,
    now: i64,
) -> Result<HeldState> {
    let tx = conn.unchecked_transaction()?;
    let current = get(&tx, answer.approval)?;
    if current.state == HeldState::Held && current.expires_at.is_some_and(|e| e <= now) {
        expire_due(&tx, now)?;
        tx.commit()?;
        return Err(JobsError::Expired);
    }
    match current.state {
        HeldState::Held => {}
        HeldState::Expired => return Err(JobsError::Expired),
        _ => return Err(JobsError::NotHeld),
    }
    if answer.arguments_sha256 != current.arguments_sha256 {
        return Err(JobsError::ArgumentsChanged);
    }
    let message = answer_message(answer.approval, answer.approve, &answer.arguments_sha256);
    verifier
        .verify(&answer.answered_by, &message, answer.signature.as_deref())
        .map_err(JobsError::BadAnswer)?;
    let state = if answer.approve {
        HeldState::Approved
    } else {
        HeldState::Denied
    };
    let changed = tx.execute(
        "UPDATE held_approvals SET state = ?2, answered_by = ?3, answered_at = ?4, signature = ?5
         WHERE id = ?1 AND state = 'held'",
        params![
            answer.approval.to_string(),
            state.as_str(),
            answer.answered_by,
            now,
            answer.signature
        ],
    )?;
    if changed != 1 {
        return Err(JobsError::NotHeld);
    }
    tx.commit()?;
    Ok(state)
}

/// The user dismissed a held call: it won't run.
pub fn dismiss(conn: &Connection, id: Id) -> Result<()> {
    let changed = conn.execute(
        "UPDATE held_approvals SET state = 'dismissed' WHERE id = ?1 AND state = 'held'",
        [id.to_string()],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        get(conn, id).and(Err(JobsError::NotHeld))
    }
}

/// Hands an approved call to the runner, once. A second call gets [`JobsError::NotApproved`].
pub fn take_approved(conn: &Connection, id: Id) -> Result<HeldApproval> {
    let changed = conn.execute(
        "UPDATE held_approvals SET state = 'running' WHERE id = ?1 AND state = 'approved'",
        [id.to_string()],
    )?;
    if changed != 1 {
        return get(conn, id).and(Err(JobsError::NotApproved));
    }
    get(conn, id)
}

/// Records how an approved call went.
pub fn finish(conn: &Connection, id: Id, result: std::result::Result<(), String>) -> Result<()> {
    let (state, error) = match result {
        Ok(()) => (HeldState::Ran, None),
        Err(e) => (HeldState::Failed, Some(e)),
    };
    let changed = conn.execute(
        "UPDATE held_approvals SET state = ?2, error = ?3 WHERE id = ?1 AND state = 'running'",
        params![id.to_string(), state.as_str(), error],
    )?;
    if changed != 1 {
        return Err(JobsError::NotApproved);
    }
    Ok(())
}

/// Deletes one add-on's held calls.
pub fn delete_plugin(conn: &Connection, plugin: &dn_plugin_api::PluginId) -> Result<()> {
    conn.execute(
        "DELETE FROM held_approvals WHERE plugin_id = ?1",
        [plugin.as_str()],
    )?;
    Ok(())
}

/// "Delete all local data": the whole queue.
pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM held_approvals", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{db, key};

    /// Accepts answers signed "ok:<device>" by the devices it lists.
    struct Devices(&'static [&'static str]);
    impl AnswerVerifier for Devices {
        fn verify(
            &self,
            by: &str,
            message: &[u8],
            signature: Option<&[u8]>,
        ) -> std::result::Result<(), String> {
            if !self.0.contains(&by) {
                return Err(format!("{by} isn't a paired device"));
            }
            let mut want = format!("ok:{by}:").into_bytes();
            want.extend_from_slice(message);
            (signature == Some(want.as_slice()))
                .then_some(())
                .ok_or_else(|| "bad signature".into())
        }
    }

    fn sign(by: &str, approval: Id, approve: bool, sha: &str) -> Vec<u8> {
        let mut sig = format!("ok:{by}:").into_bytes();
        sig.extend_from_slice(&answer_message(approval, approve, sha));
        sig
    }

    fn request() -> HoldRequest {
        HoldRequest {
            job: key("daily"),
            run: Id::new(),
            tool: "plugin.brief.send_mail".into(),
            arguments: serde_json::json!({"to": "team@example.com", "subject": "Brief"}),
            summary: "Send the morning brief to team@example.com".into(),
        }
    }

    fn answer_for(h: &HeldApproval, by: &str, approve: bool) -> Answer {
        Answer {
            approval: h.id,
            approve,
            arguments_sha256: h.arguments_sha256.clone(),
            answered_by: by.into(),
            signature: Some(sign(by, h.id, approve, &h.arguments_sha256)),
        }
    }

    const PHONE: Devices = Devices(&["phone", "laptop"]);

    #[test]
    fn a_held_call_waits_and_is_never_approved_by_itself() {
        let conn = db();
        let h = hold(&conn, &request(), None, 100).unwrap();
        assert_eq!(h.state, HeldState::Held);
        assert_eq!(
            waiting(&conn, 1_000_000).unwrap().len(),
            1,
            "no expiry, no timeout"
        );
        assert!(matches!(
            take_approved(&conn, h.id),
            Err(JobsError::NotApproved)
        ));
    }

    #[test]
    fn a_signed_answer_applies_once_and_the_call_runs_once() {
        let conn = db();
        let h = hold(&conn, &request(), None, 100).unwrap();
        let a = answer_for(&h, "phone", true);
        assert_eq!(answer(&conn, &PHONE, &a, 110).unwrap(), HeldState::Approved);
        // Replay of the same answer, or a second device's answer: nothing is held.
        assert!(matches!(
            answer(&conn, &PHONE, &a, 111),
            Err(JobsError::NotHeld)
        ));
        let other = answer_for(&h, "laptop", false);
        assert!(matches!(
            answer(&conn, &PHONE, &other, 112),
            Err(JobsError::NotHeld)
        ));
        let taken = take_approved(&conn, h.id).unwrap();
        assert_eq!(taken.answered_by.as_deref(), Some("phone"));
        assert!(matches!(
            take_approved(&conn, h.id),
            Err(JobsError::NotApproved)
        ));
        finish(&conn, h.id, Ok(())).unwrap();
        assert_eq!(get(&conn, h.id).unwrap().state, HeldState::Ran);
    }

    #[test]
    fn forged_unsigned_or_removed_device_answers_are_refused() {
        let conn = db();
        let h = hold(&conn, &request(), None, 100).unwrap();
        let mut forged = answer_for(&h, "phone", true);
        forged.signature = Some(sign("phone", h.id, false, &h.arguments_sha256));
        assert!(matches!(
            answer(&conn, &PHONE, &forged, 101),
            Err(JobsError::BadAnswer(_))
        ));
        let mut unsigned = answer_for(&h, "phone", true);
        unsigned.signature = None;
        assert!(matches!(
            answer(&conn, &PHONE, &unsigned, 101),
            Err(JobsError::BadAnswer(_))
        ));
        // A removed device's valid-looking answer.
        let removed = answer_for(&h, "stolen-tablet", true);
        assert!(matches!(
            answer(&conn, &PHONE, &removed, 101),
            Err(JobsError::BadAnswer(_))
        ));
        assert_eq!(get(&conn, h.id).unwrap().state, HeldState::Held);
    }

    #[test]
    fn an_answer_for_other_arguments_is_refused() {
        let conn = db();
        let h = hold(&conn, &request(), None, 100).unwrap();
        let mut other = request();
        other.arguments = serde_json::json!({"to": "attacker@example.com"});
        let o = hold(&conn, &other, None, 100).unwrap();
        let mut a = answer_for(&h, "phone", true);
        a.arguments_sha256 = o.arguments_sha256.clone();
        a.signature = Some(sign("phone", h.id, true, &o.arguments_sha256));
        assert!(matches!(
            answer(&conn, &PHONE, &a, 101),
            Err(JobsError::ArgumentsChanged)
        ));
    }

    #[test]
    fn expiry_is_a_refusal_never_a_run() {
        let conn = db();
        let h = hold(&conn, &request(), Some(200), 100).unwrap();
        let a = answer_for(&h, "phone", true);
        assert!(matches!(
            answer(&conn, &PHONE, &a, 200),
            Err(JobsError::Expired)
        ));
        assert_eq!(get(&conn, h.id).unwrap().state, HeldState::Expired);
        assert!(waiting(&conn, 300).unwrap().is_empty());
        assert!(matches!(
            take_approved(&conn, h.id),
            Err(JobsError::NotApproved)
        ));
        let h2 = hold(&conn, &request(), Some(500), 100).unwrap();
        assert_eq!(expire_due(&conn, 499).unwrap(), 0);
        assert_eq!(expire_due(&conn, 500).unwrap(), 1);
        assert_eq!(get(&conn, h2.id).unwrap().state, HeldState::Expired);
    }

    #[test]
    fn dismissed_and_denied_calls_never_run() {
        let conn = db();
        let h = hold(&conn, &request(), None, 100).unwrap();
        dismiss(&conn, h.id).unwrap();
        assert!(matches!(dismiss(&conn, h.id), Err(JobsError::NotHeld)));
        assert!(matches!(
            take_approved(&conn, h.id),
            Err(JobsError::NotApproved)
        ));
        let d = hold(&conn, &request(), None, 100).unwrap();
        assert_eq!(
            answer(&conn, &PHONE, &answer_for(&d, "phone", false), 101).unwrap(),
            HeldState::Denied
        );
        assert!(matches!(
            take_approved(&conn, d.id),
            Err(JobsError::NotApproved)
        ));
        assert!(matches!(
            dismiss(&conn, Id::new()),
            Err(JobsError::UnknownApproval)
        ));
    }

    #[test]
    fn delete_all_empties_the_queue() {
        let conn = db();
        hold(&conn, &request(), None, 100).unwrap();
        let mut other = request();
        other.job = crate::JobKey::new(dn_plugin_api::PluginId::new("notes").unwrap(), "x");
        hold(&conn, &other, None, 100).unwrap();
        delete_plugin(&conn, &key("daily").plugin).unwrap();
        assert_eq!(waiting(&conn, 100).unwrap().len(), 1);
        delete_all(&conn).unwrap();
        assert!(waiting(&conn, 100).unwrap().is_empty());
    }

    #[test]
    fn the_signed_message_names_the_call_the_decision_and_the_arguments() {
        let id = Id::new();
        let m = String::from_utf8(answer_message(id, true, "ab")).unwrap();
        assert_eq!(m, format!("dn-held-answer v1\n{id}\napprove\nab"));
        assert_ne!(
            answer_message(id, true, "ab"),
            answer_message(id, false, "ab")
        );
    }
}
