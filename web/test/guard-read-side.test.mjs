import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { test } from "node:test"

const root = new URL("../../", import.meta.url)

test("the retired aggregate route, cursor header and assembly stay deleted", () => {
  // The Apple migration is deferred. Published release notes and applied migrations are immutable.
  const historical = [
    "apple/Packages/SteveClientKit/Sources/SteveClientKit/HubClientAPI.swift",
    "apple/Packages/SteveClientKit/Tests/SteveClientKitTests/SteveClientKitTests.swift",
    "docs/releases/0.2.98.md",
    "hub-rs/crates/steve-store/migrations/004.sql",
  ]
  const deleted = ["api/" + "state", "x-steve-" + "cursor", "compute_" + "state", String.raw`["']/sta` + `te["']`]
  const result = spawnSync("git", [
    "grep", "-I", "-l", "--untracked", "--exclude-standard", "-E", deleted.join("|"), "--",
    ...historical.map((path) => `:(exclude)${path}`),
  ], { cwd: root, encoding: "utf8" })
  assert.equal(result.status, 1, result.error?.message || result.stderr || result.stdout)
})
