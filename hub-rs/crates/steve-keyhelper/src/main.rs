//! `steve-keyhelper`: on macOS, the only process that reads or writes the vault master keys (KEKs) in the
//! keychain (service `io.github.infranite.steve.vault`). A binary without an Apple Team ID is recorded in the
//! keychain partition by its cdhash, so a rebuilt Hub could not read its own keys; this helper is rebuilt only
//! when its own source changes and stands in for the Hub.
//!
//! Why a separate binary: a keychain item remembers the code that created it. Code without an Apple Team ID gets
//! the partition `cdhash:<hash of that binary>`, so an item the Hub created was refused to every rebuilt Hub. The
//! helper is built and installed apart from the Hub releases (`$STEVE_HOME/bin/steve-keyhelper`), only when its own
//! source changes, so its cdhash, and the item's partition, stay the same across Hub releases.
//!
//! ```text
//! steve-keyhelper get <account>      prints the stored value (base64 of 32 bytes) to stdout
//! steve-keyhelper set <account>      reads the value from stdin (never argv), replaces the item (delete + add)
//! steve-keyhelper delete <account>   a missing item is not an error
//! steve-keyhelper status             prints `unlocked` (exit 0) or `locked` (exit 10)
//! steve-keyhelper migrate-from <old helper>   release.sh, after a helper upgrade: see `mac::migrate`
//! ```
//!
//! Exit codes: 0 ok, 10 locked, 11 denied, 12 not found, 13 malformed, 1 other (and usage). The Hub maps them in
//! `steve-modules/src/vault/keystore/helper.rs`; keep both in step.
//!
//! Caller check: before anything else, the parent process must satisfy
//! `(identifier "io.github.infranite.steve.hub" or identifier "io.github.infranite.steve.keyhelper") and certificate leaf = H"<leaf>"`,
//! where `<leaf>` is the leaf certificate hash of this helper's own signature, read at runtime. A helper without a
//! certificate leaf (ad-hoc, a plain `cargo build`) refuses everything (exit 11) unless
//! `STEVE_KEYHELPER_INSECURE_TEST=1`, which is for tests only and has no effect on a signed helper.
//! `migrate-from` is run by release.sh, so it skips the parent check; it never writes key material anywhere but
//! into the keychain, and only talks to an old helper signed with the same certificate.
//!
//! Keep this crate small and its dependencies few: every change to it means a new cdhash and a key migration.

use std::process::ExitCode;

/// Distribution identity is fixed at compilation; never let runtime environment weaken caller
/// authorization. Validate before embedding it in a Security.framework requirement string.
const fn checked_identifier(value: &str) -> &str {
    assert!(
        !value.is_empty() && value.len() <= 255,
        "invalid keyhelper identity length"
    );
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'),
            "keyhelper identity accepts only ASCII letters, digits, dot, dash or underscore"
        );
        i += 1;
    }
    value
}

/// The keychain service of every vault entry.
pub const SERVICE: &str = match option_env!("STEVE_KEYHELPER_SERVICE") {
    Some(value) => checked_identifier(value),
    None => "io.github.infranite.steve.vault", // existing installations: never silently change key lookup
};
/// The code-signing identifier of the Hub (ops/release.sh `sign_hub`).
pub const HUB_IDENTIFIER: &str = match option_env!("STEVE_KEYHELPER_HUB_IDENTIFIER") {
    Some(value) => checked_identifier(value),
    None => "io.github.infranite.steve.hub",
};
/// The code-signing identifier of this helper (ops/release.sh `keyhelper_install`).
pub const HELPER_IDENTIFIER: &str = match option_env!("STEVE_KEYHELPER_IDENTIFIER") {
    Some(value) => checked_identifier(value),
    None => "io.github.infranite.steve.keyhelper",
};
/// `1`: an unsigned or ad-hoc signed helper skips the caller check. Tests only.
pub const INSECURE_ENV: &str = "STEVE_KEYHELPER_INSECURE_TEST";
/// A keychain file to use instead of the login keychain (the ignored integration test).
pub const KEYCHAIN_ENV: &str = "STEVE_KEYHELPER_KEYCHAIN";
/// The longest stdin `set` accepts (a 32-byte key is 44 base64 characters).
const MAX_INPUT: usize = 1024;

/// Why a command failed; [`Fail::code`] is the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fail {
    Locked,
    Denied,
    NotFound,
    Malformed,
    Other,
}

impl Fail {
    pub const fn code(self) -> u8 {
        match self {
            Fail::Locked => 10,
            Fail::Denied => 11,
            Fail::NotFound => 12,
            Fail::Malformed => 13,
            Fail::Other => 1,
        }
    }

    pub const fn from_code(code: i32) -> Option<Fail> {
        Some(match code {
            10 => Fail::Locked,
            11 => Fail::Denied,
            12 => Fail::NotFound,
            13 => Fail::Malformed,
            1 => Fail::Other,
            _ => return None,
        })
    }
}

/// A failure and what to say on stderr (never key material).
#[derive(Debug)]
pub struct Error {
    pub fail: Fail,
    pub msg: String,
}

impl Error {
    pub fn new(fail: Fail, msg: impl Into<String>) -> Self {
        Self {
            fail,
            msg: msg.into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Get(String),
    Set(String),
    Delete(String),
    Status,
    MigrateFrom(String),
}

pub fn parse(args: &[String]) -> Result<Command, Error> {
    let usage = || {
        Error::new(
            Fail::Other,
            "usage: steve-keyhelper get|set|delete <account> | status | migrate-from <old helper>",
        )
    };
    let account = |a: &String| {
        if a.is_empty() || a.len() > 256 || a.chars().any(char::is_control) {
            Err(Error::new(Fail::Other, "bad account name"))
        } else {
            Ok(a.clone())
        }
    };
    match args {
        [c, a] if c == "get" => Ok(Command::Get(account(a)?)),
        [c, a] if c == "set" => Ok(Command::Set(account(a)?)),
        [c, a] if c == "delete" => Ok(Command::Delete(account(a)?)),
        [c] if c == "status" => Ok(Command::Status),
        [c, p] if c == "migrate-from" && !p.is_empty() => Ok(Command::MigrateFrom(p.clone())),
        _ => Err(usage()),
    }
}

/// The `certificate leaf = H"<hex>"` hash of a designated requirement, `None` for ad-hoc (`cdhash H"..."`) ones.
pub fn leaf_of(designated: &str) -> Option<&str> {
    const MARK: &str = "certificate leaf = H\"";
    let start = designated.find(MARK)? + MARK.len();
    let rest = &designated[start..];
    let hex = &rest[..rest.find('"')?];
    (hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The requirement code with `identifier` and certificate `leaf` must satisfy.
pub fn requirement(identifier: &str, leaf: &str) -> String {
    format!("identifier \"{identifier}\" and certificate leaf = H\"{leaf}\"")
}

/// What the parent process must satisfy: the Hub or another helper (a migration), signed with `leaf`.
pub fn caller_requirement(leaf: &str) -> String {
    format!(
        "(identifier \"{HUB_IDENTIFIER}\" or identifier \"{HELPER_IDENTIFIER}\") and certificate leaf = H\"{leaf}\""
    )
}

/// A stored or given value must be base64 of exactly 32 bytes (surrounding whitespace allowed). Returns it trimmed.
pub fn check_value(v: &[u8]) -> Result<&[u8], Fail> {
    let v = v.trim_ascii();
    let raw = zeroize::Zeroizing::new(
        data_encoding::BASE64
            .decode(v)
            .map_err(|_| Fail::Malformed)?,
    );
    if raw.len() == 32 {
        Ok(v)
    } else {
        Err(Fail::Malformed)
    }
}

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args).and_then(mac::run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("steve-keyhelper: {}", e.msg);
            ExitCode::from(e.fail.code())
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("steve-keyhelper: macOS only");
    ExitCode::from(Fail::Other.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exit_codes_are_stable_and_round_trip() {
        let all = [
            (Fail::Locked, 10),
            (Fail::Denied, 11),
            (Fail::NotFound, 12),
            (Fail::Malformed, 13),
            (Fail::Other, 1),
        ];
        for (f, c) in all {
            assert_eq!(f.code(), c);
            assert_eq!(Fail::from_code(i32::from(c)), Some(f));
        }
        assert_eq!(Fail::from_code(0), None);
        assert_eq!(Fail::from_code(2), None);
    }

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse(&args(&["get", "default/kek/v1"])).unwrap(),
            Command::Get("default/kek/v1".into())
        );
        assert_eq!(
            parse(&args(&["set", "a"])).unwrap(),
            Command::Set("a".into())
        );
        assert_eq!(
            parse(&args(&["delete", "a"])).unwrap(),
            Command::Delete("a".into())
        );
        assert_eq!(parse(&args(&["status"])).unwrap(), Command::Status);
        assert_eq!(
            parse(&args(&["migrate-from", "/x/old"])).unwrap(),
            Command::MigrateFrom("/x/old".into())
        );
        // the value never comes from argv
        for bad in [
            &["set", "a", "c2VjcmV0"][..],
            &["get"],
            &[],
            &["status", "x"],
            &["get", ""],
            &["get", "a\nb"],
            &["frob", "a"],
        ] {
            assert_eq!(parse(&args(bad)).unwrap_err().fail, Fail::Other, "{bad:?}");
        }
    }

    #[test]
    fn reads_the_leaf_from_a_designated_requirement() {
        let leaf = "0123456789abcdef0123456789ABCDEF01234567";
        let dr = format!("identifier \"{HELPER_IDENTIFIER}\" and certificate leaf = H\"{leaf}\"");
        assert_eq!(leaf_of(&dr), Some(leaf));
        assert_eq!(leaf_of(&requirement(HELPER_IDENTIFIER, leaf)), Some(leaf));
        // ad-hoc: no certificate
        assert_eq!(
            leaf_of("cdhash H\"0123456789abcdef0123456789abcdef01234567\""),
            None
        );
        assert_eq!(leaf_of("certificate leaf = H\"abc\""), None);
        assert_eq!(
            leaf_of("certificate leaf = H\"0123456789abcdef0123456789abcdef0123456g\""),
            None
        );
        assert_eq!(leaf_of("certificate leaf = H\"0123"), None);
    }

    #[test]
    fn builds_the_caller_requirement() {
        let leaf = "aa".repeat(20);
        assert_eq!(
            caller_requirement(&leaf),
            format!(
                "(identifier \"{HUB_IDENTIFIER}\" or identifier \"{HELPER_IDENTIFIER}\") and certificate leaf = H\"{leaf}\""
            )
        );
        assert_eq!(
            requirement(HELPER_IDENTIFIER, &leaf),
            format!("identifier \"{HELPER_IDENTIFIER}\" and certificate leaf = H\"{leaf}\"")
        );
    }

    #[test]
    fn checks_values() {
        let k = data_encoding::BASE64.encode(&[7u8; 32]);
        assert_eq!(check_value(format!(" {k}\n").as_bytes()), Ok(k.as_bytes()));
        assert_eq!(
            check_value(data_encoding::BASE64.encode(&[7u8; 31]).as_bytes()),
            Err(Fail::Malformed)
        );
        assert_eq!(check_value(b"not base64!"), Err(Fail::Malformed));
        assert_eq!(check_value(b""), Err(Fail::Malformed));
    }
}
