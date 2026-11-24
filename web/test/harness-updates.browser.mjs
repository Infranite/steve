// Real device components, isolated API fixtures. Vendor packages and a remote machine are not used.
// Fixture shapes match web/src/gen/* (HarnessUpdatesResponse, HarnessUpdateView, HarnessComponentUpdateView, HarnessAutoUpgradePolicy).
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { chromium } from "playwright"

const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:19421"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 950 } })
page.setDefaultTimeout(12000)

const errors = []
const writes = []
const autoWrites = []
let harnessProbes = 0
page.on("pageerror", (e) => errors.push(e.stack || e.message))

const node = (id) => ({
  node_id: id,
  name: id === "local" ? "Local Hub" : "Studio Mac",
  state: "active",
  linked: true,
  local: id === "local",
  running: 0,
  waiting: 0,
  max_sessions: 4,
  fingerprint: "fixture",
  upgrade: { reason: "manual", can: false },
})

const component = (id, installed, opts = {}) => ({
  id,
  name: opts.name ?? (id === "cli" ? "Fixture CLI" : "Fixture ACP"),
  package: opts.package ?? ("@example/" + id),
  managed: opts.managed ?? true,
  source: opts.source ?? ((opts.managed ?? true) ? "steve_managed" : "external"),
  upgrade_method: opts.upgrade_method ?? ((opts.managed ?? true) ? "steve_managed" : "unsupported"),
  upgrade_command: opts.upgrade_command,
  can_upgrade: opts.can_upgrade ?? (opts.managed ?? true),
  state: opts.state ?? "available",
  installed,
  latest: opts.latest === undefined ? "2.0.0" : opts.latest,
  checked: Date.now(),
  reason: opts.reason,
})

const report = (id, version) => ({
  report: {
    node: id,
    checking: false,
    harnesses: [
      {
        harness: "fixture",
        name: "Fixture Runtime",
        can_upgrade: true,
        active_sessions: 0,
        components: [
          component("cli", version, {
            managed: false,
            source: "homebrew_cask",
            upgrade_method: "homebrew_cask",
            upgrade_command: "brew upgrade --cask fixture-cli",
            can_upgrade: true,
            state: "available",
            latest: "2.0.0",
          }),
          component("adapter", version, {
            managed: true,
            source: "steve_managed",
            upgrade_method: "steve_managed",
            can_upgrade: true,
            state: "available",
            latest: "2.0.0",
          }),
        ],
      },
      {
        harness: "self_updater",
        name: "Standalone CLI",
        can_upgrade: true,
        active_sessions: 0,
        components: [
          component("cli", "1.5.16", {
            name: "Standalone CLI",
            package: undefined,
            managed: false,
            source: "standalone",
            upgrade_method: "self_update",
            upgrade_command: "standalone update",
            can_upgrade: true,
            state: "check_on_upgrade",
            latest: undefined,
          }),
        ],
      },
      {
        harness: "readonly_ext",
        name: "Read-only Wrapper",
        can_upgrade: false,
        active_sessions: 0,
        components: [
          component("cli", "1.3.0", {
            name: "Wrapper CLI",
            managed: false,
            source: "external",
            upgrade_method: "unsupported",
            can_upgrade: false,
            state: "unsupported",
            latest: "1.4.0",
            reason: "Installed under a root-owned custom wrapper; upgrade manually on the host",
          }),
        ],
      },
    ],
  },
  runs: {},
  retryable_unknown: [],
  auto_upgrade: { enabled: false, harnesses: {} },
  at: Date.now(),
  error: null,
  code: null,
})

const data = { local: report("local", "1.0.0"), remote: report("remote", "1.5.0") }
let holdPost, releasePost, failPost = false

await page.route("**/*", async (route) => {
  const req = route.request(), u = new URL(req.url()), p = u.pathname
  if (u.origin !== base) return route.abort("blockedbyclient")
  if (!p.startsWith("/api/")) return route.continue()

  if (p.startsWith("/api/node-harness-updates/")) {
    const [, , , id, sub] = p.split("/")
    if (req.method() === "PUT" && sub === "auto-upgrade") {
      const body = req.postDataJSON()
      autoWrites.push({ id, ...body })
      const policy = data[id].auto_upgrade
      if (typeof body.enabled === "boolean") policy.enabled = body.enabled
      if (body.harness && body.mode) {
        if (body.mode === "inherit") delete policy.harnesses[body.harness]
        else policy.harnesses[body.harness] = body.mode === "on"
      }
      return route.fulfill({ json: policy })
    }
    if (req.method() === "POST") {
      assert.equal(id, "remote")
      const body = req.postDataJSON()
      writes.push({ harness: sub, ...body })
      if (holdPost) await holdPost
      if (failPost) return route.fulfill({ status: 403, json: { error: "Node policy denies exec" } })
      const targetHarness = data.remote.report?.harnesses.find((h) => h.harness === sub)
      const state = (targetHarness?.active_sessions ?? 0) > 0 ? { state: "waiting" } : { state: "running" }
      const receipt = { operation: body.operation, state }
      data.remote.runs[sub] = receipt
      return route.fulfill({ json: { receipt } })
    }
    return route.fulfill({ json: data[id] })
  }

  if (p.startsWith("/api/node-harnesses/")) {
    const id = p.split("/")[3]
    harnessProbes += 1
    return route.fulfill({
      json: {
        node_id: id,
        report: {
          state: "reported",
          stale: false,
          refreshing: false,
          checked_at: Date.now(),
          harnesses: [{ id: "fixture", name: "Fixture Runtime", available: true, version: "2.0.0", adapter_version: "2.0.0", models: [id + "/model"] }],
        },
      },
    })
  }

  const response = p === "/api/nodes" ? { nodes: [node("local"), node("remote")] }
    : p === "/api/node-harnesses" ? {
      nodes: Object.fromEntries(["local", "remote"].map((id) => [
        id,
        {
          state: "reported",
          stale: false,
          refreshing: false,
          checked_at: Date.now(),
          harnesses: [{ id: "fixture", name: "Fixture Runtime", available: true, version: id === "local" ? "1.0.0" : "1.5.0", adapter_version: "1.0.0", models: [id + "/model"] }],
        },
      ])),
    }
    : p === "/api/devices" ? { devices: [{ id: "hub", name: "Local", roles: ["Hub"], node_id: "local", connected: true, status: "正常", max_sessions: 4 }], runtimes: [] }
    : p === "/api/vitals" ? { nodes: {} }
    : p === "/api/node-context" ? { owner_context: "off" }
    : p === "/api/nodes/upgrade-default" ? { policy: "notify", drain_secs: 1800, health_secs: 180 }
    : p === "/api/access" ? { clients: [], tokens: [], current: null }
    : null
  if (p.includes("contacts") || p.startsWith("/api/people")) return route.fulfill({ status: 404, json: {} })
  return route.fulfill({ status: response ? 200 : 404, json: response ?? {} })
})

const panel = () => page.locator("[data-harness-updates]")
const open = async (id, tab = "harness") => {
  const hash = tab === "overview" ? `#/devices/nodes/${id}` : `#/devices/nodes/${id}/${tab}`
  await page.goto(base + "/test/fixtures/devices.html?strict" + hash)
  if (tab === "harness") {
    await panel().locator("[data-harness-update=fixture]").waitFor()
    await page.waitForFunction(
      (nid) => document.querySelector(`[data-harness-updates="${nid}"] [data-harness-update=fixture]`)?.textContent.includes(nid === "local" ? "1.0.0" : "1.5.0"),
      id,
    )
    await page.locator(`[data-node-harnesses="${id}"] [data-node-harness-models]`).getByText(id + "/model", { exact: true }).waitFor()
  }
}
const reread = () => page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")))

try {
  // 1. Node identity isolation
  await open("local")
  assert.match(await panel().innerText(), /1\.0\.0/)
  await open("remote")
  assert.match(await panel().innerText(), /1\.5\.0/)
  assert.doesNotMatch(await panel().innerText(), /1\.0\.0/)
  assert.match(await panel().innerText(), /Homebrew Cask/)
  assert.match(await panel().innerText(), /Installed under a root-owned custom wrapper/)
  assert.equal(await panel().locator("[data-harness-upgrade=readonly_ext]").count(), 0, "unsupported external wrapper has no upgrade button")

  // 2. External Homebrew Cask + managed ACP upgrade submits BOTH targets & guards duplicate clicks
  holdPost = new Promise((resolve) => { releasePost = resolve })
  const upgrade = () => panel().locator("[data-harness-upgrade=fixture]")
  await upgrade().click()
  await page.waitForFunction(() => document.querySelector("[data-harness-upgrade=fixture]")?.disabled)
  await page.waitForTimeout(150)
  assert.equal(writes.length, 1, "duplicate clicks cannot dispatch another command")
  assert.deepEqual(writes[0].versions, { cli: "2.0.0", adapter: "2.0.0" }, "both external brew CLI and managed ACP are included")
  assert.equal(await panel().locator("[data-harness-upgrade-result=succeeded]").count(), 0)
  holdPost = null; releasePost()
  await panel().locator("[data-harness-upgrade-result=running]").waitFor()

  // 3. Unknown outcome while offline blocks retry; completion triggers automatic harness re-probe
  data.remote.runs.fixture.state = { state: "unknown", reason: "Link lost after acceptance" }
  data.remote.error = "Node offline"; data.remote.code = "offline"
  await reread()
  await panel().locator("[data-harness-upgrade-result=unknown]").waitFor()
  assert.equal(await upgrade().isDisabled(), true)
  assert.equal(await panel().locator("[data-harness-upgrade-result=succeeded]").count(), 0)

  const probesBefore = harnessProbes
  data.remote.error = null; data.remote.code = null
  data.remote.runs.fixture.state = { state: "finished", outcome: "succeeded" }
  data.remote.report.harnesses[0].components[0] = component("cli", "2.0.0", { managed: false, source: "homebrew_cask", upgrade_method: "homebrew_cask", can_upgrade: true, state: "current", latest: "2.0.0" })
  data.remote.report.harnesses[0].components[1] = component("adapter", "2.0.0", { managed: true, state: "current", latest: "2.0.0" })
  await reread()
  await panel().locator("[data-harness-upgrade-result=succeeded]").waitFor()
  await page.waitForFunction(() => true)
  await page.waitForTimeout(150)
  assert.ok(harnessProbes > probesBefore, "completed upgrade automatically re-probes Node harnesses")
  await page.reload()
  await panel().locator("[data-harness-upgrade-result=succeeded]").waitFor()
  assert.equal(writes.length, 1, "refresh only reads the persisted receipt")

  // 4. Standalone check-on-upgrade CLI triggers self_update explicitly
  await panel().locator("[data-harness-upgrade=self_updater]").click()
  await panel().locator("[data-harness-update=self_updater] [data-harness-upgrade-result=running]").waitFor()
  assert.equal(writes.length, 2)
  assert.equal(writes[1].harness, "self_updater")
  assert.deepEqual(writes[1].versions, {})
  assert.deepEqual(writes[1].self_update, ["cli"])
  delete data.remote.runs.self_updater

  // 5. Active sessions show "空闲后升级" and enter waiting receipt state
  data.remote.report.harnesses[0].active_sessions = 2
  data.remote.report.harnesses[0].components[0] = component("cli", "1.5.0", { managed: false, source: "homebrew_cask", upgrade_method: "homebrew_cask", can_upgrade: true, state: "available", latest: "2.0.0" })
  delete data.remote.runs.fixture
  await reread()
  await panel().locator("[data-harness-active-sessions='2']").waitFor()
  assert.match(await upgrade().innerText(), /空闲后升级/)
  await upgrade().click()
  await panel().locator("[data-harness-upgrade-result=waiting]").waitFor()
  assert.equal(await upgrade().isDisabled(), true)
  data.remote.report.harnesses[0].active_sessions = 0

  // 6. Visible failure reason (not buried in details) + retryable after cause fixed
  data.remote.runs.fixture = { operation: "op_fail", state: { state: "finished", outcome: "failed", error: { code: "denied", safe_detail: "Node policy denies exec" } } }
  failPost = true
  await reread()
  await panel().locator("[data-harness-upgrade-result=failed]").waitFor()
  assert.match(await panel().locator("[data-harness-upgrade-detail]").innerText(), /Node policy denies exec/)
  await upgrade().click()
  await panel().locator("[data-harness-update-error]").waitFor()
  failPost = false

  // 7. Auto-upgrade switches: Node-level (default OFF) + per-harness override + inherit reset; check_on_upgrade disabled
  const nodeSwitch = page.locator("[data-harness-auto-upgrade-node]")
  const fixtureSwitch = page.locator("[data-harness-auto-upgrade=fixture]")
  const selfSwitch = page.locator("[data-harness-auto-upgrade=self_updater]")
  assert.equal(await nodeSwitch.isChecked(), false, "Node auto-upgrade defaults to off")
  assert.equal(await fixtureSwitch.isChecked(), false)
  assert.equal(await selfSwitch.isDisabled(), true, "check_on_upgrade CLI cannot auto-upgrade")

  await nodeSwitch.click()
  await page.waitForFunction(() => document.querySelector("[data-harness-auto-upgrade-node]")?.checked === true)
  assert.equal(await fixtureSwitch.isChecked(), true, "inheriting harness follows Node default")

  await fixtureSwitch.click()
  await page.waitForFunction(() => document.querySelector("[data-harness-auto-upgrade=fixture]")?.checked === false)
  await page.locator("[data-harness-auto-upgrade-reset=fixture]").waitFor()

  await page.locator("[data-harness-auto-upgrade-reset=fixture]").click()
  await page.waitForFunction(() => document.querySelector("[data-harness-auto-upgrade=fixture]")?.checked === true)
  assert.equal(await page.locator("[data-harness-auto-upgrade-reset=fixture]").count(), 0)
  assert.deepEqual(autoWrites, [
    { id: "remote", enabled: true },
    { id: "remote", harness: "fixture", mode: "off" },
    { id: "remote", harness: "fixture", mode: "inherit" },
  ])

  // 8. Overview "Needs your attention" shows harness update and shares the exact same upgrade action
  delete data.remote.runs.fixture
  const writesBeforeOverview = writes.length
  await open("remote", "overview")
  await page.locator('[data-node-action="harnessUpdatesAvailable"]').waitFor()
  await page.locator('[data-node-action="harnessUpdatesAvailable"] [data-node-action-run="upgradeHarnesses"]').click()
  await page.locator('[data-node-action="harnessUpdatesAvailable"]').waitFor({ state: "detached" })
  assert.equal(writes.length, writesBeforeOverview + 1)
  assert.equal(writes[writes.length - 1].harness, "fixture")

  // 9. Old Node explicitly asks for a Node upgrade, without enabled package writes
  await open("remote", "harness")
  data.remote.report = null; data.remote.error = "unsupported harness-updates-v1"; data.remote.code = "unsupported"
  await reread()
  await panel().getByText(/此 Node 尚不支持组件更新/).waitFor()
  assert.equal(await panel().locator("[data-harness-upgrade]").count(), 0)

  // 10. Desktop + narrow mobile layouts fit without horizontal overflow
  for (const width of [1280, 390]) {
    data.remote.report = report("remote", "1.5.0").report
    data.remote.runs = {}
    data.remote.error = null
    data.remote.code = null
    await reread()
    await panel().locator("[data-harness-update=fixture]").waitFor()
    await page.setViewportSize({ width, height: 950 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no horizontal overflow at ${width}px`)
    if (process.env.STEVE_TEST_SHOTS) {
      await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true })
      await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + `/harness-updates-${width}.png`, fullPage: true })
    }
  }

  assert.deepEqual(errors, [])
  console.log("harness updates browser: passed (external brew/self-update/unsupported, idle queue, visible failure, auto-upgrade switches, overview shared action, narrow layout)")
} catch (e) {
  console.error({ errors, body: await page.locator("body").innerText() })
  throw e
} finally {
  await browser.close()
}
