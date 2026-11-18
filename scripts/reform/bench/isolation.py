"""Shared disabled adapter commands for every isolated acceptance tool."""
ADAPTERS = "CLAUDE CODEX PI GEMINI COPILOT OPENCODE QWEN GOOSE KIMI GROK KILO DROID CODEBUDDY CURSOR KIRO VIBE QODER".split()
DISABLED_ENV = {**{f"STEVE_ADAPTER_{name}": "/usr/bin/false" for name in ADAPTERS},
                "STEVE_UPDATE_CHECK": "0"}
