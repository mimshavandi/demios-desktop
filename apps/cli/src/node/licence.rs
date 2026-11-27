//! The Node's licence (ADR-0014 §9a, #377): a Pro or trial licence, checked offline.
//!
//! - The licence file is kept in the Node's secret store (ADR-0013 §6), with the latest time
//!   the Node has seen, so moving the clock back never buys time ([`dn_licence::checked_now`]).
//! - It's checked when the Node starts and again at least once a day, and at the moment it
//!   ends. Nothing is ever sent anywhere.
//! - Without a licence, or with one that can't be used, the Node doesn't start and pairing
//!   refuses. A licence that ended keeps the Node answering devices' status requests (the
//!   owner's choice, 2026-11-27) so they can say why, but it doesn't sync, run jobs or pair.
//!   Nothing is locked or deleted.

use std::sync::RwLock;

use dn_licence::{Kind, Licence, PublicKey, Status};
use dn_secrets::{Secret, SecretId, SecretStore};

/// Where the licence file is kept in the secret store.
pub fn licence_id() -> SecretId {
    SecretId::device("licence")
}

/// The latest time (Unix seconds) the Node has seen while checking its licence.
pub fn seen_id() -> SecretId {
    SecretId::device("licence.seen")
}

/// How often the licence is checked while the Node runs.
pub const CHECK_SECS: i64 = 86_400;

/// Where to get a licence, in every message that asks for one.
pub const GET_ONE: &str = "Get one at demios.io/pro";

/// The keys this build trusts. Tests use their own key.
pub fn trusted() -> &'static [PublicKey] {
    #[cfg(not(test))]
    {
        dn_licence::TRUSTED
    }
    #[cfg(test)]
    {
        test_key::keys()
    }
}

/// Reads and checks the licence at `now` (Unix seconds), and records the time seen.
pub fn check(store: &dyn SecretStore, keys: &[PublicKey], now: i64) -> Status {
    let text = match store.get(&licence_id()) {
        Ok(Some(t)) => t,
        Ok(None) => return Status::Missing,
        Err(e) => {
            return Status::Unusable {
                reason: format!("the secret store couldn't be read ({e})"),
            };
        }
    };
    let licence = match dn_licence::verify(text.expose().as_bytes(), keys) {
        Ok(l) => l,
        Err(e) => return Status::unusable(&e),
    };
    let seen = match store.get(&seen_id()) {
        Ok(s) => s.and_then(|s| s.expose().trim().parse::<i64>().ok()),
        Err(e) => {
            return Status::Unusable {
                reason: format!("the secret store couldn't be read ({e})"),
            };
        }
    };
    let at = match dn_licence::checked_now(now, seen) {
        Ok(t) => t,
        Err(e) => return Status::unusable(&e),
    };
    if seen != Some(at)
        && let Err(e) = store.set(&seen_id(), &Secret::new(at.to_string()))
    {
        return Status::Unusable {
            reason: format!("the secret store couldn't be written ({e})"),
        };
    }
    licence.status(dn_licence::NODE, at)
}

/// Checks `text` and keeps it as the Node's licence. A file that doesn't check out, doesn't
/// include the Node or has ended is refused, and the licence kept before stays.
pub fn add(
    store: &dyn SecretStore,
    keys: &[PublicKey],
    text: &str,
    now: i64,
) -> Result<String, String> {
    let licence = dn_licence::verify(text.as_bytes(), keys)
        .map_err(|e| format!("That licence can't be used: {e}. Nothing was changed."))?;
    let seen = store
        .get(&seen_id())
        .map_err(|e| format!("couldn't read the secret store: {e}"))?
        .and_then(|s| s.expose().trim().parse::<i64>().ok());
    let at = dn_licence::checked_now(now, seen)
        .map_err(|e| format!("That licence can't be used: {e}. Nothing was changed."))?;
    match licence.status(dn_licence::NODE, at) {
        Status::Valid { .. } => {}
        Status::Ended { ended_at, .. } => {
            return Err(format!(
                "That licence ended on {}. Nothing was changed.",
                date(ended_at)
            ));
        }
        Status::Unusable { reason } => {
            return Err(format!(
                "That licence can't be used: {reason}. Nothing was changed."
            ));
        }
        Status::Missing => unreachable!("a checked licence is never missing"),
    }
    store
        .set(&licence_id(), &Secret::new(text.to_owned()))
        .map_err(|e| format!("couldn't keep the licence in the secret store: {e}"))?;
    Ok(added(&licence, at))
}

/// "Licence added: Pro for Jo Rivera until 31 Dec 2027." and the like.
fn added(l: &Licence, now: i64) -> String {
    let p = &l.payload;
    match (p.kind, p.expires_at) {
        (Kind::Trial, Some(end)) => format!(
            "Trial licence added: {} days, until {}.",
            days_left(now, end),
            date(end)
        ),
        (kind, Some(end)) => format!(
            "Licence added: {} for {} until {}.",
            kind_name(kind),
            p.licensee,
            date(end)
        ),
        (kind, None) => format!("Licence added: {} for {}.", kind_name(kind), p.licensee),
    }
}

/// Why the Node won't start, or `None` when it may: a licence that ended starts in
/// status-only mode.
pub fn refusal_to_start(status: &Status, store_label: &str) -> Option<String> {
    match status {
        Status::Valid { .. } | Status::Ended { .. } => None,
        Status::Missing => Some(format!(
            "The Node is part of Demios Pro, and this Node has no licence. Add a Pro or trial \
             licence with `dn node licence add --data-dir <dir> <file>`. {GET_ONE}. Nothing on \
             this Node was changed."
        )),
        Status::Unusable { reason } => Some(format!(
            "The licence in {store_label} can't be used: {reason}. Add the file you got with \
             `dn node licence add --data-dir <dir> <file>`. Nothing on this Node was changed."
        )),
    }
}

/// What a device that asks to sync or pair is told while the licence doesn't allow it.
pub fn refusal(status: &Status) -> Option<String> {
    match status {
        Status::Valid { .. } => None,
        Status::Ended { .. } => Some("the Node's licence ended".into()),
        Status::Missing => Some("the Node has no licence".into()),
        Status::Unusable { reason } => Some(format!("the Node's licence can't be used: {reason}")),
    }
}

/// The line `dn node run` prints when the licence ended.
pub fn ended_notice(status: &Status) -> Option<String> {
    match status {
        Status::Ended { kind, ended_at, .. } => Some(format!(
            "dn node: this Node's {} licence ended on {}. It answers your devices but doesn't \
             sync or run jobs. Add a new licence with `dn node licence add --data-dir <dir> \
             <file>`. Your data on the Node is kept, and `dn node delete-all` still works.",
            kind_name(*kind),
            date(*ended_at)
        )),
        _ => None,
    }
}

/// Seconds until the next check: a day, or sooner when the licence ends sooner.
pub fn next_check_in(status: &Status, now: i64) -> i64 {
    match status {
        Status::Valid {
            expires_at: Some(end),
            ..
        } => (end - now).clamp(1, CHECK_SECS),
        _ => CHECK_SECS,
    }
}

/// The licence's state as the Node keeps it while running.
pub struct Gate {
    status: RwLock<Status>,
}

impl Gate {
    pub fn new(status: Status) -> Self {
        Self {
            status: RwLock::new(status),
        }
    }

    pub fn get(&self) -> Status {
        self.status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn set(&self, status: Status) {
        *self.status.write().unwrap_or_else(|p| p.into_inner()) = status;
    }

    /// Why sync and pairing are refused right now, if they are.
    pub fn refusal(&self) -> Option<String> {
        refusal(&self.get())
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Trial => "trial",
        Kind::Founding => "founding",
        Kind::Pro => "Pro",
        Kind::Alpha => "alpha",
    }
}

fn days_left(now: i64, end: i64) -> i64 {
    (end - now + 86_399).max(0) / 86_400
}

/// "30 Nov 2026" (UTC), without a date library.
pub fn date(unix: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day} {} {year}", MONTHS[(month - 1) as usize])
}

/// A key made for this test run only; never stored or printed.
#[cfg(test)]
pub mod test_key {
    use std::sync::OnceLock;

    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
    use dn_licence::{Kind, Payload, PublicKey};

    struct Key {
        pair: Ed25519KeyPair,
        public: [PublicKey; 1],
    }

    fn key() -> &'static Key {
        static KEY: OnceLock<Key> = OnceLock::new();
        KEY.get_or_init(|| {
            let pair = Ed25519KeyPair::generate().unwrap();
            let ed25519 = pair.public_key().as_ref().try_into().unwrap();
            Key {
                pair,
                public: [PublicKey {
                    id: "test",
                    ed25519,
                }],
            }
        })
    }

    pub fn keys() -> &'static [PublicKey] {
        &key().public
    }

    /// A licence file signed with the test key.
    pub fn file(
        kind: Kind,
        entitlements: &[&str],
        issued_at: i64,
        expires_at: Option<i64>,
    ) -> String {
        let payload = Payload {
            issue_id: "T-1".into(),
            licensee: "Jo Rivera".into(),
            kind,
            entitlements: entitlements.iter().map(|e| (*e).to_owned()).collect(),
            issued_at,
            expires_at,
        };
        let sig = key()
            .pair
            .sign(&dn_licence::signing_bytes(&payload).unwrap());
        dn_licence::write_file(&payload, "test", sig.as_ref()).unwrap()
    }

    /// A Pro licence for the Node that ends far in the future.
    pub fn pro() -> String {
        file(Kind::Pro, &[dn_licence::NODE], 1_000_000_000, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_secrets::MemoryStore;

    const T0: i64 = 1_795_000_000;
    const DAY: i64 = 86_400;

    fn store_with(text: &str) -> MemoryStore {
        let store = MemoryStore::new();
        store
            .set(&licence_id(), &Secret::new(text.to_owned()))
            .unwrap();
        store
    }

    #[test]
    fn no_licence_means_the_node_doesnt_start() {
        let store = MemoryStore::new();
        let status = check(&store, test_key::keys(), T0);
        assert_eq!(status, Status::Missing);
        let msg = refusal_to_start(&status, "the system keychain").unwrap();
        assert!(msg.starts_with("The Node is part of Demios Pro"), "{msg}");
        assert!(msg.contains("dn node licence add"));
    }

    #[test]
    fn a_valid_licence_starts_and_records_the_time_seen() {
        let store = store_with(&test_key::file(
            Kind::Alpha,
            &["node"],
            T0,
            Some(T0 + 90 * DAY),
        ));
        let status = check(&store, test_key::keys(), T0 + DAY);
        assert!(status.allows_use());
        assert_eq!(refusal_to_start(&status, "x"), None);
        assert_eq!(refusal(&status), None);
        assert_eq!(
            store.get(&seen_id()).unwrap().unwrap().expose(),
            (T0 + DAY).to_string()
        );
        assert_eq!(next_check_in(&status, T0 + 89 * DAY + 3600), DAY - 3600);
        assert_eq!(next_check_in(&status, T0 + DAY), CHECK_SECS);
    }

    #[test]
    fn an_ended_licence_starts_in_status_only_mode() {
        let store = store_with(&test_key::file(Kind::Alpha, &["node"], T0, Some(T0 + DAY)));
        let status = check(&store, test_key::keys(), T0 + 2 * DAY);
        assert!(matches!(status, Status::Ended { .. }));
        assert_eq!(refusal_to_start(&status, "x"), None, "it starts");
        assert_eq!(refusal(&status).unwrap(), "the Node's licence ended");
        let notice = ended_notice(&status).unwrap();
        assert!(
            notice.contains("alpha licence ended on 19 Nov 2026"),
            "{notice}"
        );
        assert!(notice.contains("doesn't sync or run jobs"));
    }

    #[test]
    fn a_file_that_cant_be_used_stops_the_node_with_the_reason() {
        let mut text = test_key::pro();
        text = text.replace("Jo Rivera", "Someone else");
        let status = check(&store_with(&text), test_key::keys(), T0);
        let msg = refusal_to_start(&status, "the system keychain").unwrap();
        assert!(msg.contains("its signature doesn't match"), "{msg}");

        let other = test_key::file(Kind::Pro, &["some-addon"], T0, None);
        let status = check(&store_with(&other), test_key::keys(), T0);
        assert_eq!(
            refusal(&status).unwrap(),
            "the Node's licence can't be used: it doesn't include the Node"
        );
    }

    #[test]
    fn moving_the_clock_back_doesnt_bring_a_licence_back() {
        let store = store_with(&test_key::file(
            Kind::Alpha,
            &["node"],
            T0,
            Some(T0 + 10 * DAY),
        ));
        assert!(matches!(
            check(&store, test_key::keys(), T0 + 11 * DAY),
            Status::Ended { .. }
        ));
        // An hour back is still the latest time seen: still ended.
        assert!(matches!(
            check(&store, test_key::keys(), T0 + 11 * DAY - 3600),
            Status::Ended { .. }
        ));
        // Days back is refused outright.
        let status = check(&store, test_key::keys(), T0 + 2 * DAY);
        assert!(
            refusal(&status).unwrap().contains("clock is behind"),
            "{status:?}"
        );
    }

    #[test]
    fn adding_checks_the_file_first_and_keeps_the_old_one_otherwise() {
        let store = MemoryStore::new();
        let trial = test_key::file(Kind::Trial, &["node"], T0, Some(T0 + 14 * DAY));
        assert_eq!(
            add(&store, test_key::keys(), &trial, T0).unwrap(),
            "Trial licence added: 14 days, until 2 Dec 2026."
        );
        let ended = test_key::file(Kind::Pro, &["node"], T0 - 9 * DAY, Some(T0 - DAY));
        let err = add(&store, test_key::keys(), &ended, T0).unwrap_err();
        assert!(err.starts_with("That licence ended on"), "{err}");
        let err = add(&store, test_key::keys(), "junk", T0).unwrap_err();
        assert!(err.contains("Nothing was changed"), "{err}");
        assert_eq!(store.get(&licence_id()).unwrap().unwrap().expose(), trial);

        let pro = test_key::file(Kind::Pro, &["node"], T0, Some(T0 + 400 * DAY));
        assert_eq!(
            add(&store, test_key::keys(), &pro, T0).unwrap(),
            "Licence added: Pro for Jo Rivera until 23 Dec 2027."
        );
    }

    #[test]
    fn dates_are_written_in_words() {
        assert_eq!(date(0), "1 Jan 1970");
        assert_eq!(date(951_782_400), "29 Feb 2000");
        assert_eq!(date(1_796_083_200), "1 Dec 2026");
    }
}
