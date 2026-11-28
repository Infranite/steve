import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"

const { OPTION_IDS, WITH_WORDS, WORDS_OPTIONS, hasOptionIds, optionKey } = await import("../src/decision-options.ts")
const zh = (await import("../src/i18n/locales/zh-CN/index.ts")).default
const en = (await import("../src/i18n/locales/en/index.ts")).default

/* the Hub's channel decisions: every option list it opens a `chan_message` decision with */
const CHANNEL_SOURCES = ["respond.rs", "outbound.rs"].map((f) => readFileSync(new URL(`../../hub-rs/crates/steve-channels/src/${f}`, import.meta.url), "utf8"))
function hubChannelOptions() {
  const src = CHANNEL_SOURCES.join("\n")
  const consts = Object.fromEntries([...src.matchAll(/pub const (\w+): &str = "([^"]*)";/g)].map((m) => [m[1], m[2]]))
  const lists = [...src.matchAll(/pub const (\w+_OPTIONS): \[&str; \d+\] = \[([^\]]*)\];/g)]
  assert.ok(lists.length >= 6, `found the Hub's option lists: ${lists.map((m) => m[1])}`)
  return new Map(lists.map((m) => [m[1], m[2].split(",").map((x) => x.trim()).filter(Boolean).map((x) => {
    const q = /^"([^"]*)"$/.exec(x)
    if (q) return q[1]
    assert.ok(x in consts, `${m[1]}: ${x} is a string constant`)
    return consts[x]
  })]))
}
const label = (lang, key) => {
  const [ns, path] = key.split(":")
  return path.split(".").reduce((o, k) => o?.[k], lang[ns])
}

test("every option of a channel decision has a label in zh-CN and en", () => {
  const lists = hubChannelOptions()
  for (const name of ["HANDOFF_OPTIONS", "HANDOFF_NO_DRAFT_OPTIONS", "DRAFT_OPTIONS", "REWRITE_OPTIONS", "FAIL_OPTIONS", "UNKNOWN_OPTIONS", "BLOCKED_OPTIONS"]) assert.ok(lists.has(name), name)
  for (const [name, ids] of lists) for (const id of ids) {
    const key = optionKey("chan_message", id)
    assert.ok(key, `${name}: "${id}" is listed in decision-options.ts`)
    for (const [lang, l] of [["zh-CN", zh], ["en", en]]) {
      const s = label(l, key)
      assert.equal(typeof s, "string", `${lang} ${key}`)
      assert.ok(s.trim() && s !== id, `${lang} ${key} is words, not the id: ${s}`)
    }
  }
})

test("every listed option id has its label in both languages", () => {
  for (const [subject, ids] of Object.entries(OPTION_IDS)) for (const id of ids)
    for (const l of [zh, en]) assert.ok(label(l, optionKey(subject, id))?.trim(), `${subject}.${id}`)
})

test("a message handed over to you: send the draft, tell it how to reply, send your words, an item, leave it", () => {
  const lists = hubChannelOptions()
  assert.deepEqual(lists.get("HANDOFF_OPTIONS"), ["send_draft", "instruct", "verbatim", "to_item", "done"])
  assert.deepEqual(lists.get("HANDOFF_NO_DRAFT_OPTIONS"), ["instruct", "verbatim", "to_item", "done"])
  /* how to reply is the field; your words as they are go with their own button */
  assert.ok(lists.get("HANDOFF_OPTIONS").includes(WORDS_OPTIONS.chan_message[0]))
  assert.deepEqual(WITH_WORDS.chan_message, ["verbatim"])
  const zhOf = (id) => label(zh, optionKey("chan_message", id))
  assert.deepEqual(["send_draft", "instruct", "verbatim", "to_item", "done"].map(zhOf), ["发这条草稿", "告诉它怎么回", "按原文发", "转事项", "不用管"])
  assert.equal(label(en, optionKey("chan_message", "to_item")), "Make it an item")
})

test("options of other subjects stay words of their own", () => {
  assert.equal(hasOptionIds("chan_message"), true)
  assert.equal(hasOptionIds("wf_run"), true)
  for (const s of ["design_implementation", "flow_step", "toString", "__proto__", ""]) assert.equal(hasOptionIds(s), false, s)
  assert.equal(optionKey("flow_step", "done"), undefined)
  assert.equal(optionKey("chan_message", "answer"), undefined)
})

test("every WfDecisionChoice in workflow_run.rs has a label in zh-CN and en", () => {
  const src = readFileSync(new URL("../../hub-rs/crates/steve-core/src/workflow_run.rs", import.meta.url), "utf8")
  const block = /impl WfDecisionChoice \{[\s\S]*?pub const fn as_str\(self\) -> &'static str \{([\s\S]*?)\n    \}/.exec(src)
  assert.ok(block, "found WfDecisionChoice::as_str")
  const ids = [...block[1].matchAll(/Self::\w+\s*=>\s*"([^"]+)"/g)].map((m) => m[1])
  assert.ok(ids.length >= 5, `found WfDecisionChoice variants: ${ids}`)
  for (const id of ids) {
    const key = optionKey("wf_run", id)
    assert.ok(key, `wf_run option "${id}" is listed in decision-options.ts`)
    for (const [lang, l] of [["zh-CN", zh], ["en", en]]) {
      const s = label(l, key)
      assert.equal(typeof s, "string", `${lang} ${key}`)
      assert.ok(s.trim() && s !== id, `${lang} ${key} is words, not the id: ${s}`)
      const decidedKey = `core:decided.wf_run.${id}`
      const decidedStr = label(l, decidedKey)
      assert.equal(typeof decidedStr, "string", `${lang} ${decidedKey}`)
      assert.ok(decidedStr.trim(), `${lang} ${decidedKey}`)
    }
  }
})
