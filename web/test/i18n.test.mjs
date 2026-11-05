import assert from "node:assert/strict"
import { test } from "node:test"

const i18n = await import("../src/i18n/index.ts")
const f = await import("../src/i18n/format.ts")
const zh = (await import("../src/i18n/locales/zh-CN/index.ts")).default
const en = (await import("../src/i18n/locales/en/index.ts")).default

const PLURAL = /_(zero|one|two|few|many|other)$/
const keys = (o, p = "") => Object.entries(o).flatMap(([k, v]) => (v && typeof v === "object" ? keys(v, p + k + ".") : [p + k.replace(PLURAL, "")]))
const inLang = async (l, fn) => { await i18n.default.changeLanguage(l); try { return fn() } finally { await i18n.default.changeLanguage("zh-CN") } }

test("outside a browser the UI language is zh-CN, initialized synchronously", () => {
  assert.equal(i18n.default.isInitialized, true)
  assert.equal(i18n.lang(), "zh-CN")
  assert.equal(i18n.t("common:action.save"), "保存")
})

test("every namespace is registered in both languages with the same keys", () => {
  assert.deepEqual(Object.keys(zh).sort(), [...i18n.NAMESPACES].sort())
  assert.deepEqual(Object.keys(en).sort(), [...i18n.NAMESPACES].sort())
  for (const ns of i18n.NAMESPACES) assert.deepEqual([...new Set(keys(en[ns]))].sort(), [...new Set(keys(zh[ns]))].sort(), `namespace ${ns}`)
})

test("browser languages map to a supported one", () => {
  assert.equal(i18n.matchLang("zh-TW"), "zh-CN")
  assert.equal(i18n.matchLang("zh"), "zh-CN")
  assert.equal(i18n.matchLang("en-GB"), "en")
  assert.equal(i18n.matchLang("fr-FR"), undefined)
  assert.equal(i18n.matchLang(undefined), undefined)
})

/* the hand-written zh-CN helpers these replaced, kept verbatim as the reference */
const pad = (n) => String(n).padStart(2, "0")
const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime()
const old = {
  agoNoJust: (t, now) => { const m = Math.round((now - t) / 60000); return m < 60 ? `${Math.max(m, 1)} 分钟前` : m < 1440 ? `${Math.round(m / 60)} 小时前` : `${Math.round(m / 1440)} 天前` },
  ago: (ms, now) => { const m = Math.round((now - ms) / 60000); return m < 1 ? "刚刚" : m < 60 ? `${m} 分钟前` : m < 1440 ? `${Math.round(m / 60)} 小时前` : `${Math.round(m / 1440)} 天前` },
  tm: (ts, nowMs) => {
    if (!ts) return ""
    const d = new Date(ts), now = new Date(nowMs), diff = now.getTime() - ts
    if (diff < 60_000) return "刚刚"
    const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`
    const days = Math.round((day(now) - day(d)) / 86400_000)
    if (days === 0) return hm
    if (days === 1) return "昨天 " + hm
    if (days < 7) return "周" + "日一二三四五六"[d.getDay()]
    return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
  },
  when: (ts, nowMs) => {
    if (!ts) return ""
    const d = new Date(ts), now = new Date(nowMs)
    const days = Math.round((day(d) - day(now)) / 86400_000), hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`
    if (days === 0) return "今天 " + hm
    if (days === 1) return "明天 " + hm
    if (days === -1) return "昨天 " + hm
    if (days > 1 && days < 7) return "周" + "日一二三四五六"[d.getDay()] + " " + hm
    return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`
  },
  since: (ms) => { const m = Math.max(1, Math.round(ms / 60_000)); return m < 60 ? `${m} 分钟` : m < 1440 ? `${Math.round(m / 60)} 小时` : `${Math.round(m / 1440)} 天` },
  dur: (ms) => { const m = Math.max(0, Math.round(ms / 60_000)); return m < 1 ? "刚开始" : m < 60 ? `${m} 分钟` : `${Math.floor(m / 60)} 小时 ${m % 60} 分` },
  fmtDur: (ms) => { const s = Math.max(0, Math.round(ms / 1000)); return s < 60 ? `${s} 秒` : s < 3600 ? `${Math.floor(s / 60)} 分 ${s % 60} 秒` : `${Math.floor(s / 3600)} 小时 ${Math.floor((s % 3600) / 60)} 分` },
  fmtMs: (ms) => {
    const m = Math.round(ms / 60000)
    if (ms < 60000) return Math.max(0, Math.round(ms / 1000)) + " 秒"
    if (m < 60) return m + " 分钟"
    const h = Math.floor(m / 60)
    if (h < 48) return h + " 小时" + (m % 60 ? ` ${m % 60} 分` : "")
    const d = Math.floor(h / 24)
    return d + " 天" + (h % 24 ? ` ${h % 24} 小时` : "")
  },
  ms: (n) => (n < 1 ? "<1 ms" : n < 1000 ? `${Math.round(n)} ms` : `${(n / 1000).toFixed(1)} 秒`),
  time: (ts) => { const d = new Date(ts); return `${d.getMonth() + 1}月${d.getDate()}日 ${pad(d.getHours())}:${pad(d.getMinutes())}` },
  fmtRange: (from, to) => { const d = (x) => { const [, m, dd] = x.split("-"); return `${Number(m)}月${Number(dd)}日` }; return from === to ? d(from) : `${d(from)} – ${d(to)}` },
  durUnit: (maxMs) => (maxMs >= 2 * 86400000 ? ["天", 86400000] : maxMs >= 2 * 3600000 ? ["小时", 3600000] : ["分钟", 60000]),
}

const NOW = new Date(2026, 10, 5, 14, 30).getTime()
const offsets = [0, 1, 20_000, 59_999, 60_000, 89_999, 90_000, 30 * 60_000, 59 * 60_000 + 40_000, 3_600_000, 5_400_000, 23 * 3_600_000, 86_400_000 - 1, 86_400_000, 1.5 * 86_400_000, 2 * 86_400_000 + 3_600_000, 3 * 86_400_000, 6 * 86_400_000, 8 * 86_400_000, 40 * 86_400_000, 400 * 86_400_000]
  .flatMap((o) => [o, o + 7 * 60_000 + 13_000])

test("zh-CN output is byte-identical to the helpers it replaced", () => {
  for (const o of offsets) {
    assert.equal(f.ago(NOW - o, { now: NOW, justNow: false }), old.agoNoJust(NOW - o, NOW), `ago(no just) ${o}`)
    assert.equal(f.ago(NOW - o, { now: NOW }), old.ago(NOW - o, NOW), `ago ${o}`)
    assert.equal(f.pastTime(NOW - o, NOW), old.tm(NOW - o, NOW), `pastTime ${o}`)
    assert.equal(f.nearTime(NOW + o, NOW), old.when(NOW + o, NOW), `nearTime +${o}`)
    assert.equal(f.nearTime(NOW - o, NOW), old.when(NOW - o, NOW), `nearTime -${o}`)
    assert.equal(f.span(o), old.since(o), `span ${o}`)
    assert.equal(f.elapsed(o), old.dur(o), `elapsed ${o}`)
    assert.equal(f.clockDur(o), old.fmtDur(o), `clockDur ${o}`)
    assert.equal(f.longDur(o), old.fmtMs(o), `longDur ${o}`)
    assert.equal(f.monthDayTime(NOW - o), old.time(NOW - o), `monthDayTime ${o}`)
    assert.deepEqual(f.durUnit(o), old.durUnit(o), `durUnit ${o}`)
  }
  for (const n of [0, 0.4, 1, 240.6, 999, 1000, 1499, 2000, 65_432]) assert.equal(f.msDur(n), old.ms(n), `msDur ${n}`)
  assert.equal(f.pastTime(null), "")
  assert.equal(f.nearTime(0), "")
  for (const [a, b] of [["2026-11-01", "2026-11-07"], ["2026-11-05", "2026-11-05"], ["2026-01-31", "2026-12-01"]]) assert.equal(f.dayRange(a, b), old.fmtRange(a, b))
})

test("en uses its own words and plurals", async () => {
  await inLang("en", () => {
    assert.equal(i18n.lang(), "en")
    assert.equal(f.ago(NOW - 60_000, { now: NOW }), "1 minute ago")
    assert.equal(f.ago(NOW - 3 * 60_000, { now: NOW }), "3 minutes ago")
    assert.equal(f.ago(NOW, { now: NOW }), "just now")
    assert.equal(f.ago(NOW - 2 * 86_400_000, { now: NOW }), "2 days ago")
    assert.equal(f.pastTime(NOW - 86_400_000, NOW), "Yesterday 14:30")
    assert.equal(f.elapsed(65 * 60_000), "1 hr 5 min")
    assert.equal(f.dayRange("2026-11-01", "2026-11-07"), "Nov 1 – Nov 7")
    assert.equal(f.num(12345.5), "12,345.5")
    assert.equal(i18n.t("common:action.save"), "Save")
  })
  assert.equal(i18n.lang(), "zh-CN")
})
