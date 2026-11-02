//! The keychain and code-signing side (macOS).

use std::ffi::c_void;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;

use core_foundation::base::{CFType, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::CFURL;
use security_framework::base::Error as SecError;
use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement, SecStaticCode,
};
use security_framework::os::macos::keychain::SecKeychain;
use zeroize::Zeroizing;

use crate::{
    Command, Error, Fail, HELPER_IDENTIFIER, INSECURE_ENV, KEYCHAIN_ENV, MAX_INPUT, SERVICE,
    caller_requirement, check_value, leaf_of, requirement,
};

const ERR_ITEM_NOT_FOUND: i32 = -25300;
const ERR_DUPLICATE_ITEM: i32 = -25299;
const ERR_INTERACTION_NOT_ALLOWED: i32 = -25308;
const ERR_AUTH_FAILED: i32 = -25293;
const ERR_USER_CANCELED: i32 = -128;
const ERR_NO_ACCESS: i32 = -25243;
const ERR_INTERACTION_REQUIRED: i32 = -25315;
const UNLOCKED: u32 = 1; // kSecUnlockStateStatus

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainGetStatus(keychain: *const c_void, status: *mut u32) -> i32;
    fn SecCodeCopyStaticCode(code: *const c_void, flags: u32, out: *mut *const c_void) -> i32;
    fn SecCodeCopyDesignatedRequirement(
        code: *const c_void,
        flags: u32,
        out: *mut *const c_void,
    ) -> i32;
    fn SecRequirementCopyString(req: *const c_void, flags: u32, out: *mut CFStringRef) -> i32;
}

pub fn run(cmd: Command) -> Result<(), Error> {
    // a keychain that wants a dialog fails fast instead: nobody is at the screen of a launchd agent
    let _no_ui = SecKeychain::disable_user_interaction()
        .map_err(|e| other("disable user interaction", e))?;
    let leaf = own_leaf()?;
    match &cmd {
        // run by release.sh: no Hub parent; never prints key material (see `migrate`)
        Command::MigrateFrom(_) => {
            if leaf.is_none() {
                insecure_or_refuse()?;
            }
        }
        _ => match &leaf {
            Some(leaf) => check_parent(leaf)?,
            None => insecure_or_refuse()?,
        },
    }
    let kc = Keychain::open()?;
    match cmd {
        Command::Get(account) => {
            let v = kc.get(&account)?;
            let mut out = std::io::stdout().lock();
            out.write_all(&v)
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush())
                .map_err(|e| Error::new(Fail::Other, format!("stdout: {e}")))
        }
        Command::Set(account) => {
            let mut buf = Zeroizing::new(Vec::with_capacity(MAX_INPUT + 1));
            std::io::stdin()
                .lock()
                .take(MAX_INPUT as u64 + 1)
                .read_to_end(&mut buf)
                .map_err(|e| Error::new(Fail::Other, format!("stdin: {e}")))?;
            kc.set(&account, &buf)
        }
        Command::Delete(account) => kc.delete(&account),
        Command::Status => match kc.locked() {
            Some(false) => {
                println!("unlocked");
                Ok(())
            }
            Some(true) => {
                println!("locked");
                Err(Error::new(Fail::Locked, "keychain locked"))
            }
            None => Err(Error::new(
                Fail::Other,
                "cannot read the keychain lock state",
            )),
        },
        Command::MigrateFrom(old) => migrate(&kc, Path::new(&old), leaf.as_deref()),
    }
}

fn other(what: &str, e: SecError) -> Error {
    Error::new(Fail::Other, format!("{what}: OSStatus {}", e.code()))
}

fn insecure_or_refuse() -> Result<(), Error> {
    if std::env::var(INSECURE_ENV).as_deref() == Ok("1") {
        eprintln!(
            "steve-keyhelper: WARNING: not signed with a certificate, caller check skipped ({INSECURE_ENV}=1, tests only)"
        );
        Ok(())
    } else {
        Err(Error::new(
            Fail::Denied,
            "this helper is not signed with a certificate (ops/release.sh signs it); refusing",
        ))
    }
}

/// The leaf certificate hash of this process's own designated requirement, `None` if it has none (ad-hoc or
/// unsigned).
fn own_leaf() -> Result<Option<String>, Error> {
    let me = SecCode::for_self(Flags::NONE).map_err(|e| other("SecCodeCopySelf", e))?;
    let mut stat = std::ptr::null();
    // SAFETY: `me` is a live SecCodeRef; `stat` is a valid out pointer, released by the CFType wrapper.
    let rc = unsafe { SecCodeCopyStaticCode(me.as_concrete_TypeRef().cast(), 0, &mut stat) };
    if rc != 0 || stat.is_null() {
        return Err(Error::new(
            Fail::Other,
            format!("SecCodeCopyStaticCode: OSStatus {rc}"),
        ));
    }
    // SAFETY: a +1 reference from a Copy function.
    let stat = unsafe { CFType::wrap_under_create_rule(stat) };
    let mut req = std::ptr::null();
    // SAFETY: `stat` is a live SecStaticCodeRef; `req` is a valid out pointer.
    let rc = unsafe { SecCodeCopyDesignatedRequirement(stat.as_CFTypeRef(), 0, &mut req) };
    if rc != 0 || req.is_null() {
        // errSecCSUnsigned and friends: no signature, so no certificate
        return Ok(None);
    }
    // SAFETY: a +1 reference from a Copy function.
    let req = unsafe { CFType::wrap_under_create_rule(req) };
    let mut text: CFStringRef = std::ptr::null();
    // SAFETY: `req` is a live SecRequirementRef; `text` is a valid out pointer.
    let rc = unsafe { SecRequirementCopyString(req.as_CFTypeRef(), 0, &mut text) };
    if rc != 0 || text.is_null() {
        return Err(Error::new(
            Fail::Other,
            format!("SecRequirementCopyString: OSStatus {rc}"),
        ));
    }
    // SAFETY: a +1 CFString from a Copy function.
    let text = unsafe { CFString::wrap_under_create_rule(text) }.to_string();
    Ok(leaf_of(&text).map(str::to_string))
}

/// The parent process (the Hub that spawned us, or the newer helper during a migration) must be signed with our
/// certificate.
fn check_parent(leaf: &str) -> Result<(), Error> {
    let ppid = std::os::unix::process::parent_id();
    let req: SecRequirement = caller_requirement(leaf)
        .parse()
        .map_err(|e| other("SecRequirementCreateWithString", e))?;
    let mut attrs = GuestAttributes::new();
    attrs.set_pid(ppid as _);
    let code = SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE).map_err(|e| {
        Error::new(
            Fail::Denied,
            format!("caller pid {ppid}: no code object (OSStatus {})", e.code()),
        )
    })?;
    code.check_validity(Flags::NONE, &req).map_err(|e| {
        Error::new(
            Fail::Denied,
            format!(
                "caller pid {ppid} is not the Steve Hub signed with this helper's certificate (OSStatus {})",
                e.code()
            ),
        )
    })
}

/// The executable at `path` must be a helper signed with our certificate.
fn check_helper_file(path: &Path, leaf: &str) -> Result<(), Error> {
    let url = CFURL::from_path(path, false)
        .ok_or_else(|| Error::new(Fail::Other, format!("bad path {}", path.display())))?;
    let code = SecStaticCode::from_path(&url, Flags::NONE).map_err(|e| {
        other(
            &format!("SecStaticCodeCreateWithPath {}", path.display()),
            e,
        )
    })?;
    let req: SecRequirement = requirement(HELPER_IDENTIFIER, leaf)
        .parse()
        .map_err(|e| other("SecRequirementCreateWithString", e))?;
    code.check_validity(Flags::NONE, &req).map_err(|e| {
        Error::new(
            Fail::Denied,
            format!(
                "{} is not a steve-keyhelper signed with this helper's certificate (OSStatus {})",
                path.display(),
                e.code()
            ),
        )
    })
}

struct Keychain(SecKeychain);

impl Keychain {
    fn open() -> Result<Self, Error> {
        let kc = match std::env::var_os(KEYCHAIN_ENV).filter(|p| !p.is_empty()) {
            Some(p) => SecKeychain::open(p),
            None => SecKeychain::default(),
        };
        kc.map(Self).map_err(|e| other("open keychain", e))
    }

    fn locked(&self) -> Option<bool> {
        let mut status = 0u32;
        // SAFETY: a live SecKeychainRef and a valid out pointer.
        let rc = unsafe { SecKeychainGetStatus(self.0.as_concrete_TypeRef().cast(), &mut status) };
        (rc == 0).then_some(status & UNLOCKED == 0)
    }

    /// With interaction off, a locked keychain and an ACL / partition refusal answer the same codes, so the lock
    /// state is asked for explicitly.
    fn classify(&self, what: &str, e: SecError) -> Error {
        let code = e.code();
        let fail = match self.locked() {
            Some(true) => Fail::Locked,
            Some(false)
                if matches!(code, ERR_INTERACTION_NOT_ALLOWED | ERR_INTERACTION_REQUIRED) =>
            {
                Fail::Denied
            }
            _ => match code {
                ERR_INTERACTION_NOT_ALLOWED | ERR_INTERACTION_REQUIRED => Fail::Locked,
                ERR_AUTH_FAILED | ERR_USER_CANCELED | ERR_NO_ACCESS => Fail::Denied,
                _ => Fail::Other,
            },
        };
        Error::new(fail, format!("{what}: OSStatus {code}"))
    }

    fn search(&self, account: Option<&str>) -> ItemSearchOptions {
        let mut o = ItemSearchOptions::new();
        o.class(ItemClass::generic_password())
            .keychains(std::slice::from_ref(&self.0))
            .service(SERVICE);
        if let Some(a) = account {
            o.account(a);
        }
        o
    }

    /// The stored value, trimmed, checked to be base64 of 32 bytes.
    fn get(&self, account: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
        match self.0.find_generic_password(SERVICE, account) {
            Ok((pw, _item)) => {
                let v = check_value(pw.as_ref()).map_err(|f| {
                    Error::new(
                        f,
                        format!("{account}: stored value is not a 32-byte base64 key"),
                    )
                })?;
                Ok(Zeroizing::new(v.to_vec()))
            }
            Err(e) if e.code() == ERR_ITEM_NOT_FOUND => {
                Err(Error::new(Fail::NotFound, format!("{account}: no entry")))
            }
            Err(e) => Err(self.classify(account, e)),
        }
    }

    /// Replaces the entry: delete, then add, so the new item's access belongs to this helper.
    fn set(&self, account: &str, value: &[u8]) -> Result<(), Error> {
        let v = check_value(value)
            .map_err(|f| Error::new(f, "the value on stdin is not a 32-byte base64 key"))?;
        self.delete(account)?;
        self.0
            .add_generic_password(SERVICE, account, v)
            .map_err(|e| {
                if e.code() == ERR_DUPLICATE_ITEM {
                    Error::new(
                        Fail::Denied,
                        format!("{account}: the old entry could not be removed"),
                    )
                } else {
                    self.classify(account, e)
                }
            })
    }

    /// Adds the entry (it must not exist), so its access belongs to this helper.
    fn add(&self, account: &str, value: &[u8]) -> Result<(), Error> {
        let v = check_value(value).map_err(|f| Error::new(f, "not a 32-byte base64 key"))?;
        self.0
            .add_generic_password(SERVICE, account, v)
            .map_err(|e| self.classify(account, e))
    }

    /// A missing entry is not an error.
    fn delete(&self, account: &str) -> Result<(), Error> {
        match self.search(Some(account)).delete() {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(self.classify(&format!("delete {account}"), e)),
        }
    }

    /// The accounts of every vault entry (attributes only, no values).
    fn accounts(&self) -> Result<Vec<String>, Error> {
        let found = self
            .search(None)
            .load_attributes(true)
            .limit(Limit::All)
            .search();
        let found = match found {
            Ok(f) => f,
            Err(e) if e.code() == ERR_ITEM_NOT_FOUND => return Ok(vec![]),
            Err(e) => return Err(self.classify("list entries", e)),
        };
        let mut out: Vec<String> = found
            .iter()
            .filter_map(|r| r.simplify_dict())
            .filter_map(|mut d| d.remove("acct"))
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
}

/// Runs `old <args>` (the helper being replaced), with `input` on its stdin; its stdout, zeroized.
fn run_old(old: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut child = std::process::Command::new(old)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| Error::new(Fail::Other, format!("run {}: {e}", old.display())))?;
    if let (Some(v), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(v)
            .map_err(|e| Error::new(Fail::Other, format!("write to {}: {e}", old.display())))?;
    }
    let mut out = Zeroizing::new(Vec::new());
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(MAX_INPUT as u64)
            .read_to_end(&mut out)
            .map_err(|e| Error::new(Fail::Other, format!("read from {}: {e}", old.display())))?;
    }
    let status = child
        .wait()
        .map_err(|e| Error::new(Fail::Other, format!("wait for {}: {e}", old.display())))?;
    match status.code() {
        Some(0) => Ok(out),
        Some(c) => Err(Error::new(
            Fail::from_code(c).unwrap_or(Fail::Other),
            format!("old helper `{}` exited {c}", args.join(" ")),
        )),
        None => Err(Error::new(
            Fail::Other,
            format!("old helper `{}` was killed", args.join(" ")),
        )),
    }
}

/// Deletes the entry through Apple's `security` tool: an item can only be deleted by code its ACL owner trusts,
/// and neither helper is trusted for an item the other created, while `/usr/bin/security` (partition
/// `apple-tool:`) is. Attributes only; no key material passes through it.
fn security_delete(kc: &Keychain, account: &str) -> Result<(), Error> {
    let mut cmd = std::process::Command::new("/usr/bin/security");
    cmd.args(["delete-generic-password", "-s", SERVICE, "-a", account]);
    if let Some(p) = std::env::var_os(KEYCHAIN_ENV).filter(|p| !p.is_empty()) {
        cmd.arg(p);
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::new(Fail::Other, format!("run security: {e}")))?;
    if out.status.success()
        || kc
            .get(account)
            .err()
            .is_some_and(|e| e.fail == Fail::NotFound)
    {
        Ok(())
    } else {
        Err(Error::new(
            Fail::Denied,
            format!(
                "security delete-generic-password {account}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ))
    }
}

fn log(msg: &str) {
    eprintln!("steve-keyhelper migrate: {msg}");
}

/// After a helper upgrade (release.sh, before the new helper replaces the old one): the entries the old helper
/// created are in its cdhash partition, which this helper cannot read. For every vault entry this helper cannot
/// read yet, the old helper reads it (`old get`, its parent being this helper, signed with the same certificate),
/// then this helper replaces it (delete + add), so it lands in this helper's partition.
///
/// Every value is read before anything changes; if a replace fails, the entries replaced so far are put back
/// through the old helper (`old set`), so the old helper stays in charge and the release keeps it. Values only
/// ever travel over pipes between the two helpers, never argv, files or stdout.
fn migrate(kc: &Keychain, old: &Path, leaf: Option<&str>) -> Result<(), Error> {
    let old_canon = old
        .canonicalize()
        .map_err(|e| Error::new(Fail::Other, format!("{}: {e}", old.display())))?;
    let me = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
    if me.as_deref() == Some(old_canon.as_path()) {
        return Err(Error::new(
            Fail::Other,
            "migrate-from: the old helper is this helper",
        ));
    }
    match leaf {
        Some(leaf) => check_helper_file(old, leaf)?,
        None => {
            log("WARNING: unsigned helper (tests only): not checking the old helper's signature")
        }
    }
    let accounts = kc.accounts()?;
    log(&format!(
        "{} vault entr{} in the keychain",
        accounts.len(),
        if accounts.len() == 1 { "y" } else { "ies" }
    ));
    // Every value is read through the old helper first; an entry the old helper cannot read any more (another
    // partition) but this helper can is already migrated.
    let mut todo: Vec<(String, Zeroizing<Vec<u8>>)> = Vec::new();
    let mut unchanged = 0usize;
    for a in &accounts {
        match run_old(old, &["get", a], None) {
            Ok(v) => {
                let v = check_value(&v).map_err(|f| {
                    Error::new(
                        f,
                        format!("nothing changed: {a}: the old helper returned a malformed value"),
                    )
                })?;
                todo.push((a.clone(), Zeroizing::new(v.to_vec())));
            }
            Err(e) if matches!(e.fail, Fail::Denied | Fail::NotFound) && kc.get(a).is_ok() => {
                log(&format!(
                    "{a}: already readable by the new helper, unchanged"
                ));
                unchanged += 1;
            }
            Err(e) if e.fail == Fail::NotFound => log(&format!("{a}: gone, skipped")),
            Err(e) => {
                return Err(Error::new(
                    e.fail,
                    format!("nothing changed: {a}: {}", e.msg),
                ));
            }
        }
    }
    // An item can only be deleted by the code that owns it: the old helper deletes, this helper adds.
    for (i, (a, v)) in todo.iter().enumerate() {
        let moved = security_delete(kc, a).and_then(|_| kc.add(a, v));
        if let Err(e) = moved {
            log(&format!(
                "{a}: moving failed ({}), putting back what changed through the old helper",
                e.msg
            ));
            for (b, w) in todo[..=i].iter().rev() {
                let _ = kc.delete(b);
                match run_old(old, &["set", b], Some(w)) {
                    Ok(_) => log(&format!("{b}: restored for the old helper")),
                    Err(r) => log(&format!(
                        "{b}: RESTORE FAILED ({}); recover with the vault recovery code",
                        r.msg
                    )),
                }
            }
            return Err(e);
        }
        log(&format!("{a}: migrated to the new helper"));
    }
    log(&format!(
        "done: {} migrated, {} unchanged",
        todo.len(),
        unchanged
    ));
    Ok(())
}
