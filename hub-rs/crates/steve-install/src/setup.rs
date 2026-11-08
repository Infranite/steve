//! Resumable first installation. Existing identities and unknown homes are never adopted.
use crate::common::*;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

mod reconfigure;

#[derive(Default)]
pub struct Options {
    pub role: Option<String>,
    pub home: Option<PathBuf>,
    pub hub: Option<String>,
    pub pin: Option<String>,
    pub listen: Option<String>,
    pub reconfigure: bool,
}

trait Prompts {
    fn interactive(&self) -> bool;
    fn line(&mut self, title: &str, default: Option<&str>, optional: bool) -> Result<String>;
    fn secret(&mut self, title: &str) -> Result<Zeroizing<String>>;
}

struct Terminal;
impl Prompts for Terminal {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn line(&mut self, title: &str, default: Option<&str>, optional: bool) -> Result<String> {
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
    fn secret(&mut self, title: &str) -> Result<Zeroizing<String>> {
        use std::os::fd::AsRawFd;
        if !io::stdin().is_terminal() {
            bail!("需要交互终端输入 secret；不会从环境或命令行读取。");
        }
        let fd = io::stdin().as_raw_fd();
        // SAFETY: tcgetattr fills the termios structure for this process's terminal.
        let mut before: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut before) } != 0 {
            bail!("无法读取终端属性");
        }
        let mut hidden = before;
        // Treat Ctrl-C/Ctrl-D as input while the secret is read, so unwinding restores
        // the terminal instead of the process exiting with echo disabled.
        hidden.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
        hidden.c_cc[libc::VMIN] = 1;
        hidden.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
            bail!("无法关闭终端回显");
        }
        struct Restore(i32, libc::termios);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    libc::tcsetattr(self.0, libc::TCSAFLUSH, &self.1);
                }
            }
        }
        let restore = Restore(fd, before);
        print!("{title}: ");
        io::stdout().flush()?;
        let mut bytes = Zeroizing::new(Vec::new());
        loop {
            let mut byte = 0u8;
            // SAFETY: byte is writable for one byte, and fd is this terminal's stdin.
            let count = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
            if count < 0 {
                return Err(io::Error::last_os_error().into());
            }
            if count == 0 || matches!(byte, 3 | 4) {
                bail!("secret 输入已中断；初始化进度保留");
            }
            match byte {
                b'\n' | b'\r' => break,
                8 | 127 => {
                    if let Some(last) = bytes.pop() {
                        if last & 0b1100_0000 == 0b1000_0000 {
                            while bytes.last().is_some_and(|b| b & 0b1100_0000 == 0b1000_0000) {
                                bytes.pop();
                            }
                            bytes.pop();
                        }
                    }
                }
                21 => bytes.clear(),
                b if b < 32 => bail!("secret 不能包含控制字符"),
                b => {
                    if bytes.len() >= 64 * 1024 {
                        bail!("secret 长度超过限制");
                    }
                    bytes.push(b);
                }
            }
        }
        drop(restore);
        println!();
        let value = Zeroizing::new(
            std::str::from_utf8(&bytes)
                .context("secret 必须为 UTF-8")?
                .to_string(),
        );
        if value.is_empty() || value.contains(['\n', '\r', '\0']) {
            bail!("secret 不能为空或包含换行");
        }
        Ok(value)
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
                eprintln!("{error}；请重新输入当前字段。");
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

fn node_hub_url(value: &str) -> Result<String> {
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
    if role == "node" && options.listen.is_some() {
        bail!("--listen 只用于 Hub，请选择 hub");
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
    if let Some(hub) = &options.hub {
        node_hub_url(hub)?;
    }
    if options.pin.as_ref().is_some_and(|v| {
        !v.starts_with("sha256:") || v.len() != 71 || !v[7..].bytes().all(|b| b.is_ascii_hexdigit())
    }) {
        bail!("--pin 需要 sha256: 加 64 位证书指纹");
    }
    Ok(())
}

fn modified_setup_file(name: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "初始化文件已被修改：{name}；setup 只续接未完成的初始化，不会覆盖现有配置。请保留当前文件、setup.json 和身份密钥；先核对修改，并用对应角色的 status/doctor 确认安装进度。只有原服务已注册且配置核验通过时才显式重启；重启不能补完初始化。不要删除收据或重置数据目录来绕过检查。"
    )
}

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
        lines.push("下一步：启动后在上述浏览器地址完成 OIDC 登录，并用该 Hub 的 /healthz 核验运行版本。服务启动和本机安装检查不代表登录已通过。".into());
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

    fn hub_config(&mut self, listen: Option<&str>) -> Result<Value> {
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
            self.owned_file("steve.env", Some(&env_bytes(env.as_object().unwrap())?))?;
            self.owned_file("secrets/oidc-client-secret", None)?;
            return Ok(env);
        }
        let bind = match listen {
            Some(value) => listen_address(value)?,
            None => ask_value(
                self.input,
                "监听地址",
                Some("0.0.0.0:8440"),
                false,
                listen_address,
            )?,
        };
        println!(
            "监听地址决定哪些网络可连接；浏览器地址用于登录及 OIDC 回调。0.0.0.0 不能用作回调地址。"
        );
        let default_url = if bind.ip().is_unspecified() || bind.ip().is_loopback() {
            Some(format!(
                "http://{}:{}",
                if bind.is_ipv6() { "[::1]" } else { "127.0.0.1" },
                bind.port()
            ))
        } else {
            None
        };
        let public = ask_value(
            self.input,
            "浏览器访问地址",
            default_url.as_deref(),
            false,
            |value| browser_url(value, "访问地址"),
        )?;
        let issuer = ask_value(self.input, "OIDC issuer", None, false, oidc_issuer_url)?;
        let client = self.input.line("OIDC client ID", None, false)?;
        println!("请在 OIDC 提供方登记回调：{public}/auth/callback");
        let secret_path = self.home.join("secrets/oidc-client-secret");
        if self.record["files"]
            .get("secrets/oidc-client-secret")
            .is_none()
            || !exists(&secret_path)
        {
            let secret = self.input.secret("OIDC client secret（不回显）")?;
            self.owned_file("secrets/oidc-client-secret", Some(secret.as_bytes()))?;
        } else {
            self.owned_file("secrets/oidc-client-secret", None)?;
        }
        let mut env = json!({"STEVE_HOST":bind.ip().to_string(),"STEVE_PORT":bind.port().to_string(),"STEVE_PUBLIC_URL":public,
            "STEVE_OIDC_ISSUER":issuer,"STEVE_OIDC_CLIENT_ID":client,"STEVE_OIDC_CLIENT_SECRET_FILE":secret_path,
            "STEVE_WEB_DIR":self.home.join("web/current")});
        if bind.ip().is_unspecified() {
            env["STEVE_ALLOW_ANY_HOST"] = "1".into();
        }
        if cfg!(target_os = "linux") {
            let selected = match self.record["key_directory"].as_str() {
                Some(value) => value.to_string(),
                None => {
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

pub fn run(source: &Path, options: Options) -> Result<()> {
    if options.reconfigure {
        return reconfigure_existing(options);
    }
    validate_options(&options)?;
    let mut input = Terminal;
    let role = match options.role.as_deref() {
        Some(value) => setup_role(value, &options)?,
        None => ask_value(
            &mut input,
            "安装角色（hub / node）",
            Some("hub"),
            false,
            |value| setup_role(value, &options),
        )?,
    };
    let home = canonical(&options.home.unwrap_or(user_home()?.join(if role == "hub" {
        ".steve"
    } else {
        ".steve-node"
    })))?;
    println!("正在校验安装包…");
    let expected = manifest(source)?;
    verify_bundle(source, &expected, &home)?;
    let mut wizard = Wizard::claim(source, &home, &role, &expected, &mut input)?;
    println!("正在准备运行目录…");
    let candidate = wizard.copy_bundle(&expected)?;
    if role == "hub" {
        let env = wizard.hub_config(options.listen.as_deref())?;
        if cfg!(target_os = "macos") {
            if !wizard.record["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "keychain")
            {
                wizard.record["keychain_started"] = true.into();
                wizard.save()?;
                crate::signing::setup_keychain(&home, &candidate, "io.github.infranite.steve.hub")?;
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
        let check = captured(
            &candidate.join("bin/steve-hub"),
            &home,
            &["--check-config"],
            Some(&env),
        )?;
        if check["ok"] != true {
            bail!("Hub 配置检查失败，请用 steve hub doctor 检查");
        }
        if let Some(warnings) = check["warnings"].as_array() {
            for warning in warnings {
                println!("{}", warning.as_str().unwrap_or("配置警告"));
            }
        }
    } else {
        wizard.owned_file("roots.json", Some(b"[]\n"))?;
        wizard.owned_file("policy.json", Some(b"{\"max_permissions\":\"auto\",\"full_network\":true,\"exec\":false,\"jobs\":false}\n"))?;
        wizard.owned_file("harness.json", Some(b"{\"adapters\":{}}\n"))?;
        let pairing = home.join("node/node.json");
        if exists(&pairing) {
            if wizard.record["pair_started"] != true {
                bail!("已有配对不属于本次初始化");
            }
            let paired = read_json(&pairing)?;
            let requested_hub = options
                .hub
                .as_ref()
                .map(|v| browser_url(v, "Hub"))
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
        } else {
            let hub = match options
                .hub
                .or_else(|| wizard.record["hub_url"].as_str().map(str::to_string))
            {
                Some(url) => node_hub_url(&url)?,
                None => ask_value(
                    wizard.input,
                    "Hub HTTPS URL（留空稍后配对）",
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
            if !hub.is_empty() {
                if wizard.record["pair_started"] != true && exists(&home.join("node/identity.key"))
                {
                    bail!("已有 Node 身份不属于本次初始化");
                }
                if wizard.record["pair_started"] == true && wizard.record["hub_url"] != hub {
                    bail!("配对曾向另一个 Hub 发出，不会自动更换身份");
                }
                let pin = options
                    .pin
                    .map(|p| p.to_lowercase())
                    .or_else(|| wizard.record["pin"].as_str().map(str::to_string));
                wizard.record["pair_started"] = true.into();
                wizard.record["hub_url"] = hub.clone().into();
                wizard.record["pin"] = json!(pin);
                wizard.save()?;
                println!(
                    "输入 Hub Devices 页生成的一次性配对码，随后仍需在 Hub 核对并确认完整指纹。"
                );
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
            }
        }
    }
    let mut service = crate::service::Service::new(&role, &home, None)?;
    println!("正在注册 {role} 服务…");
    service.dispatch("install", Some(&candidate), 30)?;
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
    for line in completion_lines(
        &role,
        &home,
        &wizard.record["environment"],
        home.join("node/node.json").is_file(),
    )? {
        println!("{line}");
    }
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
        fn secret(&mut self, _: &str) -> Result<Zeroizing<String>> {
            Ok(Zeroizing::new("fixture-only".into()))
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
            fn secret(&mut self, _: &str) -> Result<Zeroizing<String>> {
                unreachable!()
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
            assert!(lines.iter().any(|line| line.contains("完成 OIDC 登录")));
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
    fn new_hub_configuration_binds_all_interfaces_and_keeps_callback_separate() {
        for issuer in [
            "https://identity.example",
            "https://identity.example/",
            "https://identity.example/realms/workspace",
            "https://identity.example/application/o/steve/",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("package");
            private_dir(&source).unwrap();
            let home = temp.path().join("home");
            let keys = temp.path().join("keys");
            let mut answers = vec![
                "https://hub.example/".into(),
                format!(" {issuer} "),
                "client".into(),
            ];
            if cfg!(target_os = "linux") {
                answers.push(keys.to_str().unwrap().into());
            }
            let mut input = Answers(answers);
            let mut wizard =
                Wizard::claim(&source, &home, "hub", &meta("1.0.0"), &mut input).unwrap();
            let env = wizard.hub_config(Some("0.0.0.0:8440")).unwrap();
            assert_eq!(env["STEVE_HOST"], "0.0.0.0");
            assert_eq!(env["STEVE_ALLOW_ANY_HOST"], "1");
            assert_eq!(env["STEVE_PUBLIC_URL"], "https://hub.example");
            assert_eq!(env["STEVE_OIDC_ISSUER"], issuer);
            let bytes = fs::read_to_string(home.join("steve.env")).unwrap();
            assert!(
                bytes
                    .lines()
                    .any(|line| line == format!("STEVE_OIDC_ISSUER='{issuer}'"))
            );
            assert!(!bytes.contains("fixture-only"));
            assert_eq!(
                fs::read(home.join("secrets/oidc-client-secret")).unwrap(),
                b"fixture-only"
            );
        }
    }
}
