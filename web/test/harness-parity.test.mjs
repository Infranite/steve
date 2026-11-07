import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs"
import { join, relative } from "node:path"
/* Harness parity guard (S0), in ratchet mode: the web chooses by what an adapter declares, never by a harness's
 * name. Every place that still names one is listed in harness-parity.txt (file, pattern, count); a new one fails,
 * and so does a listed one that is gone, so the list only shrinks. Comments are left out.
 * STEVE_PARITY_BLESS=1 npm run test:web rewrites the list. */
const root = new URL("..", import.meta.url).pathname
const LIST = join(root, "test/harness-parity.txt")
const PATTERNS = ['"claude-code"', '"codex"', '"claude"', "'claude-code'", "'codex'"]
const files = (d) => readdirSync(d).flatMap((n) => { const p = join(d, n); return statSync(p).isDirectory() ? files(p) : /\.(ts|tsx)$/.test(n) ? [p] : [] })
const code = (src) => src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`])\/\/.*$/gm, "$1")

test("harnesses are not told apart by name in the web", () => {
  const now = new Map()
  for (const f of files(join(root, "src"))) {
    const c = code(readFileSync(f, "utf8"))
    for (const p of PATTERNS) {
      const n = c.split(p).length - 1
      if (n) now.set(`${relative(root, f)}\t${p}`, n)
    }
  }
  const text = [...now].sort(([a], [b]) => (a < b ? -1 : 1)).map(([k, n]) => `${k}\t${n}\n`).join("")
  if (process.env.STEVE_PARITY_BLESS) {
    writeFileSync(LIST, "# Places in web/src that still tell harnesses apart by name (harness-parity.test.mjs). Only shrinks.\n" + text)
    return
  }
  const listed = new Map(readFileSync(LIST, "utf8").split("\n").filter((l) => l && !l.startsWith("#")).map((l) => { const [f, p, n] = l.split("\t"); return [`${f}\t${p}`, Number(n)] }))
  const problems = []
  for (const [k, n] of now) { const was = listed.get(k) ?? 0; if (n > was) problems.push(`${k}: ${n} (listed ${was}): choose by a declared adapter property`); else if (n < was) problems.push(`${k}: ${n}, listed ${was}: lower the list`) }
  for (const [k, was] of listed) if (!now.has(k)) problems.push(`${k}: gone, listed ${was}: drop it`)
  assert.deepEqual(problems, [])
})
