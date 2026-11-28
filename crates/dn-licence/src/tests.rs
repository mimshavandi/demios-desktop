use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};

use super::*;

const DAY: i64 = 86_400;
const T0: i64 = 1_795_000_000;

/// A key made for this test only; nothing is stored or printed.
struct TestKey {
    pair: Ed25519KeyPair,
}

impl TestKey {
    fn new() -> Self {
        Self {
            pair: Ed25519KeyPair::generate().unwrap(),
        }
    }

    fn public(&self, id: &'static str) -> PublicKey {
        PublicKey {
            id,
            ed25519: self.pair.public_key().as_ref().try_into().unwrap(),
        }
    }

    fn sign(&self, payload: &Payload, id: &str) -> String {
        let sig = self.pair.sign(&signing_bytes(payload).unwrap());
        write_file(payload, id, sig.as_ref()).unwrap()
    }
}

fn payload() -> Payload {
    Payload {
        issue_id: "L-0001".into(),
        licensee: "Jo Rivera".into(),
        kind: Kind::Alpha,
        entitlements: vec![NODE.into(), "example-addon".into()],
        issued_at: T0,
        expires_at: Some(T0 + 30 * DAY),
    }
}

#[test]
fn a_signed_file_checks_out_and_reads_back() {
    let key = TestKey::new();
    let file = key.sign(&payload(), "test-1");
    let licence = verify(file.as_bytes(), &[key.public("test-1")]).unwrap();
    assert_eq!(licence.key, "test-1");
    assert_eq!(licence.payload, payload());
}

#[test]
fn layout_and_key_order_dont_matter_but_content_does() {
    let key = TestKey::new();
    let file = key.sign(&payload(), "test-1");
    let mut value: Value = serde_json::from_str(&file).unwrap();
    // The same content written compactly, with the payload's keys in another order.
    let p = value["payload"].as_object().unwrap().clone();
    let reordered: serde_json::Map<String, Value> = p.into_iter().rev().collect();
    value["payload"] = Value::Object(reordered);
    let compact = serde_json::to_vec(&value).unwrap();
    assert!(verify(&compact, &[key.public("test-1")]).is_ok());

    value["payload"]["licensee"] = "Someone else".into();
    let tampered = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        verify(&tampered, &[key.public("test-1")]),
        Err(LicenceError::Signature)
    );
}

#[test]
fn a_longer_licence_or_more_entitlements_breaks_the_signature() {
    let key = TestKey::new();
    let file = key.sign(&payload(), "test-1");
    let mut value: Value = serde_json::from_str(&file).unwrap();
    value["payload"]["expires_at"] = (T0 + 3650 * DAY).into();
    assert_eq!(
        verify(
            &serde_json::to_vec(&value).unwrap(),
            &[key.public("test-1")]
        ),
        Err(LicenceError::Signature)
    );
    let mut value: Value = serde_json::from_str(&file).unwrap();
    value["payload"]["entitlements"]
        .as_array_mut()
        .unwrap()
        .push("other".into());
    assert_eq!(
        verify(
            &serde_json::to_vec(&value).unwrap(),
            &[key.public("test-1")]
        ),
        Err(LicenceError::Signature)
    );
}

#[test]
fn another_key_or_an_unknown_key_id_is_refused() {
    let key = TestKey::new();
    let other = TestKey::new();
    let file = key.sign(&payload(), "test-1");
    assert_eq!(
        verify(file.as_bytes(), &[other.public("test-1")]),
        Err(LicenceError::Signature)
    );
    assert_eq!(
        verify(file.as_bytes(), &[key.public("test-2")]),
        Err(LicenceError::UnknownKey("test-1".into()))
    );
    assert_eq!(
        verify(file.as_bytes(), TRUSTED),
        Err(LicenceError::UnknownKey("test-1".into())),
        "no key is trusted until the owner's is added"
    );
}

#[test]
fn damaged_and_newer_files_are_refused_with_a_reason() {
    let key = TestKey::new();
    let keys = [key.public("test-1")];
    let file = key.sign(&payload(), "test-1");
    let mut value: Value = serde_json::from_str(&file).unwrap();

    assert!(matches!(
        verify(b"not json", &keys),
        Err(LicenceError::Damaged(_))
    ));
    assert!(matches!(
        verify(&vec![b' '; MAX_FILE_BYTES + 1], &keys),
        Err(LicenceError::Damaged(_))
    ));

    value["format"] = 2.into();
    assert_eq!(
        verify(&serde_json::to_vec(&value).unwrap(), &keys),
        Err(LicenceError::NewerFormat(2))
    );
    value["format"] = 1.into();

    value["extra"] = true.into();
    assert!(matches!(
        verify(&serde_json::to_vec(&value).unwrap(), &keys),
        Err(LicenceError::Damaged(_))
    ));
    value.as_object_mut().unwrap().remove("extra");

    value["signature"] = "zz".into();
    assert_eq!(
        verify(&serde_json::to_vec(&value).unwrap(), &keys),
        Err(LicenceError::Signature)
    );
}

#[test]
fn a_signed_payload_must_still_make_sense() {
    let key = TestKey::new();
    let keys = [key.public("test-1")];
    let sign_value = |payload: Value| -> Vec<u8> {
        let mut bytes = SIGNING_LABEL.to_vec();
        canonical(&payload, &mut bytes).unwrap();
        let sig = key.pair.sign(&bytes);
        let envelope = Envelope {
            format: FORMAT,
            key: "test-1".into(),
            payload,
            signature: sig.as_ref().iter().map(|b| format!("{b:02x}")).collect(),
        };
        serde_json::to_vec(&envelope).unwrap()
    };
    let mut p = serde_json::to_value(payload()).unwrap();
    p["unknown"] = 1.into();
    assert!(matches!(
        verify(&sign_value(p), &keys),
        Err(LicenceError::Damaged(_))
    ));

    let mut p = serde_json::to_value(payload()).unwrap();
    p.as_object_mut().unwrap().remove("expires_at");
    assert!(
        matches!(verify(&sign_value(p), &keys), Err(LicenceError::Damaged(_))),
        "an alpha licence that never ends is refused"
    );

    // A fraction can't be put in canonical form, so it's refused before the signature.
    let mut p = serde_json::to_value(payload()).unwrap();
    p["issued_at"] = serde_json::json!(1.5);
    let envelope = Envelope {
        format: FORMAT,
        key: "test-1".into(),
        payload: p,
        signature: "00".repeat(64),
    };
    assert!(matches!(
        verify(&serde_json::to_vec(&envelope).unwrap(), &keys),
        Err(LicenceError::Damaged(_))
    ));
}

#[test]
fn the_signer_refuses_a_payload_that_makes_no_sense() {
    let mut p = payload();
    p.entitlements.clear();
    assert!(signing_bytes(&p).is_err());
    let mut p = payload();
    p.kind = Kind::Trial;
    p.expires_at = None;
    assert!(signing_bytes(&p).is_err());
    let mut p = payload();
    p.kind = Kind::Pro;
    p.expires_at = None;
    assert!(signing_bytes(&p).is_ok(), "a Pro licence may not end");
}

#[test]
fn status_follows_the_dates_and_the_entitlements() {
    let key = TestKey::new();
    let file = key.sign(&payload(), "test-1");
    let licence = verify(file.as_bytes(), &[key.public("test-1")]).unwrap();
    let end = T0 + 30 * DAY;

    assert_eq!(
        licence.status(NODE, T0 + DAY),
        Status::Valid {
            kind: Kind::Alpha,
            licensee: "Jo Rivera".into(),
            expires_at: Some(end),
        }
    );
    assert!(licence.status(NODE, end - 1).allows_use());
    assert_eq!(
        licence.status(NODE, end),
        Status::Ended {
            kind: Kind::Alpha,
            licensee: "Jo Rivera".into(),
            ended_at: end,
        }
    );
    assert_eq!(
        licence.status("not-included", T0 + DAY),
        Status::Unusable {
            reason: "it doesn't include this add-on".into()
        }
    );
    assert_eq!(
        licence.status(NODE, T0 - 2 * DAY),
        Status::unusable(&LicenceError::ClockBehind),
        "a clock before the licence was made"
    );
}

#[test]
fn moving_the_clock_back_never_buys_time() {
    assert_eq!(checked_now(T0, None), Ok(T0));
    assert_eq!(checked_now(T0 + 10, Some(T0)), Ok(T0 + 10));
    assert_eq!(
        checked_now(T0 - 3600, Some(T0)),
        Ok(T0),
        "an hour back uses the latest time seen"
    );
    assert_eq!(
        checked_now(T0 - 2 * DAY, Some(T0)),
        Err(LicenceError::ClockBehind)
    );
}

#[test]
fn the_status_reads_the_same_on_every_device() {
    let s = Status::Valid {
        kind: Kind::Trial,
        licensee: "Jo".into(),
        expires_at: Some(T0),
    };
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"state": "valid", "kind": "trial", "licensee": "Jo", "expires_at": T0})
    );
    assert_eq!(serde_json::from_value::<Status>(json).unwrap(), s);
    assert_eq!(
        serde_json::to_value(Status::Missing).unwrap(),
        serde_json::json!({"state": "missing"})
    );
}

#[test]
fn canonical_json_is_sorted_and_compact() {
    let v = serde_json::json!({"b": [1, true, null], "a": {"y": "é\n", "x": -3}});
    let mut out = Vec::new();
    canonical(&v, &mut out).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        r#"{"a":{"x":-3,"y":"é\n"},"b":[1,true,null]}"#
    );
}

#[test]
fn a_signature_with_a_sign_or_spaces_inside_is_refused() {
    assert_eq!(hex("+a"), None);
    assert_eq!(hex("0a 0b"), None);
    assert_eq!(hex("0a0B"), Some(vec![0x0a, 0x0b]));
}

#[test]
fn times_at_the_edge_of_the_range_dont_overflow() {
    assert_eq!(checked_now(i64::MAX, Some(i64::MAX)), Ok(i64::MAX));
    assert_eq!(
        checked_now(i64::MIN, Some(0)),
        Err(LicenceError::ClockBehind)
    );
}
