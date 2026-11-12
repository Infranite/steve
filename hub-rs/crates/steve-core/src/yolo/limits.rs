//! Hard limits: what YOLO never does,
//! whatever the settings say. Nothing here reads a setting; [`check`] runs before every other rule.
//!
//! 1. money: bank cards, an over-budget flow, payment words anywhere in the card or command;
//! 2. `high` vault items;
//! 3. speaking as the user, unless the outbound path marks the message `[AI] ` and the disclosure check passed
//!    (a program that sends as the user from the agent's shell — `lark-cli`, `sendmail`, `osascript`… — never is);
//! 4. deleting, moving or writing outside the item's own directory; touching `STEVE_HOME`, port 8440, launchd;
//! 5. pushing to a remote or publishing (only the user's own Steve deployment is exempt);
//! 6. outbound content the disclosure check judged Leak or Blocked;
//! 7. what only the user can do in person (sign in, scan a code, type a one-time code, confirm on a
//!    device, plug something in), or a claim that they did: a question about it, or an agent that
//!    stopped waiting for it, stays with the user. Nobody's text, the agent's or the model's, counts
//!    as the user having done it.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::judge::{Disclosure, Env, Facts, Identity, Outbound, OutboundStage, Subject, Tool};
use super::shell::{self, Argv, Parsed};

pub use steve_modules::outbound::{AI_PREFIX, strip_ai_prefix, with_ai_prefix};

/// A hard limit that was hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    /// 1–7, as numbered in the module docs.
    pub n: u8,
    pub what: String,
}

impl Limit {
    fn new(n: u8, what: impl Into<String>) -> Self {
        Limit {
            n,
            what: what.into(),
        }
    }
    /// `limit.<n>`
    pub fn rule(&self) -> String {
        format!("limit.{}", self.n)
    }
    /// `硬限制：<what>`
    pub fn reason(&self) -> String {
        format!("{REASON_PREFIX}{}", self.what)
    }
}

/// The stored prefix of a hard limit's reason ([`Limit::reason`]).
pub const REASON_PREFIX: &str = "硬限制：";

static MONEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)支付|付款|转账|下单|购买|充值|续费|\bpay\b|\bcheckout\b|\bcharge\b|\bpurchase\b",
    )
    .expect("valid regex")
});
/// `git checkout` is not a payment.
static GIT_CHECKOUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bgit(\s+-\S+(\s+\S+)?)*\s+checkout\b").expect("valid regex")
});
/// What `steve_request_approval` must not be about; under full access also what a question or an
/// answer must not be about (nothing checks what the agent then does with the answer).
static OUTWARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)推送|发布|发送|付款|删除|部署|生产|上线|\bpush\b|\bpublish|\bdeploy|\brelease\b|\bsend\b|\bprod(uction)?\b|\bdelete\b|\bemail\b|\bupload\b")
        .expect("valid regex")
});
/// What a question or an answer must not be about while YOLO is on without full access: only going
/// live and mail. Whatever the agent does with the answer still meets the tool guard and these limits.
static OUTWARD_ASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)推送|发布|部署|上线|发送邮件|发邮件|\bpush\b|\bpublish|\bdeploy|\bsend\s+(an?\s+)?e-?mail")
        .expect("valid regex")
});

/// The outward words of a question or answer: [`OUTWARD_ASK`], or [`OUTWARD`] under full access.
fn outward_ask(text: &str, full: bool) -> Option<regex::Match<'_>> {
    if full {
        OUTWARD.find(text)
    } else {
        OUTWARD_ASK.find(text)
    }
}
/// Hard limit 7: something the user has to do themselves, or to say they did.
static OWN_ACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)本人|亲自|手动(操作|完成|确认|登录|点击|执行|授权|批准|输入|扫码)",
        r"|扫码|扫一扫|二维码|验证码|动态码|短信码|一次性密码|两步验证|双重验证|二次验证|\b(2fa|mfa|otp)\b",
        r"|指纹|面容|touch ?id|face ?id|硬件密钥|yubikey|u盾",
        r"|插(上|入|好)(手机|设备|硬件|数据线|u盘|网线|电源)|重新插拔",
        r"|你[^。？?！!\n]{0,8}(在|用|到)(你的)?(手机|电脑|浏览器|设备|app|应用|系统设置|钥匙串|终端)",
        r"|(登录|授权|操作|扫码|确认|安装|批准|验证)(完|好)?(了|后|之后|以后)[^。？?\n]{0,4}(告诉|通知|回复|跟|叫)我",
        r"|(在|用)(手机|设备|手表|平板)(上|里)?[^。？?\n]{0,4}(确认|登录|授权|批准|扫|点|验证|输入)",
        r"|(你|用户|本人)[^。？?\n]{0,4}已(经)?[^。？?\n]{0,6}(登录|扫码|确认|授权|批准|操作|验证)",
        r"|\b(qr ?code|verification code|one-time (code|password)|passkey|in person|yourself)\b",
        r"|\bon your (phone|device|computer|laptop|browser)\b|\b(sign|log) ?in (manually|yourself)\b",
    ))
    .expect("valid regex")
});

/// What in `text` asks for (or claims) the user's own action: hard limit 7.
pub fn own_action(text: &str) -> Option<String> {
    OWN_ACTION.find(text).map(|m| m.as_str().to_string())
}

static LAUNCHD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)launchctl|launchd|LaunchAgents|LaunchDaemons").expect("valid regex")
});

/// The first hard limit `f` hits, if any.
pub fn check(f: &Facts, env: &Env) -> Option<Limit> {
    // money words anywhere on the card (a git checkout is not a checkout)
    let card = format!("{}\n{}", f.question, f.draft);
    if let Some(w) = money(&card) {
        return Some(Limit::new(1, format!("涉及付款（{w}）")));
    }
    match &f.subject {
        Subject::SecretUse {
            template,
            sensitivity,
        } => {
            if template == "bank_card" {
                return Some(Limit::new(1, "银行卡"));
            }
            if sensitivity == "high" {
                return Some(Limit::new(2, "高敏感的保险箱条目"));
            }
            None
        }
        Subject::FlowRun { over_budget: true } => Some(Limit::new(1, "流程超出预算")),
        Subject::Mail(o) | Subject::Chan(o) => outbound(f, o),
        Subject::Github => Some(Limit::new(5, "GitHub 上的操作是对外的")),
        Subject::Release(r) => {
            if r.release_kind != "script" || !own_release(&r.cmd, env) {
                Some(Limit::new(5, "发布脚本不是这台 Steve 自己的部署"))
            } else {
                None
            }
        }
        Subject::Action => OUTWARD
            .find(&card)
            .map(|m| Limit::new(5, format!("对外或不可撤回的动作（{}）", m.as_str()))),
        Subject::Tool(t) => tool(t, env),
        // a question about pushing, publishing, sending or deleting is never YOLO's to answer
        Subject::Ask(a) => ask(f, a),
        Subject::Stall(s) => s
            .own_action
            .as_ref()
            .map(|w| Limit::new(7, format!("它在等你本人操作（{w}）")))
            .or_else(|| s.asked.as_ref().and_then(|a| ask(f, a))),
        _ => None,
    }
}

fn ask(f: &Facts, a: &super::judge::Ask) -> Option<Limit> {
    let words = [
        Some(f.question.as_str()),
        Some(a.key.as_str()),
        a.answer.as_ref().map(|x| x.answer.as_str()),
        a.recommended.as_deref(),
    ];
    for t in words.into_iter().flatten() {
        if let Some(w) = money(t) {
            return Some(Limit::new(1, format!("涉及付款（{w}）")));
        }
    }
    // the answer's own words included: a model that says it was done is no proof that it was
    for t in words.into_iter().flatten() {
        if let Some(w) = own_action(t) {
            return Some(Limit::new(7, format!("需要你本人操作或确认（{w}）")));
        }
    }
    outward_ask(&f.question, a.full)
        .or_else(|| outward_ask(&a.key, a.full))
        .map(|m| Limit::new(5, format!("问题涉及对外或不可撤回的动作（{}）", m.as_str())))
}

fn money(text: &str) -> Option<String> {
    let t = GIT_CHECKOUT.replace_all(text, "git switch");
    MONEY.find(&t).map(|m| m.as_str().to_string())
}

/// Payment words, or an answer that walks outside (used on model answers before they reach a session);
/// `full`: the session runs with full access, where the whole [`OUTWARD`] list counts.
pub fn money_or_outside_words(text: &str, full: bool) -> Option<String> {
    money(text).or_else(|| outward_ask(text, full).map(|m| m.as_str().to_string()))
}

/// What a `steve_request_approval` text gives away about leaving the worktree; `None` reads as local.
pub fn action_outside(text: &str) -> Option<String> {
    static OUTSIDE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)~/|/Users/|/etc/|/usr/|/opt/|全局|系统|\bglobal\b|\bsudo\b|安装到|\binstall\b|重启|\brestart\b|\bkill\b|密码|password|token|密钥|\bkey\b|账号|\.steve|8440|launchctl")
            .expect("valid regex")
    });
    OUTSIDE.find(text).map(|m| m.as_str().to_string())
}

fn outbound(f: &Facts, o: &Outbound) -> Option<Limit> {
    if matches!(o.disclosure, Disclosure::Leak | Disclosure::Blocked)
        || matches!(o.stage, OutboundStage::LeakDraft | OutboundStage::Blocked)
    {
        return Some(Limit::new(6, "披露检查判为泄露或拦截"));
    }
    let as_user = matches!(o.identity, Identity::SelfAccount | Identity::Proxy)
        || f.subject_type == "mail_draft";
    if as_user && o.stage != OutboundStage::Handoff {
        if !o.ai_prefix {
            return Some(Limit::new(
                3,
                "以你本人的身份发出，但出站路径不会加 [AI] 标识",
            ));
        }
        if !o.disclosure.passed() {
            return Some(Limit::new(3, "以你本人的身份发出，但没有通过披露检查"));
        }
    }
    None
}

/// `release.cmd` is the user's own deployment: its program resolves under `STEVE_HOME`.
pub fn own_release(cmd: &str, env: &Env) -> bool {
    let Ok(p) = shell::parse(cmd) else {
        return false;
    };
    if !p.opaque.is_empty() || p.cmds.len() != 1 || p.cmds[0].expands {
        return false;
    }
    let Some(prog) = p.cmds[0].words.first() else {
        return false;
    };
    let path = Path::new(prog);
    if !path.is_absolute() {
        return false;
    }
    canonical(path).starts_with(canonical(&env.steve_home))
}

// ── paths ──

/// `p` with symlinks and `..` resolved: the longest existing prefix is canonicalized, the rest appended
/// component by component (a missing component cannot be a symlink, so `..` after it is lexical).
pub fn canonical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut real = true;
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => {
                out.push(n);
                if real {
                    match std::fs::canonicalize(&out) {
                        Ok(c) => out = c,
                        Err(_) => {
                            // a dangling symlink points somewhere we cannot see: treat it as that target
                            if let Ok(t) = std::fs::read_link(&out) {
                                let base = out.parent().map(Path::to_path_buf).unwrap_or_default();
                                out = canonical(&base.join(t));
                            }
                            real = false;
                        }
                    }
                }
            }
        }
    }
    out
}

/// Where a tool call may write and what it must not touch.
#[derive(Debug, Clone)]
pub(crate) struct Places {
    /// The item's own directory, canonical.
    pub cwd: Option<PathBuf>,
    /// What relative paths are relative to: `cwd` until a `cd`; `None` once it cannot be known.
    pub base: Option<PathBuf>,
    pub home: PathBuf,
    /// `STEVE_HOME` and `~/.steve`: never touched outside the item's own directory.
    pub protected: Vec<PathBuf>,
    /// Shared temporary directories (`/tmp`, `/private/tmp`, `$TMPDIR`): files may be created and
    /// written there, not deleted or moved (other agents use them too).
    pub tmp: Vec<PathBuf>,
}

impl Places {
    pub fn new(cwd: Option<&Path>, env: &Env) -> Self {
        let cwd = cwd.map(canonical);
        let home = canonical(&env.home);
        let steve_home = canonical(&env.steve_home);
        let mut protected = vec![steve_home.clone()];
        let dot = canonical(&home.join(".steve"));
        if !protected.contains(&dot) {
            protected.push(dot);
        }
        let mut tmp: Vec<PathBuf> = Vec::new();
        for t in &env.tmp {
            let t = canonical(t);
            // a temp dir that is (or holds) home or Steve's data is not scratch space
            if t.parent().is_some()
                && !home.starts_with(&t)
                && !protected.iter().any(|p| p.starts_with(&t))
                && !tmp.contains(&t)
            {
                tmp.push(t);
            }
        }
        Places {
            base: cwd.clone(),
            cwd,
            home,
            protected,
            tmp,
        }
    }

    /// The canonical path a word names; `None` when it cannot be known here (`$VAR`, `~user`, relative
    /// without a working directory).
    pub fn resolve(&self, w: &str) -> Option<PathBuf> {
        if w.contains('$') || w.contains('`') {
            return None;
        }
        let p = if w == "~" {
            self.home.clone()
        } else if let Some(rest) = w.strip_prefix("~/") {
            self.home.join(rest)
        } else if w.starts_with('~') {
            return None;
        } else if Path::new(w).is_absolute() {
            PathBuf::from(w)
        } else {
            self.base.as_ref()?.join(w)
        };
        Some(canonical(&p))
    }

    pub fn inside(&self, p: &Path) -> bool {
        self.cwd.as_ref().is_some_and(|c| p.starts_with(c))
    }

    /// Inside `STEVE_HOME` (or `~/.steve`) but not in the item's own directory (which may live under it).
    pub fn in_steve(&self, p: &Path) -> bool {
        self.protected.iter().any(|s| p.starts_with(s)) && !self.inside(p)
    }

    /// Below a shared temporary directory (not the directory itself).
    pub fn in_tmp(&self, p: &Path) -> bool {
        !self.in_steve(p) && self.tmp.iter().any(|t| p.starts_with(t) && p != t)
    }

    /// Where files may be created or written: the item's own directory or a temporary directory.
    pub fn writable(&self, p: &Path) -> bool {
        self.inside(p) || self.in_tmp(p)
    }

    /// A word naming a file that is created or written: inside the item's directory or under a
    /// temporary directory.
    fn write_target(&self, w: &str) -> Option<Limit> {
        match self.resolve(w) {
            Some(p) if self.in_tmp(&p) => None,
            _ => self.remove_target(w),
        }
    }

    /// A word naming something deleted, moved or rewritten in bulk: inside the item's directory only.
    fn remove_target(&self, w: &str) -> Option<Limit> {
        if matches!(
            w,
            "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty" | "-"
        ) {
            return None;
        }
        match self.resolve(w) {
            Some(p) if self.in_steve(&p) => Some(Limit::new(4, format!("碰到 STEVE_HOME：{w}"))),
            Some(p) if self.inside(&p) => None,
            Some(_) => Some(Limit::new(4, format!("改动或删除工作区以外的路径：{w}"))),
            None => Some(Limit::new(4, format!("看不出 {w} 指向哪里"))),
        }
    }
}

// ── tool calls ──

/// The ACP kind of a tool call, from `toolCall.kind` or, missing that, from its input.
pub(crate) fn tool_kind(t: &Tool) -> &str {
    if let Some(k) = t.kind.as_deref() {
        return k;
    }
    let has = |k: &str| t.raw.get(k).is_some();
    if has("command") {
        "execute"
    } else if has("old_string")
        || has("new_string")
        || has("content")
        || has("edits")
        || has("changes")
    {
        "edit"
    } else if has("url") {
        "fetch"
    } else if has("pattern") || has("query") {
        "search"
    } else if has("file_path") || has("path") || has("notebook_path") {
        "read"
    } else {
        "other"
    }
}

/// Every path a tool call names: its locations, the usual path keys of its input, and the files of a
/// Codex patch (`changes: {path: {type, move_path}}`).
pub(crate) fn tool_paths(t: &Tool) -> Vec<String> {
    tool_targets(t).into_iter().map(|(p, _)| p).collect()
}

/// [`tool_paths`] with whether the call removes or moves that path away (`true`) or creates or writes
/// it (`false`), as far as the input says; the tool's kind decides for the plain path keys.
pub(crate) fn tool_targets(t: &Tool) -> Vec<(String, bool)> {
    const KEYS: [&str; 9] = [
        "file_path",
        "path",
        "notebook_path",
        "old_path",
        "new_path",
        "source",
        "destination",
        "target",
        "dir",
    ];
    let removes = matches!(tool_kind(t), "delete" | "move");
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut add = |p: &str, r: bool| {
        if let Some(x) = out.iter_mut().find(|(q, _)| q == p) {
            x.1 |= r;
        } else {
            out.push((p.to_string(), r));
        }
    };
    for l in &t.locations {
        add(l, removes);
    }
    for k in KEYS {
        if let Some(s) = t.raw.get(k).and_then(Value::as_str) {
            add(s, removes);
        }
    }
    if let Some(edits) = t.raw.get("edits").and_then(Value::as_array) {
        for e in edits {
            if let Some(s) = e.get("file_path").and_then(Value::as_str) {
                add(s, removes);
            }
        }
    }
    if let Some(changes) = t.raw.get("changes").and_then(Value::as_object) {
        for (path, c) in changes {
            let ty = c.get("type").and_then(Value::as_str).unwrap_or("");
            let to = c.get("move_path").and_then(Value::as_str);
            // a deleted or moved-away file is gone from its place; an added or updated one is written
            add(path, removes || ty == "delete" || to.is_some());
            if let Some(to) = to {
                add(to, removes);
            }
        }
    }
    out
}

/// The command line of an `execute` call: a string, or Codex's argv (`["/bin/zsh", "-lc", "…"]`).
pub(crate) fn tool_command(t: &Tool) -> Option<String> {
    match t.raw.get("command")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let words: Option<Vec<String>> =
                a.iter().map(|w| w.as_str().map(String::from)).collect();
            words
                .filter(|w| !w.is_empty())
                .map(|w| shell::from_argv(&w))
        }
        _ => None,
    }
}

/// Where the command runs: Codex sends its own `cwd`, which may be below the item's directory.
pub(crate) fn tool_base(t: &Tool, pl: &Places) -> Option<Option<PathBuf>> {
    t.raw
        .get("cwd")
        .and_then(Value::as_str)
        .map(|c| pl.resolve(c))
}

fn tool(t: &Tool, env: &Env) -> Option<Limit> {
    let mut pl = Places::new(t.cwd.as_deref(), env);
    let kind = tool_kind(t);
    let raw_text = t.raw.to_string();
    if let Some(w) = money(&raw_text) {
        return Some(Limit::new(1, format!("涉及付款（{w}）")));
    }
    if let Some(base) = tool_base(t, &pl) {
        pl.base = base;
    }
    for (p, removes) in tool_targets(t) {
        if let Some(rp) = pl.resolve(&p)
            && pl.in_steve(&rp)
        {
            return Some(Limit::new(4, format!("碰到 STEVE_HOME：{p}")));
        }
        if !matches!(kind, "edit" | "delete" | "move") {
            continue;
        }
        let hit = if removes {
            pl.remove_target(&p)
        } else {
            pl.write_target(&p)
        };
        if hit.is_some() {
            return hit;
        }
    }
    match kind {
        "execute" => command(
            &tool_command(t).unwrap_or_default(),
            &pl,
            t.branch.as_deref(),
        ),
        "fetch" => {
            let method = t.raw.get("method").and_then(Value::as_str).unwrap_or("GET");
            if !method.eq_ignore_ascii_case("GET") {
                return Some(Limit::new(5, format!("{method} 请求会把数据发出去")));
            }
            text_limits(&raw_text, &pl)
        }
        _ => None,
    }
}

/// `~/.steve`, `$HOME/.steve`, `$STEVE_HOME`: Steve's home named without a path that could be resolved.
static STEVE_NAMED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"~/\.steve\b|\$\{?HOME\}?/\.steve\b|\$\{?STEVE_HOME\b").expect("valid regex")
});

/// Port 8440, launchd, Steve's release script and `STEVE_HOME` named anywhere in a command line.
/// Relative paths (`.steve/qa` inside the item's own directory) are left to the path checks, which
/// resolve them.
fn text_limits(text: &str, pl: &Places) -> Option<Limit> {
    steve_text(text, pl, true)
}

/// [`text_limits`]; `release`: Steve's release script counts too (full access leaves it out: a
/// project's own `release.sh` is not Steve's, and the dialog promises only the rest).
fn steve_text(text: &str, pl: &Places, release: bool) -> Option<Limit> {
    let t = own_dir_masked(text, pl);
    if t.contains("8440") {
        return Some(Limit::new(4, "碰到端口 8440（正在运行的 Steve）"));
    }
    if let Some(m) = LAUNCHD.find(&t) {
        return Some(Limit::new(4, format!("碰到 launchd（{}）", m.as_str())));
    }
    if release && t.contains("release.sh") {
        return Some(Limit::new(4, "Steve 的发布脚本"));
    }
    steve_named(&t, pl)
}

/// `text` with the item's own directory (which may sit under STEVE_HOME; naming it is fine)
/// replaced by `<cwd>`.
fn own_dir_masked(text: &str, pl: &Places) -> String {
    let mut t = text.to_string();
    if let Some(c) = &pl.cwd {
        let c = c.to_string_lossy();
        t = t.replace(&*c, "<cwd>");
        if let Ok(rel) = Path::new(&*c).strip_prefix(&pl.home) {
            t = t.replace(&format!("~/{}", rel.to_string_lossy()), "<cwd>");
        }
    }
    t
}

/// `STEVE_HOME` named in `t` (as `~/.steve`, `$STEVE_HOME` or its path).
fn steve_named(t: &str, pl: &Places) -> Option<Limit> {
    if STEVE_NAMED.is_match(t)
        || pl
            .protected
            .iter()
            .any(|p| t.contains(&*p.to_string_lossy()))
    {
        return Some(Limit::new(4, "碰到 STEVE_HOME"));
    }
    None
}

/// The limits a shell command line hits.
pub(crate) fn command(cmd: &str, pl: &Places, branch: Option<&str>) -> Option<Limit> {
    if let Some(w) = money(cmd) {
        return Some(Limit::new(1, format!("涉及付款（{w}）")));
    }
    if let Some(l) = text_limits(cmd, pl) {
        return Some(l);
    }
    match shell::parse(cmd) {
        Ok(p) => parsed(&p, pl, branch, 0),
        // the guard never lets an unreadable command through; nothing more to see here
        Err(_) => None,
    }
}

const MAX_DEPTH: u8 = 4;

fn parsed(p: &Parsed, pl: &Places, branch: Option<&str>, depth: u8) -> Option<Limit> {
    if depth > MAX_DEPTH {
        return Some(Limit::new(4, "命令嵌套太深，看不清"));
    }
    let mut pl = pl.clone();
    for c in &p.cmds {
        // `cd` moves what later relative paths mean
        if let Some(a) = shell::argv(&c.words)
            && matches!(a.program.as_str(), "cd" | "pushd" | "popd")
        {
            pl.base = match (a.program.as_str(), operands(&a.args).first()) {
                ("popd", _) => None,
                (_, None) => Some(pl.home.clone()),
                (_, Some(d)) if d.as_str() == "-" => None,
                (_, Some(d)) => pl.resolve(d),
            };
            continue;
        }
        let pl = &pl;
        for w in &c.writes {
            if let Some(l) = pl.write_target(w) {
                return Some(l);
            }
        }
        for w in &c.words {
            if let Some(rp) = pl.resolve(w)
                && (w.contains('/') || w.starts_with('.') || w.starts_with('~'))
                && pl.in_steve(&rp)
            {
                return Some(Limit::new(4, format!("碰到 STEVE_HOME：{w}")));
            }
        }
        let Some(a) = shell::argv(&c.words) else {
            continue;
        };
        // a here-document fed to a shell is a script of its own
        if let Some(body) = &c.stdin
            && SHELLS.contains(&a.program.as_str())
        {
            match shell::parse(body) {
                Ok(n) => {
                    if let Some(l) = parsed(&n, pl, branch, depth + 1) {
                        return Some(l);
                    }
                }
                Err(_) => return Some(Limit::new(4, "喂给 shell 的 here-document 看不清")),
            }
        }
        if let Some(l) = program(&a, c.expands, pl, branch, depth) {
            return Some(l);
        }
        // quoted arguments that are command lines of their own (`sh -c`, `ssh host '…'`, `-x '…'`)
        for n in shell::nested(&a.args) {
            if let Some(l) = parsed(&n, pl, branch, depth + 1) {
                return Some(l);
            }
        }
    }
    None
}

fn words_of(a: &Argv, from: usize) -> Vec<String> {
    a.args
        .get(from..)
        .map(<[String]>::to_vec)
        .unwrap_or_default()
}

/// Runs `argv` as a command of its own (`npx X`, `python -m X`, `find -exec X`).
fn sub_argv(words: Vec<String>, pl: &Places, branch: Option<&str>, depth: u8) -> Option<Limit> {
    if depth > MAX_DEPTH {
        return Some(Limit::new(4, "命令嵌套太深，看不清"));
    }
    let a = shell::argv(&words)?;
    program(&a, false, pl, branch, depth + 1)
}

/// Non-option arguments (`--` ends the options).
fn operands(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut opts = true;
    for a in args {
        if opts && a == "--" {
            opts = false;
            continue;
        }
        if opts && a.starts_with('-') && a != "-" {
            continue;
        }
        out.push(a);
    }
    out
}

/// The value of `-x v`, `-xv`, `--long v` or `--long=v`.
fn opt_value<'a>(args: &'a [String], short: &str, long: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == short || (!long.is_empty() && a == long) {
            return it.next().map(String::as_str);
        }
        if !long.is_empty()
            && let Some(v) = a.strip_prefix(long).and_then(|r| r.strip_prefix('='))
        {
            return Some(v);
        }
        if short.len() == 2
            && let Some(v) = a.strip_prefix(short)
            && !v.is_empty()
            && !a.starts_with("--")
        {
            return Some(v);
        }
    }
    None
}

const DEPLOY_TOOLS: &[&str] = &[
    "vercel",
    "netlify",
    "fly",
    "flyctl",
    "wrangler",
    "firebase",
    "heroku",
    "surge",
    "aws",
    "gcloud",
    "az",
    "kubectl",
    "helm",
    "terraform",
    "pulumi",
    "eb",
    "serverless",
    "sls",
    "gsutil",
    "s3cmd",
    "rclone",
];
const REMOTE_SHELLS: &[&str] = &[
    "ssh", "scp", "sftp", "ftp", "telnet", "nc", "ncat", "netcat", "socat", "mosh", "lftp",
];
const DELETERS: &[&str] = &[
    "rm", "rmdir", "unlink", "shred", "srm", "trash", "truncate", "touch", "mkdir", "tee",
];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh", "csh", "tcsh"];
/// Programs that send messages or mail as the user with the user's own credentials: nothing marks
/// what they send `[AI] ` and no disclosure check reads it.
pub(crate) const AS_USER: &[&str] = &[
    "lark-cli",
    "sendmail",
    "mail",
    "mailx",
    "msmtp",
    "mutt",
    "neomutt",
    "osascript",
];

fn as_user(p: &str) -> Option<Limit> {
    AS_USER
        .contains(&p)
        .then(|| Limit::new(3, format!("{p} 会以你本人的身份发消息或邮件")))
}

fn program(a: &Argv, expands: bool, pl: &Places, branch: Option<&str>, depth: u8) -> Option<Limit> {
    let p = a.program.as_str();
    let args = &a.args;
    let first = args.first().map(String::as_str).unwrap_or("");
    let push = |what: &str| Some(Limit::new(5, what.to_string()));
    // writes whose target cannot be read off the text
    let unknown_targets = || {
        if a.xargs || expands {
            Some(Limit::new(
                4,
                format!("{p} 的目标来自变量或标准输入，看不出在哪"),
            ))
        } else {
            None
        }
    };
    let all_inside = |list: Vec<&String>| list.into_iter().find_map(|w| pl.remove_target(w));
    let all_writable = |list: Vec<&String>| list.into_iter().find_map(|w| pl.write_target(w));

    if let Some(l) = as_user(p) {
        return Some(l);
    }
    if REMOTE_SHELLS.contains(&p) {
        return push("连到远端主机");
    }
    if DEPLOY_TOOLS.contains(&p) {
        return push("部署或云端工具会改远端");
    }
    match p {
        "git" => return git(args, pl, branch, depth),
        "gh" => return gh(args),
        "curl" => return curl(args),
        "wget" => {
            if args.iter().any(|x| {
                x.starts_with("--post-")
                    || x.starts_with("--body-")
                    || x.starts_with("--method")
                        && !opt_value(args, "", "--method")
                            .is_some_and(|m| m.eq_ignore_ascii_case("GET"))
            }) {
                return push("wget 发出数据");
            }
            if let Some(d) = opt_value(args, "-P", "--directory-prefix") {
                return pl.write_target(d);
            }
            return opt_value(args, "-O", "--output-document").and_then(|o| pl.write_target(o));
        }
        "http" | "https" | "xh" | "xhs" | "httpie" => {
            if args.iter().any(|x| {
                matches!(
                    x.to_uppercase().as_str(),
                    "POST" | "PUT" | "PATCH" | "DELETE"
                ) || x.contains('=')
                    || x.contains(":=")
                    || x.starts_with('@')
            }) {
                return push("HTTP 请求会把数据发出去");
            }
        }
        "rsync" => {
            if args.iter().any(|x| {
                x.starts_with("rsync://")
                    || (!x.starts_with('-') && !x.starts_with('/') && x.contains(':'))
            }) {
                return push("rsync 到远端");
            }
            let ops = operands(args);
            if let Some(dest) = ops.last()
                && ops.len() > 1
            {
                return pl.remove_target(dest);
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            let sub = operands(args).first().map(|s| s.as_str()).unwrap_or("");
            if matches!(
                sub,
                "publish"
                    | "unpublish"
                    | "deprecate"
                    | "dist-tag"
                    | "owner"
                    | "access"
                    | "login"
                    | "adduser"
                    | "token"
                    | "team"
            ) {
                return push("发布到包仓库");
            }
            if matches!(sub, "run" | "run-script") || p == "yarn" && !sub.is_empty() {
                let script = if matches!(sub, "run" | "run-script") {
                    operands(args).get(1).map(|s| s.as_str()).unwrap_or("")
                } else {
                    sub
                };
                if script_is_outward(script) {
                    return push("运行发布或部署脚本");
                }
            }
            if matches!(sub, "exec" | "dlx" | "x") {
                let rest: Vec<String> = operands(args)
                    .iter()
                    .skip(1)
                    .map(|s| s.to_string())
                    .collect();
                return sub_argv(rest, pl, branch, depth);
            }
        }
        "npx" | "bunx" | "pnpx" | "uvx" | "pipx" => {
            let rest: Vec<String> = operands(args)
                .iter()
                .skip(usize::from(p == "pipx" && first == "run"))
                .map(|s| s.to_string())
                .collect();
            return sub_argv(rest, pl, branch, depth);
        }
        "cargo" => {
            if matches!(
                operands(args).first().map(|s| s.as_str()),
                Some("publish" | "yank" | "owner" | "login")
            ) {
                return push("发布到 crates.io");
            }
        }
        "twine" | "flit" | "poetry" | "uv" | "hatch" | "maturin" | "gem" => {
            if args
                .iter()
                .any(|x| matches!(x.as_str(), "upload" | "publish" | "push"))
            {
                return push("发布到包仓库");
            }
        }
        "docker" | "podman" | "buildah" | "skopeo" | "crane" => {
            if args
                .iter()
                .any(|x| matches!(x.as_str(), "push" | "login" | "copy"))
            {
                return push("推送镜像");
            }
        }
        "make" | "just" | "task" | "rake" => {
            if operands(args).iter().any(|t| script_is_outward(t)) {
                return push("运行发布或部署目标");
            }
        }
        "python" | "python3" | "py" => {
            if first == "-m" {
                return sub_argv(words_of(a, 1), pl, branch, depth);
            }
        }
        "eval" | "source" | "." => {
            let joined = args.join(" ");
            return match shell::parse(&joined) {
                Ok(n) => parsed(&n, pl, branch, depth + 1),
                Err(_) => Some(Limit::new(4, "eval 的内容看不清")),
            };
        }
        "find" => return find(args, pl, branch, depth),
        "mv" => {
            if let Some(l) = unknown_targets() {
                return Some(l);
            }
            if let Some(t) = opt_value(args, "-t", "--target-directory")
                && let Some(l) = pl.remove_target(t)
            {
                return Some(l);
            }
            return all_inside(operands(args));
        }
        "cp" | "install" | "ln" => {
            if let Some(t) = opt_value(args, "-t", "--target-directory") {
                return pl.write_target(t);
            }
            if a.xargs {
                return unknown_targets();
            }
            let ops = operands(args);
            if ops.len() >= 2 || p == "ln" && ops.len() == 1 {
                if expands {
                    return unknown_targets();
                }
                return ops.last().and_then(|d| pl.write_target(d));
            }
        }
        "dd" => {
            return args
                .iter()
                .filter_map(|x| x.strip_prefix("of="))
                .find_map(|o| pl.write_target(o));
        }
        "chmod" | "chown" | "chgrp" | "chflags" | "xattr" | "setfacl" => {
            if let Some(l) = unknown_targets() {
                return Some(l);
            }
            return all_inside(operands(args).into_iter().skip(1).collect());
        }
        "sed" | "perl" | "ruby" => {
            let in_place = args.iter().any(|x| {
                x.starts_with("-i")
                    || x.starts_with("--in-place")
                    || (p != "sed" && x.starts_with("-p") && x.contains('i'))
            });
            if in_place {
                if let Some(l) = unknown_targets() {
                    return Some(l);
                }
                let has_script_opt = args
                    .iter()
                    .any(|x| x == "-e" || x == "-f" || x == "-E" && p != "sed");
                let ops = operands(args);
                let files: Vec<&String> = if has_script_opt {
                    ops
                } else {
                    ops.into_iter().skip(1).collect()
                };
                return all_writable(files);
            }
        }
        "tar" | "bsdtar" | "gtar" => {
            let extract = args.iter().any(|x| {
                x == "--extract"
                    || x == "--get"
                    || (!x.starts_with("--") && x.trim_start_matches('-').contains('x'))
            });
            if extract && let Some(d) = opt_value(args, "-C", "--directory") {
                return pl.remove_target(d);
            }
        }
        "unzip" | "ditto" | "7z" => {
            if let Some(d) = opt_value(args, "-d", "") {
                return pl.remove_target(d);
            }
            if p == "ditto" {
                return operands(args).last().and_then(|d| pl.remove_target(d));
            }
        }
        _ => {}
    }
    if DELETERS.contains(&p) {
        if let Some(l) = unknown_targets() {
            return Some(l);
        }
        // creating files may also happen in a temporary directory; removing only in the item's own
        if matches!(p, "touch" | "mkdir" | "tee") {
            return all_writable(operands(args));
        }
        return all_inside(operands(args));
    }
    if SHELLS.contains(&p) {
        // `bash script.sh` runs a file we cannot read here; `-c` strings are read by `nested`
        if let Some(i) = args.iter().position(|x| shell::is_dash_c(x)) {
            let body = args.get(i + 1).cloned().unwrap_or_default();
            return match shell::parse(&body) {
                Ok(n) => parsed(&n, pl, branch, depth + 1),
                Err(_) => Some(Limit::new(4, "sh -c 的内容看不清")),
            };
        }
    }
    None
}

fn script_is_outward(name: &str) -> bool {
    let n = name.to_lowercase();
    ["deploy", "release", "publish", "push", "upload", "ship"]
        .iter()
        .any(|w| n.contains(w))
}

/// A curl command line read completely: the URLs it fetches and the files it writes (`-o`).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct CurlLine {
    pub urls: Vec<String>,
    pub outputs: Vec<String>,
}

/// Reads `args` when every word is understood and every URL is this machine (`127.0.0.1`, `localhost`,
/// `[::1]`); `None` otherwise (an unknown option, a proxy, `--resolve`, a second host…).
pub(crate) fn curl_loopback(args: &[String]) -> Option<CurlLine> {
    const FLAGS: &[&str] = &[
        "silent",
        "show-error",
        "fail",
        "fail-with-body",
        "location",
        "include",
        "head",
        "compressed",
        "verbose",
        "insecure",
        "http1.1",
        "http2",
        "globoff",
        "no-progress-meter",
        "progress-bar",
        "raw",
        "no-buffer",
        "ipv4",
        "ipv6",
    ];
    const VALUED: &[&str] = &[
        "request",
        "header",
        "data",
        "data-raw",
        "data-binary",
        "data-urlencode",
        "json",
        "user",
        "user-agent",
        "referer",
        "max-time",
        "connect-timeout",
        "write-out",
        "cookie",
        "form",
        "form-string",
        "retry",
        "range",
        "output",
        "retry-delay",
        "retry-max-time",
    ];
    // short options: s S f L i I v k g N 4 6 are flags; these take a value
    const SHORT_VALUED: &str = "XHdFuAemwbroT";
    const SHORT_FLAGS: &str = "sSfLiIvkgN46";
    let mut out = CurlLine::default();
    let mut i = 0;
    while let Some(x) = args.get(i) {
        i += 1;
        if let Some(long) = x.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            if FLAGS.contains(&name) && val.is_none() {
                continue;
            }
            if !VALUED.contains(&name) {
                return None;
            }
            let v = match val {
                Some(v) => v,
                None => {
                    i += 1;
                    args.get(i - 1)?.clone()
                }
            };
            if name == "output" {
                out.outputs.push(v);
            }
            continue;
        }
        if let Some(cluster) = x.strip_prefix('-').filter(|c| !c.is_empty()) {
            for (k, ch) in cluster.char_indices() {
                if SHORT_FLAGS.contains(ch) {
                    continue;
                }
                if !SHORT_VALUED.contains(ch) {
                    return None;
                }
                let rest = &cluster[k + ch.len_utf8()..];
                let v = if rest.is_empty() {
                    i += 1;
                    args.get(i - 1)?.clone()
                } else {
                    rest.to_string()
                };
                if ch == 'o' {
                    out.outputs.push(v);
                }
                break;
            }
            continue;
        }
        out.urls.push(x.clone());
    }
    let local = |u: &str| {
        let rest = u
            .strip_prefix("http://")
            .or_else(|| u.strip_prefix("https://"))
            .unwrap_or(u);
        if rest.contains('@') || rest.contains('\\') {
            return false;
        }
        let host = match rest.strip_prefix('[') {
            Some(r) => r.split(']').next().unwrap_or(""),
            None => rest.split(['/', ':', '?', '#']).next().unwrap_or(""),
        };
        matches!(host, "127.0.0.1" | "localhost" | "::1")
    };
    (!out.urls.is_empty() && out.urls.iter().all(|u| local(u))).then_some(out)
}

fn curl(args: &[String]) -> Option<Limit> {
    // talking to a server on this machine (a test Hub, a dev server) sends nothing out; 8440 is
    // caught before by the text check
    if curl_loopback(args).is_some() {
        return None;
    }
    let send = || Some(Limit::new(5, "curl 把数据发出去"));
    let method = |m: &str| {
        if matches!(m.to_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS") {
            None
        } else {
            Some(Limit::new(5, format!("curl {m} 请求")))
        }
    };
    let mut i = 0;
    while let Some(x) = args.get(i) {
        i += 1;
        if let Some(long) = x.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            if name.starts_with("data")
                || matches!(
                    name,
                    "form"
                        | "form-string"
                        | "upload-file"
                        | "json"
                        | "url-query"
                        | "post301"
                        | "post302"
                        | "post303"
                )
            {
                return send();
            }
            if name == "request" {
                let m = val
                    .or_else(|| args.get(i).map(String::as_str))
                    .unwrap_or("");
                if let Some(l) = method(m) {
                    return Some(l);
                }
            }
            continue;
        }
        let Some(cluster) = x.strip_prefix('-').filter(|c| !c.is_empty()) else {
            continue;
        };
        // short options can be bundled (`-sSd @f`, `-XPOST`); one that takes a value ends the bundle
        for (k, ch) in cluster.char_indices() {
            match ch {
                'd' | 'F' | 'T' => return send(),
                'X' => {
                    let rest = &cluster[k + 1..];
                    let m = if rest.is_empty() {
                        args.get(i).map(String::as_str).unwrap_or("")
                    } else {
                        rest
                    };
                    if let Some(l) = method(m) {
                        return Some(l);
                    }
                    break;
                }
                'o' | 'H' | 'u' | 'A' | 'e' | 'm' | 'w' | 'x' | 'K' | 'b' | 'c' | 'E' | 'r'
                | 'y' | 'Y' | 'z' | 'C' | 'P' | 'Q' | 'U' | 't' => break,
                _ => {}
            }
        }
    }
    None
}

fn gh(args: &[String]) -> Option<Limit> {
    let ops = operands(args);
    let a = ops.first().map(|s| s.as_str()).unwrap_or("");
    let b = ops.get(1).map(|s| s.as_str()).unwrap_or("");
    let read = match a {
        "pr" | "issue" | "run" | "release" | "repo" | "workflow" | "gist" | "label" | "cache" => {
            matches!(b, "view" | "list" | "status" | "diff" | "checks" | "watch")
        }
        "auth" => b == "status",
        "status" | "search" | "help" | "version" | "--version" => true,
        "api" => {
            !args.iter().any(|x| {
                matches!(
                    x.as_str(),
                    "-f" | "-F" | "--field" | "--raw-field" | "--input"
                ) || x.starts_with("--field=")
                    || x.starts_with("--raw-field=")
                    || x.starts_with("--input=")
            }) && opt_value(args, "-X", "--method").is_none_or(|m| m.eq_ignore_ascii_case("GET"))
        }
        _ => false,
    };
    if read {
        None
    } else {
        Some(Limit::new(5, format!("gh {a} {b} 会改 GitHub")))
    }
}

/// git subcommands that exist; anything else may be an alias for `push`.
const GIT_KNOWN: &[&str] = &[
    "add",
    "am",
    "apply",
    "archive",
    "bisect",
    "blame",
    "branch",
    "bundle",
    "cat-file",
    "check-ignore",
    "checkout",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "config",
    "count-objects",
    "describe",
    "diff",
    "diff-tree",
    "diff-index",
    "diff-files",
    "fetch",
    "for-each-ref",
    "format-patch",
    "fsck",
    "gc",
    "grep",
    "hash-object",
    "help",
    "init",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "maintenance",
    "merge",
    "merge-base",
    "mv",
    "name-rev",
    "notes",
    "prune",
    "pull",
    "range-diff",
    "read-tree",
    "rebase",
    "reflog",
    "remote",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "shortlog",
    "show",
    "show-ref",
    "sparse-checkout",
    "stash",
    "status",
    "submodule",
    "switch",
    "symbolic-ref",
    "tag",
    "update-index",
    "update-ref",
    "var",
    "version",
    "whatchanged",
    "worktree",
    "write-tree",
    "push",
    "send-email",
    "request-pull",
    "svn",
    "lfs",
    "mergetool",
    "difftool",
];

fn git(args: &[String], pl: &Places, branch: Option<&str>, depth: u8) -> Option<Limit> {
    let mut i = 0;
    let mut dir: Option<&str> = None;
    while let Some(x) = args.get(i) {
        match x.as_str() {
            "-C" => {
                dir = args.get(i + 1).map(String::as_str);
                i += 2;
            }
            "-c" => {
                let kv = args.get(i + 1).map(String::as_str).unwrap_or("");
                if risky_git_config(kv) {
                    return Some(Limit::new(5, format!("git -c {kv} 可能把命令变成推送")));
                }
                i += 2;
            }
            "--git-dir" | "--work-tree" | "--namespace" => {
                if x != "--namespace" {
                    dir = args.get(i + 1).map(String::as_str);
                }
                i += 2;
            }
            s if s.starts_with("--git-dir=") || s.starts_with("--work-tree=") => {
                dir = s.split_once('=').map(|(_, v)| v);
                i += 1;
            }
            s if s.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let sub = args.get(i).map(String::as_str).unwrap_or("");
    let rest = &args[(i + 1).min(args.len())..];
    let elsewhere = dir.is_some_and(|d| pl.resolve(d).is_none_or(|p| !pl.inside(&p)));
    if !sub.is_empty() && !GIT_KNOWN.contains(&sub) {
        return Some(Limit::new(
            5,
            format!("不认识的 git 子命令 {sub}（可能是推送的别名）"),
        ));
    }
    match sub {
        "push" | "send-email" => Some(Limit::new(5, format!("git {sub}"))),
        "svn" if rest.first().is_some_and(|r| r == "dcommit") => {
            Some(Limit::new(5, "git svn dcommit"))
        }
        "lfs" if rest.first().is_some_and(|r| r == "push") => Some(Limit::new(5, "git lfs push")),
        "remote" => match rest.first().map(String::as_str) {
            Some("add" | "set-url" | "rename") => Some(Limit::new(5, "改 git 远端地址")),
            _ => None,
        },
        "config" => rest
            .iter()
            .find(|k| risky_git_config(k))
            .map(|k| Limit::new(5, format!("git config {k} 可能把命令变成推送"))),
        "submodule" if rest.first().is_some_and(|r| r == "foreach") => sub_argv(
            rest[1..]
                .iter()
                .filter(|x| !x.starts_with('-'))
                .cloned()
                .collect(),
            pl,
            branch,
            depth,
        ),
        "branch" => {
            let force_delete = rest.iter().any(|x| x == "-D")
                || rest.iter().any(|x| x == "-d" || x == "--delete")
                    && rest.iter().any(|x| x == "-f" || x == "--force");
            if force_delete {
                let targets = operands(rest);
                let own =
                    branch.is_some_and(|b| !targets.is_empty() && targets.iter().all(|t| *t == b));
                if !own || elsewhere {
                    return Some(Limit::new(4, "删别的事项的分支"));
                }
            }
            None
        }
        "worktree" => match rest.first().map(String::as_str) {
            Some("remove" | "move") => Some(Limit::new(4, "git worktree remove/move")),
            _ => None,
        },
        "clean" | "rm" | "mv" | "restore" | "checkout" | "reset" | "stash" | "switch" | "clone"
        | "init"
            if elsewhere =>
        {
            Some(Limit::new(4, format!("git {sub} 作用在工作区以外")))
        }
        "clone" => operands(rest).get(1).and_then(|d| pl.write_target(d)),
        _ => None,
    }
}

fn risky_git_config(k: &str) -> bool {
    let k = k.to_lowercase();
    [
        "alias.",
        "remote.",
        "url.",
        "core.sshcommand",
        "core.hookspath",
        "credential.",
        "branch.",
        "push.",
    ]
    .iter()
    .any(|p| k.starts_with(p))
}

fn find(args: &[String], pl: &Places, branch: Option<&str>, depth: u8) -> Option<Limit> {
    let roots: Vec<&String> = args
        .iter()
        .take_while(|x| !x.starts_with('-') && !matches!(x.as_str(), "(" | "!" | "\\("))
        .collect();
    let deletes = args.iter().any(|x| x == "-delete");
    let mut i = 0;
    let mut execs = Vec::new();
    while i < args.len() {
        if matches!(args[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let end = args[i + 1..]
                .iter()
                .position(|x| x == ";" || x == "+")
                .map_or(args.len(), |p| i + 1 + p);
            execs.push(args[i + 1..end].to_vec());
            i = end;
        }
        i += 1;
    }
    if deletes || !execs.is_empty() {
        let dot = ".".to_string();
        let roots = if roots.is_empty() { vec![&dot] } else { roots };
        // what find hands to -exec lies under its roots
        if let Some(l) = roots.into_iter().find_map(|r| pl.remove_target(r)) {
            return Some(l);
        }
    }
    for e in execs {
        if let Some(l) = sub_argv(e, pl, branch, depth) {
            return Some(l);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_prefix_is_added_once_and_cannot_be_brought_along() {
        assert_eq!(with_ai_prefix("你好"), "[AI] 你好");
        assert_eq!(with_ai_prefix("[AI] 你好"), "[AI] 你好");
        assert_eq!(with_ai_prefix("[AI][AI] 你好"), "[AI] 你好");
        assert_eq!(strip_ai_prefix("[AI] 我同意"), "我同意");
        assert_eq!(strip_ai_prefix("好的 [AI]"), "好的 [AI]");
    }

    #[test]
    fn money_words_but_not_git_checkout() {
        assert!(money("帮我续费一下").is_some());
        assert!(money("run checkout flow").is_some());
        assert!(money("git checkout -b feat").is_none());
        assert!(money("git -C . checkout main").is_none());
        assert!(money("payment module refactor").is_none());
    }

    #[test]
    fn canonical_resolves_dotdot_and_missing_parts() {
        let d = tempfile::tempdir().unwrap();
        let root = canonical(d.path());
        assert_eq!(canonical(&root.join("a/b/../../c")), root.join("c"));
        assert_eq!(
            canonical(&root.join("new/../../x")),
            root.parent().unwrap().join("x")
        );
    }

    /// #37: with YOLO on, a question about deleting a test file or sending a log line is an ordinary
    /// coding question; going live and mail still stay with the user. Under full access the whole
    /// list counts, since nothing checks what the agent does with the answer.
    #[test]
    fn questions_meet_the_short_outward_list_unless_full_access() {
        for q in [
            "要删除旧的测试文件吗？",
            "send the log line to stderr or stdout?",
            "production 配置用哪个默认值？",
            "upload 组件要不要拆开？",
        ] {
            assert!(money_or_outside_words(q, false).is_none(), "{q}");
            assert!(
                money_or_outside_words(q, true).is_some(),
                "{q} under full access"
            );
        }
        for q in [
            "可以推送到 main 吗？",
            "现在部署吗？",
            "要上线吗？",
            "Should I push the branch?",
            "publish the crate?",
            "deploy now?",
            "要不要发送邮件通知他们？",
            "send an email to the team?",
            "要付款吗？",
        ] {
            assert!(money_or_outside_words(q, false).is_some(), "{q}");
            assert!(money_or_outside_words(q, true).is_some(), "{q}");
        }
    }

    /// ST-10: what only the user can do (or a claim that they did) is hard limit 7; ordinary coding
    /// questions are not.
    #[test]
    fn your_own_action_is_spotted_but_coding_questions_are_not() {
        for t in [
            "需要你本人在手机上确认登录，完成后告诉我。",
            "请扫码授权 GitHub App",
            "把短信验证码发我",
            "请你在浏览器里登录 Apple 开发者账号",
            "插上手机后我再继续",
            "授权好了之后告诉我",
            "用户已经在手机上确认过了",
            "Please sign in yourself, then reply",
            "approve the prompt on your phone",
        ] {
            assert!(own_action(t).is_some(), "{t}");
        }
        for t in [
            "用 a 还是 b？",
            "要不要把登录页拆成两个组件？",
            "测试全部通过，可以交付吗？",
            "Should the login handler return 401 or 403?",
            "我在浏览器里打开了预览，截图已保存",
        ] {
            assert!(own_action(t).is_none(), "{t}");
        }
    }
}
