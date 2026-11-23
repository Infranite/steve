// Sustained main-chat workload (#525). Use a private release Hub serving a production Web build;
// see CONTRIBUTING's development checks. The browser can read only that loopback Hub and this SSE fixture.
import assert from "node:assert/strict"
import { createServer } from "node:http"
import { loadavg, platform } from "node:os"
import { setTimeout as sleep } from "node:timers/promises"
import { chromium } from "playwright"

const base = process.env.BENCH_BASE ?? "http://127.0.0.1:" + (process.env.BENCH_PORT ?? 18700)
assert.match(base, /^http:\/\/127\.0\.0\.1:\d+$/)
assert.notEqual(new URL(base).port, "8440")
const profile = process.argv.includes("--profile")
const gate = process.argv.includes("--gate")
const seconds = 60
const chapter = [
  "### Verification notes",
  "",
  "We checked the **rendering timeline** against the original result, then repeated it with a longer conversation.",
  "This keeps the response readable while the tool calls are still arriving. The key is to distinguish work done from work scheduled.",
  "",
  "- Use the measured value, not an estimate.",
  "- Keep the input draft and its selection intact.",
  "- Review the [documentation](https://example.invalid/docs) before changing defaults.",
  "",
  "```javascript",
  "const frames = samples.map(({ end, start }) => end - start);",
  "const mean = frames.reduce((sum, frame) => sum + frame, 0) / frames.length;",
  "console.log({ mean, total: frames.length });",
  "```",
  "",
  "| Check | Result |",
  "| --- | --- |",
  "| Layout | Stable |",
  "| Input | Responsive |",
  "",
].join("\n") + "\n"
const tokens = chapter.repeat(45).match(/\s+|\p{L}+|[^\s\p{L}]/gu)
const draftCount = 1200, tokenCount = 2400, mid = "bench-stream-question", finalId = "bench-stream-final"
const last = "Stream finished with all fixture text."
const typing = "inserted text"
const pages = new Set()
const fixture = createServer((_req, res) => {
  res.writeHead(200, {
    "Access-Control-Allow-Origin": base, "Content-Type": "text/event-stream; charset=utf-8", "Cache-Control": "no-store",
  })
  res.flushHeaders()
  pages.add(res)
  res.on("close", () => pages.delete(res))
})
await new Promise((resolve, reject) => fixture.listen(0, "127.0.0.1", resolve).once("error", reject))
const sse = "http://127.0.0.1:" + fixture.address().port
const send = (event) => {
  for (const page of pages) page.write("data: " + JSON.stringify(event) + "\n\n")
}
const ping = setInterval(() => { for (const page of pages) page.write("event: ping\ndata: ok\n\n") }, 5000)

async function cool() {
  const until = Date.now() + Number(process.env.BENCH_LOAD_WAIT_S ?? 1200) * 1000
  for (;;) {
    const load = loadavg()
    if (load[0] <= 3) return load
    if (Date.now() >= until) throw new Error("Timed out waiting for 1-minute load <= 3")
    await sleep(5000)
  }
}

function instrument({ origin, sse, profile }) {
  // Substitute only live frames; keep EventSource, JSON decoding, the client reducers, actual
  // snapshot/history reads, and production rendering on their normal paths.
  localStorage.setItem("steve-onboarded", "1")
  const Original = window.EventSource
  window.EventSource = class extends Original {
    constructor(url, options) {
      const target = new URL(url, location.href)
      if (target.origin !== origin || target.pathname !== "/api/stream") { super(url, options); return }
      super(sse)
      this.addEventListener("message", (e) => {
        if (!e.data.startsWith('{"type":"steve.draft",')) return
        if (!bench.start) bench.start = performance.now()
        bench.received++
      })
    }
  }
  const bench = window.chatBench = {
    start: 0, received: 0, commits: 0, draftCommits: 0, renders: {}, frames: [], long: [], events: [], shifts: [], jumpTimes: [],
    anchor: null, anchorDrift: 0, anchorLost: 0, jumps: 0, worstJump: 0, direction: 0, followGaps: [],
  }
  for (const [type, key] of [["longtask", "long"], ["event", "events"], ["layout-shift", "shifts"]]) {
    if (!PerformanceObserver.supportedEntryTypes.includes(type)) continue
    new PerformanceObserver((entries) => {
      for (const e of entries.getEntries()) bench[key].push({
        t: e.startTime, d: e.duration, id: e.interactionId, name: e.name, shift: e.value, input: e.hadRecentInput,
      })
    }).observe({ type, ...(type === "event" ? { durationThreshold: 16 } : {}) })
  }
  if (profile) {
    // In a production build React sets PerformedWork (bit 0) on fibers that rendered.
    // Skip reused child lists so flags from past commits aren't counted again. Diagnostic run only.
    window.__REACT_DEVTOOLS_GLOBAL_HOOK__ = {
      supportsFiber: true, renderers: new Map(),
      inject(renderer) { const id = this.renderers.size + 1; this.renderers.set(id, renderer); return id },
      onCommitFiberRoot(_id, root) {
        if (!bench.start || bench.end) return
        bench.commits++
        let draft = false
        function visit(first) {
          for (let f = first; f; f = f.sibling) {
            const name = typeof f.type === "function" ? f.type.name : f.type?.render?.name
            if (name && (f.flags & 1)) {
              bench.renders[name] = (bench.renders[name] ?? 0) + 1
              if (name === "LiveTail" || name === "LiveReply") draft = true
            }
            if (f.child && f.child !== f.alternate?.child) visit(f.child)
          }
        }
        visit(root.current)
        if (draft) bench.draftCommits++
      },
      onCommitFiberUnmount() {},
    }
  }
  let prev = 0, lastGeometry = 0, activeDirection = 0, wheelRow, wheelY, wheelTop
  function frame(t) {
    if (!bench.end) requestAnimationFrame(frame)
    if (bench.start && t > bench.start && !bench.end) {
      if (prev) bench.frames.push(t - prev)
      prev = t
      const scroller = bench.scroller
      if (scroller) {
        if (bench.direction) {
          // A virtualizer can correct scrollTop as heights above the viewport are learned.
          // Measure what a row did on screen, not the scrollbar's change of coordinates.
          const viewport = scroller.getBoundingClientRect()
          const top = scroller.scrollTop
          const box = wheelRow?.isConnected && wheelRow.getBoundingClientRect()
          if (box && activeDirection === bench.direction) {
            const delta = (wheelY - box.top) * bench.direction
            if (delta < -3 || delta > 420) {
              bench.jumps++
              bench.worstJump = Math.max(bench.worstJump, Math.abs(delta))
              bench.jumpTimes.push({ ms: Math.round(t - bench.start), delta_px: Math.round(delta),
                scroll_top_delta_px: Math.round(top - wheelTop), index: wheelRow.dataset.index ?? null })
            }
          }
          if (!box || box.bottom < viewport.top || box.top > viewport.bottom) {
            wheelRow = [...scroller.querySelectorAll("[data-index], [data-chat-draft]")].find(el => {
              const r = el.getBoundingClientRect()
              return r.bottom > viewport.top && r.top < viewport.bottom
            })
          }
          wheelY = wheelRow?.getBoundingClientRect().top
          wheelTop = top
        } else wheelRow = null
        activeDirection = bench.direction
        if (t - lastGeometry >= 100) {
          lastGeometry = t
          if (bench.follow) bench.followGaps.push(scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight)
          if (bench.anchor) {
            const { row, y } = bench.anchor
            if (!row.isConnected) bench.anchorLost++
            else bench.anchorDrift = Math.max(bench.anchorDrift, Math.abs(row.getBoundingClientRect().top - scroller.getBoundingClientRect().top - y))
          }
        }
      }
    }
  }
  requestAnimationFrame(frame)
}

let browser
try {
  const flags = process.env.BENCH_SINGLE_PROCESS === "1" ? ["--single-process", "--no-zygote"] : []
  browser = await chromium.launch({ args: flags })
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce", serviceWorkers: "block" })
  await context.route("**/*", (route) => {
    const u = new URL(route.request().url())
    return u.origin === sse || u.origin === base ? route.continue() : route.abort()
  })
  await context.addInitScript(instrument, { origin: base, sse, profile })
  const page = await context.newPage()
  let errors = 0
  page.on("pageerror", () => errors++)
  await page.goto(base + "/auth/dev")
  await page.goto(base + "/#/")
  const ws = await page.evaluate(() => fetch("/api/shell").then(r => r.json()).then(s => s.workspace.id))
  const scroll = page.locator("[data-chat-scroll]")
  const input = page.locator('[data-composer="chat"] textarea')
  const draft = page.locator("[data-chat-draft] [data-steve-msg]")
  const live = page.locator("[data-chat-draft] [data-live-steps]")
  await input.waitFor()
  for (let i = 0; i < 100 && !pages.size; i++) await sleep(50)
  assert.equal(pages.size, 1, "main chat must be connected to the fixture")
  console.error("Loopback Hub authenticated; streaming fixture connected")
  const now = Date.now(), rev = now
  const row = (id, text, created, author = "steve", meta = null) => ({ id, ws, item_id: null, text, created, author, meta })
  const upsert = (view) => ({ type: "message", op: "upsert", id: view.id, view })
  const patch = (...rows) => send({ type: "patch", ws, rev, ops: rows.map(upsert) })
  const tools = Array.from({ length: 12 }, (_, i) => ({
    name: i % 3 ? "execute" : "search", kind: i % 3 ? "exec" : "search", call: i,
    args: { brief: "Fixture execution " + i }, result: { lines: Array.from({ length: 60 }, (_, j) => "Output record " + j) },
    ms: 900, status: "ok",
  }))
  patch(...Array.from({ length: 120 }, (_, i) => row("bench-history-" + i, i % 2 ? chapter.repeat(1 + i % 5) : "Please review the measured values from this run.", now - 120000 + 500 * i, i % 2 ? "steve" : "me")),
    row(mid, "Please review the complete browser timeline.", now - 1, "me"))
  await page.locator(`[data-msg="${mid}"]`).waitFor()
  await page.waitForFunction(() => {
    const el = document.querySelector("[data-chat-scroll]")
    return el.scrollHeight - el.clientHeight > 4000 && el.scrollHeight - el.clientHeight - el.scrollTop < 4
  })
  await page.evaluate(async () => {
    await document.fonts.ready
    chatBench.scroller = document.querySelector("[data-chat-scroll]")
    chatBench.scroller.addEventListener("wheel", (e) => {
      if (e.isTrusted && Math.sign(e.deltaY) === chatBench.wheelPhase) chatBench.direction = chatBench.wheelPhase
    }, { passive: true, capture: true })
    chatBench.follow = true
  })
  const rect = await scroll.boundingBox()
  const load_before = await cool()
  console.error("Streaming fixture: 60 s, pre-run load", load_before[0])
  const started = performance.now()
  const at = (ms) => sleep(Math.max(0, started + ms - performance.now()))
  send({ type: "steve.thinking", ws, rev, on: true, phase: "answer" })
  let content = ""
  const feed = async () => {
    for (let i = 0, n = 0; i < draftCount; i++) {
      await at(i * 50)
      // Two deltas 3 ms apart every 50 ms: 40 lexical tokens/s on average, with small bursts.
      content += tokens[n++]
      send({ type: "steve.draft", ws, rev, mid, thought: "", text: content })
      await at(i * 50 + 3)
      content += tokens[n++] + (i === draftCount - 1 ? "\n\n" + last : "")
      send({ type: "steve.draft", ws, rev, mid, thought: "", text: content })
      if (i >= 130 && i < 610 && i % 20 === 10) {
        const call = Math.floor((i - 130) / 40), tool = tools[call]
        const tick = Math.floor((i - 130) / 20)
        send({ type: "steve.tool", ws, rev, mid, run: "bench-stream-run", name: tool.name, kind: tool.kind,
          brief: tool.args.brief, call, ok: tick % 2 === 1, running: tick % 2 === 0, at: Date.now(), started_at: now })
      }
    }
  }
  const type = async () => {
    await at(31000)
    const prefix = "A wrapped editing probe with multiple lines. ".repeat(22)
    await input.fill(prefix)
    const start = 470
    await input.evaluate((el, start) => { el.setSelectionRange(start, start) }, start)
    await page.keyboard.type(typing, { delay: 75 })
    return input.evaluate((el, { start, prefix, typing }) => {
      const mirror = el.parentElement.querySelector("[data-composer-marks]")
      const a = el.getBoundingClientRect(), b = mirror.getBoundingClientRect()
      const sa = getComputedStyle(el), sb = getComputedStyle(mirror)
      const styleMatch = ["fontFamily", "fontSize", "lineHeight", "letterSpacing", "paddingInlineStart", "paddingInlineEnd"].every(k => sa[k] === sb[k])
      return {
        selection_ok: document.activeElement === el && el.selectionStart === start + typing.length && el.selectionEnd === el.selectionStart,
        value_ok: el.value === prefix.slice(0, start) + typing + prefix.slice(start) && mirror.textContent === el.value,
        mirror_error_px: Math.max(Math.abs(a.left - b.left), Math.abs(a.top - b.top), Math.abs(el.clientWidth - mirror.clientWidth), Math.abs(el.scrollTop - mirror.scrollTop)),
        style_match: styleMatch,
      }
    }, { start, prefix, typing })
  }
  const scrollWork = async () => {
    await at(10200)
    await page.evaluate(() => { chatBench.follow = false })
    await live.locator("[data-steps-toggle]").click()
    const card = live.locator("[data-card-row] .ui-tclc__head").first()
    await card.click()
    await live.locator(".ui-tclc__body pre").first().waitFor()
    await at(12900)
    await live.locator("[data-steps-toggle]").click()
    await page.locator("[data-jump-latest]").click()
    await at(15100)
    await page.evaluate(() => { chatBench.follow = false; chatBench.wheelPhase = -1 })
    await page.mouse.move(rect.x + rect.width / 2, rect.y + 120)
    for (let i = 0; i < 39; i++) { await at(15200 + 295 * i); await page.mouse.wheel(0, -260) }
    await at(27000)
    await page.evaluate(() => {
      const b = chatBench, s = b.scroller, top = s.getBoundingClientRect().top
      b.wheelPhase = b.direction = 0
      const row = [...s.querySelectorAll("[data-msg]")].find(el => el.getBoundingClientRect().bottom > top + 1)
      if (row) b.anchor = { row, y: row.getBoundingClientRect().top - top }
    })
    await at(34000)
    // A previously started execution persists while the reader is up in the history.
    patch(row("bench-stream-execution", "The verification command completed.", Date.now(), "steve", { steps: { tools } }))
    await at(38000)
    const held = await page.evaluate(() => {
      const b = chatBench
      const held = !!b.anchor
      b.anchor = null
      b.wheelPhase = 1
      return held
    })
    for (let i = 0; i < 62; i++) { await at(38100 + 180 * i); await page.mouse.wheel(0, 260) }
    await page.evaluate(() => { chatBench.wheelPhase = chatBench.direction = 0; chatBench.follow = true })
    return held
  }
  const [, caret, held] = await Promise.all([feed(), type(), scrollWork()])
  await at(seconds * 1000)
  await page.waitForFunction(({ last, tokenCount }) => chatBench.received === tokenCount &&
    document.querySelector("[data-chat-draft] [data-steve-msg]")?.textContent.includes(last), { last, tokenCount })
  const data = await page.evaluate(() => {
    const b = chatBench
    b.end = performance.now()
    return { ...b, anchor: null, scroller: null, event_timing: PerformanceObserver.supportedEntryTypes.includes("event") }
  })
  patch(row(finalId, content, Date.now(), "steve", { origin: mid, steps: { research: { run: "bench-stream-run" }, tools } }))
  send({ type: "steve.thinking", ws, rev, on: false })
  await page.locator(`[data-msg="${finalId}"]`).waitFor()
  await draft.waitFor({ state: "detached" })
  assert.equal(errors, 0, "uncaught page errors")
  const percentile = (values, q) => [...values].sort((a, b) => a - b)[Math.ceil(values.length * q) - 1]
  const long = data.long.filter(e => e.t >= data.start && e.t <= data.end && e.d > 50)
  const inp = data.events.filter(e => e.t >= data.start && e.t <= data.end && e.id && /^key/.test(e.name))
  const round = x => Math.round(x * 100) / 100
  const result = {
    chromium: browser.version(), platform: platform(), mode: flags.length ? "single-process" : "default", profile,
    load_before, load_after: loadavg(), duration_ms: round(data.end - data.start), tokens: tokenCount, drafts_received: data.received,
    frames: data.frames.length, p95_frame_ms: round(percentile(data.frames, .95)), dropped_frames: data.frames.reduce((n, t) => n + Math.max(0, Math.round(t / (1000 / 60)) - 1), 0),
    long_tasks_gt_50ms: long.length, max_long_task_ms: Math.max(0, ...long.map(e => e.d)),
    long_task_offsets_ms: long.map(e => Math.round(e.t - data.start)),
    layout_shift_no_input: round(data.shifts.filter(e => e.t >= data.start && e.t <= data.end && !e.input).reduce((n, e) => n + e.shift, 0)),
    held_history_row: held, anchor_drift_px: round(data.anchorDrift), anchor_lost_samples: data.anchorLost,
    wheel_jumps: data.jumps, worst_wheel_jump_px: round(data.worstJump), wheel_jump_samples: data.jumpTimes,
    follow_p95_gap_px: percentile(data.followGaps, .95),
    typing_inp_ms: inp.length ? Math.max(...inp.map(e => e.d)) : null,
    typing_inp_observation_floor_ms: data.event_timing ? 16 : null, typing_interactions_observed: new Set(inp.map(e => e.id)).size, caret,
    ...(profile && { commits: data.commits, draft_commits: data.draftCommits,
      commits_per_token: round(data.commits / tokenCount), draft_commits_per_token: round(data.draftCommits / tokenCount),
      renders: Object.fromEntries(Object.entries(data.renders).sort((a, b) => b[1] - a[1])) }),
  }
  console.log(JSON.stringify(result, null, 2))
  if (profile) assert.ok(data.draftCommits && data.renders.MarkdownImpl, "use a named production build for --profile")
  if (gate) assert.ok(!long.length && result.p95_frame_ms <= 16.7 && held && data.anchorDrift <= 2 && !data.anchorLost &&
    !data.jumps && result.follow_p95_gap_px <= 4 && data.event_timing && (result.typing_inp_ms ?? 16) <= 100 &&
    caret.value_ok && caret.selection_ok && caret.style_match && caret.mirror_error_px <= 1, "chat performance gate failed")
} finally {
  await browser?.close()
  clearInterval(ping)
  for (const page of pages) page.end()
  fixture.close()
}
