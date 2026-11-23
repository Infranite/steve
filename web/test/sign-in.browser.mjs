// The full app's sign-in page against fixture answers (not a real Hub): /auth/hello offers token sign-in and /api/ says unauthenticated.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5232"
const errors = []

async function open({ lang = "zh-CN", reducedMotion = "no-preference" } = {}) {
  const page = await browser.newPage({ locale: lang, viewport: { width: 1000, height: 800 }, reducedMotion })
  page.setDefaultTimeout(12000)
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript((lang) => { localStorage.setItem("steve-lang", lang) }, lang)
  await page.route((u) => u.pathname === "/auth/hello" || u.pathname.startsWith("/api/"), (route) => {
    const p = new URL(route.request().url()).pathname
    const json = (body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
    if (p === "/auth/hello") return json({ name: "steve", api: 2, version: "fixture", login: { token: true, oidc: false } })
    if (p === "/api/plugins/themes") return json({ themes: [] })
    return json({ error: "unauthenticated", code: "unauthenticated", retry: { kind: "never" }, reason: null, params: {}, login: null, token: true }, 401)
  })
  await page.goto(base + "/")
  await page.locator("[data-token-sign-in]").waitFor()
  return page
}

/** where Steve's left eye is drawn, once the live face has rendered it */
async function eyeX(page) {
  return page.locator("agent-robot-avatar").evaluate((el) => {
    const r = el.shadowRoot.getElementById("leftEye").getBoundingClientRect()
    return r.left + r.width / 2
  })
}

/** moves the pointer beside the face for a while (the face looks at a fresh pointer only), then reads the eye */
async function lookFrom(page, side) {
  const box = await page.locator("agent-robot-avatar").boundingBox()
  const cx = box.x + box.width / 2, cy = box.y + box.height / 2
  for (let i = 0; i < 24; i++) {
    await page.mouse.move(cx + side * (55 + (i % 2)), cy + (i % 3))
    await page.waitForTimeout(25)
  }
  return eyeX(page)
}

async function face(page) {
  await page.locator("agent-robot-avatar").waitFor({ state: "attached" })
  await page.waitForFunction(() => !!document.querySelector("agent-robot-avatar")?.shadowRoot?.getElementById("leftEye"))
  await page.waitForTimeout(400)
}

try {
  for (const lang of ["zh-CN", "en"]) {
    const page = await open({ lang })
    /* the Hub's token guidance lives in docs/deployment.md, not under the button */
    const text = await page.locator("body").innerText()
    for (const gone of ["owner.token", "steve setup", "token create", "~/.steve"]) assert.ok(!text.includes(gone), `${lang}: no "${gone}" on the sign-in page`)
    await page.close()
  }

  {
    const page = await open()
    await face(page)
    const left = await lookFrom(page, -1), right = await lookFrom(page, 1), back = await lookFrom(page, -1)
    assert.ok(right - left > 2, `the eyes follow the pointer to the right (left ${left.toFixed(2)}, right ${right.toFixed(2)})`)
    assert.ok(right - back > 2, `and back to the left (right ${right.toFixed(2)}, left ${back.toFixed(2)})`)
    if (process.env.STEVE_TEST_ARTIFACTS) await page.screenshot({ path: process.env.STEVE_TEST_ARTIFACTS + "/sign-in.png" })
    await page.close()
  }

  {
    const page = await open({ reducedMotion: "reduce" })
    await face(page)
    const still = await eyeX(page), left = await lookFrom(page, -1), right = await lookFrom(page, 1)
    assert.ok(Math.abs(right - left) < 0.5 && Math.abs(left - still) < 0.5, `reduced motion: the eyes stay put (${still.toFixed(2)}, ${left.toFixed(2)}, ${right.toFixed(2)})`)
    await page.close()
  }

  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["no token hint (zh-CN, en)", "eyes follow the pointer", "still under reduced motion"] }))
} finally { await browser.close() }
