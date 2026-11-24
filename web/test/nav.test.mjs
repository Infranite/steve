import assert from "node:assert/strict"
import { test } from "node:test"

import { MOBILE_TAB_KEYS, NAV_GROUPS, SIDEBAR_FOOTER_NAV, SIDEBAR_NAV, SIDEBAR_SCROLL_GROUPS, navItemOf, navMetaOf } from "../src/nav.ts"
import { flushNavOpens, recordNavOpen, resetNavTracker } from "../src/perf.ts"

test("sidebar IA groups and item order put highest-frequency items first", () => {
  assert.deepEqual([...NAV_GROUPS], ["primary", "schedule", "messages", "library", "automate", "more"])
  assert.deepEqual([...SIDEBAR_SCROLL_GROUPS], ["primary", "schedule", "messages", "library", "automate"])
  assert.deepEqual(SIDEBAR_FOOTER_NAV.map((x) => x.l), ["stats"])
  const byGroup = Object.fromEntries(NAV_GROUPS.map((g) => [g, SIDEBAR_NAV.filter((x) => x.group === g).map((x) => x.l)]))
  assert.deepEqual(byGroup, {
    primary: ["chat", "inbox", "items", "projects"],
    schedule: ["calendar", "todos"],
    messages: ["feed", "channels", "mail", "contacts"],
    library: ["knowledge", "integrations"],
    automate: ["bots", "sessions", "auto", "capabilities", "signals"],
    more: ["stats"],
  })
  assert.deepEqual([...MOBILE_TAB_KEYS], ["chat", "inbox", "items", "calendar", "todos"])
})

test("navItemOf maps routes and sub-routes to bounded nav keys", () => {
  for (const [hash, expected] of [
    ["", "chat"],
    ["#/", "chat"],
    ["#/c/chan:123", "chat"],
    ["#/inbox", "inbox"],
    ["#/items/ST-42", "items"],
    ["#/todos?view=goals", "todos"],
    ["#/calendar/e/ev_1", "calendar"],
    ["#/contacts", "contacts"],
    ["#/contacts/p_1", "contacts"],
    ["#/orgs/o_1", "contacts"],
    ["#/teams/t_1", "contacts"],
    ["#/contacts/me", "me"],
    ["#/stats/items/ST-1", "stats"],
    ["#/settings/general", "settings"],
    ["#/more", null],
    ["#/design", null],
  ]) {
    assert.equal(navItemOf(hash), expected, hash)
  }
  assert.deepEqual(navMetaOf("inbox"), { href: "#/inbox", group: "primary" })
  assert.deepEqual(navMetaOf("settings"), { href: "#/settings", group: "account" })
  assert.equal(navMetaOf("unknown"), null)
})

test("recordNavOpen deduplicates consecutive hits within one section and reports via web-perf sender", async () => {
  const sent = []
  resetNavTracker(async (report) => { sent.push(report) })
  try {
    assert.equal(recordNavOpen("#/inbox"), true)
    assert.equal(recordNavOpen("#/inbox"), false)
    assert.equal(recordNavOpen("#/items"), true)
    assert.equal(recordNavOpen("#/items/ST-42"), false)
    assert.equal(recordNavOpen("#/items"), false)
    assert.equal(recordNavOpen("#/todos?view=goals"), true)
    assert.equal(recordNavOpen("#/inbox"), true)
    assert.equal(recordNavOpen("#/more"), false)
    await flushNavOpens()

    const totals = {}
    for (const report of sent) {
      for (const [item, count] of Object.entries(report.nav_opens ?? {})) {
        totals[item] = (totals[item] ?? 0) + count
      }
    }
    assert.deepEqual(totals, { inbox: 2, items: 1, todos: 1 })
  } finally {
    resetNavTracker(null)
  }
})
