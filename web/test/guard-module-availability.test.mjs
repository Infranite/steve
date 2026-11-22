import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { test } from "node:test"

const root = new URL("../../", import.meta.url)

test("Web module availability never depends on HTTP not-found", () => {
  const result = spawnSync("git", [
    "grep", "-I", "-o", "--untracked", "--exclude-standard", "-w", "-Ei",
    "(404|405|NotWired|isNotWired|Unwired)",
    "--", ":(glob)web/src/**/*.ts", ":(glob)web/src/**/*.tsx",
  ], { cwd: root, encoding: "utf8" })
  assert.ok(result.status === 0 || result.status === 1, result.error?.message || result.stderr)
  const counts = {}
  for (const line of result.stdout.trim().split("\n").filter(Boolean)) {
    const [file, code] = line.split(":")
    assert.equal(code, "404", line)
    const path = file.slice("web/src/".length)
    counts[path] = (counts[path] ?? 0) + 1
  }
  // Every HTTP check below reports a specific missing resource.
  assert.deepEqual(counts, {
    "accounts/index.tsx": 1, // account
    "auto/rule.tsx": 1, // flow
    "calendar/event.tsx": 1, // event
    "channels/message-resource.tsx": 1, // attachment
    "channels/persona.tsx": 2, // persona
    "channels/space.tsx": 2, // space
    "delivery-page.tsx": 1, // shared output
    "feed.tsx": 1, // feed entry
    "integrations/feishu.tsx": 1, // device flow
    "mail/thread.tsx": 1, // thread
    "people/contact.tsx": 2, // contact
    "people/org.tsx": 2, // organization
    "people/teams.tsx": 2, // team
    "qr.ts": 1, // QR code encoding table, unrelated to HTTP
    "thread/conversations.tsx": 1, // conversation
    "todos/page.tsx": 1, // todo
    "vault/index.tsx": 1, // vault item
  })
})
