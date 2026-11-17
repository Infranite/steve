import assert from "node:assert/strict"
import { test } from "node:test"
import { readFile } from "node:fs/promises"
const m = await import("../src/caps/tool-catalog.ts")
const zh = (await import("../src/i18n/locales/zh-CN/caps.ts")).default
const en = (await import("../src/i18n/locales/en/caps.ts")).default
const catalog = [
  { name: "steve_contacts", aliases: ["steve_contacts_find", "steve_whois", "steve_person_holdings"] },
  { name: "steve_github_read", aliases: ["steve_github_status", "steve_github_ci_log"] },
]

test("legacy pins and usage present one family without rewriting remote pins", () => {
  assert.deepEqual(m.catalogPin({ kind: "tool", target: "steve_whois" }, catalog), { kind: "tool", target: "steve_contacts" })
  assert.deepEqual(m.catalogPin({ kind: "mcp", target: "steve_whois" }, catalog), { kind: "mcp", target: "steve_whois" })
  assert.equal(m.canonicalTool("custom__steve_whois", catalog), "custom__steve_whois")
  assert.deepEqual(m.catalogUsage([
    { tool: "steve_contacts", searched: 1, called: 2 },
    { tool: "steve_whois", searched: 3, called: 4 },
    { tool: "steve_contacts_find", searched: 5, called: 6 },
  ], catalog), [{ tool: "steve_contacts", searched: 9, called: 12 }])
  assert.equal(m.toolCount(catalog[0], new Map([["steve_contacts", 2], ["steve_whois", 4], ["steve_contacts_find", 6]])), 12)
})

test("all built-in tools have a separate user-facing description in both languages", async () => {
  const names = new Set([
    ...JSON.parse(await readFile(new URL("../../hub-rs/crates/steve-mcp/src/tools.json", import.meta.url), "utf8")).map((t) => t.name),
    "steve_tools_search", "steve_tool_call", "steve_contacts", "steve_contacts_find", "steve_contacts_remember", "steve_whois", "steve_person_holdings", "steve_steward_propose",
    "steve_item_read", "steve_items_search", "steve_item_dependencies", "steve_sessions_list", "steve_workflow", "steve_debug", "steve_upgrade", "steve_devices", "steve_reply_source", "steve_event_get", "steve_im_read",
    "steve_knowledge_search", "steve_knowledge_read", "steve_memory_list", "steve_memory_tidy", "steve_remember", "steve_secret_list", "steve_secret_use",
    "steve_digest_batch", "steve_digest_read", "steve_digest_chunk", "steve_digest_finish", "steve_digest_control", "steve_digest_report", "steve_matters", "steve_matters_search", "steve_matter_read",
    "steve_todo_list", "steve_todo_add", "steve_todo_update", "steve_todo_complete",
    "steve_github_read", "steve_github_status", "steve_github_ci_log", "steve_github_comment", "steve_github_open_pr", "steve_mail_read", "steve_mail_thread", "steve_mail_attachment", "steve_mail_draft",
    "steve_onboard_profile", "steve_onboard_models", "steve_onboard_bots", "steve_onboard_done", "steve_item_create", "steve_item_supplement",
  ])
  for (const name of names) {
    const key = m.toolDescKey(name)
    assert.ok(key, name)
    assert.match(zh.tool[key], /\p{Script=Han}/u, name)
    assert.ok(en.tool[key] && !/\p{Script=Han}/u.test(en.tool[key]), name)
  }
  assert.equal(m.toolDescKey("external_user_authored_tool"), undefined)
})
