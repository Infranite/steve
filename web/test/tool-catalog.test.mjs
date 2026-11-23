import assert from "node:assert/strict"
import { test } from "node:test"
import { readFile } from "node:fs/promises"
const m = await import("../src/caps/tool-catalog.ts")
const zh = (await import("../src/i18n/locales/zh-CN/caps.ts")).default
const en = (await import("../src/i18n/locales/en/caps.ts")).default
test("all built-in tools have a separate user-facing description in both languages", async () => {
  const names = new Set([
    ...JSON.parse(await readFile(new URL("../../hub-rs/crates/steve-mcp/src/tools.json", import.meta.url), "utf8")).map((t) => t.name),
    "steve_tools_search", "steve_tool_call", "steve_contacts", "steve_contacts_remember", "steve_steward_propose",
    "steve_item_read", "steve_items_search", "steve_item_dependencies", "steve_sessions_list", "steve_workflow", "steve_debug", "steve_upgrade", "steve_devices", "steve_reply_source", "steve_event_get", "steve_im_read",
    "steve_knowledge_search", "steve_knowledge_read", "steve_memory_list", "steve_memory_tidy", "steve_remember", "steve_secret_list", "steve_secret_use",
    "steve_digest_batch", "steve_digest_read", "steve_digest_chunk", "steve_digest_finish", "steve_digest_control", "steve_digest_report", "steve_matters",
    "steve_todo_list", "steve_todo_add", "steve_todo_update", "steve_todo_complete",
    "steve_github_read", "steve_github_comment", "steve_github_open_pr", "steve_mail_read", "steve_mail_draft",
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
