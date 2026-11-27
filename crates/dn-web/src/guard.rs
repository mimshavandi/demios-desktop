//! Which addresses a fetch may reach (#369).
//!
//! A page the model asks for must not be a way into the user's own network or the machine
//! itself: a router's admin page, a service on `localhost`, the cloud metadata address
//! (169.254.169.254) on a Node in the cloud, another device on the LAN. So every address a
//! host resolves to is checked, and only public unicast addresses are kept:
//!
//! - **IPv4:** this-network 0/8, private 10/8, 172.16/12 and 192.168/16, CGNAT 100.64/10,
//!   loopback 127/8, link-local 169.254/16 (cloud metadata), IETF assignments 192.0.0/24,
//!   the documentation and benchmarking ranges, the 6to4 relay 192.88.99/24, multicast
//!   224/4 and reserved 240/4 (with broadcast) are refused.
//! - **IPv6:** unspecified and loopback, IPv4-mapped and IPv4-compatible addresses (checked
//!   as the IPv4 address inside), NAT64 64:ff9b::/96 (checked on the IPv4 inside) and the
//!   local-use 64:ff9b:1::/48, discard 100::/64, the special-purpose 2001::/23 (Teredo and
//!   others), documentation 2001:db8::/32, 6to4 2002::/16 (checked on the IPv4 inside),
//!   unique-local fc00::/7, link-local fe80::/10, site-local fec0::/10 and multicast
//!   ff00::/8 are refused.
//!
//! [`GuardResolver`] does this at DNS time and hands the HTTP client only the addresses
//! it kept, so the connection goes to an address that was checked; there is no second
//! lookup to race. A URL whose host is an IP literal skips DNS, so the fetcher checks it
//! with [`check`] itself, on the first request and on every redirect.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

/// Why an address is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocked(pub &'static str);

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

fn v4_in(ip: Ipv4Addr, net: [u8; 4], bits: u32) -> bool {
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    (u32::from(ip) & mask) == (u32::from(Ipv4Addr::from(net)) & mask)
}

fn v6_in(ip: Ipv6Addr, net: Ipv6Addr, bits: u32) -> bool {
    let mask = if bits == 0 {
        0
    } else {
        u128::MAX << (128 - bits)
    };
    (u128::from(ip) & mask) == (u128::from(net) & mask)
}

/// Checks one IPv4 address.
pub fn check_v4(ip: Ipv4Addr) -> Result<(), Blocked> {
    const RANGES: &[([u8; 4], u32, &str)] = &[
        ([0, 0, 0, 0], 8, "a \"this network\" address"),
        ([10, 0, 0, 0], 8, "a private-network address"),
        ([100, 64, 0, 0], 10, "a carrier-grade NAT address"),
        ([127, 0, 0, 0], 8, "this computer (loopback)"),
        (
            [169, 254, 0, 0],
            16,
            "a link-local address (such as cloud metadata)",
        ),
        ([172, 16, 0, 0], 12, "a private-network address"),
        ([192, 0, 0, 0], 24, "a reserved address"),
        ([192, 0, 2, 0], 24, "a documentation address"),
        ([192, 88, 99, 0], 24, "a reserved address"),
        ([192, 168, 0, 0], 16, "a private-network address"),
        ([198, 18, 0, 0], 15, "a benchmarking address"),
        ([198, 51, 100, 0], 24, "a documentation address"),
        ([203, 0, 113, 0], 24, "a documentation address"),
        ([224, 0, 0, 0], 4, "a multicast address"),
        ([240, 0, 0, 0], 4, "a reserved address"),
    ];
    match RANGES.iter().find(|(net, bits, _)| v4_in(ip, *net, *bits)) {
        Some((_, _, why)) => Err(Blocked(why)),
        None => Ok(()),
    }
}

/// The IPv4 address inside an IPv6 address's last 32 bits.
fn low_v4(ip: Ipv6Addr) -> Ipv4Addr {
    Ipv4Addr::from((u128::from(ip) & 0xffff_ffff) as u32)
}

/// Checks one IPv6 address.
pub fn check_v6(ip: Ipv6Addr) -> Result<(), Blocked> {
    let net = |s: &str| s.parse::<Ipv6Addr>().unwrap_or(Ipv6Addr::UNSPECIFIED);
    if ip.is_unspecified() {
        return Err(Blocked("an unspecified address"));
    }
    if ip.is_loopback() {
        return Err(Blocked("this computer (loopback)"));
    }
    // Addresses that carry an IPv4 address: checked as that address.
    if v6_in(ip, net("::ffff:0:0"), 96) || v6_in(ip, net("::"), 96) {
        return check_v4(low_v4(ip));
    }
    if v6_in(ip, net("64:ff9b::"), 96) {
        return check_v4(low_v4(ip));
    }
    if v6_in(ip, net("2002::"), 16) {
        let embedded = ((u128::from(ip) >> 80) & 0xffff_ffff) as u32;
        return check_v4(Ipv4Addr::from(embedded));
    }
    const RANGES: &[(&str, u32, &str)] = &[
        ("64:ff9b:1::", 48, "a local NAT64 address"),
        ("100::", 64, "a discard address"),
        (
            "2001::",
            23,
            "a special-purpose address (such as a Teredo tunnel)",
        ),
        ("2001:db8::", 32, "a documentation address"),
        ("fc00::", 7, "a private-network (unique-local) address"),
        ("fe80::", 10, "a link-local address"),
        ("fec0::", 10, "a site-local address"),
        ("ff00::", 8, "a multicast address"),
    ];
    match RANGES.iter().find(|(n, bits, _)| v6_in(ip, net(n), *bits)) {
        Some((_, _, why)) => Err(Blocked(why)),
        None => Ok(()),
    }
}

/// Checks one address: `Ok` only for a public unicast address.
pub fn check(ip: IpAddr) -> Result<(), Blocked> {
    match ip {
        IpAddr::V4(v4) => check_v4(v4),
        IpAddr::V6(v6) => check_v6(v6),
    }
}

/// The host could only be reached on addresses [`check`] refuses.
#[derive(Debug, thiserror::Error)]
#[error("{host} is {reason}, which pages can't be fetched from")]
pub struct BlockedHost {
    pub host: String,
    pub reason: Blocked,
}

/// Looks a host name up. The default is the system's resolver; tests give their own.
pub type Lookup = Arc<dyn Fn(String) -> reqwest::dns::Resolving + Send + Sync>;

/// A DNS resolver for the HTTP client that keeps only addresses [`check`] allows.
pub struct GuardResolver {
    lookup: Lookup,
}

impl GuardResolver {
    /// Resolves with the system's resolver.
    pub fn system() -> Self {
        Self {
            lookup: Arc::new(|host: String| {
                Box::pin(async move {
                    let addrs: Vec<SocketAddr> =
                        tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
                    let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
                    Ok(addrs)
                })
            }),
        }
    }

    /// Resolves with `lookup` (tests).
    pub fn with_lookup(lookup: Lookup) -> Self {
        Self { lookup }
    }
}

impl reqwest::dns::Resolve for GuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let lookup = (self.lookup)(host.clone());
        Box::pin(async move {
            let mut first_refusal = None;
            let kept: Vec<SocketAddr> = lookup
                .await?
                .filter(|a| match check(a.ip()) {
                    Ok(()) => true,
                    Err(why) => {
                        first_refusal.get_or_insert(why);
                        false
                    }
                })
                .collect();
            if kept.is_empty() {
                let reason = first_refusal.unwrap_or(Blocked("not found"));
                return Err(Box::new(BlockedHost { host, reason }) as _);
            }
            let addrs: reqwest::dns::Addrs = Box::new(kept.into_iter());
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> Result<(), Blocked> {
        check(s.parse().unwrap())
    }

    #[test]
    fn non_public_ipv4_is_refused() {
        for s in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(v4(s).is_err(), "{s} was allowed");
        }
        assert_eq!(
            v4("169.254.169.254"),
            Err(Blocked("a link-local address (such as cloud metadata)"))
        );
    }

    #[test]
    fn public_ipv4_is_allowed_including_the_edges_of_blocked_ranges() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.167.255.255",
            "192.169.0.0",
            "223.255.255.255",
        ] {
            assert_eq!(v4(s), Ok(()), "{s} was refused");
        }
    }

    #[test]
    fn non_public_ipv6_is_refused_including_ipv4_inside_it() {
        for s in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "2002:7f00:0001::1",
            "2002:c0a8:0101::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ] {
            assert!(check(s.parse().unwrap()).is_err(), "{s} was allowed");
        }
    }

    #[test]
    fn public_ipv6_is_allowed() {
        for s in [
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:0808:0808::1",
        ] {
            assert_eq!(check(s.parse().unwrap()), Ok(()), "{s} was refused");
        }
    }

    fn fixed(addrs: Vec<&'static str>) -> GuardResolver {
        GuardResolver::with_lookup(Arc::new(move |_| {
            let a: Vec<SocketAddr> = addrs.iter().map(|s| s.parse().unwrap()).collect();
            Box::pin(async move {
                let a: reqwest::dns::Addrs = Box::new(a.into_iter());
                Ok(a)
            })
        }))
    }

    async fn resolve(r: &GuardResolver, host: &str) -> Result<Vec<SocketAddr>, String> {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = host.parse().unwrap();
        r.resolve(name)
            .await
            .map(|a| a.collect())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn the_resolver_keeps_only_public_addresses() {
        let r = fixed(vec!["10.0.0.5:0", "93.184.216.34:0", "[::1]:0"]);
        assert_eq!(
            resolve(&r, "mixed.example").await.unwrap(),
            vec!["93.184.216.34:0".parse::<SocketAddr>().unwrap()]
        );
        let r = fixed(vec!["169.254.169.254:0"]);
        let e = resolve(&r, "metadata.example").await.unwrap_err();
        assert!(
            e.contains("metadata.example is a link-local address"),
            "{e}"
        );
    }
}
