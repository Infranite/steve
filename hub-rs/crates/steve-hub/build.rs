fn main() {
    println!(
        "cargo:rustc-env=STEVE_BUILD_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );
    println!("cargo:rerun-if-env-changed=STEVE_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=STEVE_RELEASE_COMMIT");
    let version = std::env::var("STEVE_RELEASE_VERSION").ok();
    let commit = std::env::var("STEVE_RELEASE_COMMIT").ok();
    match (version, commit) {
        (None, None) => {}
        (Some(version), Some(commit)) => {
            assert!(
                version.split('.').count() == 3
                    && version.split('.').all(|v| !v.is_empty()
                        && (v == "0" || !v.starts_with('0'))
                        && v.bytes().all(|c| c.is_ascii_digit())),
                "release version must be stable SemVer"
            );
            assert!(
                commit.len() == 40 && commit.bytes().all(|c| c.is_ascii_hexdigit()),
                "release commit must be a full Git SHA"
            );
            println!("cargo:rustc-env=STEVE_BUILD_VERSION={version}");
            println!("cargo:rustc-env=STEVE_BUILD_COMMIT={commit}");
        }
        _ => panic!("set both STEVE_RELEASE_VERSION and STEVE_RELEASE_COMMIT"),
    }
}
