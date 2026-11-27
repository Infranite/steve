//! Resumable first installation. Existing identities and unknown homes are never adopted.
use crate::common::*;
use crate::ui::{self, Choice};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use steve_i18n::t;

mod reconfigure;

#[derive(Default)]
pub struct Options {
    pub role: Option<String>,
    pub home: Option<PathBuf>,
    pub hub: Option<String>,
    pub pin: Option<String>,
    pub listen: Option<String>,
    /// The address the web client is opened at (`--public-url`); without it, the one that follows
    /// from the listen address, asked for only when none does.
    pub public_url: Option<String>,
    /// The built-in ACP adapters to install (`--adapters`): ids separated by commas, `all` or
    /// `none`; without it, asked on a terminal and all of them elsewhere.
    pub adapters: Option<String>,
    pub reconfigure: bool,
    /// Set an existing installation in the home aside (renamed, kept whole) and install anew.
    pub fresh: bool,
    /// The service label (`--label`): another installation next to the usual one, under the same
    /// user (its own launchd job or systemd unit and signing identity).
    pub label: Option<String>,
    /// The Hub's vault keys in this directory (`--vault-dir`), on macOS too, instead of the login
    /// keychain.
    pub vault_dir: Option<PathBuf>,
}

trait Prompts {
    fn interactive(&self) -> bool;
    fn line(&mut self, title: &str, default: Option<&str>, optional: bool) -> Result<String>;
    /// Whether choices are widgets on a terminal ([`crate::ui`]) rather than typed lines.
    fn rich(&self) -> bool {
        false
    }
    /// One of `choices`; typed, the answer is a choice's key.
    fn select(&mut self, title: &str, choices: &[Choice], initial: usize) -> Result<usize> {
        typed_select(self, title, choices, initial)
    }
    /// Any of `choices`; typed, the answer is keys separated by commas, `all` or `none`.
    fn multiselect(
        &mut self,
        title: &str,
        choices: &[Choice],
        ticked: &[bool],
    ) -> Result<Vec<bool>> {
        typed_multiselect(self, title, choices, ticked)
    }
    fn confirm(&mut self, title: &str, initial: bool) -> Result<bool> {
        typed_confirm(self, title, initial)
    }
}

fn typed_select<P: Prompts + ?Sized>(
    input: &mut P,
    title: &str,
    choices: &[Choice],
    initial: usize,
) -> Result<usize> {
    let keys: Vec<&str> = choices.iter().map(|c| c.key.as_str()).collect();
    let title = t!(
        "install.setup.typed.select_title",
        title = title,
        keys = keys.join(" / ")
    );
    loop {
        let answer = input
            .line(&title, Some(keys[initial]), false)?
            .trim()
            .to_lowercase();
        match keys.iter().position(|k| *k == answer) {
            Some(at) => return Ok(at),
            None if input.interactive() => ui::warn(&t!(
                "install.setup.typed.choose_one",
                keys = keys.join(" / ")
            )),
            None => bail!(
                "{}",
                t!("install.setup.typed.choose_one", keys = keys.join(" / "))
            ),
        }
    }
}

/// `all`, `none`, or keys separated by commas, as ticks over `choices`.
fn ticks(answer: &str, choices: &[Choice]) -> Result<Vec<bool>> {
    ticks_allowing(answer, choices, &[])
}

fn ticks_allowing(answer: &str, choices: &[Choice], allowed_noop: &[String]) -> Result<Vec<bool>> {
    let answer = answer.trim().to_lowercase();
    if matches!(answer.as_str(), "all" | "none") {
        return Ok(vec![answer == "all"; choices.len()]);
    }
    let wanted: Vec<&str> = answer
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();
    if let Some(unknown) = wanted
        .iter()
        .find(|k| !choices.iter().any(|c| c.key == **k) && !allowed_noop.iter().any(|a| a == **k))
    {
        let mut keys: Vec<&str> = choices.iter().map(|c| c.key.as_str()).collect();
        keys.extend(allowed_noop.iter().map(String::as_str));
        bail!(
            "{}",
            t!(
                "install.setup.typed.unknown_choice",
                unknown = unknown,
                keys = keys.join(&t!("install.setup.list_separator"))
            )
        );
    }
    Ok(choices
        .iter()
        .map(|c| wanted.contains(&c.key.as_str()))
        .collect())
}

fn typed_multiselect<P: Prompts + ?Sized>(
    input: &mut P,
    title: &str,
    choices: &[Choice],
    ticked: &[bool],
) -> Result<Vec<bool>> {
    let default: Vec<&str> = choices
        .iter()
        .zip(ticked)
        .filter(|(_, on)| **on)
        .map(|(c, _)| c.key.as_str())
        .collect();
    let default = if default.is_empty() {
        "none".to_owned()
    } else {
        default.join(",")
    };
    let title = t!("install.setup.typed.multiselect_title", title = title);
    loop {
        let answer = input.line(&title, Some(&default), false)?;
        match ticks(&answer, choices) {
            Ok(on) => return Ok(on),
            Err(error) if input.interactive() => ui::warn(&error.to_string()),
            Err(error) => return Err(error),
        }
    }
}

fn typed_confirm<P: Prompts + ?Sized>(input: &mut P, title: &str, initial: bool) -> Result<bool> {
    loop {
        let answer = input.line(
            &format!("{title} [y/n]"),
            Some(if initial { "y" } else { "n" }),
            false,
        )?;
        match answer.trim().to_lowercase().as_str() {
            "y" | "yes" | "是" => return Ok(true),
            "n" | "no" | "否" => return Ok(false),
            _ if input.interactive() => ui::warn(&t!("install.setup.typed.answer_yes_no")),
            _ => bail!("{}", t!("install.setup.typed.answer_yes_no")),
        }
    }
}

struct Terminal;
impl Prompts for Terminal {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn rich(&self) -> bool {
        ui::fancy()
    }

    fn select(&mut self, title: &str, choices: &[Choice], initial: usize) -> Result<usize> {
        if ui::fancy() {
            ui::select(title, choices, initial)
        } else {
            typed_select(self, title, choices, initial)
        }
    }

    fn multiselect(
        &mut self,
        title: &str,
        choices: &[Choice],
        ticked: &[bool],
    ) -> Result<Vec<bool>> {
        if ui::fancy() {
            ui::multiselect(title, choices, ticked)
        } else {
            typed_multiselect(self, title, choices, ticked)
        }
    }

    fn confirm(&mut self, title: &str, initial: bool) -> Result<bool> {
        if ui::fancy() {
            ui::confirm(title, initial)
        } else {
            typed_confirm(self, title, initial)
        }
    }

    fn line(&mut self, title: &str, default: Option<&str>, optional: bool) -> Result<String> {
        while ui::fancy() {
            let answer = ui::input(title, default)?;
            if (!optional && answer.is_empty()) || answer.contains(['\n', '\r', '\0']) {
                ui::warn(&t!("install.setup.input.need_value"));
                continue;
            }
            return Ok(answer);
        }
        loop {
            print!(
                "{title}{}: ",
                default.map(|v| format!(" [{v}]")).unwrap_or_default()
            );
            io::stdout().flush()?;
            let mut line = String::new();
            if io::stdin().read_line(&mut line)? == 0 {
                bail!("{}", t!("install.setup.input.interrupted"));
            }
            let answer = line.trim();
            let answer = if answer.is_empty() {
                default.unwrap_or("")
            } else {
                answer
            };
            if (!optional && answer.is_empty()) || answer.contains(['\n', '\r', '\0']) {
                if self.interactive() {
                    eprintln!("{}", t!("install.setup.input.need_value_field"));
                    continue;
                }
                bail!("{}", t!("install.setup.input.need_value_abort"));
            }
            return Ok(answer.into());
        }
    }
}

/// Only retry validation of a value read from an interactive prompt. Cancellation,
/// EOF and I/O errors come from `line` and must leave the wizard immediately.
fn ask_value<T>(
    input: &mut dyn Prompts,
    title: &str,
    default: Option<&str>,
    optional: bool,
    validate: impl Fn(&str) -> Result<T>,
) -> Result<T> {
    loop {
        let value = input.line(title, default, optional)?;
        match validate(&value) {
            Ok(value) => return Ok(value),
            Err(error) if input.interactive() => {
                ui::warn(&t!("install.setup.input.retry_field", error = error));
            }
            Err(error) => return Err(error),
        }
    }
}

fn listen_address(value: &str) -> Result<SocketAddr> {
    let address: SocketAddr = value
        .trim()
        .parse()
        .context(t!("install.setup.listen.format"))?;
    if address.port() == 0 {
        bail!("{}", t!("install.setup.listen.port_zero"));
    }
    Ok(address)
}

/// `tailcat:<address>:<port>`, a Hub reached over tailcat (steve_node_wire::tailcat checks it fully).
fn tailcat_hub(value: &str) -> Option<String> {
    let (addr, port) = value.trim().strip_prefix("tailcat:")?.rsplit_once(':')?;
    let ok = addr.starts_with("tc")
        && addr.len() >= 8
        && addr
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && port.parse::<u16>().is_ok_and(|p| p != 0);
    ok.then(|| format!("tailcat:{addr}:{port}"))
}

fn node_hub_url(value: &str) -> Result<String> {
    if let Some(hub) = tailcat_hub(value) {
        return Ok(hub);
    }
    let hub = browser_url(value, "Hub")?;
    if !hub.starts_with("https://") {
        bail!("{}", t!("install.setup.node.https_hub_required"));
    }
    Ok(hub)
}

fn setup_role(value: &str, options: &Options) -> Result<String> {
    let role = value.trim().to_lowercase();
    if !matches!(role.as_str(), "hub" | "node") {
        bail!("{}", t!("install.setup.option.role_choice"));
    }
    if role == "hub" && (options.hub.is_some() || options.pin.is_some()) {
        bail!("{}", t!("install.setup.option.node_only"));
    }
    if role == "node" && (options.listen.is_some() || options.public_url.is_some()) {
        bail!("{}", t!("install.setup.option.hub_only"));
    }
    Ok(role)
}

/// Reject invalid explicit arguments before prompting, inspecting a bundle or creating a home.
fn validate_options(options: &Options) -> Result<()> {
    if options.fresh && options.reconfigure {
        bail!("{}", t!("install.setup.option.fresh_vs_reconfigure"));
    }
    if let Some(role) = &options.role {
        setup_role(role, options)?;
    }
    if let Some(listen) = &options.listen {
        listen_address(listen)?;
        if options.hub.is_some() || options.pin.is_some() {
            bail!("{}", t!("install.setup.option.listen_vs_node"));
        }
    }
    if let Some(public) = &options.public_url {
        browser_url(public, &t!("install.setup.label.access_address"))?;
        if options.hub.is_some() || options.pin.is_some() {
            bail!("{}", t!("install.setup.option.public_url_vs_node"));
        }
    }
    if let Some(hub) = &options.hub {
        node_hub_url(hub)?;
    }
    if let Some(label) = &options.label {
        anyhow::ensure!(
            crate::signing::valid_identity(label),
            "{}",
            t!("install.setup.option.label_chars")
        );
    }
    // one pin, or several comma-separated while the Hub's certificate is being changed
    if options.pin.as_ref().is_some_and(|v| {
        v.split(',').any(|p| {
            !p.starts_with("sha256:")
                || p.len() != 71
                || !p[7..].bytes().all(|b| b.is_ascii_hexdigit())
        })
    }) {
        bail!("{}", t!("install.setup.option.pin_format"));
    }
    Ok(())
}

fn modified_setup_file(name: &str) -> anyhow::Error {
    anyhow::anyhow!("{}", t!("install.setup.receipt.file_modified", name = name))
}

/// Where setup keeps the owner's first access token (`user=…`, `token=…`; mode 0600).
const OWNER_TOKEN: &str = "secrets/owner.token";

/// The command that issues another access token for the installation in `home`: the Hub's own binary there (it is
/// not on the PATH), told which installation it is.
fn token_command(home: &Path) -> Result<String> {
    let quoted = |p: &Path| shell_quote(p.to_str().context(t!("install.setup.home_not_utf8"))?);
    Ok(format!(
        "STEVE_HOME={} {} --access token create",
        quoted(home)?,
        quoted(&home.join("current/bin/steve-hub"))?
    ))
}

/// Parses a `STEVE_HOST` value (one or more comma-separated IPs or `localhost`) into host tokens.
fn parse_host_entries(raw: &str) -> Option<Vec<std::net::IpAddr>> {
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let bare = part
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(part);
        let ip = if bare.eq_ignore_ascii_case("localhost") {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            bare.parse::<std::net::IpAddr>().ok()?
        };
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn completion_lines(
    role: &str,
    home: &Path,
    label: Option<&str>,
    environment: &Value,
    paired: bool,
) -> Result<Vec<String>> {
    /* what selects this installation in a command: its home always, its label when it has its own */
    let mut selected = format!(
        "--home {}",
        shell_quote(home.to_str().context(t!("install.setup.home_not_utf8"))?)?
    );
    if let Some(label) = label {
        selected.push_str(&format!(" --label {}", shell_quote(label)?));
    }
    let mut lines = vec![t!(
        "install.setup.plain.configured",
        role = role,
        home = home.display()
    )];
    if role == "hub" {
        let host_raw = environment["STEVE_HOST"]
            .as_str()
            .context(t!("install.setup.listen_host_missing"))?;
        let hosts =
            parse_host_entries(host_raw).with_context(|| t!("install.setup.listen.format"))?;
        let port: u16 = environment["STEVE_PORT"]
            .as_str()
            .context(t!("install.setup.listen_port_missing"))?
            .parse()?;
        let address_display = hosts
            .iter()
            .map(|&ip| SocketAddr::new(ip, port).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(t!("install.setup.plain.listen", address = address_display));
        if let Some(web) = environment["STEVE_WEB_PORT"].as_str() {
            /* the address to type: the one it listens on, or this machine's when it listens on all */
            let ip = hosts
                .iter()
                .copied()
                .find(|ip| !ip.is_unspecified())
                .unwrap_or(hosts[0]);
            let hub = if ip.is_unspecified() {
                format!("http://{}:{port}", t!("install.setup.plain.this_machine"))
            } else {
                format!("http://{}", SocketAddr::new(ip, port))
            };
            lines.push(t!("install.setup.plain.web_port", web = web, hub = hub));
        }
        lines.push(t!(
            "install.setup.plain.browser_address",
            url = environment["STEVE_PUBLIC_URL"].as_str().unwrap_or("")
        ));
    } else if paired {
        lines.push(t!("install.setup.plain.node_paired"));
    } else {
        lines.push(t!("install.setup.plain.node_unpaired"));
        lines.push(format!("steve setup node {selected}"));
    }
    lines.push(if role == "node" {
        t!("install.setup.plain.start_after_pairing")
    } else {
        t!("install.setup.plain.start_service")
    });
    let start = if role == "node" && cfg!(target_os = "macos") {
        "enable-boot"
    } else {
        "start"
    };
    lines.push(format!("steve {role} {start} {selected}"));
    lines.push(t!("install.setup.plain.status_commands"));
    lines.push(format!("steve {role} status {selected}"));
    lines.push(format!("steve {role} doctor {selected}"));
    if role == "hub" {
        match fs::read_to_string(home.join(OWNER_TOKEN)) {
            Ok(saved) => {
                let field = |k: &str| {
                    saved
                        .lines()
                        .find_map(|l| l.strip_prefix(&format!("{k}=")))
                        .unwrap_or("")
                        .to_string()
                };
                lines.push(t!("install.setup.plain.sign_in_user", user = field("user")));
                lines.push(t!(
                    "install.setup.plain.access_token",
                    token = field("token")
                ));
                lines.push(t!(
                    "install.setup.plain.token_saved",
                    path = home.join(OWNER_TOKEN).display(),
                    command = token_command(&home)?
                ));
            }
            Err(_) => lines.push(t!(
                "install.setup.plain.token_missing",
                command = token_command(&home)?
            )),
        }
        lines.push(t!("install.setup.plain.next_hub"));
    } else {
        lines.push(t!("install.setup.plain.next_node"));
    }
    Ok(lines)
}

pub fn browser_url(value: &str, label: &str) -> Result<String> {
    Ok(http_url(value, label)?.trim_end_matches('/').into())
}

fn oidc_issuer_url(value: &str) -> Result<String> {
    // An issuer is an exact identifier: discovery and ID tokens must match it,
    // including any trailing slash in its path.
    http_url(value, "OIDC issuer")
}

fn http_url(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    let url = url::Url::parse(value)
        .with_context(|| t!("install.setup.url.full_required", label = label))?;
    let host = url
        .host_str()
        .context(t!("install.setup.url.host_missing"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("{}", t!("install.setup.url.plain_only", label = label));
    }
    if matches!(host, "0.0.0.0" | "::" | "[::]") {
        bail!(
            "{}",
            t!("install.setup.url.unspecified_host", label = label)
        );
    }
    if url.scheme() != "https" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") {
        bail!("{}", t!("install.setup.url.https_required", label = label));
    }
    Ok(value.into())
}

fn manifest(root: &Path) -> Result<Value> {
    let value = read_json(&root.join("release.json"))?;
    let version = value["version"]
        .as_str()
        .context(t!("install.setup.bundle.version_missing"))?;
    let parsed = semver::Version::parse(version)?;
    let commit = value["commit"]
        .as_str()
        .context(t!("install.setup.bundle.commit_missing"))?;
    if value["format_version"] != 1
        || parsed.to_string() != version
        || !parsed.pre.is_empty()
        || !parsed.build.is_empty()
        || commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !value["schema"].is_object()
    {
        bail!("{}", t!("install.setup.bundle.identity_invalid"));
    }
    Ok(value)
}

/// The ACP adapters of a Hub or Node at `home` (`steve_acp::managed`), through the release in
/// `release`: `status`, `install [<id>…]` or `upgrade`. Their output goes straight to the terminal.
pub fn adapters(role: &str, home: &Path, release: &Path, args: &[&str]) -> Result<i32> {
    let status = adapter_command(role, home, release, args)
        .status()
        .with_context(|| t!("install.setup.run_failed", path = release.display()))?;
    Ok(status.code().unwrap_or(1))
}

fn captured(
    program: &Path,
    home: &Path,
    args: &[&str],
    environment: Option<&Value>,
) -> Result<Value> {
    let mut command = clean_command(program, Some(home));
    command.args(args);
    if let Some(env) = environment.and_then(Value::as_object) {
        for (key, value) in env {
            command.env(
                key,
                value
                    .as_str()
                    .context(t!("install.setup.env_value_not_string"))?,
            );
        }
    }
    let output = command
        .output()
        .with_context(|| t!("install.setup.run_failed", path = program.display()))?;
    if !output.status.success() {
        bail!(
            "{}",
            t!("install.setup.command_failed", status = output.status)
        );
    }
    serde_json::from_slice(&output.stdout).context(t!("install.setup.command_no_json"))
}

/// The Hub's own check of this environment (`steve-hub --check-config`): its report, also when it
/// found the configuration wrong (exit 2, `ok: false`, the reasons under `errors`).
fn config_report(program: &Path, home: &Path, environment: &Value) -> Result<Value> {
    let mut command = clean_command(program, Some(home));
    command
        .arg("--check-config")
        .stdin(std::process::Stdio::null());
    for (key, value) in environment.as_object().into_iter().flatten() {
        command.env(
            key,
            value
                .as_str()
                .context(t!("install.setup.env_value_not_string"))?,
        );
    }
    let output = command
        .output()
        .with_context(|| t!("install.setup.run_failed", path = program.display()))?;
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(report) if report["ok"].is_boolean() => Ok(report),
        _ if !output.status.success() => bail!(
            "{}",
            t!("install.setup.command_failed", status = output.status)
        ),
        _ => bail!("{}", t!("install.setup.command_no_json")),
    }
}

fn verify_bundle(root: &Path, expected: &Value, home: &Path) -> Result<()> {
    regular_tree(root)?;
    let disk = manifest(root)?;
    let web = read_json(&root.join("web/dist/release.json"))?;
    for value in [&disk, &web] {
        for key in ["version", "commit"] {
            if value[key] != expected[key] {
                bail!("{}", t!("install.setup.bundle.web_identity_mismatch"));
            }
        }
    }
    if disk["hub_version"] != disk["version"] || disk["web_version"] != disk["version"] {
        bail!("{}", t!("install.setup.bundle.version_mismatch"));
    }
    for role in ["hub", "node"] {
        let path = root.join(format!("bin/steve-{role}"));
        if fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
            bail!("{}", t!("install.setup.bundle.binary_missing", role = role));
        }
        let info = captured(&path, home, &["--build-info"], None)?;
        if info["version"] != disk["version"]
            || info["commit"] != disk["commit"]
            || info["target"] != env!("STEVE_BUILD_TARGET")
            || (role == "hub" && info["schema"] != disk["schema"]["target"])
        {
            bail!(
                "{}",
                t!("install.setup.bundle.binary_mismatch", role = role)
            );
        }
    }
    let restic_version = disk
        .get("restic_version")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    anyhow::ensure!(
        restic_version.is_some(),
        "bundled backup engine version is missing"
    );
    let engine = fs::metadata(root.join("bin/restic"))?;
    let license = fs::metadata(root.join("bin/restic.LICENSE"))?;
    anyhow::ensure!(
        engine.is_file() && engine.permissions().mode() & 0o111 != 0 && license.is_file(),
        "bundled backup engine is incomplete"
    );
    let html = fs::read_to_string(root.join("web/dist/index.html"))?;
    let marker = regex::Regex::new(&format!(
        r#"<meta\s+name=["']steve-web-version["']\s+content=["']{}["']"#,
        regex::escape(disk["version"].as_str().unwrap())
    ))?;
    if !marker.is_match(&html) {
        bail!("{}", t!("install.setup.bundle.web_version_mismatch"));
    }
    if cfg!(target_os = "macos") && !root.join("bin/steve-keyhelper").is_file() {
        bail!("{}", t!("install.setup.bundle.keyhelper_missing"));
    }
    Ok(())
}

struct Wizard<'a> {
    source: PathBuf,
    home: PathBuf,
    record: Value,
    input: &'a mut dyn Prompts,
    _lock: File,
}

impl<'a> Wizard<'a> {
    fn claim(
        source: &Path,
        home: &Path,
        role: &str,
        expected: &Value,
        input: &'a mut dyn Prompts,
    ) -> Result<Self> {
        if home.starts_with(source) || source.starts_with(home) {
            bail!("{}", t!("install.setup.home_inside_bundle"));
        }
        if !exists(home) {
            private_dir(home)?;
            let record = json!({"format":1,"component":role,"home":home,"version":expected["version"],"commit":expected["commit"],"files":{},"steps":[]});
            private_file(
                &home.join("setup.json"),
                &serde_json::to_vec_pretty(&record)?,
            )?;
        }
        owned(home, true)?;
        // Do not create even a lock file in an unrelated, otherwise private directory.
        owned(&home.join("setup.json"), false).context(t!("install.setup.receipt.none"))?;
        let before_lock = read_json(&home.join("setup.json"))?;
        if before_lock["format"] != 1
            || before_lock["component"] != role
            || before_lock["home"].as_str() != home.to_str()
        {
            bail!("{}", t!("install.setup.receipt.not_owned"));
        }
        let lock_path = home.join(".setup.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)?;
        lock.try_lock().context(t!("install.setup.receipt.busy"))?;
        owned(&lock_path, false)?;
        owned(&home.join("setup.json"), false).context(t!("install.setup.receipt.none"))?;
        let record = read_json(&home.join("setup.json"))?;
        if record["format"] != 1
            || record["component"] != role
            || record["home"].as_str() != home.to_str()
            || !record["files"].is_object()
            || !record["steps"].is_array()
        {
            bail!("{}", t!("install.setup.receipt.not_owned"));
        }
        let old_version = record["version"]
            .as_str()
            .context(t!("install.setup.receipt.version_missing"))?;
        let parsed = semver::Version::parse(old_version)?;
        let old_commit = record["commit"]
            .as_str()
            .context(t!("install.setup.receipt.commit_missing"))?;
        if parsed.to_string() != old_version
            || !parsed.pre.is_empty()
            || !parsed.build.is_empty()
            || old_commit.len() != 40
            || !old_commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!("{}", t!("install.setup.receipt.identity_invalid"));
        }
        let mut wizard = Self {
            source: source.into(),
            home: home.into(),
            record,
            input,
            _lock: lock,
        };
        for (name, value) in wizard.record["files"].as_object().unwrap() {
            let relative = Path::new(name);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                bail!("{}", t!("install.setup.receipt.path_invalid"));
            }
            let path = home.join(relative);
            if exists(&path) {
                owned(&path, false)?;
                if digest(&path)?
                    != value
                        .as_str()
                        .context(t!("install.setup.receipt.digest_invalid"))?
                {
                    return Err(modified_setup_file(name));
                }
            }
        }
        if wizard.record["version"] != expected["version"]
            || wizard.record["commit"] != expected["commit"]
        {
            // A user may have interrupted the previous setup at its first prompt before upgrading
            // the package. Only that unsigned, unpaired, unregistered setup may change versions.
            if underway(&wizard.record, home) {
                bail!("{}", t!("install.setup.receipt.other_version"));
            }
            wizard.verify_copy()?;
            let backups = home.join("setup-backups");
            private_dir(&backups)?;
            private_file(
                &backups.join(format!("{}.json", uuid::Uuid::new_v4())),
                &serde_json::to_vec_pretty(&wizard.record)?,
            )?;
            wizard.record["version"] = expected["version"].clone();
            wizard.record["commit"] = expected["commit"].clone();
            wizard
                .record
                .as_object_mut()
                .unwrap()
                .remove("copy_planned");
            wizard
                .record
                .as_object_mut()
                .unwrap()
                .remove("bundle_files");
            wizard.save()?;
        }
        Ok(wizard)
    }

    fn save(&self) -> Result<()> {
        atomic_json(&self.home.join("setup.json"), &self.record)
    }

    fn owned_file(&mut self, name: &str, data: Option<&[u8]>) -> Result<PathBuf> {
        let path = self.home.join(name);
        if self.record["files"].get(name).is_none() {
            if exists(&path) {
                bail!(
                    "{}",
                    t!("install.setup.receipt.foreign_file", path = path.display())
                );
            }
            let bytes = data.context(t!("install.setup.receipt.file_missing"))?;
            self.record["files"][name] = bytes_digest(bytes).into();
            self.save()?;
        }
        let expected = self.record["files"][name]
            .as_str()
            .context(t!("install.setup.receipt.file_digest_invalid"))?;
        if exists(&path) {
            owned(&path, false)?;
            if digest(&path)? != expected {
                return Err(modified_setup_file(name));
            }
        } else {
            let bytes = data.context(t!("install.setup.receipt.input_needed"))?;
            if bytes_digest(bytes) != expected {
                bail!("{}", t!("install.setup.receipt.input_changed"));
            }
            private_dir(path.parent().unwrap())?;
            private_file(&path, bytes)?;
        }
        Ok(path)
    }

    fn verify_copy(&self) -> Result<()> {
        if self.record["copy_planned"] != true {
            return Ok(());
        }
        let candidate = self.home.join("releases").join(
            self.record["version"]
                .as_str()
                .context(t!("install.setup.release.version_missing"))?,
        );
        if !exists(&candidate) {
            return Ok(());
        }
        if fs::symlink_metadata(&candidate)?.file_type().is_symlink() {
            bail!("{}", t!("install.setup.release.dir_is_link"));
        }
        let expected = self.record["bundle_files"]
            .as_object()
            .context(t!("install.setup.release.manifest_missing"))?;
        let files = regular_tree(&candidate)?;
        if files.len() != expected.len() {
            bail!("{}", t!("install.setup.release.listing_changed"));
        }
        for path in files {
            let name = path
                .strip_prefix(&candidate)?
                .to_str()
                .context(t!("install.setup.release.name_not_utf8"))?;
            let actual = digest(&path)?;
            let wanted = if name == "bin/steve-hub" && self.record["keychain_started"] == true {
                self.record["signed_hub_sha256"]
                    .as_str()
                    .or_else(|| expected.get(name).and_then(Value::as_str))
            } else if name == "bin/steve-node" && self.record["node_signing_started"] == true {
                self.record["signed_node_sha256"]
                    .as_str()
                    .or_else(|| expected.get(name).and_then(Value::as_str))
            } else if name == "bin/steve" && self.record["node_signing_started"] == true {
                self.record["signed_cli_sha256"]
                    .as_str()
                    .or_else(|| expected.get(name).and_then(Value::as_str))
            } else {
                expected.get(name).and_then(Value::as_str)
            };
            if wanted != Some(actual.as_str()) {
                bail!("{}", t!("install.setup.release.file_changed", name = name));
            }
        }
        Ok(())
    }

    fn copy_bundle(&mut self, m: &Value) -> Result<PathBuf> {
        let releases = self.home.join("releases");
        private_dir(&releases)?;
        let candidate = releases.join(m["version"].as_str().unwrap());
        if exists(&candidate) {
            if self.record["copy_planned"] != true {
                bail!("{}", t!("install.setup.release.dir_foreign"));
            }
            self.verify_copy()?;
            verify_bundle(&candidate, m, &self.home)?;
            return Ok(candidate);
        }
        let mut expected = serde_json::Map::new();
        for path in regular_tree(&self.source)? {
            expected.insert(
                path.strip_prefix(&self.source)?
                    .to_str()
                    .context(t!("install.setup.release.name_invalid"))?
                    .into(),
                digest(&path)?.into(),
            );
        }
        self.record["copy_planned"] = true.into();
        self.record["bundle_files"] = expected.clone().into();
        self.save()?;
        let staging = releases.join(format!(".setup-{}", uuid::Uuid::new_v4()));
        private_dir(&staging)?;
        let result = (|| -> Result<()> {
            for (name, hash) in &expected {
                let src = self.source.join(name);
                let dest = staging.join(name);
                private_dir(dest.parent().unwrap())?;
                let mut input = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&src)?;
                if !input.metadata()?.is_file() {
                    bail!("{}", t!("install.setup.release.source_changed"));
                }
                let mode = if input.metadata()?.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                };
                let mut output = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(mode)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&dest)?;
                io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                if digest(&dest)? != hash.as_str().unwrap() {
                    bail!("{}", t!("install.setup.release.copy_mismatch", name = name));
                }
            }
            verify_bundle(&staging, m, &self.home)?;
            if exists(&candidate) {
                bail!("{}", t!("install.setup.release.dir_appeared"));
            }
            fs::rename(&staging, &candidate)?;
            File::open(&releases)?.sync_all()?;
            Ok(())
        })();
        if exists(&staging) {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        Ok(candidate)
    }

    #[cfg(test)]
    fn hub_config(&mut self, listen: Option<&str>) -> Result<Value> {
        self.hub_config_with(listen, None, None)
    }

    fn hub_config_with(
        &mut self,
        listen: Option<&str>,
        public: Option<&str>,
        vault_dir: Option<&Path>,
    ) -> Result<Value> {
        if self.record["environment"].is_object() {
            let env = self.record["environment"].clone();
            if let Some(listen) = listen {
                let selected = listen_address(listen)?;
                let saved_raw = env["STEVE_HOST"]
                    .as_str()
                    .context(t!("install.setup.listen_host_missing"))?;
                let saved_ips = parse_host_entries(saved_raw)
                    .with_context(|| t!("install.setup.listen.format"))?;
                let saved_port: u16 = env["STEVE_PORT"]
                    .as_str()
                    .context(t!("install.setup.listen_port_missing"))?
                    .parse()?;
                if saved_ips.len() != 1 || selected != SocketAddr::new(saved_ips[0], saved_port) {
                    bail!("{}", t!("install.setup.config.listen_differs"));
                }
            }
            if let Some(public) = public {
                if env["STEVE_PUBLIC_URL"].as_str()
                    != Some(
                        browser_url(public, &t!("install.setup.label.access_address"))?.as_str(),
                    )
                {
                    bail!("{}", t!("install.setup.config.public_url_differs"));
                }
            }
            if exists(&self.home.join("steve.env"))
                && self.record["files"].get("steve.env").is_some()
            {
                self.owned_file("steve.env", None)?;
            } else {
                self.owned_file("steve.env", Some(&env_bytes(env.as_object().unwrap())?))?;
            }
            /* a setup before 2026-11-09 asked for OIDC and keeps its secret */
            if self.record["files"]
                .get("secrets/oidc-client-secret")
                .is_some()
            {
                self.owned_file("secrets/oidc-client-secret", None)?;
            }
            return Ok(env);
        }
        let custom = |input: &mut dyn Prompts| {
            ask_value(
                input,
                &t!("install.setup.label.listen_address"),
                Some("0.0.0.0:8440"),
                false,
                listen_address,
            )
        };
        let bind = match listen {
            Some(value) => listen_address(value)?,
            None if self.input.rich() => {
                let choices = [
                    Choice::new(
                        "lan",
                        &t!("install.setup.network.lan"),
                        &t!("install.setup.network.lan_hint"),
                    ),
                    Choice::new(
                        "local",
                        &t!("install.setup.network.local"),
                        &t!("install.setup.network.local_hint"),
                    ),
                    Choice::new(
                        "custom",
                        &t!("install.setup.network.custom"),
                        &t!("install.setup.network.custom_hint"),
                    ),
                ];
                match self
                    .input
                    .select(&t!("install.setup.network.question"), &choices, 0)?
                {
                    0 => SocketAddr::from(([0, 0, 0, 0], 8440)),
                    1 => SocketAddr::from(([127, 0, 0, 1], 8440)),
                    _ => custom(&mut *self.input)?,
                }
            }
            None => custom(&mut *self.input)?,
        };
        /* the web client has a port of its own (A3): the Hub's port answers the API only */
        let web_port = bind
            .port()
            .checked_add(1)
            .context(t!("install.setup.network.port_too_high"))?;
        let default_url = if bind.ip().is_unspecified() || bind.ip().is_loopback() {
            Some(format!(
                "http://{}:{web_port}",
                if bind.is_ipv6() { "[::1]" } else { "127.0.0.1" },
            ))
        } else {
            None
        };
        /* the web client's address follows from the listen address; it is asked for only when
         * none does (a specific address of another network needs its HTTPS name), and
         * `steve setup --reconfigure` changes it later */
        let public = match (public, default_url) {
            (Some(value), _) => browser_url(value, &t!("install.setup.label.access_address"))?,
            (None, Some(url)) => url,
            (None, None) => {
                ui::info(&t!("install.setup.network.no_web_address"));
                ask_value(
                    self.input,
                    &t!("install.setup.label.browser_address"),
                    None,
                    false,
                    |value| browser_url(value, &t!("install.setup.label.access_address")),
                )?
            }
        };
        /* no OIDC here (2026-11-09): sign-in is a user's name and an access token, issued below;
         * single sign-on is configured later, if wanted */
        let mut env = json!({"STEVE_HOST":bind.ip().to_string(),"STEVE_PORT":bind.port().to_string(),"STEVE_PUBLIC_URL":public,
            "STEVE_WEB_PORT":web_port.to_string(),"STEVE_WEB_DIR":self.home.join("web/current")});
        if bind.ip().is_unspecified() {
            env["STEVE_ALLOW_ANY_HOST"] = "1".into();
        }
        if cfg!(target_os = "linux") || vault_dir.is_some() {
            let selected = match (vault_dir, self.record["key_directory"].as_str()) {
                (Some(dir), _) => dir.to_string_lossy().into_owned(),
                (None, Some(value)) => value.to_string(),
                (None, None) => {
                    self.input
                        .line(&t!("install.setup.vault.dir_question"), None, false)?
                }
            };
            let keys = canonical(Path::new(&selected))?;
            if keys.starts_with(&self.home) || self.home.starts_with(&keys) {
                bail!("{}", t!("install.setup.vault.dir_overlaps"));
            }
            if exists(&keys) {
                owned(&keys, true)?;
                if self.record["key_directory"].is_null() && fs::read_dir(&keys)?.next().is_some() {
                    bail!("{}", t!("install.setup.vault.dir_not_empty"));
                }
            } else {
                private_dir(&keys)?;
            }
            self.record["key_directory"] = json!(keys);
            env["STEVE_VAULT_KEYSTORE"] = "dir".into();
            env["STEVE_VAULT_KEY_DIR"] = json!(keys);
        } else {
            env["STEVE_VAULT_KEYSTORE"] = "keychain".into();
        }
        self.record["environment"] = env.clone();
        self.save()?;
        self.owned_file("steve.env", Some(&env_bytes(env.as_object().unwrap())?))?;
        Ok(env)
    }
}

/// Whether a setup recorded in `record` got as far as making something a later version must not
/// take over: a step done, an identity, a database, a service.
/// Whether `home` holds something of an installation's own. A pairing that never made the Node's
/// key (`node/identity.key`, saved before anything is sent) sent nothing: a home left by it, as a
/// failed remote install leaves one, is taken over by the next setup, of any version or Hub.
fn underway(record: &Value, home: &Path) -> bool {
    record["steps"]
        .as_array()
        .is_some_and(|steps| !steps.is_empty())
        || record["keychain_started"] == true
        || record["node_signing_started"] == true
        || [
            "current",
            "signing/identity.json",
            "node/identity.key",
            "data/steve.sqlite3",
            "services/hub.json",
            "services/node.json",
        ]
        .iter()
        .any(|name| exists(&home.join(name)))
}

/// The receipt of the installation already in `home`, when this setup cannot continue it: it is the
/// other role's, or another version's and under way.
fn other_install(home: &Path, role: &str, expected: &Value) -> Option<Value> {
    owned(home, true).ok()?;
    owned(&home.join("setup.json"), false).ok()?;
    let record = read_json(&home.join("setup.json")).ok()?;
    if record["format"] != 1 || record["home"].as_str() != home.to_str() {
        return None;
    }
    let same = record["version"] == expected["version"] && record["commit"] == expected["commit"];
    (record["component"] != role || (!same && underway(&record, home))).then_some(record)
}

/// `Hub 0.2.97 · a74c8da9`, from a receipt.
fn install_name(record: &Value) -> String {
    let commit: String = record["commit"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(8)
        .collect();
    format!(
        "{} {} · {commit}",
        if record["component"] == "node" {
            "Node"
        } else {
            "Hub"
        },
        record["version"].as_str().unwrap_or("?")
    )
}

/// `20261111-011243`, local time: what a directory set aside is named after.
fn stamp() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Moves the installation in `home` out of the way, whole: its service is stopped and unregistered
/// and the directory renamed next to itself. Nothing in it is changed or deleted, so moving it back
/// and registering the service again restores it. A Hub's key directory (outside its home) goes with
/// it under the same stamp, the second path returned: those keys open the data set aside and no
/// other, and the next installation starts with an empty directory of its own.
fn set_aside(home: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    owned(home, true)?;
    owned(&home.join("setup.json"), false).context(t!("install.setup.aside.no_receipt"))?;
    let record = read_json(&home.join("setup.json"))?;
    let component = record["component"]
        .as_str()
        .filter(|c| matches!(*c, "hub" | "node"))
        .context(t!("install.setup.aside.receipt_invalid"))?;
    if record["format"] != 1 || record["home"].as_str() != home.to_str() {
        bail!("{}", t!("install.setup.aside.receipt_elsewhere"));
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join(".setup.lock"))?;
    lock.try_lock().context(t!("install.setup.receipt.busy"))?;
    // setup.json precedes service registration: an interrupted setup has no service to stop.
    if exists(&home.join("services").join(format!("{component}.json"))) {
        /* the label is the one that installation registered, whatever this run was given */
        let mut service = crate::service::Service::new(component, home, None)?;
        for action in ["stop", "uninstall"] {
            let report = service
                .dispatch(action, None, 30)
                .context(t!("install.setup.aside.still_running"))?;
            if report["ok"] == false {
                bail!("{}", t!("install.setup.aside.still_running"));
            }
        }
    }
    let stamp = stamp();
    let beside = |dir: &Path| -> Result<PathBuf> {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .context(t!("install.setup.home_not_utf8"))?;
        let aside = dir.with_file_name(format!("{name}.bak-{stamp}"));
        if exists(&aside) {
            bail!(
                "{}",
                t!("install.setup.aside.target_exists", aside = aside.display())
            );
        }
        Ok(aside)
    };
    let aside = beside(home)?;
    /* only a directory setup made for this installation's keys: this user's, private, apart from the home */
    let keys = record["key_directory"]
        .as_str()
        .map(PathBuf::from)
        .filter(|keys| {
            keys.is_absolute()
                && !keys.starts_with(home)
                && !home.starts_with(keys)
                && owned(keys, true).is_ok()
        });
    let keys_aside = keys.as_deref().map(beside).transpose()?;
    drop(lock);
    fs::rename(home, &aside)?;
    if let (Some(keys), Some(keys_aside)) = (&keys, &keys_aside) {
        fs::rename(keys, keys_aside).with_context(|| {
            t!(
                "install.setup.aside.keys_left",
                keys = keys.display(),
                aside = aside.display()
            )
        })?;
    }
    Ok((aside, keys_aside))
}

pub fn reconfigure_existing(options: Options) -> Result<()> {
    validate_options(&options)?;
    reconfigure::run(&options, &mut Terminal)
}

/// `bin/steve-node adapters …` of `release`, for the home (both Hub and standalone Node roles
/// manage ACP adapters through the Node binary).
fn adapter_command(
    _role: &str,
    home: &Path,
    release: &Path,
    args: &[&str],
) -> std::process::Command {
    let program = release.join("bin/steve-node");
    let mut command = clean_command(&program, Some(home));
    command.arg("adapters").args(args);
    command
}

/// What the release can install (`adapters list --json`): whether npm is there, whether this home
/// already has a choice, and each built-in adapter.
fn adapter_listing(role: &str, home: &Path, release: &Path) -> Option<Value> {
    let output = adapter_command(role, home, release, &["list", "--json"])
        .output()
        .ok()?;
    let listing: Value = serde_json::from_slice(&output.stdout).ok()?;
    (output.status.success() && listing["adapters"].is_array()).then_some(listing)
}

/// Which built-in ACP adapters to install: `--adapters`, the answer an earlier run of this setup
/// recorded, or asked now on a terminal. `None` leaves it to the release: every adapter this home
/// keeps (all of them, in a new one).
fn adapter_choice(
    wizard: &mut Wizard,
    role: &str,
    release: &Path,
    flag: Option<&str>,
) -> Result<Option<Vec<String>>> {
    let Some(listing) = adapter_listing(role, &wizard.home, release) else {
        if flag.is_none()
            && let Some(saved) = wizard.record["adapters"].as_array()
        {
            return Ok(Some(
                saved
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            ));
        }
        anyhow::ensure!(
            flag.is_none(),
            "{}",
            t!("install.setup.adapters.unsupported")
        );
        return Ok(None);
    };
    let already_chosen = listing["chosen"] == true;
    let finished = wizard.record["steps"]
        .as_array()
        .is_some_and(|s| s.iter().any(|v| v == "installed"));
    if flag.is_none() && (already_chosen || finished) {
        return Ok(None);
    }
    if flag.is_none()
        && let Some(saved) = wizard.record["adapters"].as_array()
    {
        return Ok(Some(
            saved
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
        ));
    }
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let entries = listing["adapters"].as_array().unwrap();
    let path_only: Vec<String> = listing["path_only"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let any_path_only_detected = listing["detected_path_only"]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    let choices: Vec<Choice> = entries
        .iter()
        .map(|a| Choice::new(&text(a, "id"), &text(a, "name"), &text(a, "package")))
        .collect();
    let default_ticks = || -> Vec<bool> {
        let detected: Vec<bool> = entries
            .iter()
            .map(|a| a["detected"].as_bool().unwrap_or(false))
            .collect();
        if detected.iter().any(|&b| b) {
            detected
        } else if any_path_only_detected {
            vec![false; entries.len()]
        } else {
            entries
                .iter()
                .map(|a| a["selected"].as_bool().unwrap_or(false))
                .collect()
        }
    };
    let on = match flag {
        Some(flag) => ticks_allowing(flag, &choices, &path_only).context("--adapters")?,
        None if listing["npm"] != true => {
            let defaults = default_ticks();
            if defaults.iter().all(|&b| !b) {
                defaults
            } else {
                /* without npm nothing can be installed now: the install step says what to do */
                return Ok(None);
            }
        }
        None if !wizard.input.rich() => default_ticks(),
        None => wizard.input.multiselect(
            &t!("install.setup.adapters.question"),
            &choices,
            &default_ticks(),
        )?,
    };
    let chosen: Vec<String> = choices
        .iter()
        .zip(on)
        .filter(|(_, on)| *on)
        .map(|(c, _)| c.key.clone())
        .collect();
    wizard.record["adapters"] = json!(chosen);
    wizard.save()?;
    Ok(Some(chosen))
}

/// Installs the chosen adapters (their npm output stays off the terminal). `Ok`: what is installed
/// now, for people; `Err`: why not, and what to do.
fn install_adapters(
    role: &str,
    home: &Path,
    release: &Path,
    chosen: Option<&[String]>,
) -> std::result::Result<String, String> {
    let mut args = vec!["install"];
    if let Some(chosen) = chosen {
        args.push("--only");
        args.extend(chosen.iter().map(String::as_str));
    }
    let later = &t!("install.setup.adapters.postponed");
    let output = adapter_command(role, home, release, &args)
        .output()
        .map_err(|_| later.to_owned())?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let last = said
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(later);
        return Err(last.trim().to_owned());
    }
    let installed: Vec<String> = adapter_listing(role, home, release)
        .and_then(|l| l["adapters"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|a| {
            Some(format!(
                "{} {}",
                a["name"].as_str()?,
                a["installed"].as_str()?
            ))
        })
        .collect();
    Ok(installed.join(" · "))
}

/// Where a Hub with this environment answers on its own machine, and where its web client is.
fn hub_addresses(value: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let hosts = parse_host_entries(&value("STEVE_HOST")?)?;
    let port: u16 = value("STEVE_PORT")?.parse().ok()?;
    let chosen = hosts
        .iter()
        .copied()
        .find(|ip| ip.is_loopback())
        .or_else(|| hosts.iter().copied().find(|ip| ip.is_unspecified()))
        .unwrap_or(hosts[0]);
    let ip = match chosen {
        ip if !ip.is_unspecified() => ip,
        std::net::IpAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
        std::net::IpAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
    };
    Some((
        format!("http://{}", SocketAddr::new(ip, port)),
        value("STEVE_PUBLIC_URL")?,
    ))
}

/// The same for the Hub installed in `home` (its `steve.env`): (the Hub, the web client).
pub fn hub_urls(home: &Path) -> Option<(String, String)> {
    let document =
        crate::env_file::EnvDocument::parse(&fs::read(home.join("steve.env")).ok()?).ok()?;
    hub_addresses(|key| document.values().get(key).cloned())
}

/// The answer of the Hub at `api` on `/healthz`, waited for up to `wait`.
pub fn hub_ready(api: &str, wait: Duration) -> Option<Value> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let until = Instant::now() + wait;
    loop {
        let health = client
            .get(format!("{api}/healthz"))
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.json::<Value>().ok());
        if let Some(health) = health.filter(|h| h["ok"] == true) {
            return Some(health);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// The program that opens a URL in the user's browser, where there is one.
fn opener() -> Option<PathBuf> {
    let name = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Run the one-time macOS TCC file-access guide once after service registration so setup
/// confirms permissions without repeatedly interrupting later starts.
fn run_setup_file_access_guide(
    wizard: &mut Wizard<'_>,
    role: &str,
    home: &Path,
    first_boot: bool,
) -> Result<crate::service::file_access::GuideOutcome> {
    if !cfg!(target_os = "macos") {
        return Ok(crate::service::file_access::GuideOutcome::NotApplicable);
    }
    if wizard.record["steps"]
        .as_array()
        .is_some_and(|steps| steps.iter().any(|v| v == "file_access"))
    {
        return Ok(crate::service::file_access::GuideOutcome::Ready);
    }
    let outcome = crate::service::file_access::guide_for_role(role, home, first_boot, false)?;
    wizard.record["steps"]
        .as_array_mut()
        .unwrap()
        .push("file_access".into());
    wizard.save()?;
    Ok(outcome)
}

#[cfg(test)]
fn run_setup_file_access_guide_with(
    wizard: &mut Wizard<'_>,
    role: &str,
    home: &Path,
    first_boot: bool,
    is_macos: bool,
    driver: &mut dyn crate::service::file_access::GuideDriver,
) -> Result<crate::service::file_access::GuideOutcome> {
    if !is_macos {
        return Ok(crate::service::file_access::GuideOutcome::NotApplicable);
    }
    if wizard.record["steps"]
        .as_array()
        .is_some_and(|steps| steps.iter().any(|v| v == "file_access"))
    {
        return Ok(crate::service::file_access::GuideOutcome::Ready);
    }
    let outcome =
        crate::service::file_access::run_guide_macos(role, home, first_boot, false, driver)?;
    wizard.record["steps"]
        .as_array_mut()
        .unwrap()
        .push("file_access".into());
    wizard.save()?;
    Ok(outcome)
}

/// The end of a Hub's setup on a terminal: what to sign in with, then start it and open the page.
fn finish_hub(
    wizard: &mut Wizard<'_>,
    service: &mut crate::service::Service,
    home: &Path,
    environment: &Value,
    flags: &str,
) -> Result<i32> {
    let (api, web) = hub_addresses(|key| environment[key].as_str().map(str::to_owned))
        .context(t!("install.setup.listen_host_missing"))?;
    let saved = fs::read_to_string(home.join(OWNER_TOKEN)).unwrap_or_default();
    let field = |key: &str| {
        saved
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or("")
            .to_string()
    };
    ui::gap();
    ui::heading(&t!("install.setup.finish.heading"));
    ui::note(&[
        (&t!("install.setup.finish.web"), web.clone()),
        (&t!("install.setup.finish.hub_address"), api.clone()),
        (&t!("install.setup.finish.user"), field("user")),
        ("Token", field("token")),
        (&t!("install.setup.finish.home"), home.display().to_string()),
    ]);
    ui::info(&t!(
        "install.setup.finish.token_saved",
        path = home.join(OWNER_TOKEN).display()
    ));
    ui::gap();
    let how = &t!("install.setup.finish.first_visit");
    if !wizard
        .input
        .confirm(&t!("install.setup.finish.start_now"), true)?
    {
        let fa = run_setup_file_access_guide(wizard, "hub", home, false)?;
        ui::outro(&t!(
            "install.setup.finish.start_later",
            flags = flags,
            how = how
        ));
        return Ok(fa.exit_code());
    }
    let spinner = ui::Spinner::start(&t!("install.setup.finish.starting"));
    if let Err(error) = service.dispatch("start", None, 30) {
        spinner.fail(&t!("install.setup.finish.start_failed"));
        return Err(error);
    }
    match hub_ready(&api, Duration::from_secs(45)) {
        Some(health) => spinner.done(
            &t!("install.setup.finish.started"),
            &format!("v{} · {api}", health["version"].as_str().unwrap_or("?")),
        ),
        None => {
            spinner.warn(&t!("install.setup.finish.no_answer"));
            let fa = run_setup_file_access_guide(wizard, "hub", home, false)?;
            ui::outro(&t!("install.setup.finish.see_why", flags = flags));
            return Ok(fa.exit_code());
        }
    }
    let fa = run_setup_file_access_guide(wizard, "hub", home, true)?;
    if let Some(open) = opener() {
        if wizard
            .input
            .confirm(&t!("install.setup.finish.open_browser"), true)?
        {
            let _ = std::process::Command::new(open)
                .arg(&web)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    ui::outro(how);
    Ok(fa.exit_code())
}

/// The pairing code `steve-node pair` reads, when it is not typed.
const PAIR_CODE: &str = "STEVE_PAIR_CODE";
/// Set (to anything) by a program that runs setup and follows its phases: a Hub's remote install.
pub const PROGRESS: &str = "STEVE_SETUP_PROGRESS";
/// The line before each phase on stdout, with `PROGRESS` set: `steve-setup-progress <phase>`.
pub const PROGRESS_LINE: &str = "steve-setup-progress";

fn pairing_code() -> Option<String> {
    std::env::var(PAIR_CODE)
        .ok()
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
}

/// Says that `phase` (install, pair, adapters, start) begins, to the program that asked.
fn progress(phase: &str) {
    if std::env::var_os(PROGRESS).is_some() {
        println!("{PROGRESS_LINE} {phase}");
        let _ = io::stdout().flush();
    }
}

struct SetupTarget {
    role: String,
    home: PathBuf,
    flags: String,
    rich: bool,
}

fn choose_setup_role(input: &mut dyn Prompts, options: &Options, rich: bool) -> Result<String> {
    Ok(match options.role.as_deref() {
        Some(value) => setup_role(value, options)?,
        /* an option that belongs to one role has chosen it */
        None if rich && (options.hub.is_some() || options.pin.is_some()) => "node".into(),
        None if rich && (options.listen.is_some() || options.public_url.is_some()) => "hub".into(),
        None if rich => {
            let choices = [
                Choice::new("hub", "Hub", &t!("install.setup.role.hub_hint")),
                Choice::new("node", "Node", &t!("install.setup.role.node_hint")),
            ];
            choices[input.select(&t!("install.setup.role.question"), &choices, 0)?]
                .key
                .clone()
        }
        None => ask_value(
            input,
            &t!("install.setup.role.typed_question"),
            Some("hub"),
            false,
            |value| setup_role(value, options),
        )?,
    })
}

impl SetupTarget {
    fn new(role: String, options: &Options, rich: bool) -> Result<Self> {
        let default_home = options.home.is_none() && std::env::var_os("STEVE_HOME").is_none();
        let home = role_home(&role, options.home.as_deref())?;
        /* how the commands setup names reach this installation */
        let mut flags = String::new();
        if !default_home {
            flags.push_str(&format!(
                " --home {}",
                shell_quote(home.to_str().context(t!("install.setup.home_not_utf8"))?)?
            ));
        }
        if let Some(label) = &options.label {
            flags.push_str(&format!(" --label {}", shell_quote(label)?));
        }
        Ok(Self {
            role,
            home,
            flags,
            rich,
        })
    }

    /// An existing installation is kept or set aside before any new installation work.
    fn keep_existing(
        &self,
        options: &Options,
        expected: &Value,
        input: &mut dyn Prompts,
    ) -> Result<bool> {
        let Self {
            role,
            home,
            flags,
            rich,
        } = self;
        let rich = *rich;
        /* an installation this run cannot continue: set aside on request, never taken over */
        if exists(&home) && (options.fresh || other_install(&home, &role, &expected).is_some()) {
            let found = read_json(&home.join("setup.json")).unwrap_or_default();
            let fresh = if options.fresh {
                true
            } else if rich {
                let finished = found["steps"]
                    .as_array()
                    .is_some_and(|s| s.iter().any(|v| v == "installed"));
                let choices = [
                    Choice::new(
                        "fresh",
                        &t!("install.setup.existing.fresh"),
                        &t!("install.setup.existing.fresh_hint"),
                    ),
                    Choice::new(
                        "keep",
                        &t!("install.setup.existing.keep"),
                        &t!("install.setup.existing.keep_hint"),
                    ),
                ];
                let title = t!(
                    "install.setup.existing.question",
                    home = home.display(),
                    install = install_name(&found)
                );
                /* a finished installation may be in use: leaving it alone is the answer to a stray Enter */
                input.select(&title, &choices, usize::from(finished))? == 0
            } else {
                bail!(
                    "{}",
                    t!(
                        "install.setup.existing.refused",
                        home = home.display(),
                        install = install_name(&found)
                    )
                );
            };
            if !fresh {
                ui::outro(&t!(
                    "install.setup.existing.kept",
                    role = found["component"].as_str().unwrap_or(&role),
                    flags = flags
                ));
                return Ok(true);
            }
            let aside = ui::step(
                &t!("install.setup.existing.moving"),
                || set_aside(&home),
                |(aside, keys)| {
                    let keys = keys.as_ref().map(|k| {
                        format!(
                            " · {}",
                            t!("install.setup.existing.moved_keys", keys = k.display())
                        )
                    });
                    (
                        t!("install.setup.existing.moved"),
                        format!("{}{}", aside.display(), keys.unwrap_or_default()),
                    )
                },
            )?;
            let _ = aside;
        }
        Ok(false)
    }
}

fn verify_setup_package(source: &Path, expected: &Value, home: &Path, version: &str) -> Result<()> {
    progress("install");
    ui::step(
        &t!("install.setup.step.verifying"),
        || verify_bundle(source, &expected, &home),
        |_| {
            let commit: String = expected["commit"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(8)
                .collect();
            (
                t!("install.setup.step.verified"),
                format!("{version} · {commit}"),
            )
        },
    )?;
    Ok(())
}

fn configure_setup(
    wizard: &mut Wizard<'_>,
    role: &str,
    options: &Options,
) -> Result<(Option<Value>, String)> {
    /* every question first, then the work runs without waiting for anyone */
    let env = if role == "hub" {
        Some(wizard.hub_config_with(
            options.listen.as_deref(),
            options.public_url.as_deref(),
            options.vault_dir.as_deref(),
        )?)
    } else {
        None
    };
    let pairing = wizard.home.join("node/node.json");
    let mut node_hub = String::new();
    if role == "node" {
        wizard.owned_file("roots.json", Some(b"[]\n"))?;
        wizard.owned_file("policy.json", Some(b"{\"max_permissions\":\"auto\",\"full_network\":true,\"exec\":false,\"jobs\":false}\n"))?;
        wizard.owned_file("harness.json", Some(b"{\"adapters\":{}}\n"))?;
        if !exists(&pairing) {
            node_hub = match options
                .hub
                .clone()
                .or_else(|| wizard.record["hub_url"].as_str().map(str::to_string))
            {
                Some(url) => node_hub_url(&url)?,
                None => ask_value(
                    wizard.input,
                    &t!("install.setup.node.hub_question"),
                    None,
                    true,
                    |value| {
                        if value.is_empty() {
                            Ok(String::new())
                        } else {
                            node_hub_url(value)
                        }
                    },
                )?,
            };
        }
    }
    Ok((env, node_hub))
}

fn sign_setup_hub(
    wizard: &mut Wizard<'_>,
    options: &Options,
    home: &Path,
    candidate: &Path,
) -> Result<()> {
    if cfg!(target_os = "macos") {
        if !wizard.record["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "keychain")
        {
            wizard.record["keychain_started"] = true.into();
            wizard.save()?;
            let label = options
                .label
                .as_deref()
                .unwrap_or("io.github.infranite.steve.hub");
            ui::step(
                &t!("install.setup.step.signing"),
                || crate::signing::setup_keychain(&home, &candidate, label),
                |_| {
                    (
                        t!("install.setup.step.signed"),
                        t!("install.setup.step.signed_detail"),
                    )
                },
            )?;
            wizard.record["signed_hub_sha256"] = digest(&candidate.join("bin/steve-hub"))?.into();
            wizard.record["helper_sha256"] = digest(&home.join("bin/steve-keyhelper"))?.into();
            wizard.record["identity_sha256"] = digest(&home.join("signing/identity.json"))?.into();
            wizard.record["steps"]
                .as_array_mut()
                .unwrap()
                .push("keychain".into());
            wizard.save()?;
        }
        for (path, key) in [
            ("bin/steve-keyhelper", "helper_sha256"),
            ("signing/identity.json", "identity_sha256"),
        ] {
            if digest(&home.join(path))?
                != wizard.record[key]
                    .as_str()
                    .context(t!("install.setup.signing.digest_missing"))?
            {
                bail!("{}", t!("install.setup.signing.changed"));
            }
        }
    }
    Ok(())
}

fn resume_node_pairing(
    wizard: &Wizard<'_>,
    options: &Options,
    home: &Path,
    candidate: &Path,
) -> Result<()> {
    let pairing = home.join("node/node.json");
    if wizard.record["pair_started"] != true {
        bail!("{}", t!("install.setup.node.pairing_foreign"));
    }
    let paired = read_json(&pairing)?;
    let requested_hub = options.hub.as_ref().map(|v| node_hub_url(v)).transpose()?;
    if paired["hub_url"] != wizard.record["hub_url"]
        || requested_hub
            .as_ref()
            .is_some_and(|v| paired["hub_url"] != v.as_str())
        || options
            .pin
            .as_ref()
            .is_some_and(|v| paired["pin"] != v.to_lowercase())
    {
        bail!("{}", t!("install.setup.node.pairing_other_hub"));
    }
    let status = clean_command(&candidate.join("bin/steve-node"), Some(&home))
        .arg("status")
        .status()?;
    if !status.success() {
        bail!("{}", t!("install.setup.node.pairing_unknown"));
    }
    Ok(())
}

fn pair_setup_node(
    wizard: &mut Wizard<'_>,
    options: &Options,
    home: &Path,
    candidate: &Path,
    node_hub: &str,
) -> Result<()> {
    let pairing = home.join("node/node.json");
    let hub = node_hub.to_string();
    if wizard.record["pair_started"] != true && exists(&home.join("node/identity.key")) {
        bail!("{}", t!("install.setup.node.identity_foreign"));
    }
    if wizard.record["pair_started"] == true
        && wizard.record["hub_url"] != hub
        && exists(&home.join("node/identity.key"))
    {
        bail!("{}", t!("install.setup.node.pairing_sent_elsewhere"));
    }
    let pin = options
        .pin
        .as_ref()
        .map(|p| p.to_lowercase())
        /* a pin recorded for another Hub is that Hub's */
        .or_else(|| {
            (wizard.record["hub_url"] == hub)
                .then(|| wizard.record["pin"].as_str().map(str::to_string))
                .flatten()
        });
    wizard.record["pair_started"] = true.into();
    wizard.record["hub_url"] = hub.clone().into();
    wizard.record["pin"] = json!(pin);
    wizard.save()?;
    progress("pair");
    ui::info(&t!("install.setup.node.code_question"));
    let mut command = clean_command(&candidate.join("bin/steve-node"), Some(&home));
    command.args(["pair", &hub]);
    /* the code given to setup (as a Hub's remote install gives it) is the pairing's own:
     * `clean_command` leaves out the rest of `STEVE_*`, and stdin may be used up already */
    if let Some(code) = pairing_code() {
        command.env(PAIR_CODE, code);
    }
    if let Some(pin) = pin {
        command.args(["--pin", &pin]);
    }
    if !command.status()?.success() {
        bail!("{}", t!("install.setup.node.pairing_unfinished"));
    }
    if read_json(&pairing)?["hub_url"] != hub {
        bail!("{}", t!("install.setup.node.pairing_readback"));
    }
    ui::done(&t!("install.setup.node.paired"), &hub);
    Ok(())
}

fn setup_adapters(role: &str, home: &Path, candidate: &Path, chosen: Option<&[String]>) {
    /* the ACP adapters are Steve's dependencies: the chosen ones are installed now and kept up to
     * date by the Node; without npm, setup goes on and says how to add them. Before the
     * configuration check, so that it does not report them missing. */
    progress("adapters");
    if chosen.as_ref().is_some_and(|c| c.is_empty()) {
        let _ = install_adapters(&role, &home, &candidate, chosen);
        ui::done(
            &t!("install.setup.adapters.none"),
            &t!("install.setup.adapters.none_hint"),
        );
    } else {
        let spinner = ui::Spinner::start(&t!("install.setup.adapters.installing"));
        match install_adapters(&role, &home, &candidate, chosen) {
            Ok(installed) => spinner.done(&t!("install.setup.adapters.installed"), &installed),
            Err(reason) => spinner.warn(&reason),
        }
    }
}

fn check_setup_hub(candidate: &Path, home: &Path, env: &Value) -> Result<()> {
    let check = ui::step(
        &t!("install.setup.step.checking"),
        || {
            let check = config_report(&candidate.join("bin/steve-hub"), &home, env)?;
            if check["ok"] != true {
                /* and why, in the Hub's words: without them the user is sent to look elsewhere */
                let mut said = t!("install.setup.step.check_failed");
                for reason in check["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    said.push_str("\n  ");
                    said.push_str(reason);
                }
                bail!("{said}");
            }
            Ok(check)
        },
        |_| (t!("install.setup.step.checked"), String::new()),
    )?;
    if let Some(warnings) = check["warnings"].as_array() {
        for warning in warnings {
            ui::info(
                warning
                    .as_str()
                    .unwrap_or(&t!("install.setup.step.config_warning")),
            );
        }
    }
    Ok(())
}

fn setup_owner_token(
    wizard: &mut Wizard<'_>,
    candidate: &Path,
    home: &Path,
    env: &Value,
    rich: bool,
) -> Result<()> {
    /* the owner's first access token, once: the Hub makes this account's user its owner */
    if wizard.record["files"].get(OWNER_TOKEN).is_none() {
        let user = std::env::var("USER")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "owner".into());
        let issued = captured(
            &candidate.join("bin/steve-hub"),
            &home,
            &[
                "--access", "token", "create", "--user", &user, "--name", "setup", "--json",
            ],
            Some(env),
        )?;
        let token = issued["token"]
            .as_str()
            .context(t!("install.setup.token.none"))?;
        let body = format!(
            "user={}\ntoken={token}\n",
            issued["user"].as_str().unwrap_or(&user)
        );
        wizard.owned_file(OWNER_TOKEN, Some(body.as_bytes()))?;
        if rich {
            ui::done(
                &t!("install.setup.token.issued"),
                issued["user"].as_str().unwrap_or(&user),
            );
        }
    }
    Ok(())
}

fn register_setup(
    wizard: &mut Wizard<'_>,
    role: &str,
    home: &Path,
    candidate: &Path,
    label: Option<&str>,
) -> Result<crate::service::Service> {
    let mut service = crate::service::Service::new(&role, &home, label)?;
    progress("start");
    if role == "node" && cfg!(target_os = "macos") {
        wizard.record["node_signing_started"] = true.into();
        wizard.save()?;
    }
    ui::step(
        &t!("install.setup.step.registering", role = role),
        || service.dispatch("install", Some(&candidate), 30),
        |report| {
            (
                t!("install.setup.step.registered"),
                report["label"].as_str().unwrap_or_default().to_owned(),
            )
        },
    )?;
    if role == "node" && cfg!(target_os = "macos") {
        wizard.record["signed_node_sha256"] = digest(&candidate.join("bin/steve-node"))?.into();
        if candidate.join("bin/steve").is_file() {
            wizard.record["signed_cli_sha256"] = digest(&candidate.join("bin/steve"))?.into();
        }
    }
    if !wizard.record["steps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "installed")
    {
        wizard.record["steps"]
            .as_array_mut()
            .unwrap()
            .push("installed".into());
    }
    wizard.save()?;
    Ok(service)
}

pub fn run(source: &Path, options: Options) -> Result<i32> {
    if options.reconfigure {
        reconfigure_existing(options)?;
        return Ok(0);
    }
    validate_options(&options)?;
    let mut input = Terminal;
    let rich = input.rich();
    let expected = manifest(source)?;
    let version = expected["version"].as_str().unwrap_or_default().to_owned();
    ui::banner(&t!("install.setup.banner", version = version));
    ui::intro(&t!("install.setup.intro"));
    let role = choose_setup_role(&mut input, &options, rich)?;
    let target = SetupTarget::new(role, &options, rich)?;
    let SetupTarget {
        role, home, flags, ..
    } = &target;
    if target.keep_existing(&options, &expected, &mut input)? {
        return Ok(0);
    }
    verify_setup_package(source, &expected, home, &version)?;
    let mut wizard = Wizard::claim(source, home, role, &expected, &mut input)?;
    if wizard.record["copy_planned"] == true || wizard.record["environment"].is_object() {
        ui::info(&t!("install.setup.step.resuming"));
    }
    let (env, node_hub) = configure_setup(&mut wizard, role, &options)?;
    let chosen = adapter_choice(&mut wizard, role, source, options.adapters.as_deref())?;
    let candidate = ui::step(
        &t!("install.setup.step.preparing"),
        || wizard.copy_bundle(&expected),
        |candidate| {
            (
                t!("install.setup.step.prepared"),
                candidate.display().to_string(),
            )
        },
    )?;
    if role == "hub" {
        sign_setup_hub(&mut wizard, &options, home, &candidate)?;
    } else if exists(&home.join("node/node.json")) {
        resume_node_pairing(&wizard, &options, home, &candidate)?;
    } else if !node_hub.is_empty() {
        pair_setup_node(&mut wizard, &options, home, &candidate, &node_hub)?;
    }
    setup_adapters(role, home, &candidate, chosen.as_deref());
    if let Some(env) = &env {
        check_setup_hub(&candidate, home, env)?;
        setup_owner_token(&mut wizard, &candidate, home, env, rich)?;
    }
    let mut service = register_setup(
        &mut wizard,
        role,
        home,
        &candidate,
        options.label.as_deref(),
    )?;
    if let (true, Some(env)) = (rich, &env) {
        return finish_hub(&mut wizard, &mut service, &home, env, &flags);
    }
    let fa = run_setup_file_access_guide(&mut wizard, role, home, false)?;
    ui::gap();
    for line in completion_lines(
        &role,
        &home,
        options.label.as_deref(),
        &wizard.record["environment"],
        home.join("node/node.json").is_file(),
    )? {
        ui::info(&line);
    }
    ui::outro("");
    Ok(fa.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Answers(Vec<String>);
    impl Prompts for Answers {
        fn interactive(&self) -> bool {
            true
        }
        fn line(&mut self, _: &str, _: Option<&str>, _: bool) -> Result<String> {
            if self.0.is_empty() {
                bail!("fixture input exhausted");
            }
            Ok(self.0.remove(0))
        }
    }
    /// A terminal with widgets: each `select` takes the next pick, lines are typed as in `Answers`.
    struct Widgets {
        picks: Vec<usize>,
        lines: Vec<String>,
        asked: Vec<String>,
    }
    impl Prompts for Widgets {
        fn interactive(&self) -> bool {
            true
        }
        fn rich(&self) -> bool {
            true
        }
        fn select(&mut self, title: &str, choices: &[Choice], _: usize) -> Result<usize> {
            self.asked.push(format!(
                "{title} {}",
                choices
                    .iter()
                    .map(|c| c.key.as_str())
                    .collect::<Vec<_>>()
                    .join("/")
            ));
            Ok(self.picks.remove(0))
        }
        fn line(&mut self, title: &str, _: Option<&str>, _: bool) -> Result<String> {
            self.asked.push(title.to_owned());
            if self.lines.is_empty() {
                bail!("fixture input exhausted");
            }
            Ok(self.lines.remove(0))
        }
    }
    #[test]
    fn a_pairing_that_made_no_key_is_not_under_way() {
        let home = tempfile::tempdir().unwrap();
        let record = json!({"steps": [], "pair_started": true, "hub_url": "https://old.example"});
        assert!(!underway(&record, home.path()));
        fs::create_dir_all(home.path().join("node")).unwrap();
        fs::write(home.path().join("node/identity.key"), b"key").unwrap();
        assert!(underway(&record, home.path()), "a key may have been sent");
        assert!(underway(
            &json!({"steps": ["installed"]}),
            tempfile::tempdir().unwrap().path()
        ));
    }
    fn meta(version: &str) -> Value {
        json!({"version":version,"commit":"a".repeat(40)})
    }
    #[test]
    fn interactive_values_retry_but_end_of_input_is_not_retried() {
        let mut input = Answers(vec!["wrong-role".into(), "node".into()]);
        assert_eq!(
            ask_value(&mut input, "role", None, false, |value| {
                setup_role(value, &Options::default())
            })
            .unwrap(),
            "node"
        );
        let mut input = Answers(vec!["0.0.0.0:0".into(), "[::1]:8440".into()]);
        assert_eq!(
            ask_value(&mut input, "listen", None, false, listen_address)
                .unwrap()
                .to_string(),
            "[::1]:8440"
        );
        let mut input = Answers(vec![
            "http://localhost:8440".into(),
            "https://hub.example/".into(),
        ]);
        assert_eq!(
            ask_value(&mut input, "hub", None, true, node_hub_url).unwrap(),
            "https://hub.example"
        );
        // a Hub reached over tailcat (A6): the address and port, nothing that could be an argument
        assert_eq!(
            node_hub_url(" tailcat:tcAbC_-123456:8442 ").unwrap(),
            "tailcat:tcAbC_-123456:8442"
        );
        for bad in [
            "tailcat:tcAbC_-123456",
            "tailcat:tc;rm -rf /:1",
            "tailcat:xyz12345678:1",
        ] {
            assert!(node_hub_url(bad).is_err(), "{bad}");
        }
        let mut input = Answers(vec!["not-a-url".into()]);
        let error = ask_value(&mut input, "issuer", None, false, oidc_issuer_url).unwrap_err();
        assert_eq!(error.to_string(), "fixture input exhausted");
    }

    #[test]
    fn noninteractive_invalid_values_and_input_errors_never_retry() {
        struct Input {
            interactive: bool,
            fail_read: bool,
            reads: usize,
        }
        impl Prompts for Input {
            fn interactive(&self) -> bool {
                self.interactive
            }
            fn line(&mut self, _: &str, _: Option<&str>, _: bool) -> Result<String> {
                self.reads += 1;
                assert_eq!(self.reads, 1, "input must not be retried");
                if self.fail_read {
                    return Err(
                        io::Error::new(io::ErrorKind::Interrupted, "input interrupted").into(),
                    );
                }
                Ok("invalid-url".into())
            }
        }
        for (interactive, fail_read) in [(false, false), (true, true)] {
            let mut input = Input {
                interactive,
                fail_read,
                reads: 0,
            };
            let error = ask_value(&mut input, "issuer", None, false, oidc_issuer_url).unwrap_err();
            assert_eq!(input.reads, 1);
            if fail_read {
                assert!(error.downcast_ref::<io::Error>().is_some());
            }
        }
    }

    #[test]
    fn explicit_invalid_options_fail_before_creating_a_home_or_reading_a_bundle() {
        let temp = tempfile::tempdir().unwrap();
        for (mut options, expected) in [
            (
                Options {
                    listen: Some("localhost:8440".into()),
                    ..Default::default()
                },
                "监听地址应为",
            ),
            (
                Options {
                    listen: Some("127.0.0.1:0".into()),
                    ..Default::default()
                },
                "端口不能是 0",
            ),
            (
                Options {
                    hub: Some("http://localhost:8440".into()),
                    ..Default::default()
                },
                "Node 配对需要 HTTPS",
            ),
            (
                Options {
                    pin: Some("sha256:short".into()),
                    ..Default::default()
                },
                "--pin 需要",
            ),
            (
                Options {
                    role: Some("hub".into()),
                    hub: Some("https://hub.example".into()),
                    ..Default::default()
                },
                "--hub/--pin 只用于 Node",
            ),
            (
                Options {
                    role: Some("node".into()),
                    listen: Some("0.0.0.0:8440".into()),
                    ..Default::default()
                },
                "--listen/--public-url 只用于 Hub",
            ),
            (
                Options {
                    role: Some("node".into()),
                    public_url: Some("https://hub.example".into()),
                    ..Default::default()
                },
                "--listen/--public-url 只用于 Hub",
            ),
            (
                Options {
                    public_url: Some("https://hub.example".into()),
                    pin: Some(format!("sha256:{}", "a".repeat(64))),
                    ..Default::default()
                },
                "--public-url 只用于 Hub",
            ),
            (
                Options {
                    public_url: Some("http://0.0.0.0:8441".into()),
                    ..Default::default()
                },
                "访问地址",
            ),
            (
                Options {
                    fresh: true,
                    reconfigure: true,
                    ..Default::default()
                },
                "--fresh 是重新安装",
            ),
        ] {
            let home = temp.path().join("must-not-exist");
            options.home = Some(home.clone());
            let error = run(&temp.path().join("missing-bundle"), options).unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert!(!home.exists());
        }
    }

    #[test]
    fn completion_distinguishes_configuration_from_runtime_and_unpaired_nodes() {
        let home = Path::new("/tmp/steve user's data");
        for (host, address) in [("0.0.0.0", "0.0.0.0:8440"), ("::1", "[::1]:8440")] {
            let env = json!({"STEVE_HOST":host,"STEVE_PORT":"8440","STEVE_PUBLIC_URL":"https://hub.example"});
            let lines = completion_lines("hub", home, None, &env, false).unwrap();
            assert!(lines.contains(&format!("监听地址：{address}")));
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("配置值，尚未验证可达"))
            );
            for action in ["start", "status", "doctor"] {
                assert!(lines.contains(&format!(
                    r"steve hub {action} --home '/tmp/steve user'\''s data'"
                )));
            }
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("用用户名和 token 登录"))
            );
        }
        // an installation with a label of its own is named by it in every command, and the Hub
        // address to type is the one it listens on (this machine's only when it listens on all)
        let label = Some("io.example.trial.hub");
        for (host, hub) in [
            ("127.0.0.1", "http://127.0.0.1:18440"),
            ("::1", "http://[::1]:18440"),
            ("0.0.0.0", "http://<这台机器的地址>:18440"),
        ] {
            let env = json!({"STEVE_HOST":host,"STEVE_PORT":"18440","STEVE_WEB_PORT":"18441","STEVE_PUBLIC_URL":"http://127.0.0.1:18441"});
            let lines = completion_lines("hub", home, label, &env, false).unwrap();
            for action in ["start", "status", "doctor"] {
                assert!(
                    lines.contains(&format!(r"steve hub {action} --home '/tmp/steve user'\''s data' --label 'io.example.trial.hub'")),
                    "{lines:?}"
                );
            }
            assert!(
                lines.contains(&format!(
                    "网页端口：18441。第一次打开网页时，输入 Hub 地址 {hub}"
                )),
                "{lines:?}"
            );
        }
        let lines = completion_lines("node", home, label, &Value::Null, false)
            .unwrap()
            .join("\n");
        assert!(lines.contains("steve setup node --home '/tmp/steve user'\\''s data' --label 'io.example.trial.hub'"), "{lines}");
        let lines = completion_lines("node", home, None, &Value::Null, false)
            .unwrap()
            .join("\n");
        assert!(lines.contains("Node 尚未配对"));
        assert!(lines.contains("steve setup node --home"));
        assert!(!lines.contains("完整指纹"));
        let lines = completion_lines("node", home, None, &Value::Null, true)
            .unwrap()
            .join("\n");
        assert!(lines.contains("核对并确认完整指纹"));
    }
    #[test]
    fn setup_urls_reject_listener_addresses_plain_remote_http_and_ambiguous_urls() {
        for invalid in [
            "http://0.0.0.0:8440",
            "https://[::]:8440",
            "http://192.0.2.20:8440",
            "ftp://identity.example/",
            "https://user:password@identity.example/",
            "https://identity.example/?query=1",
            "https://identity.example/#fragment",
            "https://identity.example/a b",
        ] {
            assert!(browser_url(invalid, "browser").is_err(), "{invalid}");
            assert!(oidc_issuer_url(invalid).is_err(), "{invalid}");
        }
        for valid in [
            "https://identity.example/",
            "http://localhost:8440/issuer/",
            "http://127.0.0.1:8440/issuer/",
            "http://[::1]:8440/issuer/",
        ] {
            assert_eq!(oidc_issuer_url(valid).unwrap(), valid);
            assert_eq!(
                browser_url(valid, "browser").unwrap(),
                valid.trim_end_matches('/')
            );
        }
    }
    #[test]
    fn unknown_home_and_modified_receipt_files_are_not_adopted() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package");
        private_dir(&source).unwrap();
        let home = temp.path().join("home");
        private_dir(&home).unwrap();
        let mut input = Answers(vec![]);
        assert!(Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).is_err());
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
        fs::remove_dir(&home).unwrap();
        let mut wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
        wizard.owned_file("config", Some(b"initial")).unwrap();
        drop(wizard);
        fs::write(home.join("config"), "changed").unwrap();
        let receipt = fs::read(home.join("setup.json")).unwrap();
        let error = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input)
            .err()
            .unwrap();
        assert!(error.to_string().contains("setup 只续接"));
        assert_eq!(fs::read(home.join("config")).unwrap(), b"changed");
        assert_eq!(fs::read(home.join("setup.json")).unwrap(), receipt);
    }
    #[test]
    fn interrupted_unsigned_setup_can_resume_after_a_package_upgrade() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package");
        private_dir(&source).unwrap();
        let home = temp.path().join("home");
        let mut input = Answers(vec![]);
        drop(Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap());
        let wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.1"), &mut input).unwrap();
        assert_eq!(wizard.record["version"], "1.0.1");
        assert_eq!(fs::read_dir(home.join("setup-backups")).unwrap().count(), 1);
        drop(wizard);
        let mut receipt = read_json(&home.join("setup.json")).unwrap();
        receipt["keychain_started"] = true.into();
        atomic_json(&home.join("setup.json"), &receipt).unwrap();
        assert!(Wizard::claim(&source, &home, "hub", &meta("1.0.2"), &mut input).is_err());
    }
    #[test]
    fn new_hub_configuration_binds_all_interfaces_and_asks_for_no_oidc() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package");
        private_dir(&source).unwrap();
        let home = temp.path().join("home");
        let keys = temp.path().join("keys");
        // the browser address follows from the listen address: nothing is asked for it
        let mut answers = vec![];
        if cfg!(target_os = "linux") {
            answers.push(keys.to_str().unwrap().into());
        }
        let mut input = Answers(answers);
        let mut wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
        let env = wizard.hub_config(Some("0.0.0.0:8440")).unwrap();
        assert_eq!(env["STEVE_HOST"], "0.0.0.0");
        assert_eq!(env["STEVE_ALLOW_ANY_HOST"], "1");
        assert_eq!(env["STEVE_PUBLIC_URL"], "http://127.0.0.1:8441");
        assert_eq!(
            env["STEVE_WEB_PORT"], "8441",
            "the web client on a port of its own"
        );
        assert!(env.get("STEVE_OIDC_ISSUER").is_none());
        let bytes = fs::read_to_string(home.join("steve.env")).unwrap();
        assert!(!bytes.contains("OIDC"));
        assert!(!home.join("secrets/oidc-client-secret").exists());
        // resumed: the same environment, nothing asked again
        drop(wizard);
        let mut input = Answers(vec![]);
        let mut wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
        assert_eq!(wizard.hub_config(None).unwrap(), env);
        // the same options again are the saved configuration; other ones are never written over it
        assert_eq!(
            wizard
                .hub_config_with(Some("0.0.0.0:8440"), Some("http://127.0.0.1:8441/"), None)
                .unwrap(),
            env
        );
        let differs = |error: anyhow::Error| error.to_string().contains("已有初始化配置不同");
        assert!(differs(
            wizard
                .hub_config_with(None, Some("https://hub.example"), None)
                .unwrap_err()
        ));
        assert!(differs(
            wizard.hub_config(Some("127.0.0.1:8440")).unwrap_err()
        ));
    }

    /// One new Hub configuration in a home of its own: (the environment, what was asked).
    fn configured(
        picks: Option<Vec<usize>>,
        lines: &[&str],
        listen: Option<&str>,
        public: Option<&str>,
    ) -> Result<(Value, Vec<String>)> {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package");
        private_dir(&source).unwrap();
        let home = temp.path().join("home");
        let keys = temp.path().join("keys");
        let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        match picks {
            Some(picks) => {
                let mut input = Widgets {
                    picks,
                    lines,
                    asked: vec![],
                };
                let env = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input)?
                    .hub_config_with(listen, public, Some(&keys))?;
                Ok((env, input.asked))
            }
            None => {
                let mut input = Answers(lines);
                let env = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input)?
                    .hub_config_with(listen, public, Some(&keys))?;
                Ok((env, vec![]))
            }
        }
    }

    #[test]
    fn the_wizard_asks_who_can_reach_the_hub_and_derives_the_browser_address() {
        // other devices too: every interface
        let (env, asked) = configured(Some(vec![0]), &[], None, None).unwrap();
        assert_eq!(
            (env["STEVE_HOST"].as_str(), env["STEVE_PORT"].as_str()),
            (Some("0.0.0.0"), Some("8440"))
        );
        assert_eq!(env["STEVE_ALLOW_ANY_HOST"], "1");
        assert_eq!(env["STEVE_PUBLIC_URL"], "http://127.0.0.1:8441");
        assert_eq!(asked, ["谁可以连接这台 Hub？ lan/local/custom"]);
        // this machine only
        let (env, asked) = configured(Some(vec![1]), &[], None, None).unwrap();
        assert_eq!(env["STEVE_HOST"], "127.0.0.1");
        assert!(env.get("STEVE_ALLOW_ANY_HOST").is_none());
        assert_eq!(env["STEVE_PUBLIC_URL"], "http://127.0.0.1:8441");
        assert_eq!(asked.len(), 1);
        // an address of one's own: IPv6 loopback has a browser address too
        let (env, asked) = configured(Some(vec![2]), &["[::1]:9000"], None, None).unwrap();
        assert_eq!(
            (env["STEVE_HOST"].as_str(), env["STEVE_PORT"].as_str()),
            (Some("::1"), Some("9000"))
        );
        assert_eq!(env["STEVE_PUBLIC_URL"], "http://[::1]:9001");
        assert_eq!(env["STEVE_WEB_PORT"], "9001");
        assert_eq!(asked[1..], ["监听地址"]);
        // --listen answers the question
        let (env, asked) = configured(Some(vec![]), &[], Some("127.0.0.1:18440"), None).unwrap();
        assert_eq!(env["STEVE_PUBLIC_URL"], "http://127.0.0.1:18441");
        assert!(asked.is_empty());
        // --public-url names another browser address
        let (env, asked) = configured(
            Some(vec![]),
            &[],
            Some("0.0.0.0:8440"),
            Some("https://hub.example/"),
        )
        .unwrap();
        assert_eq!(env["STEVE_PUBLIC_URL"], "https://hub.example");
        assert!(asked.is_empty());
        // one specific address of another network: no browser address follows, so it is asked
        let (env, asked) = configured(
            Some(vec![]),
            &["https://hub.example/"],
            Some("192.0.2.20:8440"),
            None,
        )
        .unwrap();
        assert_eq!(env["STEVE_PUBLIC_URL"], "https://hub.example");
        assert_eq!(asked, ["浏览器访问地址"]);
        // typed, off a terminal with widgets: the listen address itself is asked
        let (env, _) = configured(None, &["127.0.0.1:8440"], None, None).unwrap();
        assert_eq!(env["STEVE_HOST"], "127.0.0.1");
        // the web client's port is the next one: the last port has none
        let error = configured(None, &[], Some("127.0.0.1:65535"), None).unwrap_err();
        assert!(error.to_string().contains("监听端口太大"), "{error:#}");
    }

    #[test]
    fn typed_choices_take_keys_and_ask_again_on_a_terminal() {
        let choices = [
            Choice::new("hub", "Hub", ""),
            Choice::new("node", "Node", ""),
        ];
        let mut input = Answers(vec!["both".into(), " NODE ".into()]);
        assert_eq!(typed_select(&mut input, "role", &choices, 0).unwrap(), 1);
        let mut input = Answers(vec!["codex".into(), "all".into()]);
        assert_eq!(
            typed_multiselect(&mut input, "which", &choices, &[true, false]).unwrap(),
            [true, true]
        );
        assert!(input.0.is_empty());
        assert_eq!(ticks("all", &choices).unwrap(), [true, true]);
        assert_eq!(ticks("none", &choices).unwrap(), [false, false]);
        assert_eq!(ticks(" Node ", &choices).unwrap(), [false, true]);
        assert_eq!(ticks("node,hub", &choices).unwrap(), [true, true]);
        assert!(
            ticks("codex", &choices)
                .unwrap_err()
                .to_string()
                .contains("没有 codex")
        );
        let mut input = Answers(vec!["maybe".into(), "是".into(), "N".into()]);
        assert!(typed_confirm(&mut input, "start", false).unwrap());
        assert!(!typed_confirm(&mut input, "start", true).unwrap());
        // off a terminal a wrong answer is the end, not another question
        struct Piped(Vec<String>);
        impl Prompts for Piped {
            fn interactive(&self) -> bool {
                false
            }
            fn line(&mut self, _: &str, _: Option<&str>, _: bool) -> Result<String> {
                Ok(self.0.remove(0))
            }
        }
        let mut input = Piped(vec!["both".into(), "node".into()]);
        assert!(typed_select(&mut input, "role", &choices, 0).is_err());
        assert_eq!(input.0, ["node"]);
        assert!(
            typed_multiselect(
                &mut Piped(vec!["codex".into()]),
                "which",
                &choices,
                &[true, true]
            )
            .is_err()
        );
        assert!(typed_confirm(&mut Piped(vec!["maybe".into()]), "start", true).is_err());
    }

    #[test]
    fn hub_addresses_are_where_the_hub_answers_and_where_the_web_client_is() {
        let of = |host: &str, port: &str| {
            let env = json!({"STEVE_HOST":host,"STEVE_PORT":port,"STEVE_PUBLIC_URL":"https://hub.example"});
            hub_addresses(|key| env[key].as_str().map(str::to_owned))
        };
        let web = "https://hub.example".to_string();
        // every interface is reached on loopback
        assert_eq!(
            of("0.0.0.0", "8440"),
            Some(("http://127.0.0.1:8440".into(), web.clone()))
        );
        assert_eq!(
            of("::", "8440"),
            Some(("http://[::1]:8440".into(), web.clone()))
        );
        assert_eq!(
            of("192.0.2.20", "9000"),
            Some(("http://192.0.2.20:9000".into(), web))
        );
        assert_eq!(of("hub.example", "8440"), None);
        assert_eq!(hub_addresses(|_| None), None);
        // the installed Hub's own file
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(hub_urls(temp.path()), None);
        fs::write(
            temp.path().join("steve.env"),
            "STEVE_HOST=0.0.0.0\nSTEVE_PORT=18440\nSTEVE_PUBLIC_URL=http://127.0.0.1:18441\n",
        )
        .unwrap();
        assert_eq!(
            hub_urls(temp.path()),
            Some((
                "http://127.0.0.1:18440".into(),
                "http://127.0.0.1:18441".into()
            ))
        );
    }

    #[test]
    fn an_installation_of_another_version_or_role_is_never_continued() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap().join("home");
        // nothing there, or a directory that is not an installation: not this question
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_none());
        private_dir(&home).unwrap();
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_none());
        let receipt = |version: &str, component: &str, steps: Value| {
            let record = json!({"format":1,"home":home,"component":component,"version":version,
                "commit":"a".repeat(40),"steps":steps});
            atomic_json(&home.join("setup.json"), &record).unwrap();
        };
        // this version's own setup, however far it got, goes on
        receipt("1.0.1", "hub", json!(["keychain", "installed"]));
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_none());
        // another version that only asked its questions is resumed (Wizard::claim takes it over)
        receipt("1.0.0", "hub", json!([]));
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_none());
        // another version under way: a step done, an identity, a database, a service
        receipt("1.0.0", "hub", json!(["installed"]));
        let found = other_install(&home, "hub", &meta("1.0.1")).unwrap();
        assert_eq!(
            install_name(&found),
            format!("Hub 1.0.0 · {}", "a".repeat(8))
        );
        receipt("1.0.0", "hub", json!([]));
        private_dir(&home.join("data")).unwrap();
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_none());
        fs::write(home.join("data/steve.sqlite3"), b"").unwrap();
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_some());
        fs::remove_file(home.join("data/steve.sqlite3")).unwrap();
        let mut record = read_json(&home.join("setup.json")).unwrap();
        record["keychain_started"] = true.into();
        atomic_json(&home.join("setup.json"), &record).unwrap();
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_some());
        // the other role's, whatever its version
        receipt("1.0.1", "node", json!([]));
        let found = other_install(&home, "hub", &meta("1.0.1")).unwrap();
        assert!(install_name(&found).starts_with("Node 1.0.1"));
        // a receipt written for another directory is not this installation's
        let mut record = read_json(&home.join("setup.json")).unwrap();
        record["home"] = json!(temp.path().join("elsewhere"));
        atomic_json(&home.join("setup.json"), &record).unwrap();
        assert!(other_install(&home, "hub", &meta("1.0.1")).is_none());
        // what a directory set aside is named after: 20261111-011243
        let stamp = stamp();
        assert!(stamp.len() == 15 && stamp.as_bytes()[8] == b'-', "{stamp}");
        assert!(
            stamp
                .bytes()
                .enumerate()
                .all(|(at, b)| at == 8 || b.is_ascii_digit()),
            "{stamp}"
        );
    }

    /// The Hub's check answers with a report also when it refuses the configuration (exit 2): setup
    /// reads it either way, so it can say why; and the check is told setup's language.
    #[test]
    fn a_refused_configuration_still_gives_its_report() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let hub = |name: &str, body: &str| {
            let path = temp.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        let env = json!({"STEVE_PORT":"18440"});
        let refused = hub(
            "refused",
            r#"[ "$1" = --check-config ] || exit 95
echo '{"ok":false,"errors":["STEVE_HOST: cannot listen on 192.0.2.9"],"listen":[],"warnings":[]}'
exit 2"#,
        );
        let report = config_report(&refused, temp.path(), &env).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(
            report["errors"],
            json!(["STEVE_HOST: cannot listen on 192.0.2.9"])
        );
        let passed = hub(
            "passed",
            r#"echo "{\"ok\":true,\"warnings\":[\"$STEVE_LANG $STEVE_PORT $STEVE_HOME\"]}""#,
        );
        let report = config_report(&passed, temp.path(), &env).unwrap();
        assert_eq!(
            report["warnings"][0],
            format!("{} 18440 {}", steve_i18n::current(), temp.path().display())
        );
        // no report at all is the command's failure, not a verdict on the configuration
        let broken = hub("broken", "exit 3");
        assert!(config_report(&broken, temp.path(), &env).is_err());
        let silent = hub("silent", "exit 0");
        assert!(config_report(&silent, temp.path(), &env).is_err());
    }

    #[test]
    fn completion_shows_the_owner_token() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        private_dir(&home.join("secrets")).unwrap();
        private_file(&home.join(OWNER_TOKEN), b"user=lee\ntoken=stt_abc\n").unwrap();
        let env = json!({"STEVE_HOST":"127.0.0.1","STEVE_PORT":"8440","STEVE_PUBLIC_URL":"http://127.0.0.1:8440"});
        let lines = completion_lines("hub", home, None, &env, false).unwrap();
        assert!(lines.contains(&"登录用户名：lee".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"访问 token：stt_abc".to_string()),
            "{lines:?}"
        );
        // more tokens: the installation's own binary by its path (it is not on the PATH), told its home
        let h = home.to_str().unwrap();
        let command =
            format!("运行 STEVE_HOME='{h}' '{h}/current/bin/steve-hub' --access token create");
        assert!(
            lines.iter().any(|line| line.contains(&command)),
            "{lines:?}"
        );
        fs::remove_file(home.join(OWNER_TOKEN)).unwrap();
        let lines = completion_lines("hub", home, None, &env, false).unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("没有找到 owner token") && line.contains(&command)),
            "{lines:?}"
        );
    }

    #[test]
    fn multihost_steve_host_resolves_local_probe_and_completion_without_regenerating_custom_env() {
        let of = |host: &str, port: &str| {
            let env = json!({"STEVE_HOST":host,"STEVE_PORT":port,"STEVE_PUBLIC_URL":"https://hub.example"});
            hub_addresses(|key| env[key].as_str().map(str::to_owned))
        };
        let web = "https://hub.example".to_string();
        assert_eq!(
            of("198.51.100.10,127.0.0.1", "8440"),
            Some(("http://127.0.0.1:8440".into(), web.clone()))
        );
        assert_eq!(
            of("0.0.0.0,::", "8440"),
            Some(("http://127.0.0.1:8440".into(), web))
        );
        let env = json!({
            "STEVE_HOST": "127.0.0.1,198.51.100.10",
            "STEVE_PORT": "8440",
            "STEVE_WEB_PORT": "8441",
            "STEVE_PUBLIC_URL": "https://hub.example"
        });
        let lines =
            completion_lines("hub", Path::new("/tmp/steve-test"), None, &env, false).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("127.0.0.1:8440, 198.51.100.10:8440")),
            "{lines:?}"
        );

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package");
        private_dir(&source).unwrap();
        let home = temp.path().join("home");
        let mut input = Answers(vec![]);
        let mut wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
        let raw_env = b"STEVE_HOST=127.0.0.1,198.51.100.10\nSTEVE_PORT=8440\nSTEVE_PUBLIC_URL=https://hub.example\nCODEX_HOME=/custom/codex\n";
        private_file(&home.join("steve.env"), raw_env).unwrap();
        wizard.record["environment"] = json!({
            "STEVE_HOST": "127.0.0.1,198.51.100.10",
            "STEVE_PORT": "8440",
            "STEVE_PUBLIC_URL": "https://hub.example",
            "CODEX_HOME": "/custom/codex"
        });
        wizard.record["files"]["steve.env"] = bytes_digest(raw_env).into();
        wizard.save().unwrap();
        let got = wizard.hub_config(None).unwrap();
        assert_eq!(got["STEVE_HOST"], "127.0.0.1,198.51.100.10");
        assert!(wizard.hub_config(Some("127.0.0.1:8440")).is_err());
    }

    #[test]
    fn adapter_choice_handles_path_only_detection_and_preserves_existing_selection_on_rerun() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("release");
        fs::create_dir_all(release.join("bin")).unwrap();
        let listing_file = temp.path().join("listing.json");
        let bin = release.join("bin/steve-node");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$1\" = adapters ] && [ \"$2\" = list ] && cat '{}'\n",
                listing_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();

        // 1) PATH-only CLI detected, npm absent: defaults to [] and persists empty selection
        fs::write(
            &listing_file,
            r#"{"npm":false,"chosen":false,"path_only":["cursor","kiro"],"detected_path_only":["cursor"],"adapters":[{"id":"claude-code","name":"Claude Code","package":"@agentclientprotocol/claude-agent-acp","installed":null,"selected":true,"detected":false},{"id":"codex","name":"Codex","package":"@zed-industries/codex-acp","installed":null,"selected":true,"detected":false}]}"#,
        )
        .unwrap();
        let home1 = temp.path().join("home1");
        let mut input = Answers(vec![]);
        let mut w1 = Wizard::claim(&release, &home1, "hub", &meta("1.0.0"), &mut input).unwrap();
        assert_eq!(
            adapter_choice(&mut w1, "hub", &release, None).unwrap(),
            Some(vec![])
        );
        // --adapters cursor accepts PATH-only built-in ID as valid 0-npm selection
        let home2 = temp.path().join("home2");
        let mut w2 = Wizard::claim(&release, &home2, "hub", &meta("1.0.0"), &mut input).unwrap();
        assert_eq!(
            adapter_choice(&mut w2, "hub", &release, Some("cursor")).unwrap(),
            Some(vec![])
        );
        assert_eq!(
            adapter_choice(&mut w2, "hub", &release, Some("cursor,codex")).unwrap(),
            Some(vec!["codex".to_string()])
        );

        // 2) Re-running setup on a home where `chosen == true` returns None instead of stale saved `record["adapters"]`
        fs::write(
            &listing_file,
            r#"{"npm":true,"chosen":true,"path_only":["cursor"],"detected_path_only":[],"adapters":[{"id":"claude-code","name":"Claude Code","package":"@agentclientprotocol/claude-agent-acp","installed":"1.0.0","selected":true,"detected":false}]}"#,
        )
        .unwrap();
        let home3 = temp.path().join("home3");
        let mut w3 = Wizard::claim(&release, &home3, "hub", &meta("1.0.0"), &mut input).unwrap();
        w3.record["adapters"] = json!(["claude-code"]);
        w3.save().unwrap();
        assert_eq!(
            adapter_choice(&mut w3, "hub", &release, None).unwrap(),
            None
        );
        // Explicit `--adapters` still overrides when passed
        assert_eq!(
            adapter_choice(&mut w3, "hub", &release, Some("none")).unwrap(),
            Some(vec![])
        );
    }

    #[test]
    fn setup_triggers_macos_file_access_guide_once_in_desktop_and_headless_modes() {
        use crate::service::file_access::{
            EXIT_ACCESS_MISSING, GuideDriver, GuideOutcome, SessionKind, tcc_executable_path,
        };
        use std::collections::VecDeque;
        use steve_types::devices::{
            FileAccessReport, FileAccessStatus, ProtectedAccess, ProtectedLocation,
        };

        struct FakeGuide {
            session: SessionKind,
            reports: VecDeque<FileAccessReport>,
            confirms: VecDeque<bool>,
            probes: usize,
            opened: Vec<String>,
            done: Vec<(String, String)>,
            info: Vec<String>,
        }

        impl GuideDriver for FakeGuide {
            fn probe(&mut self, _wait: Duration) -> FileAccessReport {
                self.probes += 1;
                self.reports
                    .pop_front()
                    .unwrap_or_else(FileAccessReport::unknown)
            }
            fn session(&self) -> SessionKind {
                self.session
            }
            fn confirm(&mut self, _prompt: &str, _default: bool) -> Result<bool> {
                Ok(self
                    .confirms
                    .pop_front()
                    .expect("unexpected confirm prompt"))
            }
            fn open_settings(&mut self, url: &str) -> bool {
                self.opened.push(url.to_owned());
                true
            }
            fn emit_done(&mut self, text: &str, detail: &str) {
                self.done.push((text.to_owned(), detail.to_owned()));
            }
            fn emit_info(&mut self, text: &str) {
                self.info.push(text.to_owned());
            }
            fn emit_warn(&mut self, _text: &str) {}
        }

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package");
        private_dir(&source).unwrap();

        // 1. Desktop macOS setup with missing permission -> guides once, opens settings, rechecks -> granted.
        let hub_home = temp.path().join("hub-desktop");
        let hub_exe = tcc_executable_path("hub", &hub_home)
            .to_string_lossy()
            .into_owned();
        let mut input = Answers(vec![]);
        let mut wizard =
            Wizard::claim(&source, &hub_home, "hub", &meta("1.0.0"), &mut input).unwrap();
        let mut desktop = FakeGuide {
            session: SessionKind::Desktop,
            reports: VecDeque::from([
                FileAccessReport {
                    status: FileAccessStatus::Missing,
                    full_disk_access: FileAccessStatus::Missing,
                    locations: vec![ProtectedAccess {
                        location: ProtectedLocation::Desktop,
                        status: FileAccessStatus::Missing,
                        path: Some("/Users/example/Desktop".into()),
                    }],
                    executable_path: Some(hub_exe.clone()),
                },
                FileAccessReport {
                    status: FileAccessStatus::Granted,
                    full_disk_access: FileAccessStatus::Granted,
                    locations: vec![ProtectedAccess {
                        location: ProtectedLocation::Desktop,
                        status: FileAccessStatus::Granted,
                        path: Some("/Users/example/Desktop".into()),
                    }],
                    executable_path: Some(hub_exe.clone()),
                },
            ]),
            confirms: VecDeque::from([true, true]),
            probes: 0,
            opened: Vec::new(),
            done: Vec::new(),
            info: Vec::new(),
        };
        let first = run_setup_file_access_guide_with(
            &mut wizard,
            "hub",
            &hub_home,
            true,
            true,
            &mut desktop,
        )
        .unwrap();
        assert_eq!(first, GuideOutcome::GuidedAndGranted);
        assert_eq!(first.exit_code(), 0);
        assert_eq!(desktop.probes, 2);
        assert_eq!(desktop.opened.len(), 1);
        assert!(
            wizard.record["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "file_access")
        );
        // Re-running setup does not repeatedly interrupt once guided.
        let second = run_setup_file_access_guide_with(
            &mut wizard,
            "hub",
            &hub_home,
            false,
            true,
            &mut desktop,
        )
        .unwrap();
        assert_eq!(second, GuideOutcome::Ready);
        assert_eq!(desktop.probes, 2, "must not re-prompt after one-time guide");

        // 2. Headless macOS setup (SSH / no desktop) -> non-blocking CLI guidance, exit code 2, no popup.
        let node_home = temp.path().join("node-headless");
        let node_exe = tcc_executable_path("node", &node_home)
            .to_string_lossy()
            .into_owned();
        let mut input2 = Answers(vec![]);
        let mut wizard2 =
            Wizard::claim(&source, &node_home, "node", &meta("1.0.0"), &mut input2).unwrap();
        let mut headless = FakeGuide {
            session: SessionKind::Headless,
            reports: VecDeque::from([FileAccessReport {
                status: FileAccessStatus::Missing,
                full_disk_access: FileAccessStatus::Missing,
                locations: vec![ProtectedAccess {
                    location: ProtectedLocation::Documents,
                    status: FileAccessStatus::Missing,
                    path: Some("/Users/example/Documents".into()),
                }],
                executable_path: Some(node_exe.clone()),
            }]),
            confirms: VecDeque::new(),
            probes: 0,
            opened: Vec::new(),
            done: Vec::new(),
            info: Vec::new(),
        };
        let outcome = run_setup_file_access_guide_with(
            &mut wizard2,
            "node",
            &node_home,
            false,
            true,
            &mut headless,
        )
        .unwrap();
        assert_eq!(outcome, GuideOutcome::MissingActionRequired);
        assert_eq!(outcome.exit_code(), EXIT_ACCESS_MISSING);
        assert_eq!(headless.probes, 1);
        assert!(headless.opened.is_empty());
        assert!(
            headless
                .info
                .iter()
                .any(|line| line.contains(&node_exe) && line.contains("steve node file-access")),
            "expected headless guidance with executable path and command: {:?}",
            headless.info
        );

        // 3. Non-macOS setup -> NotApplicable without running probes.
        let linux_home = temp.path().join("hub-linux");
        let mut input3 = Answers(vec![]);
        let mut wizard3 =
            Wizard::claim(&source, &linux_home, "hub", &meta("1.0.0"), &mut input3).unwrap();
        let mut non_mac = FakeGuide {
            session: SessionKind::Headless,
            reports: VecDeque::new(),
            confirms: VecDeque::new(),
            probes: 0,
            opened: Vec::new(),
            done: Vec::new(),
            info: Vec::new(),
        };
        let linux_outcome = run_setup_file_access_guide_with(
            &mut wizard3,
            "hub",
            &linux_home,
            false,
            false,
            &mut non_mac,
        )
        .unwrap();
        assert_eq!(linux_outcome, GuideOutcome::NotApplicable);
        assert_eq!(non_mac.probes, 0);
    }
}
