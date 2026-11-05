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
    /* the snooze label the Hub stores stays zh-CN in any UI language */
    assert.equal(i18n.t("core:snooze.hour", { lng: "zh-CN" }), "1 小时后")
  })
  assert.equal(i18n.lang(), "zh-CN")
})

/* ───────── the source scan: UI text lives in locales/, not in components ─────────
 * Every token of code (strings, template parts, regexes, JSX text; comments are skipped) under web/src outside
 * i18n/locales is checked for CJK. What is left is not text the page shows: it is a value the Hub sends or reads,
 * so it has to stay byte-identical. Each file lists the exact CJK runs it may contain and why. An entry that no
 * longer occurs fails too, so the list only shrinks. */
const { readFileSync, readdirSync, statSync } = await import("node:fs")
const { join, relative, sep } = await import("node:path")
const ts = (await import("../node_modules/typescript/lib/typescript.js")).default
const SRC = new URL("../src/", import.meta.url).pathname
const CJK = /[　-〿㐀-鿿＀-￯]+/g

const HUB = "Hub value, sent or matched as is"
const ALLOW = {
  "accounts/index.tsx": [["，、"], "separators accepted when splitting typed aliases"],
  "auto/nodes.tsx": [["，、"], "separators accepted when splitting a typed list"],
  "people/contact.tsx": [["、", "，"], "separators accepted when splitting typed aliases"],
  "people/org.tsx": [["、", "，"], "separators accepted when splitting typed names"],
  "chat2.tsx": [["。！？"], "sentence ends for clipping user text"],
  "mail/thread.tsx": [["回复", "答复", "："], "reply prefixes in incoming subjects, so Re: is not doubled"],
  "api.ts": [["插话", "排队"], `message modes: ${HUB}`],
  "api/vault.ts": [["先在这台设备上登记", "或通行密钥"], "the Hub's NEED_PASSKEY error, matched to open the passkey dialog"],
  "auto/decision.tsx": [["不执行", "不"], `decision option values: ${HUB}`],
  "auto/schedules.tsx": [["每天", "每周五", "每月", "日", "每周一", "运维", "邮件"], "schedule phrases the Hub's rule parser reads; Bot names matched to pick a template's Bot"],
  "channels/persona-intro.ts": [["（不透露身份，只回答问题本身）", "我是", "，", "。", "助手", "由", "驱动", "归", "管理", "你"], "the Hub's intro_of, ported for the live preview: what the persona says in chats"],
  "chat-view.tsx": [["（附件）", "汇报"], `attachment-only placeholder and the /汇报 command: ${HUB}`],
  "queue.tsx": [["（附件）"], `attachment-only placeholder: ${HUB}`],
  "thread/topics.ts": [["（附件）"], `attachment-only placeholder: ${HUB}`],
  "ui.tsx": [["新事项", "新项目", "汇报", "新", "（附件）", "未确认", "允许执行", "："], `slash command names the Hub accepts, attachment placeholder, contact kind, permission question prefix: ${HUB}`],
  "decision-notes.ts": [["不发布", "：", "批准", "拒绝", "。补充：", "。原因：", "添加标签", "移除标签", "稍后提醒", "暂停", "放弃", "归档", "恢复", "撤销了决定", "同意开始", "不需要", "批准发布", "回答", "返工"], "parses and rebuilds the Hub's decision timeline text"],
  "devices.tsx": [["暂停接活", "正常"], `device status: ${HUB}`],
  "graph.tsx": [["派生", "交接", "依赖", "项目"], "edge kinds from store.ts, mapped to t() labels"],
  "store.ts": [["高", "中", "低", "你", "开始会话", "分流", "规划", "收尾", "交付与验收", "发布", "正在开始", "刚开始", "定义", "执行", "不", "排队", "插话", "删除", "导出", "项目", "类型", "领域", "客户", "主题", "交接", "依赖", "派生"], `priorities, tag categories, stage and edge ids, timeline text: ${HUB}`],
  "views.tsx": [["接收", "分流", "定义", "规划", "准备", "执行", "交付与验收", "收尾", "高", "中", "低", "无", "今天", "明天", "周五", "下周一", "全部", "等你", "进行中", "动态", "产出", "关系图", "派生", "交付", "目标更新", "前置", "开新会话", "交接", "自动开始", "暂停", "放弃", "继续", "关联", "发布", "冲突", "失败", "回到", "补充信息：", "（附件）", "：", "插话", "排队", "本机"], `stage, priority and due values (labels via t()), tab ids, timeline keywords, queued-comment prefix: ${HUB}`],
  "view-shared.tsx": [["高", "今天", "无", "明天", "周五", "下周一"], `priority and due values (labels via t()): ${HUB}`],
  "stats/overview.tsx": [["全部", "等你", "进行中"], "filter ids passed to views.showItems"],
  "knowledge/index.tsx": [["搜索", "长期记忆", "知识条目", "笔记与文档", "来源", "待确认"], "tab ids (labels via t()); 长期记忆/知识条目 are the Hub's record names"],
  "knowledge/parts.tsx": [["存为长期记忆吗？", "存为知识条目吗？", "记住这条吗？", "为什么值得保留："], "the Hub's question and reason prefixes, matched"],
  "mail/list.tsx": [["等你", "需要"], "matched in the Hub's outcome label"],
  "manage.tsx": [["全部", "启用", "停用", "项目", "类型", "领域", "客户", "主题", "设置", "删除", "导出", "概览", "对外身份", "能力", "系统提示词", "定时与流程", "记忆", "自己写", "自动生成", "对话", "待处理", "事项", "自动开始", "提醒我开始", "关闭", "不发", "标准", "大", "开", "关", "天", "年", "永久"], "stored setting values, tag categories and tab ids; every one is shown through t()"],
  "out-groups.ts": [["自查", "自测", "调试", "记录", "日志", "截图", "输出", "运行", "测试", "构建", "中间", "结果", "产物", "文件", "版本", "临时", "草稿", "过程记录", "执行记录"], "words that mark an output name as a work record (names come from Bots)"],
  "outputs.tsx": [["预览", "源码", "逐张", "连续"], "view mode ids (labels via t()); data-gallery in browser tests"],
  "stats/tables.tsx": [["　"], "an ideographic space that keeps an empty footer one line tall"],
  "vault/new-item.tsx": [["私钥", "恢复码", "内容"], "matched in the Hub's field labels"],
  "version.tsx": [["拒绝", "升级已提交"], "matched in the Hub's upgrade error"],
}

const sources = (d) => readdirSync(d).flatMap((n) => {
  const p = join(d, n)
  if (statSync(p).isDirectory()) return relative(SRC, p).split(sep).join("/") === "i18n/locales" ? [] : sources(p)
  return /\.tsx?$/.test(n) ? [p] : []
})
/** CJK runs per file, from code tokens only (comments and JSDoc are not tokens) */
function cjkRuns() {
  const out = {}
  for (const f of sources(SRC)) {
    const rel = relative(SRC, f).split(sep).join("/")
    const src = ts.createSourceFile(f, readFileSync(f, "utf8"), ts.ScriptTarget.Latest, true, f.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
    const visit = (n) => {
      if (n.kind >= ts.SyntaxKind.FirstJSDocNode && n.kind <= ts.SyntaxKind.LastJSDocNode) return
      const kids = n.getChildren(src)
      if (kids.length) return kids.forEach(visit)
      for (const r of n.getText(src).match(CJK) ?? []) (out[rel] ??= new Map()).set(r, src.getLineAndCharacterOfPosition(n.getStart(src)).line + 1)
    }
    visit(src)
  }
  return out
}

test("no CJK text in web/src outside i18n/locales except the listed Hub values", () => {
  const found = cjkRuns(), bad = [], stale = []
  for (const [f, runs] of Object.entries(found)) {
    const ok = new Set(ALLOW[f]?.[0] ?? [])
    for (const [r, line] of runs) if (!ok.has(r)) bad.push(`${f}:${line} ${JSON.stringify(r)}`)
  }
  for (const [f, [words]] of Object.entries(ALLOW)) for (const w of words) if (!found[f]?.has(w)) stale.push(`${f} ${JSON.stringify(w)}`)
  assert.deepEqual(bad, [], "move these into locales/ and use t()")
  assert.deepEqual(stale, [], "these allowlist entries no longer occur; remove them")
})

const leaves = (o, p = "") => Object.entries(o).flatMap(([k, v]) => (v && typeof v === "object" ? leaves(v, p + k + ".") : [[p + k, v]]))
const vars = (s) => new Set([...String(s).matchAll(/\{\{\s*([\w.]+)[^}]*\}\}/g)].map((m) => m[1]))

test("every value is a non-empty string in both languages", () => {
  for (const [name, res] of [["zh-CN", zh], ["en", en]]) for (const ns of i18n.NAMESPACES) for (const [k, v] of leaves(res[ns])) {
    assert.equal(typeof v, "string", `${name} ${ns}:${k} is not a string`)
    assert.ok(v.trim().length > 0, `${name} ${ns}:${k} is empty`)
  }
})

test("every {{var}} in zh-CN is used by en", () => {
  const bad = []
  for (const ns of i18n.NAMESPACES) {
    const e = new Map(leaves(en[ns]))
    /* en forms of one key: the key itself, or all its plural forms */
    const forms = (k) => { const base = k.replace(PLURAL, ""); return [...e].filter(([ek]) => ek === k || ek.replace(PLURAL, "") === base).map(([, v]) => v) }
    for (const [k, v] of leaves(zh[ns])) {
      const have = new Set(forms(k).flatMap((x) => [...vars(x)]))
      for (const x of vars(v)) if (!have.has(x)) bad.push(`${ns}:${k} {{${x}}}`)
    }
  }
  assert.deepEqual(bad, [])
})

test("a new persona's defaults introduce it coherently in the Hub's words, whatever the UI language", async () => {
  const { introOf, newPersonaDefaults } = await import("../src/channels/persona-intro.ts")
  for (const l of ["zh-CN", "en"]) await inLang(l, () => {
    const p = newPersonaDefaults()
    assert.equal(introOf(p, "你"), "我是新身份，我是 AI 助手。", l)
  })
})
