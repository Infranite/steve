/* what the web gets of a session event: no screenshots, no second copy of tool output, long text cut, diffs whole */
import { test, after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const home = process.env.STEVE_HOME = mkdtempSync(join(tmpdir(), "steve-test-"))
after(() => rmSync(home, { recursive: true, force: true }))
const { evOut } = await import("../src/api.js")

test("a screenshot read ships without its base64", () => {
  const png = "iVBOR".repeat(60_000)
  const d = { toolCallId: "t1", status: "completed", _meta: { claudeCode: { toolName: "Read", toolResponse: { stdout: png } } }, rawInput: { file_path: "/x.png" },
    content: [{ type: "content", content: { type: "image", data: png, mimeType: "image/png" } }], rawOutput: [{ type: "image", source: { type: "base64", data: png } }] }
  const out = evOut(d)
  assert.equal(out._meta, undefined); assert.equal(out.tool, "Read"); assert.deepEqual(out.rawInput, { file_path: "/x.png" })
  assert.deepEqual(out.content, [{ type: "content", content: { type: "image" } }]); assert.deepEqual(out.rawOutput, [{ type: "image" }])
  assert.ok(JSON.stringify(out).length < 300)
})

test("long text is cut, diffs and short fields stay as they are", () => {
  const long = "x".repeat(10_000), file = "line\n".repeat(5000)
  const out = evOut({ title: "Bash ls", status: "completed", rawOutput: long, content: [{ type: "content", content: { type: "text", text: long } }, { type: "diff", path: "/a", oldText: file, newText: file }] })
  assert.equal(out.title, "Bash ls"); assert.equal(out.rawOutput.length, 4001); assert.equal(out.content[0].content.text.length, 4001)
  assert.equal(out.content[1].newText, file); assert.equal(out.content[1].oldText, file)
  assert.deepEqual(evOut({ rawInput: { command: "ls", description: "List", timeout: 5, dangerouslyDisableSandbox: true }, _meta: { claudeCode: {} } }), { rawInput: { command: "ls", description: "List" } })
  assert.deepEqual(evOut({ rawInput: { timeout: 5 } }), {})
  assert.equal(evOut(null), null); assert.deepEqual(evOut({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "hi" } }), { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "hi" } })
})
