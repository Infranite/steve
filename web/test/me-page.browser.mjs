/* the 我 page and a contact's page: the picture is uploaded (PUT /contacts/:id/avatar with the raw bytes), shown and
 * removed; only PNG / JPEG / WebP are sent; the details are one list changed in place; the description folds; sections
 * with nothing in them are not there; it fits a phone */
import assert from 'node:assert/strict'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 } })
page.setDefaultTimeout(10000)
await page.addInitScript(() => localStorage.setItem('steve-lang', 'zh-CN'))
const errors = []; page.on('pageerror', e => errors.push(e.message))
const long = '李总是独立开发者，也是 Steve 的作者。'.repeat(14)
let me = { id: 'ct_me', name: '李总', aliases: [], kind: 'real', rels: ['me'], important: false, org: null, tags: [], visibility: 'workspace', is_me: true, avatar: {}, updated: 1, note: long, status: 'active', merged_into: null, source: 'manual', revision: 1, created: 1, jobs: [], attrs: [], teams: [], reports_to: [], merges: [],
  profile: { checked: null, next_check: null, source: null, fields: { name: { state: 'known' }, org: { state: 'missing', reason: 'no_source' }, title: { state: 'missing', reason: 'no_source' }, email: { state: 'missing', reason: 'no_source' } } } }
const puts = [], attrs = []
let png
await page.route('**/*', async route => {
  const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
  const m = route.request().method(); let data = []
  if (u.pathname === '/api/me/contact' || u.pathname === '/api/contacts/me' || u.pathname === '/api/contacts/ct_me') data = me
  else if (u.pathname === '/api/contacts/ct_me/avatar' && m === 'PUT') { puts.push(route.request().postDataBuffer()); me = { ...me, avatar: { file: 'fil_pic' } }; data = me }
  else if (u.pathname === '/api/contacts/ct_me/avatar' && m === 'DELETE') { me = { ...me, avatar: {} }; data = me }
  else if (u.pathname === '/api/files/fil_pic') return route.fulfill({ status: 200, contentType: 'image/png', body: png })
  else if (u.pathname === '/api/parties/ct_me/attrs' && m === 'POST') { const b = route.request().postDataJSON(); attrs.push(b); me = { ...me, attrs: [...me.attrs, { id: 'a' + attrs.length, source_label: '你写的', level: 'public', status: 'confirmed', ...b }] }; data = me }
  else if (u.pathname.endsWith('/holdings')) data = { accounts: [], responsible: [] }
  return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
})
const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
try {
  await page.goto(base + '/test/fixtures/me-page.html')
  await page.getByRole('heading', { name: '李总', exact: true }).waitFor()
  png = Buffer.from((await page.evaluate(() => { const c = document.createElement('canvas'); c.width = c.height = 64; const g = c.getContext('2d'); g.fillStyle = '#3e63dd'; g.fillRect(0, 0, 64, 64); return c.toDataURL('image/png') })).split(',')[1], 'base64')

  /* one list of what Steve knows, each empty one offering 补充; no 资料完整性 and no repeated reason */
  await page.getByText('Steve 记得的你', { exact: true }).waitFor()
  for (const k of ['call', 'tz', 'lang', 'org', 'title', 'email']) assert.equal(await page.locator(`[data-fact="${k}"]`).count(), 1, k)
  assert.equal(await page.getByText('资料完整性').count(), 0)
  assert.equal(await page.getByText('没有可用于自动查询的账号').count(), 0)
  /* nothing linked, nothing held, nothing noted: those sections are not there at all */
  for (const s of ['关联账号', '负责的', '所在团队', '往来', '记下的偏好与事实']) assert.equal(await page.getByText(s, { exact: false }).count(), 0, s)

  /* the description folds to three lines */
  const bio = page.locator('[data-bio]')
  const folded = await bio.evaluate((el) => el.getBoundingClientRect().height)
  await page.getByRole('button', { name: '展开', exact: true }).click()
  assert.ok(await bio.evaluate((el) => el.getBoundingClientRect().height) > folded * 1.5, 'unfolds')
  await page.getByRole('button', { name: '收起', exact: true }).click()

  /* a value is filled in place: 补充 → type → Enter */
  await page.getByRole('button', { name: '补充时区', exact: true }).click()
  await page.getByLabel('修改 时区').fill('Asia/Shanghai'); await page.keyboard.press('Enter')
  await page.locator('[data-attr="时区"]').getByText('Asia/Shanghai').waitFor()
  assert.deepEqual(attrs, [{ key: 'tz', label: '时区', value: 'Asia/Shanghai' }])
  await page.getByText('每项右侧是披露级别', { exact: false }).waitFor()

  /* a GIF is refused before anything is sent */
  const avatar = page.locator('[data-avatar-edit]')
  let chooser = page.waitForEvent('filechooser'); await avatar.click()
  await (await chooser).setFiles({ name: 'a.gif', mimeType: 'image/gif', buffer: Buffer.from('GIF89a') })
  await page.waitForTimeout(200); assert.equal(puts.length, 0)

  /* a PNG goes up as the raw body; the page shows it */
  chooser = page.waitForEvent('filechooser'); await avatar.click()
  await (await chooser).setFiles({ name: 'me.png', mimeType: 'image/png', buffer: png })
  const img = page.locator('[data-avatar-edit] img'); await img.waitFor()
  assert.equal(puts.length, 1); assert.ok(puts[0].equals(png), 'the bytes as they are')
  assert.match(await img.getAttribute('src'), /\/api\/files\/fil_pic/)
  await page.waitForFunction(() => document.querySelector('[data-avatar-edit] img')?.naturalWidth > 0)

  /* with a picture the avatar is a menu: remove goes back to initials */
  await avatar.click(); await page.getByRole('menuitem', { name: '移除头像' }).click()
  await page.locator('[data-avatar-edit="pick"]').waitFor(); assert.equal(await page.locator('[data-avatar-edit] img').count(), 0)

  await page.setViewportSize({ width: 390, height: 844 }); await page.waitForTimeout(200); await fits()
  assert.deepEqual(errors, []); console.log('PASS: avatar upload/type check/remove, one in-place list, folded description, empty sections hidden, 390px')
} catch (e) { console.error(await page.locator('body').innerText(), errors); throw e } finally { await browser.close() }
