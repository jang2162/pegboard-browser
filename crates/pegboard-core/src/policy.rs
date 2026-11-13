//! Session policy: which classes of command a session may run, and the profile-wide
//! settings that sessions sharing a profile must agree on.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use pegboard_protocol::{codes, ActionClass, RpcError, SessionPolicy};

fn error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        code: code.to_string(),
        message: message.into(),
        data: None,
    }
}

fn invalid(message: impl Into<String>) -> RpcError {
    error(codes::INVALID_PARAMS, message)
}

/// Classes a session may run without being given them: all but `upload`, `state` and
/// `clipboard`.
pub fn default_allowed(class: ActionClass) -> bool {
    !matches!(
        class,
        ActionClass::Upload | ActionClass::State | ActionClass::Clipboard
    )
}

/// Whether the policy lets the session run a command of `class`. `allow` opens a class that
/// is closed by default, `deny` closes one that is open by default; nothing else matters.
pub fn judge_action(policy: &SessionPolicy, class: ActionClass) -> Result<(), RpcError> {
    let name = class.as_str();
    if default_allowed(class) {
        if policy.deny.contains(&class) {
            return Err(error(
                codes::POLICY_DENIED,
                format!("the session's policy denies {name} commands (--deny {name})"),
            ));
        }
    } else if !policy.allow.contains(&class) {
        // Uploading also needs the folder the files come from.
        let with = if class == ActionClass::Upload {
            " and --upload-dir <directory>"
        } else {
            ""
        };
        return Err(error(
            codes::POLICY_DENIED,
            format!(
                "the session's policy does not allow {name} commands; a person can allow it \
                 when creating the session with --allow {name}{with}"
            ),
        ));
    }
    Ok(())
}

/// Refuses a policy that contradicts itself or cannot mean what it says (`invalid_params`).
pub fn validate(policy: &SessionPolicy) -> Result<(), RpcError> {
    for class in &policy.allow {
        let name = class.as_str();
        if policy.deny.contains(class) {
            return Err(invalid(format!("{name} is both allowed and denied")));
        }
        if default_allowed(*class) {
            return Err(invalid(format!(
                "--allow {name} has no effect: {name} commands are allowed by default \
                 (did you mean --deny?)"
            )));
        }
    }
    for class in &policy.deny {
        let name = class.as_str();
        if !default_allowed(*class) {
            return Err(invalid(format!(
                "--deny {name} has no effect: {name} commands are denied by default \
                 (did you mean --allow?)"
            )));
        }
    }
    for domain in &policy.allow_domains {
        if !is_host(domain) {
            return Err(invalid(format!(
                "{domain:?} is not a host name or IP address (no scheme, path, port or \
                 wildcard) for --allow-domain"
            )));
        }
    }
    let upload_allowed = policy.allow.contains(&ActionClass::Upload);
    match (&policy.upload_dir, upload_allowed) {
        (Some(dir), true) => {
            let path = Path::new(dir);
            if !path.is_absolute() {
                return Err(invalid(format!(
                    "--upload-dir {dir:?} is not an absolute path"
                )));
            }
            if !path.is_dir() {
                return Err(invalid(format!(
                    "--upload-dir {dir:?} is not an existing directory"
                )));
            }
        }
        (Some(_), false) => {
            return Err(invalid("--upload-dir needs --allow upload"));
        }
        (None, true) => {
            return Err(invalid("--allow upload needs --upload-dir"));
        }
        (None, false) => {}
    }
    Ok(())
}

/// Refuses an upload directory that is pegboard's home (where the sessions' files, the
/// recordings and the browser profile live), inside it, or contains it. Compared by real
/// path. It needs the home, so `SessionManager::create` (the server) does it, right after
/// `validate`.
pub fn check_upload_dir_outside_home(policy: &SessionPolicy, home: &Path) -> Result<(), RpcError> {
    let Some(dir) = policy.upload_dir.as_deref() else {
        return Ok(());
    };
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (dir, home) = (real(Path::new(dir)), real(home));
    if dir.starts_with(&home) || home.starts_with(&dir) {
        return Err(invalid(
            "--upload-dir must not be pegboard's own data directory or contain it",
        ));
    }
    Ok(())
}

/// The policy as it is kept: domains lower-cased (IP literals in `IpAddr` form), sorted and
/// without duplicates; `allow` and `deny` without duplicates, in the order given.
pub fn normalized(policy: &SessionPolicy) -> SessionPolicy {
    let mut policy = policy.clone();
    policy.allow_domains = normalized_domains(&policy.allow_domains);
    dedup_keeping_order(&mut policy.allow);
    dedup_keeping_order(&mut policy.deny);
    policy
}

fn dedup_keeping_order(classes: &mut Vec<ActionClass>) {
    let mut seen = Vec::new();
    classes.retain(|c| {
        let fresh = !seen.contains(c);
        seen.push(*c);
        fresh
    });
}

fn normalized_domains(domains: &[String]) -> Vec<String> {
    let mut list: Vec<String> = domains
        .iter()
        .map(|d| match d.parse::<IpAddr>() {
            Ok(ip) => ip.to_string(),
            Err(_) => d.to_ascii_lowercase(),
        })
        .collect();
    list.sort();
    list.dedup();
    list
}

/// An allow-list entry: an IP literal `std` parses (no brackets), or a DNS name. A name is
/// dot-separated labels of ASCII letters, digits and hyphens (punycode included), none empty
/// or starting or ending with a hyphen, at most 253 characters in all; its last label is not
/// all digits and does not start with `0x`, because resolvers and URL parsers read such names
/// as numbers (`1`, `127.1`, `2130706433`, `0x7f.0.0.1` are all 127.0.0.1 or near it).
fn is_host(s: &str) -> bool {
    if s.parse::<IpAddr>().is_ok() {
        return true;
    }
    let Some(last) = s.rsplit('.').next() else {
        return false;
    };
    !s.is_empty()
        && s.len() <= 253
        && !last.bytes().all(|b| b.is_ascii_digit())
        && !last.to_ascii_lowercase().starts_with("0x")
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// The address policy, which Chrome applies to a profile's service and shared workers
/// without telling which session they work for: sessions in one profile share one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressPins {
    /// Sorted, deduplicated, lower-case.
    pub allow_domains: Vec<String>,
    pub allow_private_network: bool,
    pub no_localhost: bool,
}

impl std::fmt::Display for AddressPins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.allow_domains.is_empty() {
            write!(f, "no allow-list")?;
        } else {
            write!(f, "allow-domain {}", self.allow_domains.join(","))?;
        }
        if self.allow_private_network {
            write!(f, ", private network allowed")?;
        }
        if self.no_localhost {
            write!(f, ", localhost blocked")?;
        }
        Ok(())
    }
}

/// Settings Chrome enforces per profile (browser context) only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePins {
    pub clipboard: bool,
    pub address: AddressPins,
}

pub fn pins_of(policy: &SessionPolicy) -> ProfilePins {
    ProfilePins {
        clipboard: policy.allow.contains(&ActionClass::Clipboard),
        address: AddressPins {
            allow_domains: normalized_domains(&policy.allow_domains),
            allow_private_network: policy.allow_private_network,
            no_localhost: policy.no_localhost,
        },
    }
}

type Pinned = HashMap<String, (ProfilePins, usize)>;

/// Profile name -> what it is pinned to and how many live sessions hold the pin.
#[derive(Debug, Clone, Default)]
pub struct PinTable {
    pinned: Arc<Mutex<Pinned>>,
}

impl PinTable {
    /// Joins the profile's pin (the first session sets it), or refuses with
    /// `profile_policy_conflict` when the profile is pinned to other values.
    pub fn acquire(&self, profile: &str, pins: ProfilePins) -> Result<PinGuard, RpcError> {
        let mut table = self.pinned.lock().unwrap();
        match table.get_mut(profile) {
            Some((held, count)) => {
                if *held != pins {
                    return Err(conflict(profile, held, &pins));
                }
                *count += 1;
            }
            None => {
                table.insert(profile.to_string(), (pins, 1));
            }
        }
        Ok(PinGuard {
            pinned: self.pinned.clone(),
            profile: profile.to_string(),
        })
    }
}

fn conflict(profile: &str, held: &ProfilePins, wanted: &ProfilePins) -> RpcError {
    let mut items = Vec::new();
    if held.clipboard != wanted.clipboard {
        let value = if held.clipboard { "allowed" } else { "denied" };
        items.push(format!("clipboard (pinned to: {value})"));
    }
    if held.address != wanted.address {
        items.push(format!("address policy (pinned to: {})", held.address));
    }
    error(
        codes::PROFILE_POLICY_CONFLICT,
        format!(
            "profile {profile:?} is pinned by its open sessions to a different {}; settings \
             Chrome applies to a whole profile are set by its first session. Use another \
             profile (--profile) for this session",
            items.join(" and ")
        ),
    )
}

/// A session's hold on its profile's pin. Dropping it releases the hold; the pin clears
/// when the last one is gone.
#[derive(Debug)]
pub struct PinGuard {
    pinned: Arc<Mutex<Pinned>>,
    profile: String,
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        let mut table = self.pinned.lock().unwrap();
        if let Some((_, count)) = table.get_mut(&self.profile) {
            *count -= 1;
            if *count == 0 {
                table.remove(&self.profile);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pegboard_protocol::{codes, ActionClass, SessionPolicy};

    fn policy(allow: &[ActionClass], deny: &[ActionClass]) -> SessionPolicy {
        SessionPolicy {
            allow: allow.to_vec(),
            deny: deny.to_vec(),
            ..SessionPolicy::default()
        }
    }

    fn domains(list: &[&str]) -> SessionPolicy {
        SessionPolicy {
            allow_domains: list.iter().map(|d| d.to_string()).collect(),
            ..SessionPolicy::default()
        }
    }

    #[test]
    fn defaults() {
        for c in ActionClass::ALL {
            let closed = matches!(
                c,
                ActionClass::Upload | ActionClass::State | ActionClass::Clipboard
            );
            assert_eq!(default_allowed(c), !closed, "{c:?}");
            assert_eq!(
                judge_action(&SessionPolicy::default(), c).is_ok(),
                !closed,
                "{c:?}"
            );
        }
    }

    #[test]
    fn allow_and_deny_change_one_class_only() {
        let p = policy(&[ActionClass::State], &[ActionClass::Eval]);
        for c in ActionClass::ALL {
            let expect = match c {
                ActionClass::State => true,
                ActionClass::Eval => false,
                other => default_allowed(other),
            };
            assert_eq!(judge_action(&p, c).is_ok(), expect, "{c:?}");
        }
    }

    #[test]
    fn denial_messages() {
        let e = judge_action(&SessionPolicy::default(), ActionClass::State).unwrap_err();
        assert_eq!(e.code, codes::POLICY_DENIED);
        assert_eq!(
            e.message,
            "the session's policy does not allow state commands; a person can allow it when \
             creating the session with --allow state"
        );
        let e = judge_action(&SessionPolicy::default(), ActionClass::Upload).unwrap_err();
        assert_eq!(e.code, codes::POLICY_DENIED);
        assert!(
            e.message
                .contains("--allow upload and --upload-dir <directory>"),
            "{}",
            e.message
        );
        let e = judge_action(&policy(&[], &[ActionClass::Eval]), ActionClass::Eval).unwrap_err();
        assert_eq!(e.code, codes::POLICY_DENIED);
        assert_eq!(
            e.message,
            "the session's policy denies eval commands (--deny eval)"
        );
    }

    #[test]
    fn validate_accepts_sensible_policies() {
        assert!(validate(&SessionPolicy::default()).is_ok());
        assert!(validate(&policy(&[ActionClass::State], &[ActionClass::Eval])).is_ok());
        assert!(validate(&policy(&[ActionClass::Clipboard], &[ActionClass::Download])).is_ok());
        assert!(validate(&domains(&["a.test", "Sub.B.Test", "10.0.0.1", "::1"])).is_ok());
        let dir = std::env::temp_dir().display().to_string();
        let upload = SessionPolicy {
            allow: vec![ActionClass::Upload],
            upload_dir: Some(dir),
            ..SessionPolicy::default()
        };
        assert!(validate(&upload).is_ok());
    }

    #[test]
    fn the_upload_dir_stays_out_of_the_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home");
        let other = root.join("other");
        let inside = home.join("sessions");
        for d in [&home, &other, &inside] {
            std::fs::create_dir_all(d).unwrap();
        }
        let link = root.join("link-to-home");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let with = |d: &Path| SessionPolicy {
            allow: vec![ActionClass::Upload],
            upload_dir: Some(d.display().to_string()),
            ..SessionPolicy::default()
        };
        assert!(check_upload_dir_outside_home(&with(&other), &home).is_ok());
        assert!(check_upload_dir_outside_home(&SessionPolicy::default(), &home).is_ok());
        // The home itself, inside it, containing it, and a link to it.
        for bad in [&home, &inside, root, &link] {
            let e = check_upload_dir_outside_home(&with(bad), &home).unwrap_err();
            assert_eq!(e.code, codes::INVALID_PARAMS, "{bad:?}");
            assert_eq!(
                e.message,
                "--upload-dir must not be pegboard's own data directory or contain it"
            );
        }
        // A sibling whose name starts like the home's is outside it.
        let sibling = root.join("home2");
        std::fs::create_dir(&sibling).unwrap();
        assert!(check_upload_dir_outside_home(&with(&sibling), &home).is_ok());
    }

    #[test]
    fn validate_rejects_contradictions() {
        let bad = |p: SessionPolicy| {
            let e = validate(&p).expect_err(&format!("{p:?} was accepted"));
            assert_eq!(e.code, codes::INVALID_PARAMS, "{p:?}");
        };
        bad(policy(&[ActionClass::State], &[ActionClass::State]));
        bad(policy(&[ActionClass::Navigate], &[]));
        bad(policy(&[], &[ActionClass::Upload]));
        bad(policy(&[], &[ActionClass::State]));
        // upload_dir without allow upload, and the reverse
        let dir = std::env::temp_dir().display().to_string();
        bad(SessionPolicy {
            upload_dir: Some(dir),
            ..SessionPolicy::default()
        });
        bad(policy(&[ActionClass::Upload], &[]));
        // upload_dir must be an absolute, existing directory
        for d in ["relative/dir", "/no/such/pegboard-dir", "/etc/hosts"] {
            bad(SessionPolicy {
                allow: vec![ActionClass::Upload],
                upload_dir: Some(d.into()),
                ..SessionPolicy::default()
            });
        }
        for d in [
            "https://a.test",
            "a.test/x",
            "*.a.test",
            "a.test:80",
            "",
            "a b",
            "user@a.test",
        ] {
            bad(domains(&[d]));
        }
    }

    #[test]
    fn host_rules() {
        for ok in [
            "a.test",
            "xn--bcher-kva.example",
            "localhost",
            "10.0.0.1",
            "::1",
            "0:0:0:0:0:0:0:1",
            "A.Test",
        ] {
            assert!(validate(&domains(&[ok])).is_ok(), "{ok:?} was refused");
        }
        for bad in [
            "1",
            "127.1",
            "2130706433",
            "0x7f.0.0.1",
            "a.0x1",
            "a.test.",
            "-a.test",
            "a-.test",
            "a..test",
            "[::1]",
            "a.test:80",
            "https://a.test",
            "*.a.test",
            "bücher.example",
            "",
        ] {
            let e = validate(&domains(&[bad])).expect_err(&format!("{bad:?} was accepted"));
            assert_eq!(e.code, codes::INVALID_PARAMS, "{bad:?}");
        }
        assert_eq!(
            normalized(&domains(&["0:0:0:0:0:0:0:1"])).allow_domains,
            ["::1"]
        );
        assert_eq!(
            pins_of(&domains(&["0:0:0:0:0:0:0:1"])),
            pins_of(&domains(&["::1"]))
        );
        assert!(validate(&domains(&[&"a".repeat(64)])).is_err());
        assert!(validate(&domains(&[&format!("{}.test", "a.".repeat(130))])).is_err());
    }

    #[test]
    fn normalized_dedups_classes_in_order() {
        let p = normalized(&policy(
            &[
                ActionClass::State,
                ActionClass::Clipboard,
                ActionClass::State,
            ],
            &[ActionClass::Eval, ActionClass::Eval, ActionClass::Download],
        ));
        assert_eq!(p.allow, [ActionClass::State, ActionClass::Clipboard]);
        assert_eq!(p.deny, [ActionClass::Eval, ActionClass::Download]);
    }

    #[test]
    fn normalized_lowers_sorts_and_dedups_domains() {
        let p = normalized(&domains(&["B.test", "a.test", "b.TEST"]));
        assert_eq!(p.allow_domains, ["a.test", "b.test"]);
    }

    fn pins(clipboard: bool, allow: &[&str], private: bool, no_localhost: bool) -> ProfilePins {
        ProfilePins {
            clipboard,
            address: AddressPins {
                allow_domains: allow.iter().map(|d| d.to_string()).collect(),
                allow_private_network: private,
                no_localhost,
            },
        }
    }

    #[test]
    fn pins_of_reads_the_pinned_items() {
        assert_eq!(
            pins_of(&SessionPolicy::default()),
            pins(false, &[], false, false)
        );
        let p = SessionPolicy {
            allow: vec![ActionClass::Clipboard],
            allow_domains: vec!["B.test".into(), "a.test".into(), "b.test".into()],
            allow_private_network: true,
            no_localhost: true,
            ..SessionPolicy::default()
        };
        assert_eq!(pins_of(&p), pins(true, &["a.test", "b.test"], true, true));
        // Unrelated fields do not pin anything.
        let q = SessionPolicy {
            deny: vec![ActionClass::Eval],
            no_expire: true,
            ..SessionPolicy::default()
        };
        assert_eq!(pins_of(&q), pins_of(&SessionPolicy::default()));
    }

    #[test]
    fn pins_conflict_and_release() {
        let table = PinTable::default();
        let a = pins(false, &[], false, false);
        let b = pins(true, &[], false, false);
        let g1 = table.acquire("p", a.clone()).unwrap();
        let g2 = table.acquire("p", a.clone()).unwrap();
        let e = table.acquire("p", b.clone()).unwrap_err();
        assert_eq!(e.code, codes::PROFILE_POLICY_CONFLICT);
        assert!(e.message.contains("clipboard"), "{}", e.message);
        assert!(e.message.contains("denied"), "{}", e.message);
        assert!(e.message.contains("--profile"), "{}", e.message);
        // Another profile is independent.
        let other = table.acquire("q", b.clone()).unwrap();
        drop(g1);
        assert!(
            table.acquire("p", b.clone()).is_err(),
            "one session still holds it"
        );
        drop(g2);
        let g3 = table.acquire("p", b.clone()).unwrap();
        drop((other, g3));
        assert!(table.acquire("p", a).is_ok());
    }

    #[test]
    fn address_conflict_names_the_item_and_its_value() {
        let table = PinTable::default();
        let _g = table
            .acquire("p", pins(false, &["a.test"], false, false))
            .unwrap();
        let e = table
            .acquire("p", pins(false, &[], false, false))
            .unwrap_err();
        assert_eq!(e.code, codes::PROFILE_POLICY_CONFLICT);
        assert!(e.message.contains("address policy"), "{}", e.message);
        assert!(!e.message.contains("clipboard"), "{}", e.message);
        assert!(e.message.contains("a.test"), "{}", e.message);
        // both differ: both named
        let e = table
            .acquire("p", pins(true, &[], true, false))
            .unwrap_err();
        assert!(e.message.contains("clipboard") && e.message.contains("address policy"));
    }

    #[test]
    fn a_pin_is_released_when_the_record_holding_it_goes() {
        // The session record owns its guard, so removing the record (close, discard of a
        // failed create, shutdown) is what releases the pin. Same shape as `session.rs`.
        struct Record {
            _pin: PinGuard,
        }
        let table = PinTable::default();
        let a = pins(false, &[], false, false);
        let b = pins(true, &[], false, false);
        let mut records = HashMap::new();
        records.insert(
            1,
            Record {
                _pin: table.acquire("p", a.clone()).unwrap(),
            },
        );
        records.insert(
            2,
            Record {
                _pin: table.acquire("p", a).unwrap(),
            },
        );
        assert!(table.acquire("p", b.clone()).is_err());
        records.remove(&1);
        assert!(table.acquire("p", b.clone()).is_err());
        records.remove(&2);
        assert!(table.acquire("p", b).is_ok());
    }

    #[test]
    fn a_failed_acquire_holds_nothing() {
        let table = PinTable::default();
        let a = pins(false, &[], false, false);
        let b = pins(true, &[], false, false);
        let g = table.acquire("p", a).unwrap();
        for _ in 0..3 {
            assert!(table.acquire("p", b.clone()).is_err());
        }
        drop(g);
        assert!(table.acquire("p", b).is_ok());
    }
}
