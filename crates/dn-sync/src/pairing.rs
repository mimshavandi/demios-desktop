//! Pairing a new device (ADR-0014 §2, design: Settings › Node › PairDevice).
//!
//! 1. The Node (or a paired device) makes a [`PairingOffer`]: a 128-bit one-time secret
//!    that expires after 10 minutes. Its [`PairingLink`] (the QR code's payload) carries
//!    the Node's address, the Node's public keys, the secret and the expiry.
//! 2. The new device reads the link, connects, checks that the Node's key is the one in the
//!    link ([`PairingLink::check_node`]), and sends its own public keys with a
//!    [`join_proof`] that it knows the secret.
//! 3. The Node checks the proof ([`PairingOffer::accept`]); the secret is then used up.
//! 4. Both sides show the same [`CheckNumbers`], and the person confirms they match on the
//!    Node, or on the device that made the offer.
//!
//! The network exchange and binding the proof to the TLS channel come with the paired
//! transport. This module has the data and the checks.

use crate::identity::PublicIdentity;
use crate::{Result, SyncError, b64};
use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::hmac;
use std::fmt;
use zeroize::Zeroizing;

/// How long a pairing code works, in seconds (ADR-0014 §2: 10 minutes).
pub const OFFER_TTL_SECS: u64 = 600;
/// Wrong proofs an offer takes before it closes.
pub const MAX_WRONG_PROOFS: u32 = 5;
/// Version of the link and proof format.
pub const LINK_VERSION: u32 = 1;
const LINK_PREFIX: &str = "demios://pair?";
const PROOF_LABEL: &[u8] = b"dn-pair v1 proof";
const CHECK_LABEL: &[u8] = b"dn-pair v1 check";
const MAX_ADDRESS_LEN: usize = 261; // a 253-character host, ':' and a 5-digit port, plus brackets

/// What a pairing code or link holds. `Debug` hides the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingLink {
    /// `host:port` the new device connects to; an IPv6 host is in brackets.
    pub address: String,
    pub node: PublicIdentity,
    secret: Zeroizing<[u8; 16]>,
    /// Unix seconds after which the code no longer works.
    pub expires_at: u64,
}

impl fmt::Debug for PairingLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingLink")
            .field("address", &self.address)
            .field("node", &self.node)
            .field("secret", &"***")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

fn valid_address(address: &str) -> bool {
    if address.is_empty() || address.len() > MAX_ADDRESS_LEN {
        return false;
    }
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    let port_ok = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && matches!(port.parse::<u32>(), Ok(1..=65535));
    let host_ok = if let Some(inner) = host.strip_prefix('[') {
        inner.strip_suffix(']').is_some_and(|v6| {
            !v6.is_empty()
                && v6
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        })
    } else {
        !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            && !host.starts_with(['.', '-'])
    };
    port_ok && host_ok
}

impl PairingLink {
    /// The link text, also the QR code's payload:
    /// `demios://pair?v=1&a=<host:port>&n=<node keys>&s=<secret>&e=<expiry>`.
    pub fn to_url(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "{LINK_PREFIX}v={LINK_VERSION}&a={}&n={}&s={}&e={}",
            self.address,
            b64::encode(&self.node.to_bytes()),
            b64::encode(self.secret.as_ref()),
            self.expires_at
        ))
    }

    /// Reads a link. Strict: every field exactly once, nothing else, and not expired at
    /// `now` (Unix seconds).
    pub fn parse(text: &str, now: u64) -> Result<Self> {
        let query = text
            .trim()
            .strip_prefix(LINK_PREFIX)
            .ok_or(SyncError::BadLink("it doesn't start with demios://pair"))?;
        let (mut v, mut a, mut n, mut s, mut e) = (None, None, None, None, None);
        for pair in query.split('&') {
            let (key, value) = pair
                .split_once('=')
                .ok_or(SyncError::BadLink("a field has no value"))?;
            let slot = match key {
                "v" => &mut v,
                "a" => &mut a,
                "n" => &mut n,
                "s" => &mut s,
                "e" => &mut e,
                _ => return Err(SyncError::BadLink("it has an unknown field")),
            };
            if slot.replace(value).is_some() {
                return Err(SyncError::BadLink("a field appears twice"));
            }
        }
        let version: u32 = v
            .ok_or(SyncError::BadLink("the version is missing"))?
            .parse()
            .map_err(|_| SyncError::BadLink("the version isn't a number"))?;
        if version > LINK_VERSION {
            return Err(SyncError::NewerLinkVersion);
        }
        if version != LINK_VERSION {
            return Err(SyncError::BadLink("unknown version"));
        }
        let address = a.ok_or(SyncError::BadLink("the address is missing"))?;
        if !valid_address(address) {
            return Err(SyncError::BadLink("the address isn't host:port"));
        }
        let node =
            b64::decode_array::<64>(n.ok_or(SyncError::BadLink("the Node's key is missing"))?)
                .ok_or(SyncError::BadLink("the Node's key is damaged"))?;
        let secret = Zeroizing::new(
            b64::decode_array::<16>(s.ok_or(SyncError::BadLink("the code is missing"))?)
                .ok_or(SyncError::BadLink("the code is damaged"))?,
        );
        let expires_at: u64 = e
            .ok_or(SyncError::BadLink("the expiry is missing"))?
            .parse()
            .map_err(|_| SyncError::BadLink("the expiry isn't a number"))?;
        if now >= expires_at {
            return Err(SyncError::OfferClosed);
        }
        Ok(Self {
            address: address.to_owned(),
            node: PublicIdentity::from_bytes(&node),
            secret,
            expires_at,
        })
    }

    /// Checks the key the Node presented on connecting against the one in the link.
    pub fn check_node(&self, presented: &PublicIdentity) -> Result<()> {
        if presented == &self.node {
            Ok(())
        } else {
            Err(SyncError::NodeKeyMismatch)
        }
    }
}

fn proof_input(node: &PublicIdentity, joiner: &PublicIdentity) -> Vec<u8> {
    let mut input = Vec::with_capacity(PROOF_LABEL.len() + 128);
    input.extend_from_slice(PROOF_LABEL);
    input.extend_from_slice(&node.to_bytes());
    input.extend_from_slice(&joiner.to_bytes());
    input
}

/// The joining device's proof that it knows the link's secret: HMAC-SHA256 keyed with the
/// secret over the Node's and the joiner's public keys, so it can't be reused for other keys.
pub fn join_proof(link: &PairingLink, joiner: &PublicIdentity) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, link.secret.as_ref());
    let tag = hmac::sign(&key, &proof_input(&link.node, joiner));
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// The offer as the Node (or inviting device) keeps it while it waits.
pub struct PairingOffer {
    link: PairingLink,
    wrong_proofs: u32,
    closed: bool,
}

impl fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingOffer")
            .field("link", &self.link)
            .field("wrong_proofs", &self.wrong_proofs)
            .field("closed", &self.closed)
            .finish()
    }
}

impl PairingOffer {
    /// A new offer with a fresh random secret, valid for [`OFFER_TTL_SECS`] from `now`.
    pub fn new(address: &str, node: PublicIdentity, now: u64) -> Result<Self> {
        if !valid_address(address) {
            return Err(SyncError::BadLink("the address isn't host:port"));
        }
        let mut secret = Zeroizing::new([0u8; 16]);
        aws_lc_rs::rand::fill(secret.as_mut()).map_err(|_| SyncError::Crypto("random"))?;
        Ok(Self {
            link: PairingLink {
                address: address.to_owned(),
                node,
                secret,
                expires_at: now + OFFER_TTL_SECS,
            },
            wrong_proofs: 0,
            closed: false,
        })
    }

    pub fn link(&self) -> &PairingLink {
        &self.link
    }

    /// Seconds left at `now`, 0 once closed or expired.
    pub fn seconds_left(&self, now: u64) -> u64 {
        if self.closed {
            0
        } else {
            self.link.expires_at.saturating_sub(now)
        }
    }

    /// Checks a joining device's proof. On success the secret is used up and both sides
    /// can show the check numbers. Wrong proofs count; after [`MAX_WRONG_PROOFS`] the
    /// offer closes, so a code can't be guessed at.
    pub fn accept(
        &mut self,
        joiner: &PublicIdentity,
        proof: &[u8],
        now: u64,
    ) -> Result<CheckNumbers> {
        if self.closed || now >= self.link.expires_at {
            self.closed = true;
            return Err(SyncError::OfferClosed);
        }
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.link.secret.as_ref());
        if hmac::verify(&key, &proof_input(&self.link.node, joiner), proof).is_err() {
            self.wrong_proofs += 1;
            if self.wrong_proofs >= MAX_WRONG_PROOFS {
                self.closed = true;
            }
            return Err(SyncError::WrongProof);
        }
        self.closed = true;
        Ok(CheckNumbers::new(&self.link.node, joiner))
    }
}

/// The numbers both sides show: 12 digits from a hash of both devices' public keys, so a
/// device in the middle would show different ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CheckNumbers(u64);

impl CheckNumbers {
    pub fn new(node: &PublicIdentity, joiner: &PublicIdentity) -> Self {
        let mut input = Vec::with_capacity(CHECK_LABEL.len() + 128);
        input.extend_from_slice(CHECK_LABEL);
        input.extend_from_slice(&node.to_bytes());
        input.extend_from_slice(&joiner.to_bytes());
        let hash = digest(&SHA256, &input);
        let mut first = [0u8; 8];
        first.copy_from_slice(&hash.as_ref()[..8]);
        Self(u64::from_be_bytes(first) % 1_000_000_000_000)
    }

    /// Three groups of four digits, e.g. `["4821", "0937", "5512"]`.
    pub fn groups(&self) -> [String; 3] {
        let digits = format!("{:012}", self.0);
        [
            digits[0..4].to_owned(),
            digits[4..8].to_owned(),
            digits[8..12].to_owned(),
        ]
    }
}

impl fmt::Display for CheckNumbers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c] = self.groups();
        write!(f, "{a} {b} {c}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKeys;

    const NOW: u64 = 1_800_000_000;

    fn setup() -> (DeviceKeys, DeviceKeys, PairingOffer) {
        let node = DeviceKeys::generate().unwrap();
        let joiner = DeviceKeys::generate().unwrap();
        let offer = PairingOffer::new("192.168.1.20:7443", node.public(), NOW).unwrap();
        (node, joiner, offer)
    }

    #[test]
    fn a_device_with_the_link_pairs_and_both_sides_show_the_same_numbers() {
        let (node, joiner, mut offer) = setup();
        let url = offer.link().to_url();
        let link = PairingLink::parse(&url, NOW + 30).unwrap();
        assert_eq!(&link, offer.link());
        link.check_node(&node.public()).unwrap();
        let proof = join_proof(&link, &joiner.public());
        let on_node = offer.accept(&joiner.public(), &proof, NOW + 31).unwrap();
        let on_joiner = CheckNumbers::new(&link.node, &joiner.public());
        assert_eq!(on_node, on_joiner);
        assert_eq!(on_node.to_string().len(), 14);
    }

    #[test]
    fn the_code_works_once() {
        let (_, joiner, mut offer) = setup();
        let proof = join_proof(offer.link(), &joiner.public());
        offer.accept(&joiner.public(), &proof, NOW).unwrap();
        assert!(matches!(
            offer.accept(&joiner.public(), &proof, NOW),
            Err(SyncError::OfferClosed)
        ));
        let other = DeviceKeys::generate().unwrap();
        let proof = join_proof(offer.link(), &other.public());
        assert!(matches!(
            offer.accept(&other.public(), &proof, NOW),
            Err(SyncError::OfferClosed)
        ));
        assert_eq!(offer.seconds_left(NOW), 0);
    }

    #[test]
    fn an_expired_code_is_refused_on_both_sides() {
        let (_, joiner, mut offer) = setup();
        let url = offer.link().to_url();
        let expiry = NOW + OFFER_TTL_SECS;
        assert!(PairingLink::parse(&url, expiry - 1).is_ok());
        assert!(matches!(
            PairingLink::parse(&url, expiry),
            Err(SyncError::OfferClosed)
        ));
        let proof = join_proof(offer.link(), &joiner.public());
        assert!(matches!(
            offer.accept(&joiner.public(), &proof, expiry),
            Err(SyncError::OfferClosed)
        ));
    }

    #[test]
    fn a_proof_without_the_secret_is_refused_and_guessing_closes_the_offer() {
        let (node, joiner, mut offer) = setup();
        // Someone who saw only the Node's keys and address, not the secret.
        let mut forged = PairingOffer::new("192.168.1.20:7443", node.public(), NOW).unwrap();
        let guess = join_proof(forged.link(), &joiner.public());
        for _ in 0..MAX_WRONG_PROOFS - 1 {
            assert!(matches!(
                offer.accept(&joiner.public(), &guess, NOW),
                Err(SyncError::WrongProof)
            ));
        }
        assert!(matches!(
            offer.accept(&joiner.public(), &guess, NOW),
            Err(SyncError::WrongProof)
        ));
        // Closed now, even for the right proof.
        let right = join_proof(offer.link(), &joiner.public());
        assert!(matches!(
            offer.accept(&joiner.public(), &right, NOW),
            Err(SyncError::OfferClosed)
        ));
        // The forged offer is unrelated and still open.
        assert!(forged.accept(&joiner.public(), &guess, NOW).is_ok());
    }

    #[test]
    fn a_proof_made_for_other_keys_is_refused() {
        // A device in the middle replays a real joiner's proof with its own keys.
        let (_, joiner, mut offer) = setup();
        let attacker = DeviceKeys::generate().unwrap();
        let proof = join_proof(offer.link(), &joiner.public());
        assert!(matches!(
            offer.accept(&attacker.public(), &proof, NOW),
            Err(SyncError::WrongProof)
        ));
        assert!(offer.accept(&joiner.public(), &proof, NOW).is_ok());
    }

    #[test]
    fn a_different_node_key_is_caught() {
        let (_, _, offer) = setup();
        let impostor = DeviceKeys::generate().unwrap();
        assert!(matches!(
            offer.link().check_node(&impostor.public()),
            Err(SyncError::NodeKeyMismatch)
        ));
    }

    #[test]
    fn check_numbers_differ_when_a_device_sits_in_the_middle() {
        let (node, joiner, _) = setup();
        let middle = DeviceKeys::generate().unwrap();
        let joiner_sees = CheckNumbers::new(&middle.public(), &joiner.public());
        let node_sees = CheckNumbers::new(&node.public(), &middle.public());
        assert_ne!(joiner_sees, node_sees);
        assert_ne!(
            CheckNumbers::new(&node.public(), &joiner.public()),
            CheckNumbers::new(&joiner.public(), &node.public())
        );
    }

    #[test]
    fn check_numbers_keep_leading_zeros() {
        assert_eq!(CheckNumbers(4_821_093).to_string(), "0000 0482 1093");
    }

    #[test]
    fn malformed_links_are_refused() {
        let (_, _, offer) = setup();
        let good = offer.link().to_url().to_string();
        let without = |field: &str| {
            let q = good.strip_prefix(LINK_PREFIX).unwrap();
            let kept: Vec<_> = q.split('&').filter(|p| !p.starts_with(field)).collect();
            format!("{LINK_PREFIX}{}", kept.join("&"))
        };
        for bad in [
            "https://example.com/pair?v=1".to_owned(),
            without("s="),
            without("n="),
            without("a="),
            without("e="),
            without("v="),
            format!("{good}&x=1"),
            format!("{good}&s=AAAAAAAAAAAAAAAAAAAAAA"),
            good.replace("a=192.168.1.20:7443", "a=192.168.1.20"),
            good.replace("a=192.168.1.20:7443", "a=evil.example:0"),
            good.replace("a=192.168.1.20:7443", "a=ev il:80"),
            good.replace("a=192.168.1.20:7443", "a=-x:80"),
        ] {
            assert!(
                matches!(PairingLink::parse(&bad, NOW), Err(SyncError::BadLink(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            PairingLink::parse(&good.replace("v=1", "v=2"), NOW),
            Err(SyncError::NewerLinkVersion)
        ));
    }

    #[test]
    fn hosts_names_and_ipv6_addresses_are_accepted() {
        for ok in ["node.example.net:7443", "[fd00::1]:7443", "nas:1"] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in ["[]:80", "[fd00::1:80", "node:65536", "node:", ":80"] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let (_, _, offer) = setup();
        let secret = b64::encode(offer.link().secret.as_ref());
        assert!(!format!("{offer:?}").contains(&secret));
        assert!(format!("{:?}", offer.link()).contains("***"));
    }
}
