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
    fn multiselect(&mut self, title: &str, choices: &[Choice], ticked: &[bool]) -> Result<Vec<bool>> {
        typed_multiselect(self, title, choices, ticked)
    }
    fn confirm(&mut self, title: &str, initial: bool) -> Result<bool> {
        typed_confirm(self, title, initial)
    }
}

fn typed_select<P: Prompts + ?Sized>(input: &mut P, title: &str, choices: &[Choice], initial: usize) -> Result<usize> {
    let keys: Vec<&str> = choices.iter().map(|c| c.key.as_str()).collect();
    let title = format!("{title}（{}）", keys.join(" / "));
    loop {
        let answer = input.line(&title, Some(keys[initial]), false)?.trim().to_lowercase();
        match keys.iter().position(|k| *k == answer) {
            Some(at) => return Ok(at),
            None if input.interactive() => ui::warn(&format!("请选择 {}", keys.join(" / "))),
            None => bail!("请选择 {}", keys.join(" / ")),
        }
    }
}

/// `all`, `none`, or keys separated by commas, as ticks over `choices`.
fn ticks(answer: &str, choices: &[Choice]) -> Result<Vec<bool>> {
    let answer = answer.trim().to_lowercase();
    if matches!(answer.as_str(), "all" | "none") {
        return Ok(vec![answer == "all"; choices.len()]);
    }
    let wanted: Vec<&str> = answer.split(',').map(str::trim).filter(|k| !k.is_empty()).collect();
    if let Some(unknown) = wanted.iter().find(|k| !choices.iter().any(|c| c.key == **k)) {
        let keys: Vec<&str> = choices.iter().map(|c| c.key.as_str()).collect();
        bail!("没有 {unknown}；可选：{}、all、none", keys.join("、"));
    }
    Ok(choices.iter().map(|c| wanted.contains(&c.key.as_str())).collect())
}

fn typed_multiselect<P: Prompts + ?Sized>(input: &mut P, title: &str, choices: &[Choice], ticked: &[bool]) -> Result<Vec<bool>> {
    let default: Vec<&str> = choices
        .iter()
        .zip(ticked)
        .filter(|(_, on)| **on)
        .map(|(c, _)| c.key.as_str())
        .collect();
    let default = if default.is_empty() { "none".to_owned() } else { default.join(",") };
    let title = format!("{title}（逗号分隔，all 或 none）");
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
        let answer = input.line(&format!("{title} [y/n]"), Some(if initial { "y" } else { "n" }), false)?;
        match answer.trim().to_lowercase().as_str() {
            "y" | "yes" | "是" => return Ok(true),
            "n" | "no" | "否" => return Ok(false),
            _ if input.interactive() => ui::warn("请回答 y 或 n"),
            _ => bail!("请回答 y 或 n"),
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
        if ui::fancy() { ui::select(title, choices, initial) } else { typed_select(self, title, choices, initial) }
    }

    fn multiselect(&mut self, title: &str, choices: &[Choice], ticked: &[bool]) -> Result<Vec<bool>> {
        if ui::fancy() { ui::multiselect(title, choices, ticked) } else { typed_multiselect(self, title, choices, ticked) }
    }

    fn confirm(&mut self, title: &str, initial: bool) -> Result<bool> {
        if ui::fancy() { ui::confirm(title, initial) } else { typed_confirm(self, title, initial) }
    }

    fn line(&mut self, title: &str, default: Option<&str>, optional: bool) -> Result<String> {
        while ui::fancy() {
            let answer = ui::input(title, default)?;
            if (!optional && answer.is_empty()) || answer.contains(['\n', '\r', '\0']) {
                ui::warn("需要填写有效值，请重新输入。");
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
                bail!("初始化已中断；已完成步骤保留，可稍后继续。");
            }
            let answer = line.trim();
            let answer = if answer.is_empty() {
                default.unwrap_or("")
            } else {
                answer
            };
            if (!optional && answer.is_empty()) || answer.contains(['\n', '\r', '\0']) {
                if self.interactive() {
                    eprintln!("需要填写有效值，请重新输入当前字段。");
                    continue;
                }
                bail!("需要填写有效值；已完成的步骤保留。");
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
                ui::warn(&format!("{error}；请重新输入当前字段。"));
            }
            Err(error) => return Err(error),
        }
    }
}

fn listen_address(value: &str) -> Result<SocketAddr> {
    let address: SocketAddr = value
        .trim()
        .parse()
        .context("监听地址应为 IP:端口，例如 0.0.0.0:8440 或 [::1]:8440")?;
    if address.port() == 0 {
        bail!("服务监听端口不能是 0");
    }
    Ok(address)
}

/// `tailcat:<address>:<port>`, a Hub reached over tailcat (steve_node_wire::tailcat checks it fully).
fn tailcat_hub(value: &str) -> Option<String> {
    let (addr, port) = value.trim().strip_prefix("tailcat:")?.rsplit_once(':')?;
    let ok = addr.starts_with("tc")
        && addr.len() >= 8
        && addr.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && port.parse::<u16>().is_ok_and(|p| p != 0);
    ok.then(|| format!("tailcat:{addr}:{port}"))
}

fn node_hub_url(value: &str) -> Result<String> {
    if let Some(hub) = tailcat_hub(value) {
        return Ok(hub);
    }
    let hub = browser_url(value, "Hub")?;
    if !hub.starts_with("https://") {
        bail!("Node 配对需要 HTTPS Hub");
    }
    Ok(hub)
}

fn setup_role(value: &str, options: &Options) -> Result<String> {
    let role = value.trim().to_lowercase();
    if !matches!(role.as_str(), "hub" | "node") {
        bail!("请选择 hub 或 node");
    }
    if role == "hub" && (options.hub.is_some() || options.pin.is_some()) {
        bail!("--hub/--pin 只用于 Node，请选择 node");
    }
    if role == "node" && (options.listen.is_some() || options.public_url.is_some()) {
        bail!("--listen/--public-url 只用于 Hub，请选择 hub");
    }
    Ok(role)
}

/// Reject invalid explicit arguments before prompting, inspecting a bundle or creating a home.
fn validate_options(options: &Options) -> Result<()> {
    if let Some(role) = &options.role {
        setup_role(role, options)?;
    }
    if let Some(listen) = &options.listen {
        listen_address(listen)?;
        if options.hub.is_some() || options.pin.is_some() {
            bail!("--listen 只用于 Hub，不能与 Node 的 --hub/--pin 一起使用");
        }
    }
    if let Some(public) = &options.public_url {
        browser_url(public, "访问地址")?;
        if options.hub.is_some() || options.pin.is_some() {
            bail!("--public-url 只用于 Hub，不能与 Node 的 --hub/--pin 一起使用");
        }
    }
    if let Some(hub) = &options.hub {
        node_hub_url(hub)?;
    }
    if let Some(label) = &options.label {
        anyhow::ensure!(crate::signing::valid_identity(label), "--label 只能包含字母、数字和 . _ -");
    }
    // one pin, or several comma-separated while the Hub's certificate is being changed
    if options.pin.as_ref().is_some_and(|v| {
        v.split(',').any(|p| {
            !p.starts_with("sha256:") || p.len() != 71 || !p[7..].bytes().all(|b| b.is_ascii_hexdigit())
        })
    }) {
        bail!("--pin 需要 sha256: 加 64 位证书指纹（多个用逗号分隔）");
    }
    Ok(())
}

fn modified_setup_file(name: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "初始化文件已被修改：{name}；setup 只续接未完成的初始化，不会覆盖现有配置。请保留当前文件、setup.json 和身份密钥；先核对修改，并用对应角色的 status/doctor 确认安装进度。只有原服务已注册且配置核验通过时才显式重启；重启不能补完初始化。不要删除收据或重置数据目录来绕过检查。"
    )
}

/// Where setup keeps the owner's first access token (`user=…`, `token=…`; mode 0600).
const OWNER_TOKEN: &str = "secrets/owner.token";

fn completion_lines(
    role: &str,
    home: &Path,
    environment: &Value,
    paired: bool,
) -> Result<Vec<String>> {
    let quoted_home = shell_quote(home.to_str().context("home 路径必须为 UTF-8")?)?;
    let mut lines = vec![format!(
        "{role} 已配置并注册服务；本次 setup 未启动或重启服务。数据目录：{}",
        home.display()
    )];
    if role == "hub" {
        let host = environment["STEVE_HOST"]
            .as_str()
            .context("原监听地址缺失")?
            .parse()?;
        let port = environment["STEVE_PORT"]
            .as_str()
            .context("原监听端口缺失")?
            .parse()?;
        lines.push(format!("监听地址：{}", SocketAddr::new(host, port)));
        if let Some(web) = environment["STEVE_WEB_PORT"].as_str() {
            lines.push(format!(
                "网页端口：{web}。第一次打开网页时，输入 Hub 地址 http://<这台机器的地址>:{port}"
            ));
        }
        lines.push(format!(
            "浏览器地址（配置值，尚未验证可达）：{}",
            environment["STEVE_PUBLIC_URL"].as_str().unwrap_or("")
        ));
    } else if paired {
        lines.push(
            "Node 配对信息已保存；请先在 Hub Devices 核对并确认完整指纹，再启动。初始 roots 为空。"
                .into(),
        );
    } else {
        lines.push("Node 尚未配对，初始 roots 为空。请先准备可达的 HTTPS Hub，再继续配对：".into());
        lines.push(format!("steve setup node --home {quoted_home}"));
    }
    lines.push(if role == "node" {
        "完成配对及 Hub 确认后启动：".into()
    } else {
        "启动服务：".into()
    });
    lines.push(format!("steve {role} start --home {quoted_home}"));
    lines.push("查看本机服务状态与安装检查：".into());
    lines.push(format!("steve {role} status --home {quoted_home}"));
    lines.push(format!("steve {role} doctor --home {quoted_home}"));
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
                lines.push(format!("登录用户名：{}", field("user")));
                lines.push(format!("访问 token：{}", field("token")));
                lines.push(format!(
                    "（token 已保存在 {}，权限 0600；需要更多 token 时运行 steve-hub --access token create）",
                    home.join(OWNER_TOKEN).display()
                ));
            }
            Err(_) => lines.push(
                "没有找到 owner token；在 Hub 所在机器上运行 steve-hub --access token create 签发。".into(),
            ),
        }
        lines.push("下一步：启动后在上述浏览器地址用用户名和 token 登录，并用该 Hub 的 /healthz 核验运行版本。服务启动和本机安装检查不代表登录已通过。".into());
    } else {
        lines.push("下一步：在 Hub Devices 核验同一设备已连接；本机进程运行不代表已连接，也不代表可以执行任务。".into());
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
    let url = url::Url::parse(value).with_context(|| format!("{label}需要完整 HTTP(S) URL"))?;
    let host = url.host_str().context("URL 缺少主机")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("{label}不能包含账号、查询参数、片段或空白");
    }
    if matches!(host, "0.0.0.0" | "::" | "[::]") {
        bail!("0.0.0.0 / :: 是监听地址；{label}请填写浏览器实际可达的主机地址。");
    }
    if url.scheme() != "https" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") {
        bail!("{label}跨机器访问需要 HTTPS");
    }
    Ok(value.into())
}

fn manifest(root: &Path) -> Result<Value> {
    let value = read_json(&root.join("release.json"))?;
    let version = value["version"].as_str().context("缺少发行版本")?;
    let parsed = semver::Version::parse(version)?;
    let commit = value["commit"].as_str().context("缺少发行提交")?;
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
        bail!("安装包身份无效；请使用可信的正式 Release。");
    }
    Ok(value)
}

/// The ACP adapters of a Hub or Node at `home` (`steve_acp::managed`), through the release in
/// `release`: `status`, `install [<id>…]` or `upgrade`. Their output goes straight to the terminal.
pub fn adapters(role: &str, home: &Path, release: &Path, args: &[&str]) -> Result<i32> {
    let status = adapter_command(role, home, release, args)
        .status()
        .with_context(|| format!("运行失败：{}", release.display()))?;
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
            command.env(key, value.as_str().context("环境值必须为字符串")?);
        }
    }
    let output = command
        .output()
        .with_context(|| format!("运行失败：{}", program.display()))?;
    if !output.status.success() {
        bail!(
            "命令失败（{}）；已完成步骤保留。请运行 doctor 查看配置。",
            output.status
        );
    }
    serde_json::from_slice(&output.stdout).context("命令没有返回有效 JSON")
}

fn verify_bundle(root: &Path, expected: &Value, home: &Path) -> Result<()> {
    regular_tree(root)?;
    let disk = manifest(root)?;
    let web = read_json(&root.join("web/dist/release.json"))?;
    for value in [&disk, &web] {
        for key in ["version", "commit"] {
            if value[key] != expected[key] {
                bail!("安装包与 Web 身份不一致");
            }
        }
    }
    if disk["hub_version"] != disk["version"] || disk["web_version"] != disk["version"] {
        bail!("安装包版本不一致");
    }
    for role in ["hub", "node"] {
        let path = root.join(format!("bin/steve-{role}"));
        if fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
            bail!("安装包缺少可执行 {role}");
        }
        let info = captured(&path, home, &["--build-info"], None)?;
        if info["version"] != disk["version"]
            || info["commit"] != disk["commit"]
            || info["target"] != env!("STEVE_BUILD_TARGET")
            || (role == "hub" && info["schema"] != disk["schema"]["target"])
        {
            bail!("{role} 的版本、提交、平台或 schema 与安装包不一致");
        }
    }
    let html = fs::read_to_string(root.join("web/dist/index.html"))?;
    let marker = regex::Regex::new(&format!(
        r#"<meta\s+name=["']steve-web-version["']\s+content=["']{}["']"#,
        regex::escape(disk["version"].as_str().unwrap())
    ))?;
    if !marker.is_match(&html) {
        bail!("Web 页面版本不一致");
    }
    if cfg!(target_os = "macos") && !root.join("bin/steve-keyhelper").is_file() {
        bail!("安装包缺少 keyhelper");
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
            bail!("数据目录与只读安装包必须分开");
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
        owned(&home.join("setup.json"), false).context("目录没有本次初始化收据；不会接管旧数据")?;
        let before_lock = read_json(&home.join("setup.json"))?;
        if before_lock["format"] != 1
            || before_lock["component"] != role
            || before_lock["home"].as_str() != home.to_str()
        {
            bail!("此目录不是本次初始化拥有的目录");
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
        lock.try_lock().context("此目录已有初始化正在进行")?;
        owned(&lock_path, false)?;
        owned(&home.join("setup.json"), false).context("目录没有本次初始化收据；不会接管旧数据")?;
        let record = read_json(&home.join("setup.json"))?;
        if record["format"] != 1
            || record["component"] != role
            || record["home"].as_str() != home.to_str()
            || !record["files"].is_object()
            || !record["steps"].is_array()
        {
            bail!("此目录不是本次初始化拥有的目录");
        }
        let old_version = record["version"].as_str().context("初始化版本缺失")?;
        let parsed = semver::Version::parse(old_version)?;
        let old_commit = record["commit"].as_str().context("初始化提交缺失")?;
        if parsed.to_string() != old_version
            || !parsed.pre.is_empty()
            || !parsed.build.is_empty()
            || old_commit.len() != 40
            || !old_commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!("初始化收据身份无效");
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
                bail!("收据包含无效路径");
            }
            let path = home.join(relative);
            if exists(&path) {
                owned(&path, false)?;
                if digest(&path)? != value.as_str().context("收据摘要无效")? {
                    return Err(modified_setup_file(name));
                }
            }
        }
        if wizard.record["version"] != expected["version"]
            || wizard.record["commit"] != expected["commit"]
        {
            // A user may have interrupted the previous setup at its first prompt before upgrading
            // the package. Only that unsigned, unpaired, unregistered setup may change versions.
            if !wizard.record["steps"].as_array().unwrap().is_empty()
                || wizard.record["keychain_started"] == true
                || wizard.record["pair_started"] == true
                || exists(&home.join("current"))
                || exists(&home.join("signing/identity.json"))
                || exists(&home.join("node/identity.key"))
                || exists(&home.join("data/steve.sqlite3"))
                || exists(&home.join("services/hub.json"))
                || exists(&home.join("services/node.json"))
            {
                bail!("已有初始化或身份属于另一版本；不会覆盖，原数据已保留");
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
                bail!("已有文件不属于本次初始化，不会覆盖：{}", path.display());
            }
            let bytes = data.context("初始化文件缺失")?;
            self.record["files"][name] = bytes_digest(bytes).into();
            self.save()?;
        }
        let expected = self.record["files"][name]
            .as_str()
            .context("初始化摘要无效")?;
        if exists(&path) {
            owned(&path, false)?;
            if digest(&path)? != expected {
                return Err(modified_setup_file(name));
            }
        } else {
            let bytes = data.context("初始化中断在写文件前，需要原输入才能继续")?;
            if bytes_digest(bytes) != expected {
                bail!("原初始化输入发生变化，不会覆盖收据");
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
        let candidate = self
            .home
            .join("releases")
            .join(self.record["version"].as_str().context("版本缺失")?);
        if !exists(&candidate) {
            return Ok(());
        }
        if fs::symlink_metadata(&candidate)?.file_type().is_symlink() {
            bail!("版本目录不能是链接");
        }
        let expected = self.record["bundle_files"]
            .as_object()
            .context("复制清单缺失")?;
        let files = regular_tree(&candidate)?;
        if files.len() != expected.len() {
            bail!("初始化版本目录的文件清单已改变");
        }
        for path in files {
            let name = path
                .strip_prefix(&candidate)?
                .to_str()
                .context("文件名必须为 UTF-8")?;
            let actual = digest(&path)?;
            let wanted = if name == "bin/steve-hub" && self.record["keychain_started"] == true {
                self.record["signed_hub_sha256"]
                    .as_str()
                    .or_else(|| expected.get(name).and_then(Value::as_str))
            } else {
                expected.get(name).and_then(Value::as_str)
            };
            if wanted != Some(actual.as_str()) {
                bail!("初始化版本文件已改变：{name}；不会自动重新签名");
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
                bail!("已有版本目录不属于本次初始化");
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
                    .context("文件名无效")?
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
                    bail!("安装包文件已改变");
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
                    bail!("安装包复制校验失败：{name}");
                }
            }
            verify_bundle(&staging, m, &self.home)?;
            if exists(&candidate) {
                bail!("版本目录在复制过程中出现，不会覆盖");
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
                let saved_ip: std::net::IpAddr = env["STEVE_HOST"]
                    .as_str()
                    .context("原监听地址缺失")?
                    .parse()?;
                let saved_port: u16 = env["STEVE_PORT"]
                    .as_str()
                    .context("原监听端口缺失")?
                    .parse()?;
                if selected != SocketAddr::new(saved_ip, saved_port) {
                    bail!("已有初始化配置不同；不会自动覆盖监听地址");
                }
            }
            if let Some(public) = public {
                if env["STEVE_PUBLIC_URL"].as_str() != Some(browser_url(public, "访问地址")?.as_str()) {
                    bail!("已有初始化配置不同；不会自动覆盖浏览器访问地址");
                }
            }
            self.owned_file("steve.env", Some(&env_bytes(env.as_object().unwrap())?))?;
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
            ask_value(input, "监听地址", Some("0.0.0.0:8440"), false, listen_address)
        };
        let bind = match listen {
            Some(value) => listen_address(value)?,
            None if self.input.rich() => {
                let choices = [
                    Choice::new("lan", "本机 + 局域网", "监听 0.0.0.0:8440，其他设备和 Node 可以连进来"),
                    Choice::new("local", "仅本机", "监听 127.0.0.1:8440，只有这台机器能访问"),
                    Choice::new("custom", "自定义", "自己填监听地址和端口"),
                ];
                match self.input.select("谁可以连接这台 Hub？", &choices, 0)? {
                    0 => SocketAddr::from(([0, 0, 0, 0], 8440)),
                    1 => SocketAddr::from(([127, 0, 0, 1], 8440)),
                    _ => custom(&mut *self.input)?,
                }
            }
            None => custom(&mut *self.input)?,
        };
        /* the web client has a port of its own (A3): the Hub's port answers the API only */
        let web_port = bind.port().checked_add(1).context("监听端口太大，网页没有可用的下一个端口")?;
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
            (Some(value), _) => browser_url(value, "访问地址")?,
            (None, Some(url)) => url,
            (None, None) => {
                ui::info("这个监听地址推不出网页地址：填你在浏览器里打开 Steve 用的 HTTPS 地址。");
                ask_value(self.input, "浏览器访问地址", None, false, |value| {
                    browser_url(value, "访问地址")
                })?
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
                        .line("Vault 密钥目录（数据目录之外，权限 0700）", None, false)?
                }
            };
            let keys = canonical(Path::new(&selected))?;
            if keys.starts_with(&self.home) || self.home.starts_with(&keys) {
                bail!("Vault 密钥目录不能与数据目录互相包含");
            }
            if exists(&keys) {
                owned(&keys, true)?;
                if self.record["key_directory"].is_null() && fs::read_dir(&keys)?.next().is_some() {
                    bail!("密钥目录不是空目录；不会接管旧密钥");
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

pub fn reconfigure_existing(options: Options) -> Result<()> {
    validate_options(&options)?;
    reconfigure::run(&options, &mut Terminal)
}

/// `bin/steve-hub --adapters …` or `bin/steve-node adapters …` of `release`, for the home.
fn adapter_command(role: &str, home: &Path, release: &Path, args: &[&str]) -> std::process::Command {
    let (program, first) = if role == "hub" {
        (release.join("bin/steve-hub"), "--adapters")
    } else {
        (release.join("bin/steve-node"), "adapters")
    };
    let mut command = clean_command(&program, Some(home));
    command.arg(first).args(args);
    command
}

/// What the release can install (`adapters list --json`): whether npm is there, whether this home
/// already has a choice, and each built-in adapter.
fn adapter_listing(role: &str, home: &Path, release: &Path) -> Option<Value> {
    let output = adapter_command(role, home, release, &["list", "--json"]).output().ok()?;
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
    if let Some(saved) = wizard.record["adapters"].as_array() {
        return Ok(Some(saved.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()));
    }
    let Some(listing) = adapter_listing(role, &wizard.home, release) else {
        anyhow::ensure!(flag.is_none(), "这个安装包不支持 --adapters");
        return Ok(None);
    };
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let choices: Vec<Choice> = listing["adapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| Choice::new(&text(a, "id"), &text(a, "name"), &text(a, "package")))
        .collect();
    let on = match flag {
        Some(flag) => ticks(flag, &choices).context("--adapters")?,
        /* without npm nothing can be installed now: the install step says what to do */
        None if listing["npm"] != true || listing["chosen"] == true => return Ok(None),
        None if !wizard.input.rich() => return Ok(None),
        None => wizard.input.multiselect("安装哪些 ACP 适配器？", &choices, &vec![true; choices.len()])?,
    };
    let chosen: Vec<String> = choices.iter().zip(on).filter(|(_, on)| *on).map(|(c, _)| c.key.clone()).collect();
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
    let later = "ACP 适配器暂未安装；装好 Node.js 后运行 steve adapters install。";
    let output = adapter_command(role, home, release, &args)
        .output()
        .map_err(|_| later.to_owned())?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(later);
        return Err(last.trim().to_owned());
    }
    let installed: Vec<String> = adapter_listing(role, home, release)
        .and_then(|l| l["adapters"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|a| Some(format!("{} {}", a["name"].as_str()?, a["installed"].as_str()?)))
        .collect();
    Ok(installed.join(" · "))
}

/// Where a Hub with this environment answers on its own machine, and where its web client is.
fn hub_addresses(value: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let ip: std::net::IpAddr = value("STEVE_HOST")?.parse().ok()?;
    let port: u16 = value("STEVE_PORT")?.parse().ok()?;
    let ip = match ip {
        ip if !ip.is_unspecified() => ip,
        std::net::IpAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
        std::net::IpAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
    };
    Some((format!("http://{}", SocketAddr::new(ip, port)), value("STEVE_PUBLIC_URL")?))
}

/// The same for the Hub installed in `home` (its `steve.env`): (the Hub, the web client).
pub fn hub_urls(home: &Path) -> Option<(String, String)> {
    let document = crate::env_file::EnvDocument::parse(&fs::read(home.join("steve.env")).ok()?).ok()?;
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
    let name = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The end of a Hub's setup on a terminal: what to sign in with, then start it and open the page.
fn finish_hub(
    input: &mut dyn Prompts,
    service: &mut crate::service::Service,
    home: &Path,
    environment: &Value,
    flags: &str,
) -> Result<()> {
    let (api, web) = hub_addresses(|key| environment[key].as_str().map(str::to_owned))
        .context("原监听地址缺失")?;
    let saved = fs::read_to_string(home.join(OWNER_TOKEN)).unwrap_or_default();
    let field = |key: &str| {
        saved
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or("")
            .to_string()
    };
    ui::gap();
    ui::heading("安装完成");
    ui::note(&[
        ("网页", web.clone()),
        ("Hub 地址", api.clone()),
        ("用户名", field("user")),
        ("Token", field("token")),
        ("数据目录", home.display().to_string()),
    ]);
    ui::info(&format!("Token 也保存在 {}", home.join(OWNER_TOKEN).display()));
    ui::gap();
    let how = "第一次打开网页：先填 Hub 地址，再用上面的用户名和 Token 登录。";
    if !input.confirm("现在启动 Hub？", true)? {
        ui::outro(&format!("准备好后运行 steve hub start{flags}。{how}"));
        return Ok(());
    }
    let spinner = ui::Spinner::start("正在启动 Hub");
    if let Err(error) = service.dispatch("start", None, 30) {
        spinner.fail("Hub 没有启动");
        return Err(error);
    }
    match hub_ready(&api, Duration::from_secs(45)) {
        Some(health) => spinner.done(
            "Hub 已启动",
            &format!("v{} · {api}", health["version"].as_str().unwrap_or("?")),
        ),
        None => {
            spinner.warn("Hub 已交给服务管理器，但还没有应答。");
            ui::outro(&format!("用 steve hub status{flags} 和 steve hub doctor{flags} 查看原因。"));
            return Ok(());
        }
    }
    if let Some(open) = opener() {
        if input.confirm("在浏览器里打开？", true)? {
            let _ = std::process::Command::new(open)
                .arg(&web)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    ui::outro(how);
    Ok(())
}

pub fn run(source: &Path, options: Options) -> Result<()> {
    if options.reconfigure {
        return reconfigure_existing(options);
    }
    validate_options(&options)?;
    let mut input = Terminal;
    let rich = input.rich();
    let expected = manifest(source)?;
    let version = expected["version"].as_str().unwrap_or_default().to_owned();
    ui::banner(&format!("自托管的 agent 协作中心 · v{version}"));
    ui::intro("安装向导");
    let role = match options.role.as_deref() {
        Some(value) => setup_role(value, &options)?,
        /* an option that belongs to one role has chosen it */
        None if rich && (options.hub.is_some() || options.pin.is_some()) => "node".into(),
        None if rich && (options.listen.is_some() || options.public_url.is_some()) => "hub".into(),
        None if rich => {
            let choices = [
                Choice::new("hub", "Hub", "协作中心：网页、任务和数据都在这台机器上"),
                Choice::new("node", "Node", "执行节点：把这台机器接入一台已有的 Hub"),
            ];
            choices[input.select("安装什么？", &choices, 0)?].key.clone()
        }
        None => ask_value(
            &mut input,
            "安装角色（hub / node）",
            Some("hub"),
            false,
            |value| setup_role(value, &options),
        )?,
    };
    let default_home = options.home.is_none();
    let home = canonical(&options.home.clone().unwrap_or(user_home()?.join(if role == "hub" {
        ".steve"
    } else {
        ".steve-node"
    })))?;
    /* how the commands setup names reach this installation */
    let mut flags = String::new();
    if !default_home {
        flags.push_str(&format!(" --home {}", shell_quote(home.to_str().context("home 路径必须为 UTF-8")?)?));
    }
    if let Some(label) = &options.label {
        flags.push_str(&format!(" --label {}", shell_quote(label)?));
    }
    ui::step(
        "正在校验安装包",
        || verify_bundle(source, &expected, &home),
        |_| {
            let commit: String = expected["commit"].as_str().unwrap_or_default().chars().take(8).collect();
            ("安装包已校验".into(), format!("{version} · {commit}"))
        },
    )?;
    let mut wizard = Wizard::claim(source, &home, &role, &expected, &mut input)?;
    if wizard.record["copy_planned"] == true || wizard.record["environment"].is_object() {
        ui::info("接着上次没有完成的安装继续。");
    }
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
    let pairing = home.join("node/node.json");
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
                    "Hub HTTPS URL 或 tailcat: 地址（留空稍后配对）",
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
    let chosen = adapter_choice(&mut wizard, &role, source, options.adapters.as_deref())?;
    let candidate = ui::step(
        "正在准备运行目录",
        || wizard.copy_bundle(&expected),
        |candidate| ("运行目录已就绪".into(), candidate.display().to_string()),
    )?;
    if role == "hub" {
        if cfg!(target_os = "macos") {
            if !wizard.record["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "keychain")
            {
                wizard.record["keychain_started"] = true.into();
                wizard.save()?;
                let label = options.label.as_deref().unwrap_or("io.github.infranite.steve.hub");
                ui::step(
                    "正在创建签名身份并签名 Hub",
                    || crate::signing::setup_keychain(&home, &candidate, label),
                    |_| ("Hub 已签名".into(), "本机专用的签名身份".into()),
                )?;
                wizard.record["signed_hub_sha256"] =
                    digest(&candidate.join("bin/steve-hub"))?.into();
                wizard.record["helper_sha256"] = digest(&home.join("bin/steve-keyhelper"))?.into();
                wizard.record["identity_sha256"] =
                    digest(&home.join("signing/identity.json"))?.into();
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
                    != wizard.record[key].as_str().context("签名摘要缺失")?
                {
                    bail!("原签名身份或 keyhelper 已改变，不会替换");
                }
            }
        }
    } else if exists(&pairing) {
        if wizard.record["pair_started"] != true {
            bail!("已有配对不属于本次初始化");
        }
        let paired = read_json(&pairing)?;
        let requested_hub = options
            .hub
            .as_ref()
            .map(|v| node_hub_url(v))
            .transpose()?;
        if paired["hub_url"] != wizard.record["hub_url"]
            || requested_hub
                .as_ref()
                .is_some_and(|v| paired["hub_url"] != v.as_str())
            || options
                .pin
                .as_ref()
                .is_some_and(|v| paired["pin"] != v.to_lowercase())
        {
            bail!("已有配对指向另一个 Hub");
        }
        let status = clean_command(&candidate.join("bin/steve-node"), Some(&home))
            .arg("status")
            .status()?;
        if !status.success() {
            bail!("Node 配对状态无法确认");
        }
    } else if !node_hub.is_empty() {
        let hub = node_hub.clone();
        if wizard.record["pair_started"] != true && exists(&home.join("node/identity.key")) {
            bail!("已有 Node 身份不属于本次初始化");
        }
        if wizard.record["pair_started"] == true && wizard.record["hub_url"] != hub {
            bail!("配对曾向另一个 Hub 发出，不会自动更换身份");
        }
        let pin = options
            .pin
            .as_ref()
            .map(|p| p.to_lowercase())
            .or_else(|| wizard.record["pin"].as_str().map(str::to_string));
        wizard.record["pair_started"] = true.into();
        wizard.record["hub_url"] = hub.clone().into();
        wizard.record["pin"] = json!(pin);
        wizard.save()?;
        ui::info("输入 Hub Devices 页生成的一次性配对码，随后仍需在 Hub 核对并确认完整指纹。");
        let mut command = clean_command(&candidate.join("bin/steve-node"), Some(&home));
        command.args(["pair", &hub]);
        if let Some(pin) = pin {
            command.args(["--pin", &pin]);
        }
        if !command.status()?.success() {
            bail!("Node 配对尚未完成，原进度已保留");
        }
        if read_json(&pairing)?["hub_url"] != hub {
            bail!("Node 配对回读不一致");
        }
        ui::done("Node 已配对", &hub);
    }
    /* the ACP adapters are Steve's dependencies: the chosen ones are installed now and kept up to
     * date by the Node; without npm, setup goes on and says how to add them. Before the
     * configuration check, so that it does not report them missing. */
    if chosen.as_ref().is_some_and(|c| c.is_empty()) {
        let _ = install_adapters(&role, &home, &candidate, chosen.as_deref());
        ui::done("不安装 ACP 适配器", "之后可以用 steve adapters install <id> 添加");
    } else {
        let spinner = ui::Spinner::start("正在安装 ACP 适配器");
        match install_adapters(&role, &home, &candidate, chosen.as_deref()) {
            Ok(installed) => spinner.done("ACP 适配器已安装", &installed),
            Err(reason) => spinner.warn(&reason),
        }
    }
    if let Some(env) = &env {
        let check = ui::step(
            "正在检查配置",
            || {
                let check = captured(
                    &candidate.join("bin/steve-hub"),
                    &home,
                    &["--check-config"],
                    Some(env),
                )?;
                if check["ok"] != true {
                    bail!("Hub 配置检查失败，请用 steve hub doctor 检查");
                }
                Ok(check)
            },
            |_| ("配置检查通过".into(), String::new()),
        )?;
        if let Some(warnings) = check["warnings"].as_array() {
            for warning in warnings {
                ui::info(warning.as_str().unwrap_or("配置警告"));
            }
        }
        /* the owner's first access token, once: the Hub makes this account's user its owner */
        if wizard.record["files"].get(OWNER_TOKEN).is_none() {
            let user = std::env::var("USER")
                .ok()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| "owner".into());
            let issued = captured(
                &candidate.join("bin/steve-hub"),
                &home,
                &["--access", "token", "create", "--user", &user, "--name", "setup", "--json"],
                Some(env),
            )?;
            let token = issued["token"].as_str().context("没有拿到 owner token")?;
            let body = format!("user={}\ntoken={token}\n", issued["user"].as_str().unwrap_or(&user));
            wizard.owned_file(OWNER_TOKEN, Some(body.as_bytes()))?;
            if rich {
                ui::done("访问 token 已签发", issued["user"].as_str().unwrap_or(&user));
            }
        }
    }
    let mut service = crate::service::Service::new(&role, &home, options.label.as_deref())?;
    ui::step(
        &format!("正在注册 {role} 服务"),
        || service.dispatch("install", Some(&candidate), 30),
        |report| {
            ("服务已注册".into(), report["label"].as_str().unwrap_or_default().to_owned())
        },
    )?;
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
    if let (true, Some(env)) = (rich, &env) {
        return finish_hub(&mut *wizard.input, &mut service, &home, env, &flags);
    }
    ui::gap();
    for line in completion_lines(
        &role,
        &home,
        &wizard.record["environment"],
        home.join("node/node.json").is_file(),
    )? {
        ui::info(&line);
    }
    ui::outro("");
    Ok(())
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
        for bad in ["tailcat:tcAbC_-123456", "tailcat:tc;rm -rf /:1", "tailcat:xyz12345678:1"] {
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
                "--listen 只用于 Hub",
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
            let lines = completion_lines("hub", home, &env, false).unwrap();
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
            assert!(lines.iter().any(|line| line.contains("用用户名和 token 登录")));
        }
        let lines = completion_lines("node", home, &Value::Null, false)
            .unwrap()
            .join("\n");
        assert!(lines.contains("Node 尚未配对"));
        assert!(lines.contains("steve setup node --home"));
        assert!(!lines.contains("完整指纹"));
        let lines = completion_lines("node", home, &Value::Null, true)
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
        let mut answers = vec!["https://hub.example/".into()];
        if cfg!(target_os = "linux") {
            answers.push(keys.to_str().unwrap().into());
        }
        let mut input = Answers(answers);
        let mut wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
        let env = wizard.hub_config(Some("0.0.0.0:8440")).unwrap();
        assert_eq!(env["STEVE_HOST"], "0.0.0.0");
        assert_eq!(env["STEVE_ALLOW_ANY_HOST"], "1");
        assert_eq!(env["STEVE_PUBLIC_URL"], "https://hub.example");
        assert_eq!(env["STEVE_WEB_PORT"], "8441", "the web client on a port of its own");
        assert!(env.get("STEVE_OIDC_ISSUER").is_none());
        let bytes = fs::read_to_string(home.join("steve.env")).unwrap();
        assert!(!bytes.contains("OIDC"));
        assert!(!home.join("secrets/oidc-client-secret").exists());
        // resumed: the same environment, nothing asked again
        drop(wizard);
        let mut input = Answers(vec![]);
        let mut wizard = Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
        assert_eq!(wizard.hub_config(None).unwrap(), env);
    }

    #[test]
    fn completion_shows_the_owner_token() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        private_dir(&home.join("secrets")).unwrap();
        private_file(&home.join(OWNER_TOKEN), b"user=lee\ntoken=stt_abc\n").unwrap();
        let env = json!({"STEVE_HOST":"127.0.0.1","STEVE_PORT":"8440","STEVE_PUBLIC_URL":"http://127.0.0.1:8440"});
        let lines = completion_lines("hub", home, &env, false).unwrap();
        assert!(lines.contains(&"登录用户名：lee".to_string()), "{lines:?}");
        assert!(lines.contains(&"访问 token：stt_abc".to_string()), "{lines:?}");
    }
}
