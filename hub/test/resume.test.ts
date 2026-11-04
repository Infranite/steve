import test, { after } from "node:test"
import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { nativeId, resumeCommand, resumeInfo } from "../src/resume.js"

const root = mkdtempSync(join(tmpdir(), "steve-resume-"))
Object.assign(process.env, { HOME: root })
delete process.env.CLAUDE_CONFIG_DIR; delete process.env.CODEX_HOME
after(() => rmSync(root, { recursive: true, force: true }))

test("the harness id: the ACP id, or the id in an imported transcript's file name", () => {
  assert.equal(nativeId({ harness: "claude-code", cwd: "/w", acp_session_id: "abc" }), "abc")
  assert.equal(nativeId({ harness: "claude-code", cwd: "/w", acp_session_id: null, import_key: "cc:/h/.claude/projects/-x/3b2c458f-84be-4589-a22c-5bfd2579c313.jsonl" }), "3b2c458f-84be-4589-a22c-5bfd2579c313")
  assert.equal(nativeId({ harness: "codex", cwd: "/w", acp_session_id: null, import_key: "cx:/h/.codex/sessions/2026/11/04/rollout-2026-11-04T15-36-56-01a1a57d-52a0-7111-9c1d-0f8bf59e7c96.jsonl" }), "01a1a57d-52a0-7111-9c1d-0f8bf59e7c96")
  assert.equal(nativeId({ harness: "codex", cwd: "/w", acp_session_id: null }), null)
})

test("resume commands per harness, quoted, with the config dir only when it is not the default", () => {
  assert.equal(resumeCommand("claude-code", "id-1", "/w/ST-1"), "cd /w/ST-1 && claude --resume id-1")
  assert.equal(resumeCommand("codex", "id-2", "/w/my dir"), "cd '/w/my dir' && codex resume id-2")
  assert.equal(resumeCommand("gemini", "id", "/w"), null)
  process.env.CODEX_HOME = "/srv/codex's"
  assert.equal(resumeCommand("codex", "id", "/w"), `cd /w && CODEX_HOME='/srv/codex'\\''s' codex resume id`)
  delete process.env.CODEX_HOME
})

test("the transcript is found and checked against the session's id and directory", () => {
  const cwd = join(root, "w/ST-1"); mkdirSync(cwd, { recursive: true })
  const pdir = join(root, ".claude/projects", cwd.replace(/[^a-zA-Z0-9]/g, "-")); mkdirSync(pdir, { recursive: true })
  writeFileSync(join(pdir, "c-1.jsonl"), [{ type: "queue-operation", sessionId: "c-1" }, { type: "user", message: "A".repeat(2 << 20) }, { type: "user", sessionId: "c-1", cwd }].map((x) => JSON.stringify(x)).join("\n") + "\n")
  const day = join(root, ".codex/sessions/2026/11/04"); mkdirSync(day, { recursive: true })
  writeFileSync(join(day, "rollout-2026-11-04T10-00-00-x-2.jsonl"), JSON.stringify({ type: "session_meta", payload: { id: "x-2", cwd: "/elsewhere" } }) + "\n")

  const cc = resumeInfo({ harness: "claude-code", cwd, acp_session_id: "c-1" })
  assert.equal(cc.id, "c-1"); assert.equal(cc.cwd_exists, true)
  assert.equal(cc.transcript?.matches, true); assert.equal(cc.transcript?.cwd, cwd)
  const cx = resumeInfo({ harness: "codex", cwd, acp_session_id: "x-2" })
  assert.equal(cx.transcript?.id, "x-2"); assert.equal(cx.transcript?.matches, false)
  const gone = resumeInfo({ harness: "claude-code", cwd, acp_session_id: "gone" })
  assert.equal(gone.transcript, null); assert.match(gone.command ?? "", /claude --resume gone$/)
  const none = resumeInfo({ harness: "codex", cwd: "/nope", acp_session_id: null })
  assert.equal(none.id, null); assert.equal(none.command, null); assert.equal(none.cwd_exists, false)
})
