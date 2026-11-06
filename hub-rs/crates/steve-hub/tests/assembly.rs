//! What the Hub registers when it is wired: the connection providers, channel and profile
//! adapters, the adapters it sends as you on, mailbox adapters, flow event sources, the number of
//! background jobs, and every route under `/api` and at the root. `tests/assembly/wiring.snap`
//! holds it, one entry per line:
//!
//! - `provider <id> <adapter as GET /connections/adapters lists it>`, `channel <id>`,
//!   `profiles <id>`, `as_user <id>`, `mail <id>`, `event_source <id>`, `jobs <n>`;
//! - `api <METHOD> <path>` and `root <METHOD> <path>`, path parameters written as `x`.
//!
//! The paths tried are every `.route("…")` literal and `/…` string constant in the workspace
//! crates' `src/` and the snapshot's own, so a route added anywhere shows up as extra. To record it again:
//! `STEVE_RECORD_WIRING=1 cargo test -p steve-hub --test assembly`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use steve_hub::bus::Bus;
use steve_hub::config::Config;

const SNAPSHOT: &str = include_str!("assembly/wiring.snap");
const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

struct Home {
    _dir: tempfile::TempDir,
    cfg: Arc<Config>,
    vars: steve_hub::wire::Vars,
}

fn home() -> Home {
    steve_modules::net::forbid_in_this_process();
    let dir = tempfile::tempdir().expect("tmp");
    let h = dir.path().to_path_buf();
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", "1".into()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", "http://127.0.0.1:1".into()),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    let vars = Arc::new(vars);
    let get = move |k: &str| vars.get(k).cloned();
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    Home {
        _dir: dir,
        cfg,
        vars: Arc::new(get),
    }
}

/// The `METHOD /path` pairs of `paths` that `routes` has a route for. Without the `Caller` layer
/// the handlers refuse at their first extractor or answer a harmless read; a method the path has
/// no handler for is 405 and a path with no route is an empty 404.
async fn matched(routes: Router, paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for p in paths {
        for m in METHODS {
            let req = Request::builder()
                .method(m)
                .uri(p.as_str())
                .body(Body::empty())
                .expect("request");
            let res = routes.clone().oneshot(req).await.expect("infallible");
            let status = res.status();
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("body");
            let none = status == StatusCode::METHOD_NOT_ALLOWED
                || (status == StatusCode::NOT_FOUND && body.is_empty());
            if !none {
                out.insert(format!("{m} {p}"));
            }
        }
    }
    out
}

/// Everything but the routes, as snapshot lines.
fn registrations(w: &steve_hub::wire::Wired) -> BTreeSet<String> {
    let channels = &w.seams.channels;
    let conns = channels.connections();
    let listed: HashMap<String, serde_json::Value> = conns
        .adapters()
        .into_iter()
        .map(|a| (a.id.clone(), serde_json::to_value(&a).expect("json")))
        .collect();
    let mut out = BTreeSet::new();
    for id in conns.provider_ids() {
        let shown = listed
            .get(&id)
            .map_or_else(|| "managed".to_string(), |v| v.to_string());
        out.insert(format!("provider {id} {shown}"));
    }
    out.extend(channels.adapters().map(|a| format!("channel {}", a.id())));
    out.extend(
        channels
            .profile_adapter_ids()
            .into_iter()
            .map(|id| format!("profiles {id}")),
    );
    out.extend(
        w.tools
            .outbox
            .as_user_ids()
            .into_iter()
            .map(|id| format!("as_user {id}")),
    );
    out.extend(
        w.tools
            .mail
            .adapter_ids()
            .into_iter()
            .map(|id| format!("mail {id}")),
    );
    out.extend(
        w.seams
            .flow
            .events
            .all()
            .iter()
            .map(|a| format!("event_source {}", a.id())),
    );
    out.insert(format!("jobs {}", w.jobs.len()));
    out
}

fn prefixed(prefix: &str, routes: BTreeSet<String>) -> impl Iterator<Item = String> {
    routes.into_iter().map(move |r| format!("{prefix} {r}"))
}

/// The paths of the snapshot's `api|root METHOD /path` lines.
fn paths_of(want: &BTreeSet<String>) -> BTreeSet<String> {
    want.iter()
        .filter(|l| l.starts_with("api ") || l.starts_with("root "))
        .filter_map(|l| l.rsplit_once(' ').map(|(_, p)| p.to_string()))
        .collect()
}

/// Every `.route("…")` path literal and `const X: &str = "/…"` under the workspace crates' `src/`,
/// parameters as `x`.
fn route_literals() -> BTreeSet<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir");
    let mut files = Vec::new();
    for c in std::fs::read_dir(crates).expect("crates").flatten() {
        walk(&c.path().join("src"), &mut files);
    }
    let mut out = BTreeSet::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        // `.route("/…", …)` and the `const X: &str = "/…";` some routes are given by name
        let starts = text
            .match_indices(".route(")
            .chain(text.match_indices(": &str ="))
            .map(|(at, m)| at + m.len());
        for at in starts {
            let rest = text[at..].trim_start();
            let Some(lit) = rest.strip_prefix('"') else {
                continue;
            };
            let Some(end) = lit.find('"') else { continue };
            let mut path = String::new();
            let mut param = false;
            for ch in lit[..end].chars() {
                match ch {
                    '{' => {
                        param = true;
                        path.push('x');
                    }
                    '}' => param = false,
                    _ if !param => path.push(ch),
                    _ => {}
                }
            }
            if path.starts_with('/') {
                out.insert(path);
            }
        }
    }
    out
}

#[tokio::test]
async fn the_built_in_wiring_matches_the_snapshot() {
    let h = home();
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&h.cfg, &bus, h.vars.clone()).expect("wire");
    let want: BTreeSet<String> = SNAPSHOT
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    let mut paths = route_literals();
    paths.extend(paths_of(&want));
    let mut got = registrations(&w);
    got.extend(prefixed("api", matched(w.module_routes, &paths).await));
    got.extend(prefixed("root", matched(w.root_routes, &paths).await));

    if std::env::var_os("STEVE_RECORD_WIRING").is_some() {
        let out = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/assembly/wiring.snap");
        let text: String = got.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(out, text).expect("write snapshot");
        return;
    }
    let missing: Vec<_> = want.difference(&got).collect();
    let extra: Vec<_> = got.difference(&want).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the wiring differs from the snapshot; missing {missing:?}, extra {extra:?}"
    );
}

// ------------------------------------------------------------------------------- assemblies

use steve_hub::Assembly;
use steve_hub::extension::{Declare, EXTENSION_API, Extension, ExtensionError, Mounted, Started};

/// A test extension: what it declares and mounts is chosen per test.
#[derive(Default)]
struct Probe {
    id: &'static str,
    api: Option<u32>,
    /// Registers GitHub's token adapter under its own id.
    github_pat: bool,
    /// A channel side whose declared adapter id is not its provider's.
    mismatched_side: bool,
    /// A flow event source with no adapter registered for it.
    stray_source: bool,
    /// `GET /hello` among its routes, and one job.
    hello: bool,
}

impl Extension for Probe {
    fn id(&self) -> &str {
        self.id
    }
    fn api(&self) -> u32 {
        self.api.unwrap_or(EXTENSION_API)
    }
    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        let pat = Arc::new(steve_github::GitHubPat::new("http://127.0.0.1:9"));
        if self.github_pat {
            d.provider(pat.clone())?;
        }
        if self.mismatched_side {
            use steve_channels::ext::{ChannelDecl, ChannelSide};
            d.channel(ChannelSide::new(ChannelDecl::new("probe_self"), pat))?;
        }
        if self.stray_source {
            let src = steve_hub::wire::signals::GithubSource(Arc::new(steve_github::GitHubApp));
            d.event_source(Arc::new(src))?;
        }
        Ok(())
    }
    fn start(&self, _: &Started) -> Result<Mounted, ExtensionError> {
        let mut m = Mounted::default();
        if self.hello {
            m.api_routes = Router::new().route("/hello", axum::routing::get(|| async { "hi" }));
            m.jobs.push(Box::pin(async {}));
        }
        Ok(m)
    }
}

fn probe(id: &'static str) -> Probe {
    Probe {
        id,
        ..Default::default()
    }
}

fn wire_err(asm: Assembly) -> String {
    let h = home();
    match steve_hub::wire::wire_assembly(&h.cfg, &Bus::new(), h.vars.clone(), &asm) {
        Ok(_) => panic!("assembly accepted"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn builtin_ids_and_without() {
    assert_eq!(
        Assembly::builtin().ids(),
        ["feishu.bot", "feishu.self", "mail.imap", "github"]
    );
    let asm = Assembly::builtin()
        .without("feishu.self")
        .expect("known id");
    assert_eq!(asm.ids(), ["feishu.bot", "mail.imap", "github"]);
    let e = Assembly::builtin()
        .without("nope")
        .err()
        .expect("unknown id");
    assert_eq!(e.extension(), Some("nope"));
    assert_eq!(e.to_string(), "extension nope: not in the assembly");
}

#[tokio::test]
async fn feishu_self_is_refused_without_feishu_bot() {
    let asm = Assembly::builtin().without("feishu.bot").expect("known id");
    let e = wire_err(asm);
    assert!(
        e.contains("extension feishu.self: needs feishu.bot in the assembly"),
        "{e}"
    );
    // without both, the rest still wires
    let h = home();
    let asm = Assembly::builtin()
        .without("feishu.bot")
        .and_then(|a| a.without("feishu.self"))
        .expect("known ids");
    assert!(steve_hub::wire::wire_assembly(&h.cfg, &Bus::new(), h.vars.clone(), &asm).is_ok());
}

#[tokio::test]
async fn conflicts_are_refused_at_startup_with_the_extension_id() {
    let e = wire_err(Assembly::builtin().with(probe("github")));
    assert!(e.contains("extension github: registered twice"), "{e}");
    let e = wire_err(Assembly::builtin().with(Probe {
        api: Some(EXTENSION_API + 1),
        ..probe("probe")
    }));
    assert!(
        e.contains("extension probe: built for extension API 2, this Hub has 1"),
        "{e}"
    );
    let e = wire_err(Assembly::empty().with(probe("Probe/1")));
    assert!(e.contains("extension Probe/1: an id is lowercase"), "{e}");
    let e = wire_err(Assembly::builtin().with(Probe {
        github_pat: true,
        ..probe("probe")
    }));
    assert!(
        e.contains("extension probe: adapter github_pat is already registered by github"),
        "{e}"
    );
    let e = wire_err(Assembly::empty().with(Probe {
        mismatched_side: true,
        ..probe("probe")
    }));
    assert!(
        e.contains("extension probe: provider github_pat declared for adapter probe_self"),
        "{e}"
    );
    let e = wire_err(Assembly::empty().with(Probe {
        stray_source: true,
        ..probe("probe")
    }));
    assert!(
        e.contains("extension probe: event source github_app without a registered adapter"),
        "{e}"
    );
}

#[tokio::test]
async fn an_assembly_registers_only_its_extensions() {
    let h = home();
    let asm = Assembly::builtin()
        .without("feishu.self")
        .expect("known id")
        .with(Probe {
            hello: true,
            ..probe("probe")
        });
    let w =
        steve_hub::wire::wire_assembly(&h.cfg, &Bus::new(), h.vars.clone(), &asm).expect("wire");
    let channels = &w.seams.channels;
    assert_eq!(
        channels.connections().provider_ids(),
        ["feishu_app", "github_app", "github_pat", "imap"]
    );
    assert_eq!(channels.profile_adapter_ids(), ["feishu_app"]);
    assert!(w.tools.outbox.as_user_ids().is_empty());
    // the built-in 14 and the probe's
    assert_eq!(w.jobs.len(), 15);
    // the device flow and the OAuth start of you on Feishu went with it; the bot's stayed, and
    // another extension's routes are under its own prefix
    let paths: BTreeSet<String> = [
        "/channels/feishu/device/start",
        "/channels/feishu/user/oauth/start",
        "/channels/feishu/registrations",
        "/channels/ext/probe/hello",
        "/hello",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let got = matched(w.module_routes, &paths).await;
    let want: BTreeSet<String> = [
        "POST /channels/feishu/registrations",
        "GET /channels/ext/probe/hello",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn an_empty_assembly_still_wires_the_hub() {
    let h = home();
    let w = steve_hub::wire::wire_assembly(&h.cfg, &Bus::new(), h.vars.clone(), &Assembly::empty())
        .expect("wire");
    assert!(w.seams.channels.connections().provider_ids().is_empty());
    assert!(w.tools.mail.adapter_ids().is_empty());
    assert!(w.seams.flow.events.all().is_empty());
    // the built-in extensions' two (registrations resumed, the Feishu runtime) are gone
    assert_eq!(w.jobs.len(), 12);
}
