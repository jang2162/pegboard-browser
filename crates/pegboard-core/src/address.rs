//! Where a session may send requests: the address rules of the design (5.3), as pure
//! functions. `guard` applies them to every request of the agent profiles.
//!
//! Order: (1) pegboard's own ports on a loopback address, always refused; (2) the scheme;
//! (3) with an allow-list, only the listed domains and their subdomains (loopback and
//! private addresses still need what (4) and (5) ask for); (4) loopback, open unless
//! `no_localhost`; (5) private network, closed unless `allow_private_network`.
//!
//! Addresses are judged as parsed values, never as spellings: the URL is parsed by the
//! WHATWG rules (`url` crate, as Chrome does: `2130706433` and `0x7f.1` are 127.0.0.1), and
//! IPv6 forms that carry an IPv4 address (`::ffff:a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4
//! `2002::/16`, the deprecated `::a.b.c.d`) are judged as that IPv4 address.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use pegboard_protocol::SessionPolicy;
use url::{Host, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// One of the reasons below.
    Deny(&'static str),
}

pub const OWN_PORT: &str = "pegboard's own port";
pub const SCHEME: &str = "scheme not allowed";
pub const NOT_LISTED: &str = "not in the session's allowed domains";
pub const LOOPBACK: &str = "localhost is closed for this session";
pub const PRIVATE: &str = "private network address";

/// What a URL's host is, once parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A DNS name: lower case, without a trailing dot.
    Name(String),
    /// An IP literal, in its canonical form (see `canonical`).
    Ip(IpAddr),
}

/// Judges a URL a page or an agent wants to load. `own_ports`: the browser's debugging port
/// and the session proxy's port.
pub fn judge_url(url: &str, policy: &SessionPolicy, own_ports: &[u16]) -> Verdict {
    let Ok(url) = Url::parse(url) else {
        return Verdict::Deny(SCHEME);
    };
    let scheme = url.scheme();
    let target = target_of(&url);
    // 1. Own ports, whatever the scheme.
    if let (Some(target), Some(port)) = (&target, url.port_or_known_default()) {
        if reaches_loopback(target) && own_ports.contains(&port) {
            return Verdict::Deny(OWN_PORT);
        }
    }
    // 2. Scheme.
    match scheme {
        "data" => return Verdict::Allow,
        "about" if url.path() == "blank" => return Verdict::Allow,
        "http" | "https" | "ws" | "wss" => {}
        _ => return Verdict::Deny(SCHEME),
    }
    let Some(target) = target else {
        return Verdict::Deny(SCHEME);
    };
    // 3. Allow-list.
    if !policy.allow_domains.is_empty() && !is_listed(&target, &policy.allow_domains) {
        return Verdict::Deny(NOT_LISTED);
    }
    // 4. and 5.
    judge_class(
        reaches_loopback(&target),
        target_is_private(&target),
        policy,
    )
}

/// Second look, once an address the host name resolves to is known: `ip` and `port` are
/// where the connection goes, `host` the name it was asked for.
pub fn judge_resolved(
    ip: IpAddr,
    port: u16,
    host: &str,
    policy: &SessionPolicy,
    own_ports: &[u16],
) -> Verdict {
    let ip = canonical(ip);
    let loopback = is_loopback(ip) || ip.is_unspecified();
    if loopback && own_ports.contains(&port) {
        return Verdict::Deny(OWN_PORT);
    }
    let list = &policy.allow_domains;
    if !list.is_empty() {
        if !is_listed(&target_of_host(host), list) {
            return Verdict::Deny(NOT_LISTED);
        }
        // A listed name that resolves to loopback opens it only if the list opens loopback.
        if loopback && !list_opens_loopback(list, ip) {
            return Verdict::Deny(LOOPBACK);
        }
    }
    judge_class(loopback, is_private(ip), policy)
}

/// Rules 4 and 5.
fn judge_class(loopback: bool, private: bool, policy: &SessionPolicy) -> Verdict {
    if loopback && policy.no_localhost {
        return Verdict::Deny(LOOPBACK);
    }
    if private && !policy.allow_private_network {
        return Verdict::Deny(PRIVATE);
    }
    Verdict::Allow
}

/// `127.0.0.0/8` and `::1`, also in the IPv6 forms that carry an IPv4 address.
pub fn is_loopback(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => v4.octets()[0] == 127,
        IpAddr::V6(v6) => v6 == Ipv6Addr::LOCALHOST,
    }
}

/// RFC 1918, link-local, CGNAT, `0.0.0.0/8`, IPv6 ULA, IPv6 link-local and `::`; IPv6 forms
/// that carry an IPv4 address are judged as that address.
pub fn is_private(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 10
                || a == 0
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254)
                || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// `localhost` and `*.localhost`, ignoring case and a trailing dot.
pub fn is_loopback_name(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost")
}

/// The IPv4 address an IPv6 address carries (IPv4-mapped, NAT64 `64:ff9b::/96`, 6to4
/// `2002::/16`, the deprecated IPv4-compatible `::a.b.c.d`), or the address itself.
pub fn canonical(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    let s = v6.segments();
    let low = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return IpAddr::V4(low(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return IpAddr::V4(low(s[1], s[2]));
    }
    if s[..6] == [0; 6] && !v6.is_unspecified() && v6 != Ipv6Addr::LOCALHOST {
        return IpAddr::V4(low(s[6], s[7]));
    }
    ip
}

/// The host of a parsed URL, if it has one.
pub fn target_of(url: &Url) -> Option<Target> {
    match url.host()? {
        Host::Domain("") => None,
        Host::Domain(d) => Some(target_of_host(d)),
        Host::Ipv4(v4) => Some(Target::Ip(IpAddr::V4(v4))),
        Host::Ipv6(v6) => Some(Target::Ip(canonical(IpAddr::V6(v6)))),
    }
}

/// A host as a URL or a resolver names it: an IP literal (with or without brackets) or a
/// name.
fn target_of_host(host: &str) -> Target {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<IpAddr>() {
        Ok(ip) => Target::Ip(canonical(ip)),
        Err(_) => {
            let name = host.strip_suffix('.').unwrap_or(host);
            Target::Name(name.to_ascii_lowercase())
        }
    }
}

fn reaches_loopback(target: &Target) -> bool {
    match target {
        Target::Name(n) => is_loopback_name(n),
        // `0.0.0.0` and `::` connect to the local machine too.
        Target::Ip(ip) => is_loopback(*ip) || canonical(*ip).is_unspecified(),
    }
}

fn target_is_private(target: &Target) -> bool {
    match target {
        Target::Name(_) => false,
        Target::Ip(ip) => is_private(*ip),
    }
}

/// A name matches a listed name or is a subdomain of it; an IP literal matches only the same
/// address (no suffix matching on numbers).
fn is_listed(target: &Target, list: &[String]) -> bool {
    list.iter()
        .any(|entry| match (target, target_of_host(entry)) {
            (Target::Ip(ip), Target::Ip(listed)) => *ip == listed,
            (Target::Name(name), Target::Name(listed)) => {
                *name == listed
                    || name
                        .strip_suffix(listed.as_str())
                        .is_some_and(|head| head.ends_with('.'))
            }
            _ => false,
        })
}

/// Whether an allow-list opens the loopback address `ip`: it names `localhost` (or a
/// `*.localhost` name), or `ip` itself.
fn list_opens_loopback(list: &[String], ip: IpAddr) -> bool {
    list.iter().any(|entry| match target_of_host(entry) {
        Target::Name(n) => is_loopback_name(&n),
        Target::Ip(listed) => listed == ip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTS: &[u16] = &[9222, 9333];

    fn with(f: impl FnOnce(&mut SessionPolicy)) -> SessionPolicy {
        let mut p = SessionPolicy::default();
        f(&mut p);
        p
    }

    fn list(domains: &[&str]) -> SessionPolicy {
        with(|p| p.allow_domains = domains.iter().map(|d| d.to_string()).collect())
    }

    fn check(policy: &SessionPolicy, cases: &[(&str, Verdict)]) {
        for (url, want) in cases {
            assert_eq!(
                judge_url(url, policy, PORTS),
                *want,
                "{url} under {policy:?}"
            );
        }
    }

    use Verdict::{Allow, Deny};

    #[test]
    fn address_table() {
        let default = SessionPolicy::default();
        check(
            &default,
            &[
                ("http://localhost:3000/", Allow),
                ("http://LOCALHOST./", Allow),
                ("http://a.localhost/", Allow),
                ("http://127.0.0.2/", Allow),
                ("http://[::1]/", Allow),
                ("http://127.0.0.1:9222/json", Deny(OWN_PORT)),
                ("http://localhost:9333/", Deny(OWN_PORT)),
                ("ws://[::1]:9222/devtools/x", Deny(OWN_PORT)),
                ("http://2130706433:9222/", Deny(OWN_PORT)),
                ("http://0x7f.1:9222/", Deny(OWN_PORT)),
                ("http://0.0.0.0:9222/", Deny(OWN_PORT)),
                ("http://[::ffff:127.0.0.1]:9333/", Deny(OWN_PORT)),
                ("http://10.0.0.1/", Deny(PRIVATE)),
                ("http://192.168.1.1/", Deny(PRIVATE)),
                ("http://172.16.0.1/", Deny(PRIVATE)),
                ("http://169.254.169.254/", Deny(PRIVATE)),
                ("http://100.64.0.1/", Deny(PRIVATE)),
                ("http://[fc00::1]/", Deny(PRIVATE)),
                ("http://[fe80::1]/", Deny(PRIVATE)),
                ("http://[::ffff:10.0.0.1]/", Deny(PRIVATE)),
                ("http://[64:ff9b::a00:1]/", Deny(PRIVATE)),
                ("http://[2002:a00:1::]/", Deny(PRIVATE)),
                ("http://0.0.0.0/", Deny(PRIVATE)),
                ("http://[::]/", Deny(PRIVATE)),
                ("https://example.com/", Allow),
                ("http://172.32.0.1/", Allow),
                ("http://100.128.0.1/", Allow),
                ("data:text/html,x", Allow),
                ("about:blank", Allow),
                ("file:///etc/hosts", Deny(SCHEME)),
                ("chrome://settings", Deny(SCHEME)),
                ("javascript:1", Deny(SCHEME)),
                ("blob:x", Deny(SCHEME)),
                ("about:srcdoc", Deny(SCHEME)),
                ("not a url", Deny(SCHEME)),
            ],
        );
        check(
            &with(|p| p.no_localhost = true),
            &[
                ("http://localhost/", Deny(LOOPBACK)),
                ("http://127.0.0.1/", Deny(LOOPBACK)),
                ("http://[::ffff:7f00:1]/", Deny(LOOPBACK)),
                ("http://127.0.0.1:9222/", Deny(OWN_PORT)),
                ("https://example.com/", Allow),
            ],
        );
        check(
            &with(|p| p.allow_private_network = true),
            &[
                ("http://10.0.0.1/", Allow),
                ("http://[fd00::1]/", Allow),
                ("http://127.0.0.1:9222/", Deny(OWN_PORT)),
                ("http://0.0.0.0:9333/", Deny(OWN_PORT)),
            ],
        );
        check(
            &list(&["example.com"]),
            &[
                ("https://example.com/", Allow),
                ("https://a.b.example.com/", Allow),
                ("https://EXAMPLE.com./", Allow),
                ("wss://example.com/socket", Allow),
                ("https://example.com.evil.test/", Deny(NOT_LISTED)),
                ("https://notexample.com/", Deny(NOT_LISTED)),
                ("http://localhost/", Deny(NOT_LISTED)),
                ("https://example.org/", Deny(NOT_LISTED)),
                ("http://93.184.216.34/", Deny(NOT_LISTED)),
                ("data:text/html,x", Allow),
                ("http://127.0.0.1:9222/", Deny(OWN_PORT)),
            ],
        );
        check(
            &list(&["localhost"]),
            &[
                ("http://localhost:3000/", Allow),
                ("http://a.localhost/", Allow),
                ("http://localhost:9222/", Deny(OWN_PORT)),
                ("http://127.0.0.1:3000/", Deny(NOT_LISTED)),
            ],
        );
        check(
            &list(&["10.0.0.1"]),
            &[
                ("http://10.0.0.1/", Deny(PRIVATE)),
                ("http://10.0.0.2/", Deny(NOT_LISTED)),
            ],
        );
        let private_listed = with(|p| {
            p.allow_domains = vec!["10.0.0.1".into(), "::1".into()];
            p.allow_private_network = true;
        });
        check(
            &private_listed,
            &[
                ("http://10.0.0.1/", Allow),
                ("http://[::ffff:10.0.0.1]/", Allow),
                ("http://[::1]:3000/", Allow),
                ("http://1.0.0.10/", Deny(NOT_LISTED)),
            ],
        );
    }

    #[test]
    fn resolved_address_overrides_a_public_name() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let default = SessionPolicy::default();
        let judge = |a: &str, port: u16, policy: &SessionPolicy| {
            judge_resolved(ip(a), port, "public.example", policy, PORTS)
        };
        assert_eq!(judge("10.0.0.5", 80, &default), Deny(PRIVATE));
        assert_eq!(judge("::ffff:10.0.0.5", 80, &default), Deny(PRIVATE));
        assert_eq!(judge("127.0.0.1", 9222, &default), Deny(OWN_PORT));
        assert_eq!(judge("0.0.0.0", 9333, &default), Deny(OWN_PORT));
        assert_eq!(judge("127.0.0.1", 80, &default), Allow);
        assert_eq!(judge("93.184.216.34", 443, &default), Allow);
        let listed = list(&["public.example"]);
        assert_eq!(judge("10.0.0.5", 80, &listed), Deny(PRIVATE));
        assert_eq!(judge("127.0.0.1", 80, &listed), Deny(LOOPBACK));
        assert_eq!(judge("93.184.216.34", 443, &listed), Allow);
        assert_eq!(
            judge_resolved(ip("93.184.216.34"), 443, "other.example", &listed, PORTS),
            Deny(NOT_LISTED)
        );
        let open = with(|p| {
            p.allow_domains = vec!["public.example".into()];
            p.allow_private_network = true;
        });
        assert_eq!(judge("10.0.0.5", 80, &open), Allow);
        let no_local = with(|p| p.no_localhost = true);
        assert_eq!(judge("127.0.0.1", 80, &no_local), Deny(LOOPBACK));
    }

    #[test]
    fn address_classes() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for a in [
            "127.0.0.1",
            "127.255.0.9",
            "::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
        ] {
            assert!(is_loopback(ip(a)), "{a}");
            assert!(!is_private(ip(a)), "{a}");
        }
        for a in [
            "10.1.2.3",
            "172.31.255.255",
            "192.168.0.1",
            "169.254.1.1",
            "100.127.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "::",
            "fd12::1",
            "fe80::1",
            "febf::1",
            "2002:c0a8:101::",
        ] {
            assert!(is_private(ip(a)), "{a}");
            assert!(!is_loopback(ip(a)), "{a}");
        }
        for a in [
            "8.8.8.8",
            "172.15.0.1",
            "100.63.0.1",
            "2001:db8::1",
            "fec0::1",
        ] {
            assert!(!is_private(ip(a)) && !is_loopback(ip(a)), "{a}");
        }
        for n in ["localhost", "LocalHost.", "a.b.localhost", "x.localhost."] {
            assert!(is_loopback_name(n), "{n}");
        }
        for n in ["localhost.com", "notlocalhost", "localhostx", ""] {
            assert!(!is_loopback_name(n), "{n}");
        }
    }
}
