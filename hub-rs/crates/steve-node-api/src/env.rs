//! A user's own environment variables on a Node (`node_env`, path model §4.5.3): which keys they
//! may set. Steve's own keys are never theirs, nor what Steve sets for every run itself (`HOME`,
//! `TMPDIR`, `PATH`, `XDG_*`, git's global config), nor what loads code into another process
//! (`DYLD_*`, `LD_*`, `NODE_OPTIONS`, a shell's or interpreter's start-up file), nor what steers a
//! harness itself (its config directory, its model endpoint and key: those come with the
//! principal's own harness credentials, not as plain variables). `PATH_PREPEND` adds directories in
//! front of `PATH`. Checked by the Hub when a user saves one and again by the Node when it starts a
//! harness.

/// The key that puts directories in front of `PATH` (the only way to change it).
pub const PATH_PREPEND: &str = "PATH_PREPEND";

const LOCKED: [&str; 22] = [
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "PATH",
    "SHELL",
    "USER",
    "LOGNAME",
    "PWD",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "CLAUDE_CODE_EXECUTABLE",
    "DISABLE_AUTOUPDATER",
    "GH_TOKEN",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "PERL5OPT",
    "RUBYOPT",
    "PYTHONSTARTUP",
    "PYTHONINSPECT",
];

const LOCKED_PREFIXES: [&str; 33] = [
    "STEVE_",
    "XDG_",
    "DYLD_",
    "LD_",
    "GIT_CONFIG_",
    "NODE_",
    "CLAUDE_",
    "CODEX_",
    "ANTHROPIC_",
    "OPENAI_",
    "GEMINI_",
    "GOOGLE_",
    "COPILOT_",
    "GITHUB_COPILOT_",
    "OPENCODE_",
    "QWEN_",
    "DASHSCOPE_",
    "GOOSE_",
    "KIMI_",
    "MOONSHOT_",
    "GROK_",
    "XAI_",
    "KILO_",
    "KILOCODE_",
    "FACTORY_",
    "DROID_",
    "CODEBUDDY_",
    "CURSOR_",
    "KIRO_",
    "VIBE_",
    "MISTRAL_",
    "QODER_",
    "OPENROUTER_",
];

/// Why `key` is not one a user may set (`None`: it is).
pub fn user_env_refusal(key: &str) -> Option<&'static str> {
    let mut cs = key.chars();
    let well_formed = key.len() <= 128
        && cs
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !well_formed {
        return Some("letters, digits and _ only, not starting with a digit, up to 128");
    }
    let up = key.to_ascii_uppercase();
    if LOCKED.contains(&up.as_str()) {
        return Some("Steve sets it for every run itself");
    }
    if LOCKED_PREFIXES.iter().any(|p| up.starts_with(p)) || up.ends_with("_FILE") {
        return Some("reserved for Steve");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_users_keys_and_steves() {
        for ok in [
            "NPM_TOKEN",
            "CARGO_REGISTRIES_X_TOKEN",
            "GOPRIVATE",
            "_X",
            PATH_PREPEND,
        ] {
            assert_eq!(user_env_refusal(ok), None, "{ok}");
        }
        for no in [
            "HOME",
            "path",
            "STEVE_HOME",
            "XDG_CONFIG_HOME",
            "DYLD_INSERT_LIBRARIES",
            "LD_PRELOAD",
            "GIT_CONFIG_COUNT",
            "NODE_OPTIONS",
            "BASH_ENV",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_BASE_URL",
            "OPENAI_API_KEY",
            "GEMINI_CLI_HOME",
            "GOOGLE_API_KEY",
            "COPILOT_HOME",
            "OPENCODE_CONFIG_DIR",
            "QWEN_HOME",
            "GOOSE_PATH_ROOT",
            "KIMI_CODE_HOME",
            "GROK_HOME",
            "KILO_CONFIG_DIR",
            "FACTORY_HOME_OVERRIDE",
            "CODEBUDDY_CONFIG_DIR",
            "CURSOR_CONFIG_DIR",
            "KIRO_HOME",
            "VIBE_HOME",
            "MISTRAL_API_KEY",
            "QODER_CONFIG_DIR",
            "KEY_FILE",
            "1X",
            "A-B",
            "",
        ] {
            assert!(user_env_refusal(no).is_some(), "{no}");
        }
    }
}
