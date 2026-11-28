//! Run results (ADR-0020 §8): what each model run did, stored on this device. A chat turn
//! keeps its result with the answer it wrote; the eval suite and, later, the usage views
//! read them. Only counts, timings and names: never message or tool content. Purged with
//! its chat and by Delete all; not synced.

use dn_core::Id;
use dn_core::chat::Usage;
use dn_core::run::{RunResult, StopReason};
use dn_store::Connection;
use rusqlite::{OptionalExtension, Result as SqlResult, params};

type Result<T> = dn_store::Result<T>;

/// App-owned table.
pub fn migrations() -> &'static [&'static str] {
    &[
        "CREATE TABLE app_runs (
        run_id                TEXT    PRIMARY KEY NOT NULL,
        conversation_id       TEXT,
        message_id            TEXT,
        caller                TEXT    NOT NULL,
        model                 TEXT    NOT NULL,
        stop                  TEXT    NOT NULL,
        steps                 INTEGER NOT NULL,
        input_tokens          INTEGER NOT NULL,
        output_tokens         INTEGER NOT NULL,
        cached_input_tokens   INTEGER NOT NULL,
        cache_write_tokens    INTEGER NOT NULL,
        draft_tokens          INTEGER NOT NULL,
        draft_accepted_tokens INTEGER NOT NULL,
        wall_ms               INTEGER NOT NULL,
        first_token_ms        INTEGER,
        dropped               TEXT    NOT NULL,
        created_at            INTEGER NOT NULL
    );
    CREATE INDEX app_runs_conversation ON app_runs (conversation_id);
    CREATE INDEX app_runs_message ON app_runs (message_id);",
        // How the context assembler used the model's context (ADR-0020 §2), as JSON; NULL for
        // runs that didn't go through it.
        "ALTER TABLE app_runs ADD COLUMN context TEXT;",
    ]
}

fn stop_name(s: StopReason) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn to_sql(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// Stores a run's result, with the chat and answer it belongs to (none for runs outside a
/// chat).
pub fn record(
    conn: &Connection,
    r: &RunResult,
    conversation_id: Option<Id>,
    message_id: Option<Id>,
) -> Result<()> {
    let u = &r.usage;
    conn.execute(
        "INSERT INTO app_runs (run_id, conversation_id, message_id, caller, model, stop, steps,
             input_tokens, output_tokens, cached_input_tokens, cache_write_tokens, draft_tokens,
             draft_accepted_tokens, wall_ms, first_token_ms, dropped, created_at, context)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            r.run_id.to_string(),
            conversation_id.map(|i| i.to_string()),
            message_id.map(|i| i.to_string()),
            r.caller,
            r.model.to_string(),
            stop_name(r.stop),
            r.steps,
            u.input_tokens,
            u.output_tokens,
            u.cached_input_tokens,
            u.cache_write_tokens,
            u.draft_tokens,
            u.draft_accepted_tokens,
            i64::try_from(r.wall_ms).unwrap_or(i64::MAX),
            r.first_token_ms
                .map(|t| i64::try_from(t).unwrap_or(i64::MAX)),
            serde_json::to_string(&r.dropped).map_err(to_sql)?,
            dn_store::now_ms(),
            r.context
                .map(|c| serde_json::to_string(&c))
                .transpose()
                .map_err(to_sql)?,
        ],
    )?;
    Ok(())
}

/// The run that wrote `message_id`, if one was stored.
pub fn for_message(conn: &Connection, message_id: Id) -> Result<Option<RunResult>> {
    conn.query_row(
        "SELECT run_id, caller, model, stop, steps, input_tokens, output_tokens,
             cached_input_tokens, cache_write_tokens, draft_tokens, draft_accepted_tokens,
             wall_ms, first_token_ms, dropped, context
         FROM app_runs WHERE message_id = ?1 ORDER BY created_at DESC LIMIT 1",
        [message_id.to_string()],
        |row| {
            let text = |i: usize| -> SqlResult<serde_json::Value> {
                Ok(serde_json::Value::String(row.get(i)?))
            };
            let dropped: String = row.get(13)?;
            let context: Option<String> = row.get(14)?;
            Ok(RunResult {
                run_id: de(text(0)?, 0)?,
                caller: row.get(1)?,
                model: de(text(2)?, 2)?,
                stop: de(text(3)?, 3)?,
                steps: row.get(4)?,
                usage: Usage {
                    input_tokens: row.get(5)?,
                    output_tokens: row.get(6)?,
                    cached_input_tokens: row.get(7)?,
                    cache_write_tokens: row.get(8)?,
                    draft_tokens: row.get(9)?,
                    draft_accepted_tokens: row.get(10)?,
                },
                wall_ms: row.get::<_, i64>(11)?.max(0) as u64,
                first_token_ms: row.get::<_, Option<i64>>(12)?.map(|t| t.max(0) as u64),
                dropped: serde_json::from_str(&dropped).map_err(|e| bad_column(13, e))?,
                context: context
                    .map(|c| serde_json::from_str(&c))
                    .transpose()
                    .map_err(|e| bad_column(14, e))?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn bad_column(i: usize, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
}

fn de<T: serde::de::DeserializeOwned>(v: serde_json::Value, i: usize) -> SqlResult<T> {
    serde_json::from_value(v).map_err(|e| bad_column(i, e))
}

/// Tokens a run counts against the daily job budget: the whole prompt (cached part
/// included) plus the output.
pub fn budget_tokens(r: &RunResult) -> u64 {
    u64::from(r.usage.input_tokens) + u64::from(r.usage.output_tokens)
}

/// Counts a job's model run against the daily budget (`dn_jobs::budget`, #313). The host's
/// `run_model` calls this for a caller that is a job, with the job run's id and the local
/// day; `run_model` isn't offered yet (ADR-0020 R6, #349), so nothing calls it until then.
pub fn record_job_usage(
    conn: &Connection,
    job_run: Id,
    day: i64,
    r: &RunResult,
) -> dn_jobs::Result<()> {
    dn_jobs::budget::record_usage(conn, job_run, day, budget_tokens(r))
}

/// Forgets the runs of a purged chat. Idempotent.
pub fn forget_chat(conn: &Connection, conversation_id: Id) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM app_runs WHERE conversation_id = ?1",
        [conversation_id.to_string()],
    )?)
}

/// Delete all local data.
pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_runs", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::model::ModelRef;
    use dn_core::run::{DropAction, Dropped, RunMeter};

    fn db() -> Connection {
        dn_store::open_in_memory(&crate::state::migration_sets()).unwrap()
    }

    fn result() -> RunResult {
        let mut r = RunMeter::start(
            Id::new(),
            "chat.reply",
            ModelRef::Byok {
                provider_id: "anthropic".into(),
                model_id: "m".into(),
            },
        )
        .finish(StopReason::Completed);
        r.steps = 2;
        r.usage = Usage {
            input_tokens: 900,
            output_tokens: 40,
            cached_input_tokens: 600,
            cache_write_tokens: 300,
            draft_tokens: 0,
            draft_accepted_tokens: 0,
        };
        r.first_token_ms = Some(120);
        r.dropped = vec![Dropped {
            slot: dn_core::run::Slot::Retrieved,
            source: "f1".into(),
            tokens: 80,
            action: DropAction::Dropped,
        }];
        r.context = Some(dn_core::run::ContextUse {
            budget_tokens: 4915,
            used_tokens: 3100,
            counted_by: dn_core::run::TokenCount::ModelTokenizer,
            compacted: false,
        });
        r
    }

    #[test]
    fn a_jobs_run_counts_its_whole_prompt_and_output_against_the_budget() {
        let db = db();
        let key = dn_jobs::JobKey::new(dn_plugin_api::PluginId::new("notes").unwrap(), "daily");
        dn_jobs::store::set_enabled(&db, &key, true, 1).unwrap();
        let job_run = dn_jobs::store::start_run(&db, &key, 5, false, 1, 6).unwrap();
        let r = result(); // 900 in (600 of them cached), 40 out
        record_job_usage(&db, job_run, 7, &r).unwrap();
        record_job_usage(&db, job_run, 7, &r).unwrap();
        assert_eq!(dn_jobs::budget::used(&db, 7).unwrap(), 2 * 940);
        assert_eq!(dn_jobs::budget::used(&db, 8).unwrap(), 0);
    }

    #[test]
    fn a_stored_run_reads_back_whole_and_goes_with_its_chat_and_delete_all() {
        let db = db();
        let (conv, msg, other) = (Id::new(), Id::new(), Id::new());
        let r = result();
        record(&db, &r, Some(conv), Some(msg)).unwrap();
        record(&db, &result(), Some(other), Some(Id::new())).unwrap();
        record(&db, &result(), None, None).unwrap();
        assert_eq!(for_message(&db, msg).unwrap(), Some(r));
        assert_eq!(forget_chat(&db, conv).unwrap(), 1);
        assert_eq!(for_message(&db, msg).unwrap(), None);
        let count = |db: &Connection| -> i64 {
            db.query_row("SELECT COUNT(*) FROM app_runs", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count(&db), 2);
        delete_all(&db).unwrap();
        assert_eq!(count(&db), 0);
    }
}
