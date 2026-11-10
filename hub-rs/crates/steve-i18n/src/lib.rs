//! The language of the text the Hub shows a person, and the `t!()` macro that looks it up.
//!
//! The web sends `Accept-Language` with the language the user picked (zh-CN or en) on every API call, and
//! `?lang=` on the SSE stream, which can't carry a header. steve-hub's `locale` middleware reads it and runs the
//! request inside [`scope`]; [`current`] answers that language anywhere below it, and zh-CN outside a request.
//! zh-CN is the source language, the default and the fallback.
//!
//! A command-line program has no request: it picks its language once at start, [`set_process`] with
//! [`from_env`] (`STEVE_LANG`, then the locale variables, then the system's languages), and [`current`]
//! answers that everywhere outside a request.
//!
//! # Adding text to a crate
//!
//! 1. Depend on both crates (rust-i18n's macros expand to `rust_i18n::…` paths, so the crate needs it directly):
//!    ```toml
//!    steve-i18n = { path = "../steve-i18n" }
//!    rust-i18n = "=4.2.4"
//!    ```
//! 2. Once, at the top of the crate's `lib.rs` (not in a module): `steve_i18n::init!();`. It loads the crate's
//!    `locales/zh-CN.json` and `locales/en.json` at compile time and rebuilds the crate when either changes.
//! 3. Put each string in both files, flat keys, one object per file, sorted as you like:
//!    ```json
//!    { "hub.tag.duplicate": "已有同名标签", "hub.error.invalid": "参数不对：%{fields}" }
//!    ```
//! 4. Call `steve_i18n::t!("hub.tag.duplicate")` or `steve_i18n::t!("hub.error.invalid", fields = list)`.
//!    It returns a `String` in the request's language.
//!
//! # Conventions
//!
//! - Keys read `<crate>.<area>.<what>`: `<crate>` is the directory name without `steve-` (`hub`, `core`,
//!   `node-api`); area and what are lower_snake_case English that say what the text is for, not what it says.
//! - Keep the zh-CN text byte-identical to what the Hub sends today (full-width punctuation, "、" lists, the spaces
//!   around Latin words such as "没有这个 Bot"). Moving a string into a file must not change what a zh-CN user sees.
//! - Values go in by name, `%{name}`, with any `Display` value. Never glue translated fragments together
//!   (`format!("没有这个{}", t!(...))`): give the whole sentence one key with placeholders. A list separator is a
//!   key of its own (`hub.error.list_sep`).
//! - A key missing from en falls back to zh-CN at run time, but `tests/catalogs.rs` fails on any key in one
//!   language and not the other, on placeholders that differ, on a key without its crate prefix, and on a
//!   `t!("…")` in `src/` whose key isn't in the crate's zh-CN file. Run `cargo test -p steve-i18n`.
//! - Work off the async runtime loses the task-local: wrap the closure with [`carry`] before `spawn_blocking`,
//!   and a spawned future with [`bind`], when they build text for the response.
//!
//! # What stays Chinese (never through `t!`)
//!
//! Prompts and anything else a model reads (tool results included: `/mcp` always runs in zh-CN), text sent to
//! IM / Feishu / mail / GitHub recipients, regexes and word lists that read what users type, values stored and
//! compared later (status, stage, settings values), log lines, and text written to the database (timeline,
//! events, system entries, fail cards). Text made by background work with no request stays zh-CN for now. A
//! string used both by a model and by the UI is split into two before the UI copy gets a key.
//!
//! Text that goes through `t!` but is built inside a request and then stored for every member to read is built
//! under [`stored`], so it is zh-CN too: a fail card made from a request's error (`开不了会话：…`), and the
//! title and preview lines of a conversational-config draft (`changesets`), which other members see in the
//! changeset list and on decision cards. Such a draft comes back in zh-CN to the person who asked for it as well.

use std::future::Future;

pub use rust_i18n;

/// The source language, the default and the fallback.
pub const ZH_CN: &str = "zh-CN";
pub const EN: &str = "en";
/// Every language with a locale file, source first.
pub const LOCALES: [&str; 2] = [ZH_CN, EN];

tokio::task_local! {
    static LOCALE: &'static str;
}

static PROCESS: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The language of the current request; outside one, the process's ([`set_process`]), else zh-CN.
pub fn current() -> &'static str {
    LOCALE
        .try_with(|l| *l)
        .unwrap_or_else(|_| PROCESS.get().copied().unwrap_or(ZH_CN))
}

/// The language of everything this process says outside a request: for a command-line program, once
/// at start. The first call decides.
pub fn set_process(locale: &'static str) {
    let _ = PROCESS.set(locale);
}

/// The language a person at this terminal reads: `STEVE_LANG`, else the first of `LC_ALL`,
/// `LC_MESSAGES` and `LANG` that names one of ours, else (macOS) the system's preferred languages in
/// order, else zh-CN.
pub fn from_env() -> &'static str {
    for name in ["STEVE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(l) = std::env::var(name).ok().as_deref().and_then(match_tag) {
            return l;
        }
    }
    system_languages()
        .iter()
        .find_map(|tag| match_tag(tag))
        .unwrap_or(ZH_CN)
}

/// What System Settings lists under preferred languages; nothing elsewhere, where the locale
/// variables are the setting.
fn system_languages() -> Vec<String> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let Ok(out) = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .split(['(', ')', ',', '\n'])
        .map(|tag| tag.trim().trim_matches('"').to_owned())
        .filter(|tag| !tag.is_empty())
        .collect()
}

/// Runs `f` with `locale` as the current language.
pub async fn scope<F: Future>(locale: &'static str, f: F) -> F::Output {
    LOCALE.scope(locale, f).await
}

/// `f` with the current language, for a closure that runs on another thread (`spawn_blocking`).
pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    let locale = current();
    move || LOCALE.sync_scope(locale, f)
}

/// `f` with the current language, for a future that is spawned on its own.
pub fn bind<F: Future>(f: F) -> impl Future<Output = F::Output> {
    LOCALE.scope(current(), f)
}

/// Runs `f` in zh-CN whatever the request's language: for text built inside a request and written to the
/// database (a fail card from a request's error, a stored draft), which every member reads.
pub fn stored<R>(f: impl FnOnce() -> R) -> R {
    LOCALE.sync_scope(ZH_CN, f)
}

/// One BCP 47 tag to one of ours: any Chinese is zh-CN, any English is en.
pub fn match_tag(tag: &str) -> Option<&'static str> {
    let t = tag.trim().to_ascii_lowercase();
    let primary = t.split(['-', '_']).next().unwrap_or_default();
    match primary {
        "zh" => Some(ZH_CN),
        "en" => Some(EN),
        _ => None,
    }
}

/// An `Accept-Language` value to our language: the supported tag with the highest q (ties in order), else zh-CN.
/// `q=0` means "not this one".
pub fn from_accept_language(header: &str) -> &'static str {
    let mut best: Option<(&'static str, f32)> = None;
    for part in header.split(',') {
        let mut it = part.split(';');
        let tag = it.next().unwrap_or_default();
        let q = it
            .find_map(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .map(|v| v.trim().parse::<f32>().unwrap_or(0.0))
            })
            .unwrap_or(1.0);
        if q <= 0.0 {
            continue;
        }
        if let Some(l) = match_tag(tag)
            && best.is_none_or(|(_, b)| q > b)
        {
            best = Some((l, q));
        }
    }
    best.map_or(ZH_CN, |(l, _)| l)
}

/// Loads the calling crate's `locales/zh-CN.json` and `locales/en.json` (call once at the crate root).
#[macro_export]
macro_rules! init {
    () => {
        rust_i18n::i18n!("locales", fallback = "zh-CN");
        // rust-i18n reads the files in a proc macro, which cargo doesn't track: these make an edit rebuild the crate
        const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/locales/zh-CN.json"));
        const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/locales/en.json"));
    };
}

/// The text of `key` in the current language as a `String`: `t!("hub.tag.duplicate")`,
/// `t!("hub.error.invalid", fields = list)`.
#[macro_export]
macro_rules! t {
    ($key:literal $(, $name:ident = $val:expr)* $(,)?) => {
        ::std::string::String::from(
            rust_i18n::t!($key, locale = $crate::current() $(, $name = $val)*)
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_language() {
        assert_eq!(from_accept_language(""), ZH_CN);
        assert_eq!(from_accept_language("en"), EN);
        assert_eq!(from_accept_language("en-US,en;q=0.9"), EN);
        assert_eq!(from_accept_language("zh-CN"), ZH_CN);
        assert_eq!(from_accept_language("zh-Hans-CN, en;q=0.5"), ZH_CN);
        assert_eq!(from_accept_language("fr-FR, en;q=0.8, zh;q=0.9"), ZH_CN);
        assert_eq!(from_accept_language("fr, de;q=0.5"), ZH_CN);
        assert_eq!(from_accept_language("en;q=0, zh"), ZH_CN);
        assert_eq!(from_accept_language("*"), ZH_CN);
        assert_eq!(match_tag("EN_gb"), Some(EN));
        assert_eq!(match_tag("eng"), None);
    }

    #[tokio::test]
    async fn the_language_follows_the_scope_and_threads() {
        assert_eq!(current(), ZH_CN);
        scope(EN, async {
            assert_eq!(current(), EN);
            let off = tokio::task::spawn_blocking(carry(current)).await.unwrap();
            assert_eq!(off, EN);
            let lost = tokio::task::spawn_blocking(current).await.unwrap();
            assert_eq!(lost, ZH_CN);
            assert_eq!(tokio::spawn(bind(async { current() })).await.unwrap(), EN);
            assert_eq!(stored(current), ZH_CN);
            assert_eq!(current(), EN);
        })
        .await;
    }
}
