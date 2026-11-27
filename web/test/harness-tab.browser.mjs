// Browser smoke test for the redesigned Node Harness tab:
// MacBook-scale fixture (18 harnesses, Claude Code with 66 models) verifying section counts,
// default collapse/expand, structured model families, snapshot collapsing, live search,
// typed unavailability reasons + actions, details fold, bilingual i18n, and 390px layout.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import {
  CLAUDE_CODE_MODELS,
  DIAG_ALLOWED_HARNESS_REPORT,
  DIAG_ALLOWED_HARNESS_UPDATES,
  DIAG_ALLOWED_NODE_ID,
  DIAG_DENIED_HARNESS_REPORT,
  DIAG_DENIED_HARNESS_UPDATES,
  DIAG_DENIED_NODE_ID,
  MACBOOK_HARNESS_REPORT,
  MACBOOK_HARNESS_UPDATES,
  MACBOOK_NODE_ID,
  MACBOOK_NOW,
} from "./fixtures/harness-tab-data.ts"

const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
})
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 960 } })
page.setDefaultTimeout(12_000)

const errors = []
page.on("pageerror", (e) => errors.push(e.stack || e.message))

const GIB = 2 ** 30
const makeNode = (id, name) => ({
  node_id: id,
  name,
  fingerprint: "9e3f5a07c21b84d60f9a77e3b10c4d52",
  state: "active",
  linked: true,
  local: false,
  max_sessions: 8,
  running: 0,
  waiting: 0,
  last_seen: MACBOOK_NOW - 5_000,
  offline: null,
  link: { route: "lan", via: "direct", tls: false, since: MACBOOK_NOW - 3_600_000, latency_ms: 8, latency_at: MACBOOK_NOW - 5_000 },
  host: { hostname: `${id}.local`, os: "macos", os_name: "macOS", os_version: "26.1", arch: "aarch64", cpus: 14, mem_total: 48 * GIB },
  upgrade: { build: { version: "0.2.112", commit: "abc12345", target: "aarch64-apple-darwin" }, policy: "notify", own_policy: null, can: false, reason: "current", latest: null },
})
const nodes = [
  makeNode(MACBOOK_NODE_ID, "MacBook Pro"),
  makeNode(DIAG_DENIED_NODE_ID, "MacBook Policy Denied"),
  makeNode(DIAG_ALLOWED_NODE_ID, "MacBook Policy Allowed"),
]
let allowedReport = structuredClone(DIAG_ALLOWED_HARNESS_REPORT)
const installPosts = []

const json = (route, body, status = 200) =>
  route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })

const routeApi = async (route) => {
  const req = route.request(), url = new URL(req.url()), p = url.pathname
  if (url.origin !== base) return route.abort("blockedbyclient")
  if (!p.startsWith("/api/")) return route.continue()
  if (p === "/api/nodes") return json(route, { nodes })
  if (p === "/api/devices") return json(route, { devices: [], runtimes: [] })
  if (p === "/api/vitals") return json(route, { nodes: {} })
  if (p === "/api/node-harnesses") {
    return json(route, {
      nodes: {
        [MACBOOK_NODE_ID]: MACBOOK_HARNESS_REPORT,
        [DIAG_DENIED_NODE_ID]: DIAG_DENIED_HARNESS_REPORT,
        [DIAG_ALLOWED_NODE_ID]: allowedReport,
      },
    })
  }
  if (p === `/api/node-harnesses/${MACBOOK_NODE_ID}`) {
    return json(route, { node_id: MACBOOK_NODE_ID, report: MACBOOK_HARNESS_REPORT })
  }
  if (p === `/api/node-harnesses/${DIAG_DENIED_NODE_ID}`) {
    return json(route, { node_id: DIAG_DENIED_NODE_ID, report: DIAG_DENIED_HARNESS_REPORT })
  }
  if (p === `/api/node-harnesses/${DIAG_ALLOWED_NODE_ID}`) {
    return json(route, { node_id: DIAG_ALLOWED_NODE_ID, report: allowedReport })
  }
  if (p === `/api/node-harnesses/${DIAG_ALLOWED_NODE_ID}/pi/install` && req.method() === "POST") {
    installPosts.push({ path: p, body: req.postDataJSON() })
    allowedReport = {
      ...allowedReport,
      harnesses: allowedReport.harnesses.map((h) =>
        h.id === "pi"
          ? {
              ...h,
              degraded_reason: "installing",
              setup: { ...h.setup, state: "installing", can_install: false },
            }
          : h,
      ),
    }
    return json(route, { accepted: true, operation: "install_pi_fixture" })
  }
  if (p === `/api/node-harness-updates/${MACBOOK_NODE_ID}`) return json(route, MACBOOK_HARNESS_UPDATES)
  if (p === `/api/node-harness-updates/${DIAG_DENIED_NODE_ID}`) return json(route, DIAG_DENIED_HARNESS_UPDATES)
  if (p === `/api/node-harness-updates/${DIAG_ALLOWED_NODE_ID}`) return json(route, DIAG_ALLOWED_HARNESS_UPDATES)
  if (p === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: "0.2.112", commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
  if (p === "/api/node-context") return json(route, { owner_context: "off" })
  if (p === "/api/access") return json(route, { tokens: [], clients: [], current: null })
  return json(route, {}, 404)
}

await page.route("**/*", routeApi)

const shotsDir = process.env.STEVE_TEST_SHOTS || ""
async function snap(filename) {
  if (!shotsDir) return
  await mkdir(shotsDir, { recursive: true })
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))))
  await page.screenshot({ path: `${shotsDir}/${filename}`, fullPage: true, animations: "disabled" })
}

try {
  assert.equal(MACBOOK_HARNESS_REPORT.harnesses.length, 18, "MacBook fixture has 18 harnesses")
  assert.ok(CLAUDE_CODE_MODELS.length >= 60, "Claude Code fixture has 60+ models")

  await page.goto(`${base}/test/fixtures/devices.html?strict#/devices/nodes/${MACBOOK_NODE_ID}/harness`)
  const root = page.locator(`[data-node-harnesses="${MACBOOK_NODE_ID}"]`)
  await root.locator('[data-harness-section="available"]').waitFor()

  // 1. Section counts & default open/closed states
  assert.equal(await root.locator('[data-harness-section-count="available"]').innerText(), "4")
  assert.equal(await root.locator('[data-harness-section-count="attention"]').innerText(), "4")
  assert.equal(await root.locator('[data-harness-section-count="not_installed"]').innerText(), "10")

  assert.equal(await root.locator('[data-harness-section="available"]').getAttribute("data-open"), "true")
  assert.equal(await root.locator('[data-harness-section="attention"]').getAttribute("data-open"), "true")
  assert.equal(await root.locator('[data-harness-section="not_installed"]').getAttribute("data-open"), "false")
  assert.equal(await root.locator('[data-harness-section="not_installed"] [data-node-harness-id]').count(), 0, "Not installed starts collapsed")

  // Deterministic ordering inside Available and Needs Attention
  const availIds = await root.locator('[data-harness-section="available"] [data-node-harness-id]').evaluateAll((els) => els.map((el) => el.getAttribute("data-node-harness-id")))
  assert.deepEqual(availIds, ["claude-code", "codex", "gemini", "opencode"])

  const attnIds = await root.locator('[data-harness-section="attention"] [data-node-harness-id]').evaluateAll((els) => els.map((el) => el.getAttribute("data-node-harness-id")))
  assert.deepEqual(attnIds, ["kimi-code", "antigravity", "pi", "codebuddy-code"])

  // Typed reasons, MissingCommandDiagnostic, and concrete actions on every Needs Attention card
  const agyCard = root.locator('[data-node-harness-id="antigravity"]')
  await agyCard.locator('[data-node-harness-reason="command_not_in_service_path"]').waitFor()
  await agyCard.locator("[data-node-harness-missing-command]").waitFor()
  assert.equal(await agyCard.locator("[data-missing-command-login-path]").innerText(), "/Users/example/.local/bin/agy")
  assert.equal(await agyCard.locator("[data-missing-command-service-path]").innerText(), "/usr/bin:/bin:/usr/sbin:/sbin")
  assert.equal(await agyCard.locator("[data-missing-command-refresh]").innerText(), "steve node refresh-env")
  await agyCard.locator('[data-harness-attention-action="copy_refresh_env"][data-harness-refresh-env-copy="antigravity"]').waitFor()
  await agyCard.locator('[data-harness-attention-action="refresh"]').waitFor()
  await root.locator('[data-node-harness-id="kimi-code"] [data-node-harness-reason="authentication_required"]').waitFor()
  await root.locator('[data-node-harness-id="pi"] [data-node-harness-reason="version_outdated"]').waitFor()
  await root.locator('[data-node-harness-id="pi"] [data-harness-attention-action="upgrade"]').waitFor()
  await root.locator('[data-node-harness-id="codebuddy-code"] [data-node-harness-reason="session_unavailable"]').waitFor()

  // Secondary details & Kimi's last reported models stay folded inside Details by default
  assert.equal(await root.getByText("the adapter's command is missing", { exact: false }).isVisible(), false)
  assert.equal(await root.locator('[data-node-harness-id="kimi-code"] [data-node-harness-reported-models]').isVisible(), false)

  // Compact Claude Code card: default model + family chips + "全部 66 个模型" toggle (not a wall of 66 IDs)
  const claude = root.locator('[data-node-harness-id="claude-code"]')
  await claude.locator('[data-harness-default-model="claude-opus-4-6"]').waitFor()
  const chipFamilies = await claude.locator("[data-harness-family-chip]").evaluateAll((els) => els.map((el) => el.getAttribute("data-harness-family-chip")))
  assert.deepEqual(chipFamilies, ["claude", "deepseek", "gemini", "gpt", "kimi", "glm", "llama", "qwen"])
  assert.equal(await claude.getByText("auto-mode-classifier").count(), 0, "unrecognized IDs stay out of summary chips")
  assert.match(await claude.locator("[data-harness-models-toggle]").innerText(), /全部 66 个模型/)

  // Capture default glanceable Light + Dark desktop screenshots
  await snap("harness-tab-light.png")
  await page.evaluate(() => document.documentElement.classList.add("dark"))
  await snap("harness-tab-dark.png")
  await page.evaluate(() => document.documentElement.classList.remove("dark"))

  // 2. Expand "全部 66 个模型", verify grouping, snapshot collapsing, and search
  await claude.locator("[data-harness-models-toggle]").click()
  const drawer = claude.locator("[data-harness-models-drawer]")
  await drawer.waitFor()
  const groupNames = await drawer.locator("[data-model-family-group]").evaluateAll((els) => els.map((el) => el.getAttribute("data-model-family-group")))
  assert.deepEqual(groupNames, ["claude", "deepseek", "gemini", "gpt", "kimi", "glm", "llama", "qwen", "other"])
  // Snapshot collapsed under alias until expanded or matched by search
  await drawer.locator('[data-model-snapshots-toggle="claude-opus-4-5"]').waitFor()
  assert.equal(await drawer.locator('[data-model-snapshots-list="claude-opus-4-5"]').count(), 0)

  // Search by dated snapshot ID -> finds alias row and auto-reveals matching snapshot
  const search = drawer.locator("[data-harness-models-search]")
  await search.fill("20251101")
  await drawer.locator('[data-model-snapshots-list="claude-opus-4-5"]').getByText("claude-opus-4-5-20251101").waitFor()
  assert.equal(await drawer.locator("[data-model-family-group]").count(), 1)

  // Search by passthrough source -> finds passthrough + passthrough_local families
  await search.fill("passthrough")
  const ptGroups = await drawer.locator("[data-model-family-group]").evaluateAll((els) => els.map((el) => el.getAttribute("data-model-family-group")))
  assert.deepEqual(ptGroups, ["glm", "llama", "qwen"])

  // Search with no results -> empty message
  await search.fill("no-such-model-xyz")
  await drawer.locator("[data-harness-models-empty]").waitFor()

  // Clear search -> all 9 groups restored
  await search.fill("")
  assert.equal(await drawer.locator("[data-model-family-group]").count(), 9)

  // 3. Expand Not installed section -> 10 compact rows with install entries; expand Antigravity Details
  await root.locator('[data-harness-section-toggle="not_installed"]').click()
  assert.equal(await root.locator('[data-harness-section="not_installed"] [data-node-harness-id]').count(), 10)
  await root.locator('[data-node-harness-id="cursor"] [data-harness-install-copy="cursor"]').waitFor()

  await root.locator('[data-node-harness-id="antigravity"] [data-node-harness-details] summary').click()
  await root.getByText("the adapter's command is missing", { exact: false }).waitFor()

  // Capture expanded Light + Dark desktop screenshots
  await snap("harness-tab-expanded-light.png")
  await page.evaluate(() => document.documentElement.classList.add("dark"))
  await snap("harness-tab-expanded-dark.png")
  await page.evaluate(() => document.documentElement.classList.remove("dark"))

  // Collapse Not installed + Claude models + Antigravity details for crisp 390px overview, then check 390px expanded too
  await root.locator('[data-node-harness-id="antigravity"] [data-node-harness-details] summary').click()
  await root.locator('[data-harness-section-toggle="not_installed"]').click()
  await claude.locator("[data-harness-models-toggle]").click()
  await page.setViewportSize({ width: 390, height: 844 })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "390px collapsed fits without horizontal overflow")
  await snap("harness-tab-390-light.png")
  await page.evaluate(() => document.documentElement.classList.add("dark"))
  await snap("harness-tab-390-dark.png")
  await page.evaluate(() => document.documentElement.classList.remove("dark"))

  // Expand models + not-installed at 390px and verify still zero horizontal overflow
  await claude.locator("[data-harness-models-toggle]").click()
  await root.locator('[data-harness-section-toggle="not_installed"]').click()
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "390px expanded fits without horizontal overflow")

  // 4. Verify all 4 typed missing/broken diagnosis cards + exec_denied policy guidance + Access entry + remote install
  await page.setViewportSize({ width: 1280, height: 960 })
  await page.goto(`${base}/test/fixtures/devices.html?strict#/devices/nodes/${DIAG_DENIED_NODE_ID}/harness`)
  const diagRoot = page.locator(`[data-node-harnesses="${DIAG_DENIED_NODE_ID}"]`)
  await diagRoot.locator('[data-harness-section="attention"]').waitFor()
  assert.equal(await diagRoot.locator('[data-harness-section-count="attention"]').innerText(), "3")
  assert.equal(await diagRoot.locator('[data-harness-section-count="not_installed"]').innerText(), "1")

  // Pi: CLI present on service PATH, ACP adapter package missing + exec_denied explanation (never "服务 PATH 缺失")
  const piDiagCard = diagRoot.locator('[data-node-harness-id="pi"]')
  await piDiagCard.locator('[data-node-harness-reason="adapter_package_missing"]').waitFor()
  const piText = await piDiagCard.innerText()
  assert.match(piText, /未安装 ACP 适配器/)
  assert.match(piText, /未安装 Pi 的 ACP 适配器（@openma\/pi-acp）/)
  assert.doesNotMatch(piText, /服务 PATH 缺失/)
  assert.equal(await piDiagCard.locator("[data-missing-command-cli-path]").innerText(), "/usr/local/bin/pi")
  assert.equal(await piDiagCard.locator("[data-missing-command-adapter-pkg]").innerText(), "@openma/pi-acp")
  assert.equal(await piDiagCard.locator("[data-missing-command-install-cli]").innerText(), "steve adapters install pi node")
  await piDiagCard.locator('[data-harness-attention-action="copy_adapter_install"][data-harness-install-copy="pi"]').waitFor()
  // Single clear exec_denied explanation (not duplicated) with policy.json detail and clickable Access entry
  assert.equal(await piDiagCard.locator('[data-harness-blocked-reason="exec_denied"]').count(), 1)
  assert.match(await piDiagCard.locator('[data-harness-blocked-reason="exec_denied"]').innerText(), /policy\.json/)
  assert.match(await piDiagCard.locator('[data-harness-blocked-reason="exec_denied"]').innerText(), /steve node restart/)

  // Antigravity: command missing from service PATH -> refresh-env
  await diagRoot.locator('[data-node-harness-id="antigravity"] [data-node-harness-reason="command_not_in_service_path"]').waitFor()
  await diagRoot.locator('[data-node-harness-id="antigravity"] [data-harness-attention-action="copy_refresh_env"]').waitFor()

  // Codex: adapter installed, startup failed -> initialize_failed (no missing_command block)
  const codexDiagCard = diagRoot.locator('[data-node-harness-id="codex"]')
  await codexDiagCard.locator('[data-node-harness-reason="initialize_failed"]').waitFor()
  assert.match(await codexDiagCard.innerText(), /启动失败/)
  assert.equal(await codexDiagCard.locator("[data-node-harness-missing-command]").count(), 0)

  // Clicking the Access policy entry navigates directly to the Access tab's execution policy section
  await piDiagCard.locator("[data-open-access-policy]").click()
  await page.waitForFunction((id) => location.hash === `#/devices/nodes/${id}/access`, DIAG_DENIED_NODE_ID)
  const policySection = page.locator('[data-node-execution-policy="denied"]')
  await policySection.waitFor()
  assert.match(await policySection.innerText(), /policy\.json/)
  assert.match(await policySection.innerText(), /steve node restart/)

  // Remote Node with exec allowed: Pi shows one-click Install ACP adapter button and posts to /api/node-harnesses/{id}/pi/install
  await page.goto(`${base}/test/fixtures/devices.html?strict#/devices/nodes/${DIAG_ALLOWED_NODE_ID}/harness`)
  const allowedRoot = page.locator(`[data-node-harnesses="${DIAG_ALLOWED_NODE_ID}"]`)
  const piInstallBtn = allowedRoot.locator('[data-node-harness-id="pi"] [data-harness-attention-action="install"][data-harness-install="pi"]')
  await piInstallBtn.waitFor()
  assert.match(await piInstallBtn.innerText(), /安装 ACP 适配器/)
  await piInstallBtn.click()
  await allowedRoot.locator('[data-node-harness-id="pi"] [data-node-harness-reason="installing"]').waitFor()
  assert.deepEqual(installPosts, [{ path: `/api/node-harnesses/${DIAG_ALLOWED_NODE_ID}/pi/install`, body: {} }])

  // 5. Verify English i18n renders cleanly without missing keys across both layouts and new diagnosis cards
  const enPage = await browser.newPage({ locale: "en-US", viewport: { width: 1280, height: 960 } })
  enPage.setDefaultTimeout(12_000)
  enPage.on("pageerror", (e) => errors.push(e.stack || e.message))
  await enPage.route("**/*", routeApi)
  await enPage.goto(`${base}/test/fixtures/devices.html?strict#/devices/nodes/${MACBOOK_NODE_ID}/harness`)
  const enRoot = enPage.locator(`[data-node-harnesses="${MACBOOK_NODE_ID}"]`)
  await enRoot.locator('[data-harness-section-toggle="available"]').getByText("Available").waitFor()
  await enRoot.locator('[data-harness-section-toggle="attention"]').getByText("Needs attention").waitFor()
  await enRoot.locator('[data-harness-section-toggle="not_installed"]').getByText("Not installed").waitFor()
  await enRoot.locator('[data-node-harness-id="antigravity"]').getByText("Missing from service PATH").waitFor()
  await enRoot.locator('[data-node-harness-id="antigravity"]').getByText("Copy refresh command").waitFor()
  await enRoot.locator('[data-node-harness-id="antigravity"] [data-missing-command-refresh]').getByText("steve node refresh-env").waitFor()
  await enRoot.locator('[data-node-harness-id="pi"]').getByText("Version outdated").waitFor()
  await enRoot.locator('[data-node-harness-id="claude-code"] [data-harness-models-toggle]').getByText("All 66 models").waitFor()
  await enPage.goto(`${base}/test/fixtures/devices.html?strict#/devices/nodes/${DIAG_DENIED_NODE_ID}/harness`)
  const enDiag = enPage.locator(`[data-node-harnesses="${DIAG_DENIED_NODE_ID}"]`)
  await enDiag.locator('[data-node-harness-id="pi"]').getByText("ACP adapter not installed").waitFor()
  await enDiag.locator('[data-node-harness-id="pi"]').getByText("Pi's ACP adapter (@openma/pi-acp) is not installed", { exact: false }).waitFor()
  await enDiag.locator('[data-node-harness-id="pi"] [data-open-access-policy]').getByText("View execution policy in Access").waitFor()
  await enDiag.locator('[data-node-harness-id="codex"]').getByText("Startup failed").waitFor()
  await enPage.close()

  assert.deepEqual(errors, [])
  process.stdout.write("Harness tab layout: 18 harnesses (4/4/10), 4-way diagnosis cards, exec_denied Access flow, remote adapter install, en/zh i18n, and 390px passed\n")
} finally {
  await browser.close()
}
