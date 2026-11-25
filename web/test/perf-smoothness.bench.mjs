// Real-usage frontend smoothness benchmark (sustained streaming, cards, long-list scrolling).
// Serves the production web/dist bundle over loopback HTTP + real SSE + fixed deterministic fixtures.
// Run via `npm run build:web && npm run bench:web:smoothness` (not part of CI default tests).
// Never touches ~/.steve*, port 8440, or any external host.
import assert from "node:assert/strict"
import { existsSync } from "node:fs"
import { readFile } from "node:fs/promises"
import { createServer } from "node:http"
import { loadavg, platform } from "node:os"
import { extname, join, resolve } from "node:path"
import { setTimeout as sleep } from "node:timers/promises"
import { fileURLToPath } from "node:url"

const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const root = resolve(fileURLToPath(new URL("../..", import.meta.url)))
const distDir = resolve(root, "web/dist")
assert.ok(existsSync(join(distDir, "index.html")), "web/dist/index.html not found; run `npm run build:web` first")
const streamSeconds = Number(process.env.PERF_STREAM_S ?? 120)
const transcriptSeconds = Number(process.env.PERF_TRANSCRIPT_S ?? 30)
const only = process.env.PERF_ONLY ? new Set(process.env.PERF_ONLY.split(",")) : null

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".json": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
}

const chapter = [
  "### Performance verification summary",
  "",
  "We inspected the **rendering pipeline** across streaming output, rich item cards, and long scroll containers.",
  "Each frame should keep main-thread work strictly bounded so scrolling and typing remain immediate.",
  "",
  "- Measure real workloads with long histories and concurrent patches.",
  "- Keep completed turns static while only the active streaming reply parses incrementally.",
  "- Avoid full-table cloning and `JSON.stringify` equality checks on hot paths.",
  "",
  "```typescript",
  "export function summarize(samples: number[]): { p50: number; p95: number } {",
  "  const sorted = [...samples].sort((a, b) => a - b);",
  "  return { p50: sorted[Math.floor(sorted.length * 0.5)], p95: sorted[Math.floor(sorted.length * 0.95)] };",
  "}",
  "```",
  "",
  "| Metric | Target | Notes |",
  "| --- | --- | --- |",
  "| Scroll p95 | < 16 ms | 60 fps budget |",
  "| Long tasks | ~0 | > 50 ms main thread |",
  "",
].join("\n") + "\n"

const streamTokens = chapter.repeat(90).match(/\s+|\p{L}+|[^\s\p{L}]/gu)
const WS = "ws-perf"
const NOW = Date.UTC(2026, 10, 25, 0, 0, 0)

// Build fixed deterministic datasets
function buildFixtures() {
  const items = Array.from({ length: 40 }, (_, i) => {
    const id = `ST-${100 + i}`
    return {
      id, ws: WS, no: 100 + i, title: `Frontend smoothness task ${i + 1}`,
      bot_id: i % 2 ? "dev" : "ops",
      state: i < 20 ? { kind: "active" } : { kind: "closed", outcome: "finished", by: "owner" },
      cycle: 1, revision: 1, archived: false, star: i % 5 === 0,
      priority: i % 3 === 0 ? "高" : "中", accept_mode: "manual", design_gate: false,
      project_id: "proj-1", source: "chat", created: NOW - (40 - i) * 600_000, updated: NOW - (40 - i) * 60_000,
      goals: 1, tags: ["tag-feat"], deps: [], waits: [], op_blockers: [], delivery: [],
      plan_done: i % 4, plan_total: 4, has_branch: false,
    }
  })

  const sessions = Array.from({ length: 30 }, (_, i) => {
    const id = i === 0 ? "sess-live" : `sess-${i}`
    return {
      id, ws: WS, no: i + 1, item_id: `ST-${100 + (i % 20)}`, bot_id: i % 2 ? "dev" : "ops",
      harness: "claude-code", model: "claude-sonnet-4-6", cwd: "/repo",
      status: i === 0 ? "running" : i < 5 ? "idle" : "ended",
      title: i === 0 ? "Long streaming session perf fixture" : `Historical session ${i}`,
      device_id: "hub", source: "item", readonly: false, busy: i === 0, queued: 0,
      used: 48000, size: 200000, created: NOW - (30 - i) * 300_000, updated: NOW - (30 - i) * 30_000,
    }
  })

  const decisions = Array.from({ length: 20 }, (_, i) => ({
    id: `dec-${i}`, ws: WS, kind: i % 2 === 0 ? "accept" : "approve",
    subject_type: "item", subject_id: `ST-${100 + i}`,
    item_id: `ST-${100 + i}`, session_id: `sess-${i + 1}`,
    question: { kind: "raw", text: `Review deliverable for ST-${100 + i}?` },
    draft: `## Delivery for ST-${100 + i}\n\nCompleted implementation and verified all unit checks.\n\n- Updated module ${i}\n- Verified scroll smoothness`,
    options: ["yes", "no"], status: "open", result: null, requested_by: "dev",
    created: NOW - (20 - i) * 120_000, resolved: null, undo_until: null, tier: "decide", revision: 1,
  }))

  const toolsFixture = Array.from({ length: 12 }, (_, j) => ({
    name: j % 3 ? "Read" : "Bash", kind: j % 3 ? "read" : "exec", call: j,
    args: { brief: `Inspect component ${j}` },
    result: { lines: Array.from({ length: 40 }, (_, k) => `line ${k}: ok`) },
    ms: 350 + j * 40, status: "ok",
  }))

  // 520 messages in main chat, with 20 rich ItemCards (16 spread across history + 4 near bottom so cards are live during streaming)
  const cardIndices = new Map()
  for (let c = 0; c < 16; c++) cardIndices.set((c + 1) * 30, c)
  for (let c = 0; c < 4; c++) cardIndices.set(511 + c * 2, 16 + c)
  const messages = Array.from({ length: 520 }, (_, i) => {
    const cardIdx = cardIndices.has(i) ? cardIndices.get(i) : -1
    const isCard = cardIdx >= 0
    const isSteve = i % 2 === 1 || isCard
    let text = isSteve
      ? (i % 10 === 1 ? chapter : `Response #${i}: Checked module **${i % 20}** with \`bench_${i}\` and verified invariants.\n\n- Step A done\n- Step B done`)
      : `Question #${i}: Can you check the performance of component ${i % 15} under sustained load?`
    let meta = null
    if (isCard) {
      const itemId = `ST-${100 + cardIdx}`
      text = `Started work on ${itemId}.`
      meta = {
        card: {
          item: itemId,
          state: cardIdx % 2 === 0 ? "deliver" : "run",
          at: NOW - (520 - i) * 10_000,
          delivery: {
            text: `Delivered **${itemId}** with full benchmark verification and updated metrics.`,
            decision: `dec-${cardIdx}`,
            at: NOW - (520 - i) * 10_000,
          },
          reply: {
            text: `Verified \`${itemId}\` across desktop and mobile viewports.`,
            at: NOW - (520 - i) * 9_000,
          },
        },
        steps: {
          ms: 5400,
          research: { run: `hist-run-${cardIdx}`, model: "fast", ms: 5400, turns: 2, tools: 12 },
          tools: toolsFixture,
        },
      }
    }
    return {
      id: `m-${String(i).padStart(4, "0")}`,
      ws: WS,
      author: isSteve ? "steve" : "me",
      item_id: isCard ? `ST-${100 + cardIdx}` : null,
      text,
      created: NOW - (520 - i) * 15_000,
      meta,
    }
  })

  // Item ST-100 detail with 520 timeline entries (long activity stream!)
  const itemTimeline = []
  for (let i = 1; i <= 520; i++) {
    const ts = NOW - (520 - i) * 60_000
    if (i % 12 === 1) {
      itemTimeline.push({
        id: i, ts, actor: "me", kind: "me", session_id: null, via: "web",
        text: `Comment #${i}: Please verify **rendering latency** and \`table.ts\` batching for step ${i}.\n\n- Check scroll frames\n- Check heap usage`,
      })
    } else if (i % 30 === 5) {
      const sid = `sess-${1 + (i % 25)}`
      itemTimeline.push({ id: i, ts, actor: "steve", kind: "session", session_id: sid, text: `开始会话 #${1 + (i % 25)}` })
    } else {
      itemTimeline.push({
        id: i, ts, actor: "steve", kind: "sys", session_id: null,
        text: `Process event #${i}: updated status and synced checkpoint ${i}`,
      })
    }
  }

  // 500 debug events for ST-100
  const debugEvents = Array.from({ length: 500 }, (_, i) => ({
    event_id: `dbg-${String(500 - i).padStart(4, "0")}`,
    cursor: 500 - i,
    at: NOW - i * 5000,
    kind: i % 5 === 0 ? "turn.completed" : i % 3 === 0 ? "tool.settled" : "projection.patch",
    reason_code: i % 7 === 0 ? "retry_scheduled" : "normal_transition",
    severity: i % 20 === 0 ? "error" : i % 8 === 0 ? "warning" : "info",
    session_id: "sess-live",
    attempt_id: `att-${i % 15}`,
    details: {
      seq: 500 - i,
      duration_ms: 12 + (i % 80),
      payload: `diagnostic detail payload ${i} `.repeat(6),
      tags: ["perf", "stream", `batch-${i % 10}`],
    },
  }))

  // Session sess-live initial transcript: 6 completed turns (each 30 tools + thought + markdown answer) + open live turn start
  const transcriptEvents = []
  let seq = 100
  for (let turn = 0; turn < 6; turn++) {
    const baseTs = NOW - (6 - turn) * 120_000
    transcriptEvents.push({ seq: ++seq, ts: baseTs, kind: "user", data: { text: `Turn ${turn + 1}: Analyze module ${turn + 1} and run checks.`, via: "me" } })
    transcriptEvents.push({ seq: ++seq, ts: baseTs + 500, kind: "text", data: { role: "thought", text: `Planning turn ${turn + 1}: reading source files, checking tests, and formatting output.\n\n1. Read files\n2. Run checks\n3. Summarize findings.` } })
    for (let j = 0; j < 30; j++) {
      const cid = `hist-${turn}-${j}`
      transcriptEvents.push({
        seq: ++seq, ts: baseTs + 1000 + j * 300, kind: "tool",
        data: {
          tool: {
            id: cid, origin: "harness",
            name: j % 4 === 0 ? "Edit" : j % 3 === 0 ? "Bash" : "Read",
            category: j % 4 === 0 ? "edit" : j % 3 === 0 ? "exec" : "read",
            title: `Tool ${j}`, target: `/repo/web/src/mod-${j % 12}.ts`,
            phase: "succeeded", duration_ms: 120 + j * 5,
            action: j % 3 === 0 ? { type: "exec", command: `npm run test -- mod-${j}` } : undefined,
            result: j % 4 === 0
              ? [{ type: "diff", path: `/repo/web/src/mod-${j % 12}.ts`, old: "const a = 1\n", new: "const a = 2\nconst b = 3\n", added: 2, removed: 1 }]
              : [{ type: "text", text: `Output line for tool ${j}\n`.repeat(8) }],
          },
        },
      })
    }
    transcriptEvents.push({ seq: ++seq, ts: baseTs + 15000, kind: "text", data: { role: "assistant", text: `## Turn ${turn + 1} Result\n\n` + chapter } })
    transcriptEvents.push({ seq: ++seq, ts: baseTs + 16000, kind: "turn_end", data: { stopReason: "end_turn" } })
  }
  // Open live turn
  transcriptEvents.push({ seq: ++seq, ts: NOW - 5000, kind: "user", data: { text: "Live turn: run deep audit and stream progress.", via: "me" } })

  // 250 feed entries
  const feedEntries = Array.from({ length: 250 }, (_, i) => {
    const num = 250 - i
    return {
      id: `feed-${num}`,
      seq: num,
      kind: num % 3 === 0 ? "completion" : "report",
      title: `Activity report #${num} · Frontend performance & checks`,
      excerpt: `Summary excerpt for entry #${num}: verified streaming pipeline, scroll performance, and memory bounds.`,
      body: null,
      published_at: NOW - i * 3600_000,
      read_at: i > 120 ? NOW - 1000 : null,
      purged: false,
      source: { type: num % 2 ? "flow" : "notification", id: `src-${num}`, run_id: num % 2 ? `run-${num}` : null, node_id: null, event_id: null },
    }
  })

  return { items, sessions, decisions, messages, itemTimeline, debugEvents, transcriptEvents, lastSeq: seq, feedEntries }
}

const F = buildFixtures()
const sseClients = new Set()
const sendSSE = (ev) => {
  const line = `data: ${JSON.stringify(ev)}\n\n`
  for (const res of sseClients) res.write(line)
}

const shellPayload = {
  version: "0.10.1", web_version: "0.10.1", owner: true,
  modules: { feishuRegistration: false },
  workspace: { id: WS, name: "Perf Bench", description: "", item_prefix: "ST", sort: 0, created: NOW - 86400_000 },
  workspaces: [{ id: WS, name: "Perf Bench", description: "", item_prefix: "ST", sort: 0, created: NOW - 86400_000 }],
  bots: [
    { id: "steve", name: "Steve", role: "coordinator", main: true, harness: "claude-code", model: "claude-sonnet-4-6", rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "default", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: NOW, workspaces: [WS] },
    { id: "dev", name: "Dev Bot", role: "engineer", main: false, harness: "claude-code", model: "claude-sonnet-4-6", rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "default", avatar: {}, default_project: "proj-1", prompt: null, device: null, enabled: true, revision: 1, updated: NOW, workspaces: [WS] },
    { id: "ops", name: "Ops Bot", role: "ops", main: false, harness: "codex", model: "gpt-5", rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "default", avatar: {}, default_project: "proj-1", prompt: null, device: null, enabled: true, revision: 1, updated: NOW, workspaces: [WS] },
  ],
  allBots: [],
  projects: [{ id: "proj-1", workspace_id: WS, name: "steve", description: "", device_id: "hub", path: "/repo", kind: "git", worktree: true, default_bot: "dev", release: null, archived: false, created: NOW - 86400_000, updated: NOW }],
  tags: [{ id: "tag-feat", name: "feat", cat: "类型", description: "Feature", created_by: "Steve", created: NOW - 86400_000 }],
  settings: { theme: "light", onboard: "done", awayH: "0" },
  notifySeen: NOW,
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1")
  const p = url.pathname
  const json = (obj, status = 200) => {
    res.writeHead(status, { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" })
    res.end(JSON.stringify(obj))
  }
  if (p === "/auth/hello") return json({ name: "steve", api: 2, version: "0.10.1", login: { token: true, oidc: false } })
  if (p === "/api/stream") {
    res.writeHead(200, { "content-type": "text/event-stream; charset=utf-8", "cache-control": "no-store", connection: "keep-alive" })
    res.flushHeaders()
    sseClients.add(res)
    req.on("close", () => sseClients.delete(res))
    return
  }
  if (p === "/api/shell") return json(shellPayload)
  if (p === "/api/summary") return json({ thinking: false, hold: null })
  if (p === "/api/me") return json({ user: { id: "u1", name: "Owner", email: null, role: "owner" }, version: "0.10.1", web_version: "0.10.1" })
  if (p === "/api/plugins/themes") return json({ themes: [] })
  if (p === "/api/notifications") return json([])
  if (p === "/api/runtimes") return json([{ id: "claude-code", name: "Claude Code", ok: true, version: "1.0", path: "", models: ["claude-sonnet-4-6"], note: "", modes: ["default", "plan"] }])
  if (p === "/api/devices") return json({ devices: [{ id: "hub", node_id: "hub", name: "Local Hub", online: true }] })
  if (p === "/api/conversations") return json({ conversations: [], has_more: false, next_offset: null, total: 0 })
  if (p === "/api/todos") return json({ todos: [] })
  if (p === "/api/calendar/events") return json({ events: [] })
  if (p === "/api/tags") return json({ uses: { "tag-feat": 40 } })
  if (p === "/api/hub/info") return json({ version: "0.10.1", web_version: "0.10.1", managed: false })
  if (p.startsWith("/api/stats/")) return json({
    turns: [], age_ms: 600_000,
    facts: { created: NOW - 600_000, done_at: null, dropped_at: null, agent_ms: 42_000, wait_ms: 5_000, turns: 6, decisions: 1, reworks: 0, first_pass: null },
    usage: { total: 12000, input: 9000, output: 3000, est: 0.05, hit_rate: 0.65, coverage: { state: "complete", total: 12000, input: 9000, output: 3000, cache_read: 0, cache_write: 0, expected: 6, reported: 6, complete: 6, pending: 0, unavailable: 0, cost_reported: null } },
  })
  if (p === "/api/snapshot") {
    return json({
      rev: 100,
      cursor: "E1:100",
      views: {
        item: F.items,
        session: F.sessions,
        decision: F.decisions,
        message: { views: F.messages, next: null },
      },
    })
  }
  if (p === "/api/views/page") {
    return json({ rev: 100, views: [], next: null })
  }
  if (p === "/api/views" || p === "/api/items/sessions") {
    return json({ rev: 100, views: [], missing: [] })
  }
  if (p === "/api/items/ST-100/debug") {
    return json({
      observed_at: NOW,
      item: { id: "ST-100", status: "run", phase: "active", cycle: 1, revision: 1 },
      capture: { active: false, started_at: null, expires_at: null, stop_reason: null, bytes: 0, dropped: 0 },
      sessions: [{ id: "sess-live" }],
      coverage: { retention_days: 7, earliest_cursor: 1, latest_cursor: 500, instance_pending: 0, pending_scope: "instance", recorder_available: true, delivery: "best_effort", unobserved_gaps: "unknown", loss_count_stable: true, refresh_required: false },
      can_capture: true,
      state: { lifecycle: { blockers: [] }, dependencies: { open_dependencies: [], resume_mode: "automatic", resume_source: "workspace" } },
    })
  }
  if (p === "/api/items/ST-100/debug/events") {
    return json({
      events: F.debugEvents,
      next_cursor: null,
      has_more: false,
      coverage: { retention_days: 7, earliest_cursor: 1, latest_cursor: 500, instance_pending: 0, pending_scope: "instance", recorder_available: true, delivery: "best_effort", unobserved_gaps: "unknown", loss_count_stable: true, refresh_required: false },
      gaps: [],
      truncated: false,
    })
  }
  if (p.match(/^\/api\/items\/[^/]+$/)) {
    const id = p.split("/")[3]
    const tl = id === "ST-100" ? F.itemTimeline : [{ id: 1, ts: NOW - 60_000, actor: "steve", kind: "sys", text: "Created", session_id: null }]
    return json({
      id,
      source: ["chat", "thread"],
      goalsList: [{ version: 1, text: `Verify sustained frontend smoothness for ${id} under real workloads.`, created: NOW - 600_000 }],
      plan: [{ step: "Measure baseline", done: true }, { step: "Optimize hot paths", done: false }],
      planRounds: [],
      timeline: tl,
      artifacts: [],
      annotations: [],
      decisions: [],
      dependents: [],
      branch: "perf/web-streaming-lists",
      cwd: "/repo",
    })
  }
  if (p.match(/^\/api\/items\/[^/]+\/people$/)) return json([])
  if (p.match(/^\/api\/sessions\/[^/]+\/events$/)) {
    const sid = p.split("/")[3]
    const after = url.searchParams.get("after")
    if (sid === "sess-live") {
      if (after) return json({ events: [], next: null })
      return json({ events: F.transcriptEvents, next: 100 })
    }
    return json({ events: [{ seq: 1, ts: NOW - 60_000, kind: "user", data: { text: "Check", via: "me" } }, { seq: 2, ts: NOW - 50_000, kind: "text", data: { role: "assistant", text: "Done." } }, { seq: 3, ts: NOW - 49_000, kind: "turn_end", data: { stopReason: "end_turn" } }], next: null })
  }
  if (p.match(/^\/api\/sessions\/[^/]+\/queued$/)) return json([])
  if (p === "/api/me/feed") {
    const offset = Number(url.searchParams.get("cursor") ?? 0)
    const limit = Number(url.searchParams.get("limit") ?? 50)
    const slice = F.feedEntries.slice(offset, offset + limit)
    const next = offset + limit < F.feedEntries.length ? String(offset + limit) : null
    return json({ entries: slice, next_cursor: next, snapshot: "snap-1", unread_count: 120 })
  }
  if (p.startsWith("/api/me/feed/")) {
    const id = p.split("/").at(-1)
    const entry = F.feedEntries.find((e) => e.id === id)
    return entry ? json({ ...entry, body: { title: entry.title, markdown: chapter, fields: [["Duration", "2m"]], actions: [], references: [] } }) : json({ error: "not found" }, 404)
  }
  if (p.startsWith("/api/")) return json({}, 200)

  // Static file from web/dist
  const rel = p === "/" ? "index.html" : p.replace(/^\/+/, "")
  try {
    const data = await readFile(join(distDir, rel))
    res.writeHead(200, { "content-type": MIME[extname(rel)] ?? "application/octet-stream" })
    res.end(data)
  } catch {
    const data = await readFile(join(distDir, "index.html"))
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" })
    res.end(data)
  }
})

await new Promise((r) => server.listen(0, "127.0.0.1", r))
const port = server.address().port
assert.notEqual(port, 8440)
const base = `http://127.0.0.1:${port}`
const ping = setInterval(() => { for (const r of sseClients) r.write("event: ping\ndata: ok\n\n") }, 5000)

const r2 = (n) => Math.round(n * 100) / 100
const pct = (xs, q) => {
  if (!xs.length) return 0
  const s = [...xs].sort((a, b) => a - b)
  return r2(s[Math.min(s.length - 1, Math.ceil(s.length * q) - 1)])
}
const dropped = (frames) => frames.reduce((n, t) => n + Math.max(0, Math.round(t / (1000 / 60)) - 1), 0)

function initPerfScript() {
  localStorage.setItem("steve-onboarded", "1")
  localStorage.setItem("steve-lang", "zh-CN")
  const P = (window.__wperf = {
    active: false,
    frames: [],
    scrollFrames: [],
    scrolling: false,
    long: [],
    loaf: [],
    events: [],
    shifts: [],
    lastFrame: 0,
    commits: 0,
    fibers: 0,
  })
  window.__REACT_DEVTOOLS_GLOBAL_HOOK__ = {
    supportsFiber: true,
    renderers: new Map(),
    inject(renderer) { const id = this.renderers.size + 1; this.renderers.set(id, renderer); return id },
    onCommitFiberRoot(_id, root) {
      if (!P.active) return
      P.commits++
      function visit(first) {
        for (let f = first; f; f = f.sibling) {
          if (f.flags & 1) P.fibers++
          if (f.child && f.child !== f.alternate?.child) visit(f.child)
        }
      }
      visit(root.current)
    },
    onCommitFiberUnmount() {},
  }
  for (const [type, key] of [["longtask", "long"], ["long-animation-frame", "loaf"], ["event", "events"], ["layout-shift", "shifts"]]) {
    if (!PerformanceObserver.supportedEntryTypes?.includes(type)) continue
    try {
      new PerformanceObserver((list) => {
        for (const e of list.getEntries()) {
          P[key].push({
            t: e.startTime,
            d: e.duration,
            b: e.blockingDuration ?? 0,
            id: e.interactionId ?? 0,
            name: e.name,
            shift: e.value ?? 0,
            input: !!e.hadRecentInput,
            scrolling: P.scrolling,
          })
        }
      }).observe({ type, ...(type === "event" ? { durationThreshold: 16 } : {}) })
    } catch {}
  }
  function tick(t) {
    requestAnimationFrame(tick)
    if (P.active) {
      if (P.lastFrame) {
        const dt = t - P.lastFrame
        P.frames.push(dt)
        if (P.scrolling) P.scrollFrames.push(dt)
      }
      P.lastFrame = t
    } else {
      P.lastFrame = 0
    }
  }
  requestAnimationFrame(tick)
}

async function getMemoryMb(cdp) {
  const { metrics } = await cdp.send("Performance.getMetrics")
  const m = Object.fromEntries(metrics.map((x) => [x.name, x.value]))
  return {
    heap_mb: r2((m.JSHeapUsedSize ?? 0) / 1048576),
    nodes: Math.round(m.Nodes ?? 0),
    layouts: Math.round(m.LayoutCount ?? 0),
    recalc_styles: Math.round(m.RecalcStyleCount ?? 0),
    script_ms: r2((m.ScriptDuration ?? 0) * 1000),
    task_ms: r2((m.TaskDuration ?? 0) * 1000),
  }
}

function summarizeWindow(raw, tStart, tEnd, memStart, memPeak, memEnd, extra = {}) {
  const long = raw.long.filter((e) => e.t >= tStart && e.t <= tEnd && e.d > 50)
  const scrollLong = long.filter((e) => e.scrolling)
  const loaf = raw.loaf.filter((e) => e.t >= tStart && e.t <= tEnd)
  const cls = raw.shifts.filter((e) => e.t >= tStart && e.t <= tEnd && !e.input).reduce((n, e) => n + e.shift, 0)
  return {
    duration_ms: r2(tEnd - tStart),
    frames: raw.frames.length,
    frame_p50_ms: pct(raw.frames, 0.5),
    frame_p95_ms: pct(raw.frames, 0.95),
    frame_max_ms: pct(raw.frames, 1),
    dropped_frames: dropped(raw.frames),
    scroll_frames: raw.scrollFrames.length,
    scroll_frame_p50_ms: pct(raw.scrollFrames, 0.5),
    scroll_frame_p95_ms: pct(raw.scrollFrames, 0.95),
    scroll_dropped_frames: dropped(raw.scrollFrames),
    scroll_long_tasks: scrollLong.length,
    long_tasks_gt_50ms: long.length,
    max_long_task_ms: r2(Math.max(0, ...long.map((e) => e.d))),
    tbt_ms: r2(long.reduce((n, e) => n + Math.max(0, e.d - 50), 0)),
    loaf_count: loaf.length,
    loaf_p95_ms: pct(loaf.map((e) => e.d), 0.95),
    cls: r2(cls),
    heap_start_mb: memStart.heap_mb,
    heap_peak_mb: r2(memPeak),
    heap_end_mb: memEnd.heap_mb,
    dom_nodes: memEnd.nodes,
    script_delta_ms: r2(memEnd.script_ms - memStart.script_ms),
    task_delta_ms: r2(memEnd.task_ms - memStart.task_ms),
    layout_delta: memEnd.layouts - memStart.layouts,
    recalc_style_delta: memEnd.recalc_styles - memStart.recalc_styles,
    react_commits: raw.commits ?? 0,
    react_fibers_rendered: raw.fibers ?? 0,
    ...extra,
  }
}

async function runChatScenario(context) {
  const page = await context.newPage()
  const cdp = await context.newCDPSession(page)
  await cdp.send("Performance.enable")
  const errors = []
  page.on("pageerror", (e) => { errors.push(e.message); console.error("[pageerror chat]", e.message) })
  await page.goto(base + "/#/")
  await page.locator('[data-composer="chat"] textarea').waitFor({ timeout: 30_000 })
  await page.locator('[data-msg="m-0519"]').waitFor({ timeout: 30_000 })
  for (let i = 0; i < 60 && !sseClients.size; i++) await sleep(50)
  assert.ok(sseClients.size >= 1, "SSE connected")
  await sleep(500)

  const scroller = page.locator("[data-chat-scroll]")
  const rect = await scroller.boundingBox()
  const memStart = await getMemoryMb(cdp)
  let heapPeak = memStart.heap_mb
  const tStart = await page.evaluate(() => {
    window.__wperf.frames.length = 0
    window.__wperf.scrollFrames.length = 0
    window.__wperf.commits = 0
    window.__wperf.fibers = 0
    window.__wperf.active = true
    return performance.now()
  })

  const mid = "m-0519"
  let rev = 200
  const started = performance.now()
  const at = (ms) => sleep(Math.max(0, started + ms - performance.now()))
  const totalMs = streamSeconds * 1000
  const draftCount = Math.floor(totalMs / 50)
  sendSSE({ type: "steve.thinking", ws: WS, rev: ++rev, on: true, phase: "answer" })

  let content = ""
  const feedTask = (async () => {
    for (let i = 0, n = 0; i < draftCount; i++) {
      await at(i * 50)
      content += streamTokens[n++ % streamTokens.length]
      sendSSE({ type: "steve.draft", ws: WS, rev, mid, thought: "", text: content })
      await at(i * 50 + 3)
      content += streamTokens[n++ % streamTokens.length]
      sendSSE({ type: "steve.draft", ws: WS, rev, mid, thought: "", text: content })
      // Emit tool progress every 1.5s during first half
      if (i >= 40 && i < 800 && i % 30 === 0) {
        const call = Math.floor((i - 40) / 60)
        const running = ((i - 40) / 30) % 2 === 0
        sendSSE({
          type: "steve.tool", ws: WS, rev, mid, run: "perf-live-run",
          name: call % 2 ? "Read" : "Bash", kind: call % 2 ? "read" : "exec",
          brief: `Inspect file ${call}`, call, ok: !running, running, at: Date.now(), started_at: NOW,
        })
      }
      // Realistic background item/session/decision patches every 500ms (10 ticks)
      if (i % 10 === 5) {
        const idx = (i / 10 | 0) % 20
        const item = { ...F.items[idx], updated: NOW + i * 100, star: i % 20 === 5 }
        const sess = { ...F.sessions[1 + (idx % 20)], updated: NOW + i * 100 }
        sendSSE({
          type: "patch", ws: WS, rev: ++rev,
          ops: [
            { type: "item", op: "upsert", id: item.id, view: item },
            { type: "session", op: "upsert", id: sess.id, view: sess },
          ],
        })
      }
      if (i % 100 === 0) {
        const m = await getMemoryMb(cdp)
        if (m.heap_mb > heapPeak) heapPeak = m.heap_mb
      }
    }
  })()

  // Scroll + card interaction + typing schedule scaled to streamSeconds
  const s = (frac) => Math.floor(totalMs * frac)
  const interactTask = (async () => {
    // Open live process steps briefly
    await at(s(0.08))
    const toggle = page.locator("[data-chat-draft] [data-steps-toggle]")
    if (await toggle.count()) {
      await toggle.click().catch(() => {})
      await sleep(600)
      await toggle.click().catch(() => {})
    }
    // Scroll UP across 520 messages + 20 cards
    await at(s(0.16))
    await page.mouse.move(rect.x + rect.width / 2, rect.y + rect.height / 2)
    await page.evaluate(() => { window.__wperf.scrolling = true })
    const upSteps = Math.max(40, Math.floor(streamSeconds * 1.1))
    for (let i = 0; i < upSteps; i++) {
      await page.mouse.wheel(0, -340)
      await sleep(140)
    }
    await page.evaluate(() => { window.__wperf.scrolling = false })
    await sleep(500)
    // Type in composer while scrolled & streaming
    const input = page.locator('[data-composer="chat"] textarea')
    await input.click()
    await page.keyboard.type("Sustained streaming input responsiveness check.", { delay: 45 })
    await sleep(400)
    // Scroll DOWN back through the 520 messages + 20 cards
    await page.mouse.move(rect.x + rect.width / 2, rect.y + rect.height / 2)
    await page.evaluate(() => { window.__wperf.scrolling = true })
    for (let i = 0; i < upSteps + 10; i++) {
      await page.mouse.wheel(0, 360)
      await sleep(140)
    }
    await page.evaluate(() => { window.__wperf.scrolling = false })
    const jump = page.locator("[data-jump-latest], [data-new-messages]").first()
    if (await jump.isVisible().catch(() => false)) await jump.click().catch(() => {})
  })()

  await Promise.all([feedTask, interactTask])
  await at(totalMs)
  const finalId = "m-stream-final"
  sendSSE({
    type: "patch", ws: WS, rev: ++rev,
    ops: [{ type: "message", op: "upsert", id: finalId, view: { id: finalId, ws: WS, author: "steve", item_id: null, text: content, created: Date.now(), meta: { origin: mid } } }],
  })
  sendSSE({ type: "steve.thinking", ws: WS, rev: ++rev, on: false })
  await page.locator(`[data-msg="${finalId}"]`).waitFor({ timeout: 15_000 })

  const { tEnd, raw, mountedMsgs, mountedCards } = await page.evaluate(() => {
    window.__wperf.active = false
    return {
      tEnd: performance.now(),
      raw: { ...window.__wperf },
      mountedMsgs: document.querySelectorAll("[data-msg]").length,
      mountedCards: document.querySelectorAll("[data-item-card]").length,
    }
  })
  const memEnd = await getMemoryMb(cdp)
  if (memEnd.heap_mb > heapPeak) heapPeak = memEnd.heap_mb
  await cdp.detach()
  await page.close()
  return summarizeWindow(raw, tStart, tEnd, memStart, heapPeak, memEnd, {
    errors: errors.length,
    mounted_msgs: mountedMsgs,
    mounted_cards: mountedCards,
    tokens_streamed: draftCount * 2,
    background_patches: Math.floor(draftCount / 10),
  })
}

async function runSessionTranscriptScenario(context) {
  const page = await context.newPage()
  const cdp = await context.newCDPSession(page)
  await cdp.send("Performance.enable")
  const errors = []
  page.on("pageerror", (e) => { errors.push(e.message); console.error("[pageerror chat]", e.message) })
  await page.goto(base + "/#/sessions/sess-live")
  await page.locator('[data-transcript="sess-live"]').waitFor({ timeout: 30_000 })
  await page.locator("[data-work]").first().waitFor({ timeout: 30_000 })
  for (let i = 0; i < 60 && !sseClients.size; i++) await sleep(50)
  await sleep(400)

  const scroller = page.locator(".ui-sac__viewport").first()
  const rect = await scroller.boundingBox()
  const memStart = await getMemoryMb(cdp)
  let heapPeak = memStart.heap_mb
  const tStart = await page.evaluate(() => {
    window.__wperf.frames.length = 0
    window.__wperf.scrollFrames.length = 0
    window.__wperf.commits = 0
    window.__wperf.fibers = 0
    window.__wperf.active = true
    return performance.now()
  })

  let seq = F.lastSeq
  const totalMs = transcriptSeconds * 1000
  const eventCount = Math.floor(transcriptSeconds * 45)
  const started = performance.now()
  const at = (ms) => sleep(Math.max(0, started + ms - performance.now()))

  // Stream 1,350+ transcript events (thought + tools + subtasks + streaming answer)
  const feedTask = (async () => {
    for (let i = 0; i < eventCount; i++) {
      await at((i * totalMs) / eventCount)
      seq++
      if (i < eventCount * 0.65) {
        const mod = i % 5
        if (mod === 0) {
          sendSSE({ type: "event", session_id: "sess-live", seq, kind: "text", data: { role: "thought", text: `Analyzing step ${i} for transcript throughput. ` } })
        } else if (mod === 1 || mod === 2) {
          const cid = `live-tool-${Math.floor(i / 5)}`
          const phase = mod === 1 ? "running" : "succeeded"
          sendSSE({
            type: "event", session_id: "sess-live", seq, kind: "tool",
            data: {
              tool: {
                id: cid, origin: "harness", name: i % 2 ? "Read" : "Edit", category: i % 2 ? "read" : "edit",
                title: `Step ${i}`, target: `/repo/src/file-${i % 20}.ts`, phase, duration_ms: phase === "succeeded" ? 85 : undefined,
                result: phase === "succeeded" ? [{ type: "text", text: "ok\n".repeat(6) }] : undefined,
              },
            },
          })
        } else {
          sendSSE({ type: "event", session_id: "sess-live", seq, kind: "text", data: { role: "thought", text: `Checking detail ${i}. ` } })
        }
      } else {
        // Stream final assistant markdown answer
        sendSSE({ type: "event", session_id: "sess-live", seq, kind: "text", data: { role: "assistant", text: streamTokens[i % streamTokens.length] } })
      }
      if (i % 150 === 0) {
        const m = await getMemoryMb(cdp)
        if (m.heap_mb > heapPeak) heapPeak = m.heap_mb
      }
    }
  })()

  const scrollTask = (async () => {
    // Collapse live work block during first phase so folded path is exercised
    await at(1200)
    const liveWorkBtn = page.locator('[data-work="live"] > button').first()
    if (await liveWorkBtn.count()) await liveWorkBtn.click().catch(() => {})
    await at(3000)
    if (rect) {
      await page.mouse.move(rect.x + rect.width / 2, rect.y + rect.height / 2)
      await page.evaluate(() => { window.__wperf.scrolling = true })
      for (let i = 0; i < 35; i++) { await page.mouse.wheel(0, -320); await sleep(120) }
      for (let i = 0; i < 40; i++) { await page.mouse.wheel(0, 320); await sleep(120) }
      await page.evaluate(() => { window.__wperf.scrolling = false })
    }
    // Re-open live work block
    if (await liveWorkBtn.count()) await liveWorkBtn.click().catch(() => {})
    await at(totalMs * 0.72)
    if (rect) {
      await page.mouse.move(rect.x + rect.width / 2, rect.y + rect.height / 2)
      await page.evaluate(() => { window.__wperf.scrolling = true })
      for (let i = 0; i < 20; i++) { await page.mouse.wheel(0, -280); await sleep(120) }
      for (let i = 0; i < 25; i++) { await page.mouse.wheel(0, 300); await sleep(120) }
      await page.evaluate(() => { window.__wperf.scrolling = false })
    }
  })()

  await Promise.all([feedTask, scrollTask])
  await sleep(300)
  const { tEnd, raw, mountedSegs } = await page.evaluate(() => {
    window.__wperf.active = false
    return {
      tEnd: performance.now(),
      raw: { ...window.__wperf },
      mountedSegs: document.querySelectorAll('[data-transcript="sess-live"] [data-index]').length,
    }
  })
  const memEnd = await getMemoryMb(cdp)
  if (memEnd.heap_mb > heapPeak) heapPeak = memEnd.heap_mb
  await cdp.detach()
  await page.close()
  return summarizeWindow(raw, tStart, tEnd, memStart, heapPeak, memEnd, {
    errors: errors.length,
    events_streamed: eventCount,
    mounted_segs: mountedSegs,
  })
}

async function runItemWorklogAndDebugScenario(context) {
  const page = await context.newPage()
  const cdp = await context.newCDPSession(page)
  await cdp.send("Performance.enable")
  const errors = []
  page.on("pageerror", (e) => { errors.push(e.message); console.error("[pageerror worklog]", e.stack || e.message) })
  page.on("console", (m) => { if (m.type() === "error") console.error("[console worklog]", m.text()) })

  const memInit = await getMemoryMb(cdp)
  await page.goto(base + "/#/items/ST-100")
  await page.locator("[data-worklog]").waitFor({ timeout: 15_000 })
  await sleep(500)

  const worklogCounts = await page.evaluate(() => ({
    comments: document.querySelectorAll("[data-worklog] [data-comment]").length,
    processRuns: document.querySelectorAll("[data-worklog] [data-process-run]").length,
    sessions: document.querySelectorAll("[data-worklog] [data-session-block]").length,
    virtualRows: document.querySelectorAll("[data-worklog] [data-index]").length,
  }))
  const scroller = page.locator(".ui-sac__viewport, [data-worklog]").first()
  // Find actual scrollable ancestor of data-worklog
  const scrollRect = await page.evaluate(() => {
    let el = document.querySelector("[data-worklog]")
    while (el && el.scrollHeight <= el.clientHeight + 2) el = el.parentElement
    const r = (el ?? document.documentElement).getBoundingClientRect()
    return { x: r.x, y: r.y, width: r.width, height: r.height }
  })

  const memBeforeScroll = await getMemoryMb(cdp)
  let heapPeak = memBeforeScroll.heap_mb
  const tStart = await page.evaluate(() => {
    window.__wperf.frames.length = 0
    window.__wperf.scrollFrames.length = 0
    window.__wperf.commits = 0
    window.__wperf.fibers = 0
    window.__wperf.active = true
    window.__wperf.scrolling = true
    return performance.now()
  })
  await page.mouse.move(scrollRect.x + scrollRect.width / 2, scrollRect.y + Math.min(300, scrollRect.height / 2))
  for (let i = 0; i < 45; i++) { await page.mouse.wheel(0, 420); await sleep(60) }
  for (let i = 0; i < 45; i++) { await page.mouse.wheel(0, -420); await sleep(60) }
  const { tEnd, raw: wlRaw } = await page.evaluate(() => {
    window.__wperf.scrolling = false
    window.__wperf.active = false
    return { tEnd: performance.now(), raw: { ...window.__wperf } }
  })
  const memAfterWl = await getMemoryMb(cdp)
  if (memAfterWl.heap_mb > heapPeak) heapPeak = memAfterWl.heap_mb
  const worklogResult = summarizeWindow(wlRaw, tStart, tEnd, memBeforeScroll, heapPeak, memAfterWl, {
    errors: errors.length,
    ...worklogCounts,
    load_long_tasks: wlRaw.long.filter((e) => e.t < tStart && e.d > 50).length,
    load_tbt_ms: r2(wlRaw.long.filter((e) => e.t < tStart && e.d > 50).reduce((n, e) => n + Math.max(0, e.d - 50), 0)),
    initial_dom_nodes: memBeforeScroll.nodes,
  })

  // Now open Debug Panel with 500 events
  await page.locator('[aria-label="更多"], button[aria-label="More"]').first().click()
  const dbgStart = await page.evaluate(() => {
    window.__wperf.frames.length = 0
    window.__wperf.scrollFrames.length = 0
    window.__wperf.commits = 0
    window.__wperf.fibers = 0
    window.__wperf.active = true
    return performance.now()
  })
  const memDbgStart = await getMemoryMb(cdp)
  await page.locator("[data-item-debug]").click()
  await page.locator("[data-debug-event]").first().waitFor({ timeout: 30_000 })
  await sleep(500)
  const mountedDebugEvents = await page.locator("[data-debug-event]").count()
  // Observe 4 seconds idle (measures unconditional 1Hz setNow re-render cost!)
  const memBeforeIdle = await getMemoryMb(cdp)
  const idleStart = await page.evaluate(() => performance.now())
  await sleep(4200)
  const idleEnd = await page.evaluate(() => performance.now())
  const memAfterIdle = await getMemoryMb(cdp)

  // Scroll debug panel down & up
  const dbgRect = await page.locator("[data-debug-panel]").boundingBox()
  await page.mouse.move(dbgRect.x + dbgRect.width / 2, dbgRect.y + dbgRect.height / 2)
  await page.evaluate(() => { window.__wperf.scrolling = true })
  for (let i = 0; i < 40; i++) { await page.mouse.wheel(0, 380); await sleep(60) }
  for (let i = 0; i < 40; i++) { await page.mouse.wheel(0, -380); await sleep(60) }
  const { dbgEnd, raw: dbgRaw } = await page.evaluate(() => {
    window.__wperf.scrolling = false
    window.__wperf.active = false
    return { dbgEnd: performance.now(), raw: { ...window.__wperf } }
  })
  const memDbgEnd = await getMemoryMb(cdp)
  const idleLong = dbgRaw.long.filter((e) => e.t >= idleStart && e.t <= idleEnd && e.d > 50)
  const debugResult = summarizeWindow(dbgRaw, dbgStart, dbgEnd, memDbgStart, Math.max(memDbgStart.heap_mb, memDbgEnd.heap_mb), memDbgEnd, {
    errors: errors.length,
    total_events: 500,
    mounted_debug_events: mountedDebugEvents,
    idle_4s_long_tasks: idleLong.length,
    idle_4s_script_ms: r2(memAfterIdle.script_ms - memBeforeIdle.script_ms),
    idle_4s_layouts: memAfterIdle.layouts - memBeforeIdle.layouts,
  })

  await cdp.detach()
  await page.close()
  return { item_worklog_520: worklogResult, debug_panel_500: debugResult }
}

async function runFeedScenario(context) {
  const page = await context.newPage()
  const cdp = await context.newCDPSession(page)
  await cdp.send("Performance.enable")
  const errors = []
  page.on("pageerror", (e) => { errors.push(e.message); console.error("[pageerror chat]", e.message) })
  await page.goto(base + "/#/feed")
  await page.locator('[data-feed-entry="feed-250"]').waitFor({ timeout: 30_000 })
  for (let k = 0; k < 4; k++) {
    const more = page.locator("[data-feed-more]")
    if (await more.isVisible().catch(() => false)) {
      await more.click()
      await sleep(300)
    }
  }
  await sleep(300)
  const mountedFeedEntries = await page.locator("[data-feed-entry]").count()
  const scrollRect = await page.evaluate(() => {
    let el = document.querySelector("[data-feed]")
    while (el && el.scrollHeight <= el.clientHeight + 2) el = el.parentElement
    const r = (el ?? document.documentElement).getBoundingClientRect()
    return { x: r.x, y: r.y, width: r.width, height: r.height }
  })
  const memStart = await getMemoryMb(cdp)
  const tStart = await page.evaluate(() => {
    window.__wperf.frames.length = 0
    window.__wperf.scrollFrames.length = 0
    window.__wperf.commits = 0
    window.__wperf.fibers = 0
    window.__wperf.active = true
    window.__wperf.scrolling = true
    return performance.now()
  })
  await page.mouse.move(scrollRect.x + scrollRect.width / 2, scrollRect.y + Math.min(300, scrollRect.height / 2))
  for (let i = 0; i < 40; i++) { await page.mouse.wheel(0, 400); await sleep(60) }
  for (let i = 0; i < 40; i++) { await page.mouse.wheel(0, -400); await sleep(60) }
  await page.evaluate(() => { window.__wperf.scrolling = false })
  await page.locator('[data-feed-entry="feed-250"] [data-feed-expand]').click()
  await page.locator('[data-feed-entry="feed-250"] [data-feed-body]').waitFor({ timeout: 10_000 })
  await sleep(200)
  const { tEnd, raw } = await page.evaluate(() => {
    window.__wperf.active = false
    return { tEnd: performance.now(), raw: { ...window.__wperf } }
  })
  const memEnd = await getMemoryMb(cdp)
  await cdp.detach()
  await page.close()
  return summarizeWindow(raw, tStart, tEnd, memStart, Math.max(memStart.heap_mb, memEnd.heap_mb), memEnd, {
    errors: errors.length,
    mounted_feed_entries: mountedFeedEntries,
  })
}

async function runCards20Scenario(context) {
  const page = await context.newPage()
  const cdp = await context.newCDPSession(page)
  await cdp.send("Performance.enable")
  const errors = []
  page.on("pageerror", (e) => { errors.push(e.message); console.error("[pageerror cards]", e.message) })
  await page.goto(base + "/#/inbox")
  await page.locator("[data-decision]").first().waitFor({ timeout: 30_000 })
  for (let i = 0; i < 60 && !sseClients.size; i++) await sleep(50)
  await sleep(300)
  const mountedCards = await page.locator("[data-decision]").count()
  const scrollRect = await page.evaluate(() => {
    let el = document.querySelector("[data-decision]")
    while (el && el.scrollHeight <= el.clientHeight + 2) el = el.parentElement
    const r = (el ?? document.documentElement).getBoundingClientRect()
    return { x: r.x, y: r.y, width: r.width, height: r.height }
  })
  const memStart = await getMemoryMb(cdp)
  const tStart = await page.evaluate(() => {
    window.__wperf.frames.length = 0
    window.__wperf.scrollFrames.length = 0
    window.__wperf.commits = 0
    window.__wperf.fibers = 0
    window.__wperf.active = true
    return performance.now()
  })
  // Emit 60 background patches (messages + unrelated items/sessions) while viewing 20 cards, then scroll
  let rev = 9000
  for (let i = 0; i < 60; i++) {
    const item = { ...F.items[20 + (i % 20)], updated: NOW + 90_000 + i * 100 }
    sendSSE({
      type: "patch", ws: WS, rev: ++rev,
      ops: [
        { type: "item", op: "upsert", id: item.id, view: item },
        { type: "message", op: "upsert", id: `bg-msg-${i}`, view: { id: `bg-msg-${i}`, ws: WS, author: "steve", item_id: null, text: `Background update ${i}`, created: NOW + i * 50, meta: null } },
      ],
    })
    await sleep(50)
  }
  await page.mouse.move(scrollRect.x + scrollRect.width / 2, scrollRect.y + Math.min(300, scrollRect.height / 2))
  await page.evaluate(() => { window.__wperf.scrolling = true })
  for (let i = 0; i < 30; i++) { await page.mouse.wheel(0, 360); await sleep(60) }
  for (let i = 0; i < 30; i++) { await page.mouse.wheel(0, -360); await sleep(60) }
  const { tEnd, raw } = await page.evaluate(() => {
    window.__wperf.scrolling = false
    window.__wperf.active = false
    return { tEnd: performance.now(), raw: { ...window.__wperf } }
  })
  const memEnd = await getMemoryMb(cdp)
  await cdp.detach()
  await page.close()
  return summarizeWindow(raw, tStart, tEnd, memStart, Math.max(memStart.heap_mb, memEnd.heap_mb), memEnd, {
    errors: errors.length,
    mounted_cards: mountedCards,
    bg_patches: 60,
  })
}

let browser
try {
  const loadBefore = loadavg()
  const flags = process.env.BENCH_SINGLE_PROCESS === "1" ? ["--single-process", "--no-zygote"] : []
  browser = await chromium.launch({
    headless: true,
    args: flags,
    ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
  })
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce", serviceWorkers: "block" })
  await context.route("**/*", (route) => {
    const u = new URL(route.request().url())
    return u.origin === base ? route.continue() : route.abort()
  })
  await context.addInitScript(initPerfScript)

  const out = {
    meta: {
      chromium: browser.version(),
      platform: platform(),
      load_before: loadBefore.map(r2),
      stream_seconds: streamSeconds,
      transcript_seconds: transcriptSeconds,
    },
  }
  if (!only || only.has("chat")) {
    console.error(`[bench] running chat_stream (${streamSeconds}s, 520 msgs, 20 cards)...`)
    out.chat_stream = await runChatScenario(context)
  }
  if (!only || only.has("transcript")) {
    console.error(`[bench] running session_transcript (${transcriptSeconds}s live stream)...`)
    out.session_transcript = await runSessionTranscriptScenario(context)
  }
  if (!only || only.has("worklog")) {
    console.error("[bench] running item_worklog_520 + debug_panel_500...")
    Object.assign(out, await runItemWorklogAndDebugScenario(context))
  }
  if (!only || only.has("cards")) {
    console.error("[bench] running cards_20_inbox (20 cards + 60 bg patches + scroll)...")
    out.cards_20_inbox = await runCards20Scenario(context)
  }
  if (!only || only.has("feed")) {
    console.error("[bench] running feed_250...")
    out.feed_250 = await runFeedScenario(context)
  }
  out.meta.load_after = loadavg().map(r2)
  console.log(JSON.stringify(out, null, 2))
} finally {
  await browser?.close()
  clearInterval(ping)
  for (const r of sseClients) r.end()
  server.close()
}
