#!/usr/bin/env node
// Contract test: run the same request script against two Hubs side by side (default: the TS Hub and the Rust
// Hub), normalize ids/timestamps/paths, and diff every response (status, key headers, JSON shape and values)
// plus the SSE event sequence. No agents: the ACP adapter is /usr/bin/false (sessions fail to start the same
// way on both sides) and every harness adapter is fake-claude.mjs (canned router/draft/describe replies).
//
// Usage: node contract.mjs [--a ts|rust|<cmd>] [--b ts|rust|<cmd>] [--only <regex>] [--keep] [--verbose] [--json <file>] [--preseed]
//   ts   = node --import tsx hub/src/main.ts in $STEVE_TS_REPO (default ~/Agents/steve, i.e. current main)
//   rust = $STEVE_HUB_BIN (default <repo>/hub-rs/target/debug/steve-hub)
//   --a ts --b ts is the self-check: it must report no differences.
//   --preseed boots the TS Hub once on B's empty STEVE_HOME first (migrations + seed), so a B without
//   the domain layer still starts from the same database; it also checks B opens a TS-created DB.
// Exit code: 0 no differences, 1 differences, 2 a Hub failed to start.
import { spawn } from "node:child_process"
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readFileSync } from "node:fs"
import { tmpdir, homedir } from "node:os"
import { join, dirname, resolve } from "node:path"
import { createServer } from "node:net"
import { fileURLToPath } from "node:url"
import { steps, ROUTES } from "./steps.mjs"

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(here, "../../../../..")
const argv = process.argv.slice(2)
const opt = (k, d) => { const i = argv.indexOf(k); return i >= 0 ? argv[i + 1] : d }
const flag = (k) => argv.includes(k)
const A = opt("--a", "ts"), B = opt("--b", "rust"), only = opt("--only") ? new RegExp(opt("--only")) : null
const verbose = flag("--verbose"), keep = flag("--keep"), jsonOut = opt("--json"), preseed = flag("--preseed")

// ── Hub processes ──
const freePort = () => new Promise((res, rej) => { const s = createServer().listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => res(p)) }); s.on("error", rej) })

function command(kind) {
  if (kind === "ts") { const cwd = process.env.STEVE_TS_REPO ?? join(homedir(), "Agents/steve"); return { cmd: "node", args: ["--import", "tsx", "hub/src/main.ts"], cwd } }
  if (kind === "rust") { const bin = process.env.STEVE_HUB_BIN ?? join(repoRoot, "hub-rs/target/debug/steve-hub"); return { cmd: bin, args: [], cwd: repoRoot } }
  return { cmd: "/bin/sh", args: ["-c", kind], cwd: repoRoot }
}

/** a fake user home with local Claude Code and Codex history, and two project directories */
function fakeHome(root) {
  const h = join(root, "home")
  mkdirSync(join(h, ".claude/projects/-work-alpha"), { recursive: true })
  writeFileSync(join(h, ".claude/projects/-work-alpha/s1.jsonl"), [
    { type: "user", cwd: "/work/alpha", message: { content: "<command-name>/clear</command-name>" } },
    { type: "user", cwd: "/work/alpha", message: { content: "帮我修一下登录页的样式" } },
    { type: "assistant", message: { content: [{ type: "text", text: "好的，我看一下。" }, { type: "tool_use", id: "tu_1", name: "Read", input: { file_path: "/work/alpha/login.css" } }, { type: "tool_use", id: "tu_2", name: "Bash", input: { command: "npm test" } }] } },
    { type: "assistant", message: { content: [{ type: "text", text: "改好了，测试通过。" }] } },
  ].map((x) => JSON.stringify(x)).join("\n") + "\n")
  mkdirSync(join(h, ".codex/sessions/2026/10/31"), { recursive: true })
  writeFileSync(join(h, ".codex/sessions/2026/10/31/rollout-1.jsonl"), [
    { type: "session_meta", payload: { cwd: "/work/beta" } },
    { type: "response_item", payload: { type: "message", role: "user", content: [{ type: "input_text", text: "<environment_context>x</environment_context>" }] } },
    { type: "response_item", payload: { type: "message", role: "user", content: [{ type: "input_text", text: "写一个 README" }] } },
    { type: "response_item", payload: { type: "function_call", call_id: "call_1", name: "shell", arguments: "{\"command\":[\"ls\"]}" } },
    { type: "response_item", payload: { type: "message", role: "assistant", content: [{ type: "output_text", text: "README 写好了。" }] } },
  ].map((x) => JSON.stringify(x)).join("\n") + "\n")
  mkdirSync(join(h, "proj1"), { recursive: true })
  writeFileSync(join(h, "proj1/README.md"), "# proj1\n契约测试用的目录。\n")
  mkdirSync(join(h, "proj2/.git"), { recursive: true })
  mkdirSync(join(h, "proj3"), { recursive: true })
  writeFileSync(join(h, "afile.txt"), "x")
  return h
}

function webDir(root) {
  const w = join(root, "web")
  mkdirSync(join(w, "assets"), { recursive: true })
  writeFileSync(join(w, "index.html"), "<!doctype html><title>steve contract</title>\n")
  writeFileSync(join(w, "assets/app.js"), "console.log('contract')\n")
  // the web release's stamp: /healthz, /api/me and /api/state report it, and it is never served itself
  writeFileSync(join(w, "release.json"), JSON.stringify({ version: "contract-web", commit: "0000000" }) + "\n")
  return w
}

async function startSide(name, kind, seedFirst = false) {
  const root = mkdtempSync(join(tmpdir(), `steve-contract-${name}-`))
  const home = join(root, "steve"), fake = fakeHome(root), web = webDir(root)
  mkdirSync(home, { recursive: true })
  const port = await freePort()
  if (port === 8440) throw new Error("refusing port 8440")
  const env = {
    ...process.env, HOME: fake, STEVE_HOME: home, STEVE_PORT: String(port), STEVE_HOST: "127.0.0.1", STEVE_PUBLIC_URL: `http://127.0.0.1:${port}`,
    STEVE_DEV_LOGIN: "1", STEVE_REPO: "/nonexistent", STEVE_VERSION: "contract", STEVE_VAULT_KEYSTORE: "file", STEVE_FORBID_REAL_HOSTS: "1", STEVE_WEB_DIR: web, CODEX_HOME: join(fake, ".codex"),
    CLAUDE_CODE_EXECUTABLE: join(here, "fake-claude.mjs"), STEVE_ADAPTER_CLAUDE: join(here, "fake-claude.mjs"), STEVE_ADAPTER_CODEX: join(here, "fake-claude.mjs"),
    STEVE_MODELS_FAST: "fake-fast", STEVE_MODELS_SMART: "fake-smart", STEVE_IDLE_RELEASE_MS: "600000",
    // the stand-in adapter answers Steve's own calls; the Rust Hub with its Node off
    STEVE_NODE_MODE: "off",
  }
  for (const k of ["STEVE_PUBLIC_URLS", "STEVE_OIDC_ISSUER", "STEVE_OIDC_CLIENT_ID", "STEVE_OIDC_CLIENT_SECRET", "STEVE_OIDC_CLIENT_SECRET_FILE", "STEVE_DEBUG_ROUTE"]) delete env[k]
  const log = join(root, "hub.log")
  const out = (await import("node:fs")).openSync(log, "a")
  if (seedFirst) {
    const t = command("ts"), p = spawn(t.cmd, t.args, { cwd: t.cwd, env, stdio: ["ignore", out, out] })
    let up = false
    for (let i = 0; i < 120 && !up; i++) { try { up = (await fetch(`http://127.0.0.1:${port}/healthz`)).ok } catch {} if (!up) await new Promise((r) => setTimeout(r, 250)) }
    p.kill("SIGTERM"); await new Promise((r) => p.exitCode !== null ? r() : p.on("exit", r))
    if (!up) throw new Error(`${name}: preseed TS Hub did not start; log ${log}`)
  }
  const { cmd, args, cwd } = command(kind)
  const proc = spawn(cmd, args, { cwd, env, stdio: ["ignore", out, out] })
  const side = { name, kind, root, home, fake, web, port, base: `http://127.0.0.1:${port}`, proc, log, cookie: "", vars: {}, ids: new Map(), counters: {}, exited: null }
  proc.on("exit", (code) => { side.exited = code })
  for (let i = 0; i < 120; i++) {
    if (side.exited !== null) break
    try { const r = await fetch(side.base + "/healthz"); if (r.ok) return side } catch {}
    await new Promise((r) => setTimeout(r, 250))
  }
  throw new Error(`${name} (${kind}) did not become healthy; log ${log}:\n${existsSync(log) ? readFileSync(log, "utf8").slice(-2000) : ""}`)
}

function stopSide(s) {
  if (!s) return
  try { s.proc.kill("SIGTERM") } catch {}
  if (!keep) setTimeout(() => rmSync(s.root, { recursive: true, force: true }), 300)
}

// ── requests ──
const fill = (s, side) => s.replace(/\$\{(\w+)\}/g, (_, k) => { if (!(k in side.vars)) throw new Error(`${side.name}: no saved value ${k}`); return side.vars[k] })
const resolveArg = (x, side) => (typeof x === "function" ? x(side.vars, side) : x)

async function call(side, st) {
  const path = fill(resolveArg(st.path, side), side)
  const headers = { ...(st.headers ?? {}) }
  if (side.cookie && !st.anon) headers.cookie = side.cookie
  if (st.cookieWs) headers.cookie = `${side.cookie}; steve_ws=${side.vars.ws2}`
  if (st.ws) headers["x-steve-ws"] = st.ws === "default" ? "default" : side.vars[st.ws] ?? st.ws
  let body
  if (st.form) body = st.form(side.vars, side)
  else if (st.bytes) body = st.bytes(side.vars, side)
  else if (st.raw !== undefined) body = st.raw
  else if (st.body !== undefined) body = JSON.stringify(resolveArg(st.body, side), (_, v) => (typeof v === "string" ? v.replace(/\$\{(\w+)\}/g, (m, k) => side.vars[k] ?? m) : v))
  if (body !== undefined && !st.form && !Object.keys(headers).some((h) => h.toLowerCase() === "content-type")) headers["content-type"] = "application/json"
  const r = await fetch(side.base + path, { method: st.method ?? "GET", headers, body, redirect: "manual" })
  const text = await r.text()
  const ct = r.headers.get("content-type") ?? ""
  let json
  if (ct.includes("json")) { try { json = JSON.parse(text) } catch { json = undefined } }
  const setCookie = r.headers.getSetCookie?.() ?? []
  if (st.login) { const c = setCookie.find((x) => x.startsWith("steve_session=")); if (c) side.cookie = c.split(";")[0] }
  if (st.logout) side.cookie = ""
  const extra = Object.fromEntries(["content-disposition", "x-content-type-options", "content-security-policy", "cross-origin-resource-policy", ...(st.length ? ["content-length"] : [])].map((h) => [h, r.headers.get(h)]))
  return { status: r.status, headers: { "content-type": ct, "cache-control": r.headers.get("cache-control"), location: r.headers.get("location"), ...extra, "set-cookie": setCookie.map((c) => c.replace(/=[^;]*/, "=<v>")) }, json, text: json === undefined ? text : undefined }
}

const get = (o, path) => path.split(".").reduce((x, k) => (x == null ? undefined : x[k]), o)

// ── normalization ──
const ID = /\b(ws|usr|prj|bot|mem|tag|chat|art|ses|q|dec|msg|chg|ro|t|fil)_[0-9a-f]{16}\b/g
function normId(side, id, p) {
  if (!side.ids.has(id)) { side.counters[p] = (side.counters[p] ?? 0) + 1; side.ids.set(id, `<${p}#${side.counters[p]}>`) }
  return side.ids.get(id)
}
function normStr(side, s) {
  let x = s.split(side.home).join("$STEVE_HOME").split(side.fake).join("$HOME").split(side.root).join("$ROOT").split(String(side.port)).join("$PORT")
  return x.replace(ID, (m, p) => normId(side, m, p))
}
/** keys holding durations measured by the Hub (meta.steps.*.ms): their values are timing, not contract */
const DURATIONS = new Set(["ms"])
function norm(side, v, key = "") {
  if (typeof v === "string") return normStr(side, v)
  if (typeof v === "number") return DURATIONS.has(key) ? "<duration>" : v >= 1.5e12 && v <= 2.5e12 ? "<ms>" : v
  if (Array.isArray(v)) return v.map((x) => norm(side, x))
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [normStr(side, k), norm(side, x, k)]))
  return v
}

/** Additions the Rust Hub makes on purpose (not in the TS Hub yet): the `tz` (schedules), `keepRaw` (signals) and
 * `keepMail` (mail) workspace settings. */
const EXTRA_SETTINGS = ["tz", "keepRaw", "keepMail"]
function stripExtensions(v) {
  if (Array.isArray(v)) return v.filter((x) => !(x && typeof x === "object" && EXTRA_SETTINGS.includes(x.key) && Array.isArray(x.values))).map(stripExtensions)
  if (v && typeof v === "object") {
    const o = Object.fromEntries(Object.entries(v).map(([k, x]) => [k, stripExtensions(x)]))
    if ("lang" in o) for (const k of EXTRA_SETTINGS) delete o[k]
    return o
  }
  return v
}

// ── diff ──
const UNORDERED_KEYS = [".tagUses"]
function diff(a, b, path, out, tolerate) {
  if (tolerate.some((t) => path === t || path.startsWith(t + ".") || path.startsWith(t + "["))) return
  if (typeof a !== typeof b || Array.isArray(a) !== Array.isArray(b) || (a === null) !== (b === null)) { out.push(`${path}: ${show(a)} ≠ ${show(b)}`); return }
  if (Array.isArray(a)) {
    if (a.length !== b.length) out.push(`${path}: length ${a.length} ≠ ${b.length}`)
    for (let i = 0; i < Math.min(a.length, b.length); i++) diff(a[i], b[i], `${path}[${i}]`, out, tolerate)
    return
  }
  if (a && typeof a === "object") {
    const ka = Object.keys(a), kb = Object.keys(b)
    for (const k of ka) if (!(k in b)) out.push(`${path}.${k}: only in A (${show(a[k])})`)
    for (const k of kb) if (!(k in a)) out.push(`${path}.${k}: only in B (${show(b[k])})`)
    for (const k of ka) if (k in b) diff(a[k], b[k], `${path}.${k}`, out, tolerate)
    const ca = ka.filter((k) => k in b), cb = kb.filter((k) => k in a)
    // GROUP BY over random ids: key order is not part of the contract there
    if (ca.join() !== cb.join() && !UNORDERED_KEYS.some((u) => path.endsWith(u))) out.push(`~order ${path}: [${ca.join(",")}] vs [${cb.join(",")}]`)
    return
  }
  if (a !== b) out.push(`${path}: ${show(a)} ≠ ${show(b)}`)
}
const show = (x) => { const s = JSON.stringify(x); return s === undefined ? "undefined" : s.length > 120 ? s.slice(0, 117) + "…" : s }

// ── SSE ──
function openStream(side, ws) {
  const ctl = new AbortController(), events = []
  const headers = { cookie: side.cookie }
  if (ws) headers["x-steve-ws"] = side.vars[ws] ?? ws
  const done = (async () => {
    try {
      const r = await fetch(side.base + "/api/stream", { headers, signal: ctl.signal })
      events.headers = { status: r.status, "content-type": r.headers.get("content-type"), "cache-control": r.headers.get("cache-control") }
      const dec = new TextDecoder(); let buf = ""
      for await (const chunk of r.body) {
        buf += dec.decode(chunk, { stream: true })
        let i
        while ((i = buf.indexOf("\n\n")) >= 0) { const frame = buf.slice(0, i); buf = buf.slice(i + 2); events.push(frame) }
      }
    } catch {}
  })()
  return { close: async () => { await new Promise((r) => setTimeout(r, 400)); ctl.abort(); await done; return events } }
}
function frames(side, list) {
  return list.filter((f) => !f.startsWith("event: ping")).map((f) => { const d = f.replace(/^data: /, ""); try { return norm(side, JSON.parse(d)) } catch { return f } })
}

// ── run ──
const results = []
let sides
const streams = {}
async function waitBoth(st) {
  const until = st.until, timeout = st.timeout ?? 20000
  await Promise.all(sides.map(async (side) => {
    const t0 = Date.now()
    for (;;) {
      const r = await call(side, st)
      if (until(r.json, side.vars)) return
      if (Date.now() - t0 > timeout) throw new Error(`${side.name}: timed out waiting for ${st.name}`)
      await new Promise((res) => setTimeout(res, 200))
    }
  }))
}

async function runStep(st) {
  if (st.setup) { for (const side of sides) await st.setup(side); return }
  if (st.sleep) { await new Promise((r) => setTimeout(r, st.sleep)); return }
  if (st.sse === "open") { streams[st.id] = sides.map((s) => openStream(s, st.ws)); await new Promise((r) => setTimeout(r, 300)); return }
  if (st.sse === "close") {
    const got = await Promise.all(streams[st.id].map((x) => x.close()))
    const [fa, fb] = got.map((g, i) => frames(sides[i], g))
    const out = []
    diff(got[0].headers, got[1].headers, "headers", out, [])
    const key = (x) => JSON.stringify(x)
    diff(fa, fb, "events", out, st.tolerate ?? [])
    if (out.some((d) => d.startsWith("events")) && JSON.stringify([...fa].map(key).sort()) === JSON.stringify([...fb].map(key).sort())) {
      for (let i = out.length - 1; i >= 0; i--) if (out[i].startsWith("events")) out.splice(i, 1)
      out.push("~order events: same events, different order")
    }
    record(st, out, { A: fa, B: fb })
    return
  }
  if (st.until) await waitBoth(st)
  const [ra, rb] = await Promise.all(sides.map((s) => call(s, st)))
  for (const [side, r] of [[sides[0], ra], [sides[1], rb]]) {
    if (st.save && r.json) for (const [k, p] of Object.entries(st.save)) { const v = typeof p === "function" ? p(r.json, side.vars) : get(r.json, p); if (v !== undefined) side.vars[k] = v }
  }
  const na = { status: ra.status, headers: norm(sides[0], ra.headers), body: norm(sides[0], ra.json ?? ra.text) }
  const nb = { status: rb.status, headers: norm(sides[1], rb.headers), body: stripExtensions(norm(sides[1], rb.json ?? rb.text)) }
  na.body = stripExtensions(na.body)
  // rows without an ORDER BY (DISTINCT over random ids): compare as sets
  if (st.unordered) for (const n of [na, nb]) if (Array.isArray(n.body)) n.body.sort((x, y) => JSON.stringify(x).localeCompare(JSON.stringify(y)))
  const out = []
  diff(na, nb, "", out, (st.tolerate ?? []).map((t) => "." + t))
  if (st.expect !== undefined && ra.status !== st.expect) out.push(`!A status ${ra.status}, script expects ${st.expect}`)
  record(st, out, { A: na, B: nb })
}

function record(st, out, got) {
  const real = out.filter((d) => !d.startsWith("~")), order = out.filter((d) => d.startsWith("~"))
  // a Rust backend method that isn't wired yet: not a contract difference of the HTTP layer
  const missing = [...new Set(JSON.stringify(got).match(/not implemented in the Rust Hub yet: [\w ]+/g) ?? [])].map((m) => m.replace("not implemented in the Rust Hub yet: ", ""))
  results.push({ blocked: real.length && missing.length ? missing : undefined, name: st.name, route: st.route ?? (st.sse ? "GET /api/stream" : routeOf(st)), ok: real.length === 0, diffs: real, order, got })
  const mark = real.length ? (missing.length ? "○" : "✗") : order.length ? "≈" : "✓"
  console.log(`${mark} ${st.name}`)
  for (const d of real.slice(0, verbose ? 200 : 12)) console.log("    " + d)
  if (real.length > 12 && !verbose) console.log(`    … ${real.length - 12} more (--verbose)`)
  if (verbose) for (const d of order) console.log("    " + d)
}

function routeOf(st) {
  if (!st.path) return null
  const m = st.method ?? "GET", p = (typeof st.path === "function" ? "/x" : st.path).split("?")[0]
  const hit = ROUTES.find(([rm, rp]) => rm === m && new RegExp("^" + rp.replace(/:\w+/g, "[^/]+").replace(/\*/g, ".*") + "$").test(p.replace(/\$\{\w+\}/g, "id")))
  return hit ? `${hit[0]} ${hit[1]}` : null
}

try {
  sides = await Promise.all([startSide("A", A), startSide("B", B, preseed)])
} catch (e) { console.error(String(e.message ?? e)); for (const s of sides ?? []) stopSide(s); process.exit(2) }
console.log(`A = ${A} on ${sides[0].base} (${sides[0].root})\nB = ${B} on ${sides[1].base} (${sides[1].root})\n`)
try {
  for (const st of steps) {
    if (only && !only.test(st.name ?? "") && !st.setup && !st.always) continue
    try { await runStep(st) } catch (e) {
      const msg = String(e.message ?? e), dep = /no saved value/.test(msg)
      results.push({ name: st.name, route: st.route ?? (st.sse ? "GET /api/stream" : routeOf(st)), ok: false, blocked: dep ? ["an earlier step"] : undefined, diffs: ["error: " + msg] })
      console.log(`${dep ? "○" : "!"} ${st.name}: ${msg}`)
    }
  }
} finally {
  for (const s of sides) stopSide(s)
}

const bad = results.filter((r) => !r.ok), covered = new Set(results.map((r) => r.route).filter(Boolean))
const uncovered = ROUTES.filter(([m, p]) => !covered.has(`${m} ${p}`))
const blocked = bad.filter((r) => r.blocked), differ = bad.filter((r) => !r.blocked)
console.log(`\n${results.length - bad.length}/${results.length} steps identical, ${differ.length} differ, ${blocked.length} blocked on unwired backend methods (○), ${results.filter((r) => r.order?.length).length} with key/event order differences`)
const need = {}
for (const r of blocked) for (const m of r.blocked) need[m] = (need[m] ?? 0) + 1
if (blocked.length) console.log(`blocked by: ${Object.entries(need).sort((a, b) => b[1] - a[1]).map(([m, n]) => `${m} (${n})`).join(", ")}`)
console.log(`route coverage: ${ROUTES.length - uncovered.length}/${ROUTES.length}` + (uncovered.length ? `; not exercised: ${uncovered.map(([m, p]) => m + " " + p).join(", ")}` : ""))
const perRoute = {}
for (const r of results) if (r.route) (perRoute[r.route] ??= []).push(r.ok)
const failing = Object.entries(perRoute).filter(([, oks]) => oks.some((x) => !x)).map(([k]) => k)
if (failing.length) console.log(`routes with differences (${failing.length}): ${failing.join(", ")}`)
if (jsonOut) writeFileSync(jsonOut, JSON.stringify({ a: A, b: B, results, uncovered }, null, 1))
if (keep) console.log(`kept: ${sides.map((s) => s.root).join(" ")}`)
process.exit(bad.length ? 1 : 0)
