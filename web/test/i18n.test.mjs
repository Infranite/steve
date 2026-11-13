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

const NOW = new Date(2026, 10, 5, 14, 30).getTime()

test("zh-CN relative times preserve rounding and minute/hour/day boundaries", () => {
  for (const [age, expected] of [
    [0, "刚刚"], [29_999, "刚刚"], [30_000, "1 分钟前"],
    [3_569_999, "59 分钟前"], [3_570_000, "1 小时前"],
    [86_369_999, "24 小时前"], [86_370_000, "1 天前"], [172_800_000, "2 天前"],
  ]) assert.equal(f.ago(NOW - age, { now: NOW }), expected, `ago ${age}`)
  assert.equal(f.ago(NOW, { now: NOW, justNow: false }), "1 分钟前")
  assert.equal(f.ago(NOW - 120_000, { now: NOW, justNow: false }), "2 分钟前")
})

test("zh-CN calendar labels use local days, including the week boundary and empty dates", () => {
  const local = (month, day, hour = 14, minute = 30) => new Date(2026, month - 1, day, hour, minute).getTime()
  for (const [value, expected] of [
    [null, ""], [NOW - 59_999, "刚刚"], [NOW - 60_000, "14:29"],
    [local(11, 4), "昨天 14:30"], [local(10, 30), "周五"], [local(10, 29), "10-29"],
  ]) assert.equal(f.pastTime(value, NOW), expected, `pastTime ${value}`)
  for (const [value, expected] of [
    [0, ""], [local(11, 5), "今天 14:30"], [local(11, 6), "明天 14:30"],
    [local(11, 4), "昨天 14:30"], [local(11, 3), "11-03 14:30"],
    [local(11, 11), "周三 14:30"], [local(11, 12), "11-12 14:30"],
  ]) assert.equal(f.nearTime(value, NOW), expected, `nearTime ${value}`)
  assert.equal(f.monthDayTime(local(1, 2, 3, 4)), "1月2日 03:04")
  assert.equal(f.dayRange("2026-11-05", "2026-11-05"), "11月5日")
  assert.equal(f.dayRange("2026-01-31", "2026-12-01"), "1月31日 – 12月1日")
})

test("zh-CN durations keep each formatter's unit and precision contract", () => {
  const cases = {
    span: [[0, "1 分钟"], [3_569_999, "59 分钟"], [3_570_000, "1 小时"], [86_369_999, "24 小时"], [86_370_000, "1 天"]],
    elapsed: [[-1, "刚开始"], [29_999, "刚开始"], [30_000, "1 分钟"], [3_569_999, "59 分钟"], [3_570_000, "1 小时 0 分"], [3_900_000, "1 小时 5 分"]],
    clockDur: [[-1, "0 秒"], [59_499, "59 秒"], [59_500, "1 分 0 秒"], [65_000, "1 分 5 秒"], [3_599_499, "59 分 59 秒"], [3_599_500, "1 小时 0 分"], [3_725_000, "1 小时 2 分"]],
    longDur: [[-1, "0 秒"], [59_999, "60 秒"], [60_000, "1 分钟"], [3_569_999, "59 分钟"], [3_570_000, "1 小时"], [3_900_000, "1 小时 5 分"], [172_769_999, "47 小时 59 分"], [172_770_000, "2 天"], [187_200_000, "2 天 4 小时"]],
    msDur: [[0, "<1 毫秒"], [0.999, "<1 毫秒"], [1, "1 毫秒"], [240.6, "241 毫秒"], [999, "999 毫秒"], [1000, "1.0 秒"], [1499, "1.5 秒"]],
  }
  for (const [name, casesForFormat] of Object.entries(cases)) {
    for (const [value, expected] of casesForFormat) assert.equal(f[name](value), expected, `${name} ${value}`)
  }
  for (const [value, expected] of [
    [0, ["分钟", 60_000]], [7_199_999, ["分钟", 60_000]], [7_200_000, ["小时", 3_600_000]],
    [172_799_999, ["小时", 3_600_000]], [172_800_000, ["天", 86_400_000]],
  ]) assert.deepEqual(f.durUnit(value), expected, `durUnit ${value}`)
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
const ts = (await import("typescript")).default
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
  "ui.tsx": [["（附件）", "交付"], `tag category, attachment placeholder: ${HUB}`],
  "composer.tsx": [["新事项", "新项目", "汇报", "新", "（附件）", "未确认"], `slash command names, attachment placeholder, contact kind: ${HUB}`],
  "permission-question.ts": [["允许执行", "："], `permission question prefix: ${HUB}`],
  "decision-notes.ts": [["不发布", "：", "批准", "拒绝", "。补充：", "。原因：", "添加标签", "移除标签", "稍后提醒", "暂停", "放弃", "归档", "恢复", "撤销了决定", "同意开始", "不需要", "批准发布", "回答", "返工"], "parses and rebuilds the Hub's decision timeline text"],
  "devices.tsx": [["暂停接活", "正常", "执行节点"], `device status and the Hub's runner role: ${HUB}`],
  "store.ts": [["高", "中", "低", "你", "开始会话", "分流", "规划", "收尾", "交付与验收", "发布", "正在开始", "刚开始", "定义", "执行", "不", "排队", "插话", "删除", "导出", "项目", "类型", "交付", "领域", "客户", "主题", "交接", "依赖", "派生", "等前置事项完成后自动开始"], `priorities, tag categories, stage and edge ids, timeline text and a legacy wait label matched for translation: ${HUB}`],
  "views.tsx": [["接收", "分流", "定义", "规划", "准备", "执行", "交付与验收", "收尾", "高", "中", "低", "无", "今天", "明天", "周五", "下周一", "全部", "等你", "进行中", "动态", "产出", "补充信息：", "（附件）", "：", "插话", "排队", "本机"], `stage, priority and due values (labels via t()), tab ids, timeline keywords, queued-comment prefix: ${HUB}`],
  "view-shared.tsx": [["高", "今天", "无", "明天", "周五", "下周一"], `priority and due values (labels via t()): ${HUB}`],
  "stats/overview.tsx": [["全部", "等你", "进行中"], "filter ids passed to views.showItems"],
  "knowledge/index.tsx": [["搜索", "长期记忆", "知识条目", "笔记与文档", "来源", "待确认"], "tab ids (labels via t()); 长期记忆/知识条目 are the Hub's record names"],
  "knowledge/steward.tsx": [["每天", "每周日", "每月", "日"], "the steward's schedule phrases, which the Hub's rule parser reads"],
  "knowledge/parts.tsx": [["存为长期记忆吗？", "存为知识条目吗？", "记住这条吗？", "为什么值得保留："], "the Hub's question and reason prefixes, matched"],
  "mail/list.tsx": [["等你", "需要"], "matched in the Hub's outcome label"],
  "bot-page.tsx": [["设置", "概览", "对外身份", "能力", "系统提示词", "定时与流程", "记忆", "自己写", "自动生成"], "the Bot page's tab ids and prompt modes (manage.tsx's); every one is shown through t()"],
  "lib/motion.ts": [["标准", "大", "开", "关"], "the Hub's stored fontSize and reduceMotion values"],
  "manage.tsx": [["全部", "启用", "停用", "项目", "类型", "交付", "领域", "客户", "主题", "设置", "删除", "导出", "概览", "对外身份", "能力", "系统提示词", "定时与流程", "记忆", "对话", "待处理", "事项", "自动开始", "提醒我开始", "关闭", "不发", "标准", "大", "开", "关", "天", "年", "永久"], "stored setting values, tag categories and tab ids; every one is shown through t()"],
  "out-groups.ts": [["自查", "自测", "调试", "记录", "日志", "截图", "输出", "运行", "测试", "构建", "中间", "结果", "产物", "文件", "版本", "临时", "草稿", "过程记录", "执行记录"], "words that mark an output name as a work record (names come from Bots)"],
  "outputs.tsx": [["预览", "源码", "逐张", "连续"], "view mode ids (labels via t()); data-gallery in browser tests"],
  "vault/new-item.tsx": [["私钥", "恢复码", "内容"], "matched in the Hub's field labels"],
  "version.tsx": [["拒绝"], "matched in the Hub's upgrade error"],
  "upgrade-guide.ts": [["升级已提交"], "matched in the Hub's upgrade error"],
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
