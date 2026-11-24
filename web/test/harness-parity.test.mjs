import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync, readdirSync, statSync } from "node:fs"
import { join, relative } from "node:path"
import { checkCounts } from "./support/guard-ratchet.mjs"
/* Harness parity guard (S0), in ratchet mode: the web chooses by what an adapter declares, never by a harness's
 * name. Every place that still names one is listed in harness-parity.txt (file, pattern, count); a new one fails,
 * and so does a listed one that is gone, so the list only shrinks. Comments are left out.
 * STEVE_GUARD_BLESS=1 npm run test:web rewrites the list. */
const root = new URL("..", import.meta.url).pathname
const LIST = join(root, "test/harness-parity.txt")
const NAMES = [
  "claude-code", "claude-agent-acp", "claude-code-acp", "codex", "codex-acp",
  "claude", "claude-cli", "codex-cli", "gemini", "gemini-cli",
  "pi", "openma-pi-acp",
  "github-copilot-cli", "copilot", "opencode", "qwen-code", "qwen", "goose", "goose-acp",
  "kimi-code", "kimi", "grok-build", "grok", "kilo", "factory-droid", "droid", "codebuddy-code",
  "codebuddy", "cursor", "cursor-agent", "kiro", "kiro-cli", "mistral-vibe", "vibe", "vibe-acp", "qoder", "qodercli",
  "antigravity", "agy",
]
const PATTERNS = NAMES.flatMap((n) => [`"${n}"`, `'${n}'`, `\`${n}\``])
const files = (d) => readdirSync(d).flatMap((n) => { const p = join(d, n); return statSync(p).isDirectory() ? files(p) : /\.(ts|tsx)$/.test(n) ? [p] : [] })
const code = (src) => src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`])\/\/.*$/gm, "$1")

test("NAMES covers every built-in adapter id and binary in steve-harness-spec", () => {
  // The built-in catalog moved from steve-acp's Rust table to the declaration crate's TOML.
  const toml = readFileSync(join(root, "../hub-rs/crates/steve-harness-spec/src/builtins.toml"), "utf8")
  // each adapter's own keys come before its first sub-table ([adapters.properties], components, ...)
  const builtins = toml.split(/^\[\[adapters\]\]$/m).slice(1).map((b) => b.split(/^\[/m)[0]).join("\n")
  const ids = [...builtins.matchAll(/^id = "([^"]+)"/gm)].map((m) => m[1])
  const cliBins = [...builtins.matchAll(/^cli_bin = "([^"]+)"/gm)].map((m) => m[1])
  const detectBins = [...builtins.matchAll(/^detect_bins = \[([^\]]*)\]/gm)].flatMap((m) => [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]))
  assert.ok(ids.length >= 18, `expected >= 18 built-in adapters, found ${ids.length}`)
  for (const name of [...ids, ...cliBins, ...detectBins]) {
    assert.ok(NAMES.includes(name), `missing "${name}" in web/test/harness-parity.test.mjs NAMES`)
  }
})

test("harnesses are not told apart by name in the web", () => {
  const now = new Map()
  for (const f of files(join(root, "src"))) {
    const c = code(readFileSync(f, "utf8"))
    for (const p of PATTERNS) {
      const n = c.split(p).length - 1
      if (n) now.set(`${relative(root, f)}\t${p}`, n)
    }
  }
  checkCounts(now, {
    file: LIST, columns: 2,
    header: "# Places in web/src that still tell harnesses apart by name (harness-parity.test.mjs). Only shrinks.\n# Rewrite after removals: STEVE_GUARD_BLESS=1 node --test web/test/harness-parity.test.mjs\n",
    help: "choose by a declared adapter property; removals require STEVE_GUARD_BLESS=1",
  })
})
