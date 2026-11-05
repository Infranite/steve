// The item page's outputs (ST-38) against the fixture's fake Hub: only the latest delivery's results are the outputs,
// the rest is in the work record; pictures move by keys, buttons, swipes and the strip, or scroll one under another;
// Word, Excel and PowerPoint files render; notes on a picture region and on selected text are drafts that survive a
// reload, are sent once however often the button is pressed, and come back as one reply on the item that opens the
// output where the note is. 1440 and 390 px, light and dark.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { crc32, deflateRawSync } from "node:zlib"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

/* ── the files the outputs hold ── */
function zip(files) {
  const parts = [], central = []; let off = 0
  for (const [name, content] of Object.entries(files)) {
    const data = Buffer.isBuffer(content) ? content : Buffer.from(content), comp = deflateRawSync(data), nb = Buffer.from(name), crc = crc32(data)
    const lh = Buffer.alloc(30); lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4); lh.writeUInt16LE(0x0800, 6); lh.writeUInt16LE(8, 8)
    lh.writeUInt32LE(crc, 14); lh.writeUInt32LE(comp.length, 18); lh.writeUInt32LE(data.length, 22); lh.writeUInt16LE(nb.length, 26)
    const ch = Buffer.alloc(46); ch.writeUInt32LE(0x02014b50, 0); ch.writeUInt16LE(20, 4); ch.writeUInt16LE(20, 6); ch.writeUInt16LE(0x0800, 8); ch.writeUInt16LE(8, 10)
    ch.writeUInt32LE(crc, 16); ch.writeUInt32LE(comp.length, 20); ch.writeUInt32LE(data.length, 24); ch.writeUInt16LE(nb.length, 28); ch.writeUInt32LE(off, 42)
    parts.push(lh, nb, comp); central.push(ch, nb); off += 30 + nb.length + comp.length
  }
  const cd = Buffer.concat(central), end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(central.length / 2, 8); end.writeUInt16LE(central.length / 2, 10); end.writeUInt32LE(cd.length, 12); end.writeUInt32LE(off, 16)
  return Buffer.concat([...parts, cd, end])
}
const W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'
const A = 'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"'
const S = 'xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'
const rels = (xs) => `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">${xs.map(([id, t, ext]) => `<Relationship Id="${id}" Type="x" Target="${t}"${ext ? ' TargetMode="External"' : ""}/>`).join("")}</Relationships>`
const tc = (t) => `<w:tc><w:p><w:r><w:t>${t}</w:t></w:r></w:p></w:tc>`

const png = async (label, w, h, color) => {
  const p = await browser.newPage({ locale: "zh-CN", viewport: { width: w, height: h } })
  await p.setContent(`<body style="margin:0;background:${color};font:bold 64px sans-serif;color:#fff;display:flex;align-items:center;justify-content:center;height:${h}px">${label}</body>`)
  const b = await p.screenshot(); await p.close(); return b
}
const pics = { a_img1: await png("首页", 900, 600, "#2b6cb0"), a_img2: await png("详情", 600, 1100, "#2f855a"), a_img3: await png("设置", 900, 600, "#9c4221"), "a_rec/d1.png": await png("d1", 400, 300, "#555"), "a_rec/d2.png": await png("d2", 400, 300, "#777") }
const docx = zip({
  "word/document.xml": `<w:document ${W}><w:body><w:p><w:pPr><w:pStyle w:val="1"/></w:pPr><w:r><w:t>产出说明</w:t></w:r></w:p>
    <w:p><w:r><w:t xml:space="preserve">这一段是</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>加粗的重点</w:t></w:r><w:r><w:t>，后面还有文字。</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>列表第一项</w:t></w:r></w:p>
    <w:tbl><w:tr>${tc("指标")}${tc("数值")}</w:tr><w:tr>${tc("转化率")}${tc("12%")}</w:tr></w:tbl>
    <w:p><w:hyperlink r:id="rId9"><w:r><w:t>参考链接</w:t></w:r></w:hyperlink></w:p>
    <w:p><w:r><w:drawing><a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" r:embed="rId5"/></w:drawing></w:r></w:p></w:body></w:document>`,
  "word/styles.xml": `<w:styles ${W}><w:style w:styleId="1"><w:name w:val="heading 1"/></w:style></w:styles>`,
  "word/_rels/document.xml.rels": rels([["rId5", "media/image1.png"], ["rId9", "https://example.com/ref", true]]),
  "word/media/image1.png": pics["a_rec/d1.png"],
})
const xlsx = zip({
  "xl/workbook.xml": `<workbook ${S}><sheets><sheet name="销量" sheetId="1" r:id="rId1"/><sheet name="备注" sheetId="2" r:id="rId2"/></sheets></workbook>`,
  "xl/_rels/workbook.xml.rels": rels([["rId1", "worksheets/sheet1.xml"], ["rId2", "/xl/worksheets/sheet2.xml"]]),
  "xl/sharedStrings.xml": `<sst ${S}><si><t>名称</t></si><si><t>数量</t></si><si><t>日期</t></si><si><r><t>苹</t></r><r><t>果</t></r></si></sst>`,
  "xl/styles.xml": `<styleSheet ${S}><cellXfs><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>`,
  "xl/worksheets/sheet1.xml": `<worksheet ${S}><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>2</v></c></row><row r="2"><c r="A2" t="s"><v>3</v></c><c r="B2"><v>12</v></c><c r="C2" s="1"><v>45000</v></c></row><row r="4"><c r="D4" t="b"><v>1</v></c></row></sheetData></worksheet>`,
  "xl/worksheets/sheet2.xml": `<worksheet ${S}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>第二张表</t></is></c></row></sheetData></worksheet>`,
})
const sp = (ph, texts, y) => `<p:sp><p:nvSpPr><p:nvPr>${ph ? `<p:ph type="${ph}"/>` : ""}</p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="${y}"/></a:xfrm></p:spPr><p:txBody>${texts.map((t) => `<a:p><a:r><a:t>${t}</a:t></a:r></a:p>`).join("")}</p:txBody></p:sp>`
const pptx = zip({
  "ppt/presentation.xml": `<p:presentation ${A}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst></p:presentation>`,
  "ppt/_rels/presentation.xml.rels": rels([["rId2", "slides/slide1.xml"], ["rId3", "slides/slide2.xml"]]),
  "ppt/slides/slide1.xml": `<p:sld ${A}><p:cSld><p:spTree>${sp("", ["收入增长 20%", "用户 3 万"], 2000)}${sp("title", ["季度汇报"], 100)}</p:spTree></p:cSld></p:sld>`,
  "ppt/slides/slide2.xml": `<p:sld ${A}><p:cSld><p:spTree>${sp("title", ["下一步"], 100)}<p:pic><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="0" y="3000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>`,
  "ppt/slides/_rels/slide2.xml.rels": rels([["rId2", "../media/image1.png"]]),
  "ppt/media/image1.png": pics["a_rec/d2.png"],
})
const files = { ...pics, a_docx: docx, a_xlsx: xlsx, a_pptx: pptx }

const errors = []
/** the fake Hub's data, carried from the first page to the later ones (each page has its own storage) */
let hub = null
async function open(width, theme, fresh) {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme, hasTouch: width === 390 })
  if (!fresh && hub) await page.addInitScript((h) => { if (!localStorage.getItem("fx-hub")) localStorage.setItem("fx-hub", h) }, hub)
  page.on("pageerror", (e) => errors.push(e.message))
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => {
    const u = new URL(route.request().url()), m = /^\/api\/items\/ST-9\/artifacts\/([^/]+)\/raw$/.exec(u.pathname)
    const body = m && files[u.searchParams.get("path") ? `${m[1]}/${u.searchParams.get("path")}` : m[1]]
    return body ? route.fulfill({ status: 200, body, contentType: m[1].startsWith("a_img") || u.searchParams.get("path") ? "image/png" : "application/octet-stream" }) : route.fulfill({ status: 503, json: { error: "fixture" } })
  })
  await page.goto(base + "/test/fixtures/outputs.html")
  if (fresh) { await page.evaluate(() => localStorage.removeItem("fx-hub")); await page.reload() }
  await page.locator("[data-title]").first().waitFor()
  return page
}
const tab = async (page, name) => { await page.locator(`[data-seg="${name}"]`).first().click() }
const dialog = (page) => page.locator("[data-preview]")
const noOverflow = async (page, what) => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${what}: nothing widens the page`)

try {
  /* ── 1440 light: the whole flow ── */
  let page = await open(1440, "light", true)
  await tab(page, "产出")
  const finals = await page.locator("[data-outs-final] [data-out-id]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-out-id")))
  assert.deepEqual(finals, ["a_branch", "a_plan", "a_img1", "a_img2", "a_img3", "a_docx", "a_xlsx", "a_pptx"], "only the latest delivery's results")
  assert.match(await page.locator("[data-out-id=a_plan]").innerText(), /v2/)
  assert.match(await page.locator("[data-out-records-toggle]").innerText(), /执行记录 · 4 项/)
  await page.locator("[data-out-records-toggle]").click()
  assert.match(await page.locator("[data-out-delivery='2']").innerText(), /截图[\s\S]*过程记录/)
  const first = await page.locator("[data-out-delivery='1']").innerText()
  assert.match(first, /自查截图[\s\S]*过程记录/); assert.match(first, /方案[\s\S]*旧版本[\s\S]*v1/); assert.match(first, /steve\/st-9[\s\S]*旧版本/)
  if (shots) await page.screenshot({ path: `${shots}/outputs-list-1440-light.png`, fullPage: true })

  // pictures: the arrow keys, the buttons, a swipe and the strip
  await page.locator("[data-out-id=a_img1]").click()
  await dialog(page).locator("[data-gallery-count]").waitFor()
  assert.equal(await dialog(page).locator("[data-gallery-count]").innerText(), "1 / 3")
  await page.keyboard.press("ArrowRight")
  await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "2 / 3")
  assert.match(await dialog(page).locator("h2").innerText(), /详情/)
  await dialog(page).locator("[data-gallery-next]").click()
  await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "3 / 3")
  const stage = await dialog(page).locator("[data-gallery-stage]").boundingBox()
  await page.mouse.move(stage.x + stage.width * 0.3, stage.y + stage.height / 2); await page.mouse.down(); await page.mouse.move(stage.x + stage.width * 0.8, stage.y + stage.height / 2, { steps: 5 }); await page.mouse.up()
  await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "2 / 3")
  await dialog(page).locator("[data-pic-thumb='0']").click()
  await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "1 / 3")
  // one under another
  await dialog(page).locator("[data-seg='连续']").click()
  await page.waitForFunction(() => document.querySelectorAll("[data-gallery='连续'] [data-pic]").length === 3)
  if (shots) await page.screenshot({ path: `${shots}/outputs-gallery-scroll-1440-light.png` })
  await dialog(page).locator("[data-seg='逐张']").click()

  // a note on a region of the second picture; saving it reloads the item, the picture stays where it was
  await dialog(page).locator("[data-gallery-next]").click()
  await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "2 / 3")
  await dialog(page).locator("[data-ann-draw]").click()
  const img = await dialog(page).locator("[data-gallery-stage] [data-out-image]").boundingBox()
  await page.mouse.move(img.x + img.width * 0.2, img.y + img.height * 0.2); await page.mouse.down(); await page.mouse.move(img.x + img.width * 0.6, img.y + img.height * 0.5, { steps: 6 }); await page.mouse.up()
  await dialog(page).locator("[data-ann-new=region]").waitFor()
  await dialog(page).locator("[data-ann-input]").fill("详情按钮被裁掉了")
  await dialog(page).locator("[data-ann-save]").click()
  await dialog(page).locator("[data-ann-box=draft]").waitFor()
  assert.match(await dialog(page).locator("[data-ann-list]").innerText(), /区域（左 20%，上 20%，宽 40%，高 30%）[\s\S]*详情按钮被裁掉了/)
  assert.equal(await dialog(page).locator("[data-gallery-count]").innerText(), "2 / 3", "drawing does not swipe, a reload does not reorder")
  if (shots) await page.screenshot({ path: `${shots}/outputs-annotate-1440-light.png` })
  await page.keyboard.press("Escape")

  // Word: rendered; a note on selected text
  await page.locator("[data-out-id=a_docx]").click()
  await dialog(page).locator("[data-office=docx]").waitFor()
  const doc = await dialog(page).locator("[data-office=docx]").innerText()
  for (const t of ["产出说明", "加粗的重点", "列表第一项", "转化率", "12%", "参考链接"]) assert.ok(doc.includes(t), `docx shows ${t}`)
  assert.equal(await dialog(page).locator("[data-office=docx] img").count(), 1)
  assert.equal(await dialog(page).locator("[data-office=docx] h1, [data-office=docx] p.text-\\[22px\\]").count(), 1, "heading 1 by its style name")
  await page.evaluate(() => {
    const t = [...document.querySelectorAll("[data-office=docx] span")].find((s) => s.textContent === "加粗的重点")
    const r = document.createRange(); r.selectNodeContents(t); const s = getSelection(); s.removeAllRanges(); s.addRange(r)
  })
  await dialog(page).locator("[data-ann-sel-add]").click()
  await dialog(page).locator("[data-ann-input]").fill("这句改成结论先行")
  await dialog(page).locator("[data-ann-save]").click()
  await dialog(page).locator("[data-ann=draft]").waitFor()
  assert.match(await dialog(page).locator("[data-ann-list]").innerText(), /「加粗的重点」/)
  if (shots) await page.screenshot({ path: `${shots}/outputs-docx-1440-light.png` })
  await page.keyboard.press("Escape")

  // Excel and PowerPoint
  await page.locator("[data-out-id=a_xlsx]").click()
  await dialog(page).locator("[data-office-sheet='销量']").waitFor()
  const sheet = await dialog(page).locator("[data-office-sheet='销量']").innerText()
  for (const t of ["名称", "苹果", "12", "2023-03-15", "TRUE"]) assert.ok(sheet.includes(t), `xlsx shows ${t}: ${sheet}`)
  await dialog(page).locator("[data-seg='备注']").click()
  await dialog(page).locator("[data-office-sheet='备注']").waitFor()
  assert.match(await dialog(page).locator("[data-office-sheet='备注']").innerText(), /第二张表/)
  if (shots) await page.screenshot({ path: `${shots}/outputs-xlsx-1440-light.png` })
  await page.keyboard.press("Escape")
  await page.locator("[data-out-id=a_pptx]").click()
  await dialog(page).locator("[data-office-slide='2']").waitFor()
  const s1 = await dialog(page).locator("[data-office-slide='1']").innerText()
  assert.ok(s1.indexOf("季度汇报") < s1.indexOf("收入增长"), "the title first, then the body")
  assert.equal(await dialog(page).locator("[data-office-slide='2'] img").count(), 1)
  if (shots) await page.screenshot({ path: `${shots}/outputs-pptx-1440-light.png` })
  await page.keyboard.press("Escape")

  // a folder of records: its pictures move the same way (the work record may still be open from above)
  await dialog(page).waitFor({ state: "detached" })
  if (!await page.locator("[data-out-id=a_rec]").isVisible()) await page.locator("[data-out-records-toggle]").click()
  await page.locator("[data-out-id=a_rec]").click()
  await dialog(page).locator("[data-out-browse]").click()
  await page.waitForFunction(() => document.querySelectorAll("[data-gallery='连续'] [data-pic]").length === 2)
  await dialog(page).locator("[data-seg='逐张']").click()
  await page.keyboard.press("Escape")

  // the drafts are on the Hub: a reload keeps them
  await page.reload(); await page.locator("[data-title]").first().waitFor(); await tab(page, "产出")
  await page.locator("[data-out-id=a_img2] [data-out-drafts]").waitFor()
  assert.match(await page.locator("[data-out-id=a_docx]").innerText(), /1 条草稿/)

  // send: three quick clicks are one send
  await page.locator("[data-out-id=a_img2]").click()
  const send = dialog(page).locator("[data-ann-send]")
  assert.match(await send.innerText(), /确认发送 2 条/)
  assert.match(await dialog(page).locator("[data-ann-send-row]").innerText(), /含其他产出上的 1 条/)
  await send.click(); await send.click({ force: true }).catch(() => {}); await send.click({ force: true }).catch(() => {})
  await page.waitForFunction(() => !document.querySelector("[data-ann-send]"), null, { timeout: 5000 })
  const sends = (await page.evaluate(() => window.__calls)).filter((c) => c.send)
  assert.equal(sends.length, 1, `one request: ${JSON.stringify(sends)}`)
  await dialog(page).locator("[data-ann-box=sent]").waitFor()
  await page.keyboard.press("Escape")

  // the reply on the item: refreshed, it still shows the notes and opens the output where each one is
  await page.reload(); await page.locator("[data-title]").first().waitFor()
  const reply = page.locator("[data-comment-notes]")
  await reply.waitFor()
  assert.match(await reply.innerText(), /对产出的批注（2 条）/)
  assert.match(await reply.innerText(), /详情\.png · 第 1 版 · 区域（左 20%/)
  assert.match(await reply.innerText(), /说明\.docx · 第 1 版 · 「加粗的重点」/)
  assert.equal(await reply.locator("[data-ann-thumb]").count(), 1)
  if (shots) await page.screenshot({ path: `${shots}/outputs-reply-1440-light.png` })
  await reply.locator("[data-ann-ref]").nth(1).click()
  await dialog(page).locator("[data-office=docx]").waitFor()
  await page.waitForFunction(() => CSS.highlights?.get("steve-ann")?.size === 1, null, { timeout: 5000 })
  if (shots) await page.screenshot({ path: `${shots}/outputs-locate-text-1440-light.png` })
  await page.keyboard.press("Escape")
  await reply.locator("[data-ann-ref]").first().click()
  await dialog(page).locator("[data-ann-box=sent].ring-4").waitFor()
  assert.equal(await dialog(page).locator("[data-ann-send]").count(), 0, "nothing left to send")
  hub = await page.evaluate(() => localStorage.getItem("fx-hub"))
  await page.close()

  /* ── every width and theme: the list, a picture with its note, no overflow ── */
  for (const [width, theme] of [[1440, "dark"], [390, "light"], [390, "dark"]]) {
    page = await open(width, theme, false)
    await tab(page, "产出")
    await page.locator("[data-out-id=a_img2]").waitFor()
    await noOverflow(page, `${width} ${theme} list`)
    if (shots) await page.screenshot({ path: `${shots}/outputs-list-${width}-${theme}.png`, fullPage: true })
    await page.locator("[data-out-id=a_img2]").click()
    await dialog(page).locator("[data-ann-box=sent]").waitFor()
    if (width === 390) {
      // a swipe to the left is the next picture; the dialog fits the phone
      const box = await dialog(page).locator("[data-gallery-stage]").boundingBox()
      assert.ok(box.x >= 0 && box.x + box.width <= 390, "the picture fits the screen")
      await page.evaluate(() => {
        const el = document.querySelector("[data-gallery-stage]"), r = el.getBoundingClientRect(), y = r.top + r.height / 2
        const ev = (type, x) => el.dispatchEvent(new PointerEvent(type, { bubbles: true, clientX: x, clientY: y, pointerType: "touch", isPrimary: true }))
        ev("pointerdown", r.right - 30); ev("pointerup", r.left + 30)
      })
      await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "3 / 3")
      await dialog(page).locator("[data-gallery-prev]").click()
      await page.waitForFunction(() => document.querySelector("[data-gallery-count]")?.textContent === "2 / 3")
    }
    await noOverflow(page, `${width} ${theme} dialog`)
    if (shots) await page.screenshot({ path: `${shots}/outputs-picture-${width}-${theme}.png` })
    await page.keyboard.press("Escape")
    if (width === 390 && theme === "light") {
      await page.locator("[data-out-id=a_docx]").click()
      await dialog(page).locator("[data-office=docx]").waitFor()
      await noOverflow(page, "390 docx")
      if (shots) await page.screenshot({ path: `${shots}/outputs-docx-390-light.png` })
      await page.keyboard.press("Escape")
      await tab(page, "动态")
      await page.locator("[data-comment-notes]").waitFor()
      if (shots) await page.screenshot({ path: `${shots}/outputs-reply-390-light.png`, fullPage: true })
    }
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["latest results vs work record with versions", "keys, buttons, mouse and touch swipes, strip, 连续", "folder of pictures", "docx/xlsx/pptx render", "region and text notes as drafts", "drafts after reload", "three clicks → one send", "reply on the item after reload, locates region and text", "1440/390 × light/dark, no overflow"] }))
} finally { await browser.close() }
