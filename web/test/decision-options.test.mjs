import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"

const { OPTION_IDS, WORDS_OPTION, hasOptionIds, optionKey } = await import("../src/decision-options.ts")
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
  for (const name of ["HANDOFF_OPTIONS", "DRAFT_OPTIONS", "REWRITE_OPTIONS", "FAIL_OPTIONS", "UNKNOWN_OPTIONS", "BLOCKED_OPTIONS"]) assert.ok(lists.has(name), name)
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

test("the handoff's reply is the option that takes your words", () => {
  const handoff = hubChannelOptions().get("HANDOFF_OPTIONS")
  assert.deepEqual(handoff, ["draft_reply", "to_item", "done"])
  assert.ok(handoff.includes(WORDS_OPTION.chan_message))
  assert.equal(label(zh, optionKey("chan_message", "done")), "不用回复")
  assert.equal(label(en, optionKey("chan_message", "to_item")), "Make it an item")
})

test("options of other subjects stay words of their own", () => {
  assert.equal(hasOptionIds("chan_message"), true)
  for (const s of ["design_implementation", "flow_step", "toString", "__proto__", ""]) assert.equal(hasOptionIds(s), false, s)
  assert.equal(optionKey("flow_step", "done"), undefined)
  assert.equal(optionKey("chan_message", "answer"), undefined)
})
