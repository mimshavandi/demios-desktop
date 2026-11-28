use super::*;
use dn_sync::pairing::PairingOffer;
use dn_sync::transport::{HeldItem, NodeStatus};
use std::collections::HashMap;
use tokio::net::TcpListener;

/// The Keychain, in memory; `broken` makes every read fail.
#[derive(Default)]
struct MemoryKeys {
    items: Mutex<HashMap<String, String>>,
    broken: bool,
}

impl KeyStore for MemoryKeys {
    fn get(&self, name: String) -> Result<Option<String>, MobileError> {
        if self.broken {
            return Err(MobileError::Keychain {
                reason: "locked".into(),
            });
        }
        Ok(self.items.lock().unwrap().get(&name).cloned())
    }
    fn set(&self, name: String, value: String) -> Result<(), MobileError> {
        self.items.lock().unwrap().insert(name, value);
        Ok(())
    }
    fn delete(&self, name: String) -> Result<bool, MobileError> {
        Ok(self.items.lock().unwrap().remove(&name).is_some())
    }
}

fn temp_db(tag: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "dn-mobile-{tag}-{}-{}.db",
        std::process::id(),
        dn_store::now_ms()
    ));
    path.to_string_lossy().into_owned()
}

fn held() -> HeldItem {
    HeldItem {
        id: "h1".into(),
        plugin: "brief".into(),
        job: "morning".into(),
        tool: "plugin.brief.send_mail".into(),
        summary: "Send an email to 1 person".into(),
        arguments: r#"{"to":["ana@example.com"]}"#.into(),
        arguments_sha256: "ab".repeat(32),
        created_at: 1,
        expires_at: None,
    }
}

/// A minimal Node: the first connection pairs (answering `confirm`); later ones answer the
/// status, the held calls and a removal. Returns the pairing link.
async fn fake_node(confirm: bool) -> String {
    let keys = Arc::new(DeviceKeys::generate().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mut offer = PairingOffer::new(&addr, keys.public(), now_secs()).unwrap();
    let link = offer.link().to_url().to_string();
    let db = Arc::new(Mutex::new(
        dn_store::open_in_memory(&[MigrationSet::new("sync", dn_sync::migrations())]).unwrap(),
    ));
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ch = transport::accept(stream, &keys).await.unwrap();
        let Ok(req) = transport::read_join(&db, &mut ch, &mut offer, now_secs).await else {
            return;
        };
        assert_eq!(req.kind, DeviceKind::Phone);
        transport::finish_join(&db, &mut ch, &req, None, confirm, 2)
            .await
            .unwrap();
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut ch) = transport::accept(stream, &keys).await else {
                continue;
            };
            while let Ok(m) = ch.recv().await {
                let reply = match m {
                    Message::StatusRequest => Message::Status(NodeStatus {
                        name: "Home server".into(),
                        version: "1.4.0".into(),
                        secret_store: "systemd_credential".into(),
                        secret_store_label: "Encrypted file".into(),
                        protected_at_rest: true,
                        devices: 2,
                        reachable_from_outside: Some(false),
                    }),
                    Message::HeldRequest => Message::Held {
                        items: vec![held()],
                    },
                    Message::RemoveDevice { .. } => Message::Done,
                    _ => Message::Done,
                };
                if ch.send(&reply).await.is_err() {
                    break;
                }
            }
        }
    });
    link
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_pairs_reads_what_is_waiting_and_unpairs() {
    let link = fake_node(true).await;
    let keys = Arc::new(MemoryKeys::default());
    let phone = Phone::open(keys.clone(), temp_db("pair")).unwrap();
    assert_eq!(phone.paired_node().unwrap(), None);
    assert!(matches!(phone.waiting().await, Err(MobileError::NotPaired)));

    let numbers = phone
        .start_pairing(link.clone(), "Work phone".into())
        .await
        .unwrap();
    assert_eq!(numbers.len(), 3);
    let node = phone.finish_pairing().await.unwrap();
    assert_eq!(node.name, "Home server", "named from the Node's status");

    let waiting = phone.waiting().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].summary, "Send an email to 1 person");
    assert!(waiting[0].arguments.contains("ana@example.com"));
    assert_eq!(phone.status().await.unwrap().version, "1.4.0");

    // Already paired: a second pairing is refused before connecting.
    assert!(matches!(
        phone.start_pairing(link, "x".into()).await,
        Err(MobileError::AlreadyPaired | MobileError::Link { .. })
    ));

    assert!(phone.unpair().await.unwrap());
    assert_eq!(phone.paired_node().unwrap(), None);
    assert!(keys.items.lock().unwrap().is_empty(), "keys deleted");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_pairing_records_nothing() {
    let link = fake_node(false).await;
    let phone = Phone::open(Arc::new(MemoryKeys::default()), temp_db("decline")).unwrap();
    phone
        .start_pairing(link, "Work phone".into())
        .await
        .unwrap();
    assert!(matches!(
        phone.finish_pairing().await,
        Err(MobileError::Declined)
    ));
    assert_eq!(phone.paired_node().unwrap(), None);
}

#[test]
fn keys_are_made_once_and_an_unreadable_keychain_is_an_error() {
    let keys = Arc::new(MemoryKeys::default());
    let a = Phone::open(keys.clone(), temp_db("keys-a")).unwrap();
    let b = Phone::open(keys, temp_db("keys-b")).unwrap();
    assert_eq!(a.device_id(), b.device_id());

    let broken = Arc::new(MemoryKeys {
        broken: true,
        ..Default::default()
    });
    assert!(matches!(
        Phone::open(broken.clone(), temp_db("keys-c")),
        Err(MobileError::Keychain { .. })
    ));
    assert!(broken.items.lock().unwrap().is_empty(), "no new keys made");
}

#[test]
fn links_are_read_without_connecting() {
    let keys = DeviceKeys::generate().unwrap();
    let offer = PairingOffer::new("192.168.1.20:7443", keys.public(), now_secs()).unwrap();
    let preview = read_link(offer.link().to_url().to_string()).unwrap();
    assert_eq!(preview.address.as_deref(), Some("192.168.1.20:7443"));
    assert!(!preview.through_relay);
    assert!(matches!(
        read_link("https://example.com".into()),
        Err(MobileError::Link { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_only_link_is_refused_with_a_reason() {
    let keys = DeviceKeys::generate().unwrap();
    let b64 = dn_sync::b64::encode;
    let link = format!(
        "demios://pair?v=2&r=https://relay.example.net&m={}&n={}&s={}&e={}",
        b64(&[3; 32]),
        b64(&keys.public().to_bytes()),
        b64(&[1; 16]),
        now_secs() + 600
    );
    let phone = Phone::open(Arc::new(MemoryKeys::default()), temp_db("relay")).unwrap();
    assert!(matches!(
        phone.start_pairing(link.clone(), "x".into()).await,
        Err(MobileError::RelayOnly)
    ));
    assert!(read_link(link).unwrap().through_relay);
}

#[test]
fn the_app_shows_the_cores_words_for_an_error() {
    assert_eq!(
        error_message(MobileError::NotPaired),
        "This phone isn't paired with a Node."
    );
    assert!(error_message(MobileError::RelayOnly).starts_with("This Node can be reached only"));
}
