//! How this Hub was installed, which decides how it is upgraded. A managed supervisor upgrades itself and
//! a local_release install is upgraded by a one-shot runner job (`runner.rs`); the others get the commands
//! for their own way (Settings · 关于 composes them with the version).
use serde::Serialize;
use std::path::{Path, PathBuf};

/// `managed`: run-managed.sh + `--supervise`, upgraded from this page.
/// `local_release`: ops/local-release.py (signed Hub + steve-keyhelper, or a managed install the
/// generic bundle can't serve: Keychain). `release_script`: ops/release.sh + run.sh under launchd.
/// `source`: a cargo build in a checkout. `external`: anything else (a container, a package, …).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Install {
    pub kind: &'static str,
    pub home: String,
    /// Release directory `current` points at, by name.
    pub release: Option<String>,
    /// Steve checkout the commands run in, when one exists on this machine.
    pub repo: Option<String>,
}

impl Install {
    /// Why this page can't upgrade it itself, in the words Settings · 关于 and `steve_upgrade` show (the
    /// request's language; `/mcp` is zh-CN).
    /// `None` for `managed` and `local_release`: whether those can run is decided by what the machine
    /// has (`Updates::execution_reason`); a managed install the generic bundle can't serve has its own reason.
    pub fn reason(&self) -> Option<String> {
        Some(match self.kind {
            "managed" | "local_release" => return None,
            "release_script" => steve_i18n::t!("hub.upgrade.reason.release_script"),
            "source" => steve_i18n::t!("hub.upgrade.reason.source"),
            _ => steve_i18n::t!("hub.upgrade.reason.external"),
        })
    }
}

pub struct Probe<'a> {
    pub managed: bool,
    pub blocked: bool,
    pub home: &'a Path,
    pub exe: Option<PathBuf>,
    pub repo: &'a Path,
    /// `STEVE_INSTALL_KIND=external`: a deployment system owns the files.
    pub forced: Option<String>,
}

fn real(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}
fn checkout(p: &Path) -> bool {
    p.join(".git").exists() && p.join("hub-rs").is_dir()
}

pub fn detect(p: &Probe) -> Install {
    let releases = real(&p.home.join("releases"));
    let current = real(&p.home.join("current"));
    let exe = p.exe.as_deref().and_then(real);
    let in_release = matches!((&exe, &releases), (Some(e), Some(r)) if e.starts_with(r));
    let release = current
        .as_ref()
        .filter(|_| in_release)
        .and_then(|c| c.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    // release.sh names a release `YYYYMMDD-HHMMSS-<sha>`, local-release.py `<version>-local-<sha7>`;
    // the signing identity file is local-release.py's, for a release dir renamed by hand
    let dated = |n: &str| {
        n.len() > 16 && n.as_bytes()[8] == b'-' && n[..8].bytes().all(|c| c.is_ascii_digit())
    };
    let local = match release.as_deref() {
        Some(n) if n.contains("-local-") => true,
        Some(n) if dated(n) => false,
        _ => p.home.join("signing/identity.json").exists(),
    };
    // a cargo build runs from <checkout>/…/target/<profile>/steve-hub
    let source_repo = exe
        .as_deref()
        .filter(|_| !in_release)
        .and_then(|e| e.ancestors().find(|a| checkout(a)).map(Path::to_path_buf));
    let kind = match () {
        _ if p.forced.as_deref() == Some("external") => "external",
        _ if p.managed && !p.blocked => "managed",
        _ if p.managed || (in_release && local) => "local_release",
        _ if in_release => "release_script",
        _ if source_repo.is_some() => "source",
        _ => "external",
    };
    let repo = match kind {
        "source" => source_repo,
        "local_release" | "release_script" => Some(p.repo.to_path_buf()).filter(|r| checkout(r)),
        _ => None,
    };
    Install {
        kind,
        home: p.home.to_string_lossy().into_owned(),
        release,
        repo: repo.map(|r| r.to_string_lossy().into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn probe<'a>(home: &'a Path, repo: &'a Path, exe: Option<PathBuf>) -> Probe<'a> {
        Probe {
            managed: false,
            blocked: false,
            home,
            exe,
            repo,
            forced: None,
        }
    }
    fn release(home: &Path, name: &str) -> PathBuf {
        let dir = home.join("releases").join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/steve-hub"), "").unwrap();
        let _ = fs::remove_file(home.join("current"));
        std::os::unix::fs::symlink(&dir, home.join("current")).unwrap();
        home.join("current/bin/steve-hub")
    }
    #[test]
    fn each_installation_is_recognised_from_where_it_runs() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".git")).unwrap();
        fs::create_dir_all(repo.path().join("hub-rs/target/debug")).unwrap();
        fs::write(repo.path().join("hub-rs/target/debug/steve-hub"), "").unwrap();
        let (h, r) = (home.path(), repo.path());

        // ops/release.sh: a dated release dir under launchd
        let exe = release(h, "20261102-092840-46ac3591");
        let got = detect(&probe(h, r, Some(exe.clone())));
        assert_eq!(got.kind, "release_script");
        assert_eq!(got.release.as_deref(), Some("20261102-092840-46ac3591"));
        assert_eq!(got.repo.as_deref(), Some(r.to_str().unwrap()));

        // ops/local-release.py: a `-local-` build, or any release on a machine with a signing identity
        let exe = release(h, "0.2.17-local-51826fc");
        assert_eq!(
            detect(&probe(h, r, Some(exe.clone()))).kind,
            "local_release"
        );
        let exe = release(h, "0.2.18");
        fs::create_dir_all(h.join("signing")).unwrap();
        fs::write(h.join("signing/identity.json"), "{}").unwrap();
        assert_eq!(
            detect(&probe(h, r, Some(exe.clone()))).kind,
            "local_release"
        );
        let dated = release(h, "20261102-092840-46ac3591");
        assert_eq!(detect(&probe(h, r, Some(dated))).kind, "release_script");
        let exe = release(h, "0.2.18");

        // the supervisor: managed unless the generic bundle can't serve this machine
        let managed = Probe {
            managed: true,
            ..probe(h, r, Some(exe.clone()))
        };
        assert_eq!(detect(&managed).kind, "managed");
        assert_eq!(
            detect(&Probe {
                blocked: true,
                ..managed
            })
            .kind,
            "local_release"
        );

        // a cargo build in a checkout: its own checkout, not STEVE_REPO
        let other = tempfile::tempdir().unwrap();
        let got = detect(&probe(
            h,
            other.path(),
            Some(r.join("hub-rs/target/debug/steve-hub")),
        ));
        let real_repo = fs::canonicalize(r).unwrap();
        assert_eq!(
            (got.kind, got.release, got.repo.as_deref()),
            ("source", None, real_repo.to_str())
        );

        // nothing recognisable, or declared external
        assert_eq!(
            detect(&probe(h, r, Some("/usr/local/bin/steve-hub".into()))).kind,
            "external"
        );
        assert_eq!(detect(&probe(h, r, None)).kind, "external");
        let forced = Probe {
            forced: Some("external".into()),
            managed: true,
            ..probe(h, r, Some(exe))
        };
        let got = detect(&forced);
        assert_eq!((got.kind, got.repo), ("external", None));
    }

    #[test]
    fn every_install_the_page_cannot_upgrade_says_why() {
        let install = |kind| Install {
            kind,
            home: String::new(),
            release: None,
            repo: None,
        };
        assert_eq!(install("managed").reason(), None);
        assert_eq!(install("local_release").reason(), None);
        for (kind, says) in [
            ("release_script", "ops/release.sh"),
            ("source", "源码"),
            ("external", "部署系统"),
        ] {
            let reason = install(kind).reason().unwrap();
            assert!(reason.contains(says), "{kind}: {reason}");
            assert!(!reason.contains("managed supervisor"), "{kind}");
        }
    }
}
