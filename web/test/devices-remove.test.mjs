import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"
import { MANUAL_REMOVE, REMOVE_STEPS, removeStep, removeSteps, removeView } from "../src/devices-remove.ts"

const i18n = (await import("../src/i18n/index.ts")).default
const tIn = (lng) => (key, o) => i18n.t(key, { lng, ...o })
const run = (o) => ({ operation_id: "nrm_1", phase: "draining", error: null, requested_by: "u", grace_secs: 600, created: 1, updated: 2, ...o })
const node = (removal, o) => ({ local: false, state: "active", removal: { reason: "ok", latest: null, command: "steve node uninstall …", grace_secs: 600, ...removal }, ...o })
const dialog = readFileSync(new URL("../src/devices-remove-dialog.tsx", import.meta.url), "utf8")

test("a Node the Hub can ask is confirmed first: the dialog asks, with the grace period, and starts nothing by itself", () => {
  const v = removeView(node({}))
  assert.equal(v.kind, "ask")
  assert.equal(v.grace, 600)
  assert.equal(v.last, null)
  // one place starts the removal: the confirm button's click, never a render or an effect
  assert.equal(dialog.match(/api\.removeNode\(/g)?.length, 1)
  assert.match(dialog, /data-node-remove-confirm/)
  assert.match(dialog, /onClick=\{\(\) => void start\(\)\} data-node-remove-confirm/)
  // not asked yet whether it can: still asked, the Hub answers when it starts
  assert.equal(removeView(node({ reason: "unknown" })).kind, "ask")
})

test("an offline Node, or one that cannot remove itself, shows the command to run on its machine", () => {
  for (const why of ["offline", "manual"]) {
    const v = removeView(node({ reason: why }))
    assert.deepEqual(v, { kind: "manual", why, command: "steve node uninstall …" })
  }
  // the Hub refused when asked: what it said wins over the last list
  assert.equal(removeView(node({}), "manual").kind, "manual")
  assert.equal(removeView(node({}), "offline").why, "offline")
  // a Hub that sends no command: the default one
  assert.equal(removeView(node({ reason: "offline", command: "" })).command, MANUAL_REMOVE)
  assert.match(MANUAL_REMOVE, /node stop && .* node uninstall && rm -rf ~\/\.steve-node$/)
})

test("a removal under way shows its step; its end shows what happened", () => {
  assert.deepEqual(REMOVE_STEPS, ["draining", "ready", "removing", "unpaired"])
  for (const [phase, step] of [["requested", "draining"], ["draining", "draining"], ["cancelling", "draining"], ["ready", "ready"], ["confirming", "removing"]]) {
    const v = removeView(node({ latest: run({ phase }) }))
    assert.equal(v.kind, "running", phase)
    assert.equal(v.step, step, phase)
    assert.equal(removeStep(phase), step)
  }
  assert.deepEqual(removeSteps("removing"), ["done", "done", "active", "todo"])
  // just started, the list not read again yet: under way, not the question again
  const fresh = removeView(node({ latest: run({ operation_id: "nrm_0", phase: "failed" }) }), null, "nrm_9")
  assert.equal(fresh.kind, "running")
  assert.equal(fresh.run.operation_id, "nrm_9")
  // the dialog that started it: removed, or why not with the command
  assert.equal(removeView(node({ latest: run({ phase: "removed" }) }, { state: "revoked" }), null, "nrm_1").kind, "removed")
  const ended = removeView(node({ latest: run({ phase: "unconfirmed", error: "the node did not get ready within 780 s" }) }), null, "nrm_1")
  assert.equal(ended.kind, "ended")
  assert.equal(ended.command, "steve node uninstall …")
  // opened again later: a new attempt can be asked, saying how the last one ended
  const again = removeView(node({ latest: run({ phase: "failed", error: "x" }) }))
  assert.equal(again.kind, "ask")
  assert.equal(again.last.error, "x")
})

test("nothing to remove for the Hub's own Node, an unpaired one or a Hub that cannot say", () => {
  assert.equal(removeView(node({}, { local: true })).kind, "none")
  assert.equal(removeView(node({}, { state: "revoked" })).kind, "none")
  assert.equal(removeView({ local: false, state: "active" }).kind, "none")
  assert.equal(removeView(null).kind, "none")
})

test("both languages say what each action does to the machine", async () => {
  await i18n.loadLanguages("en")
  for (const lng of ["en", "zh-CN"]) {
    const t = tIn(lng)
    for (const k of ["action", "sub", "intro", "plan.stop", "plan.uninstall", "plan.unpair", "undo", "confirm", "why.offline", "why.manual", "runThere", "commandNote", "unpairNow", "ended.unconfirmed", "ended.failed", ...REMOVE_STEPS.map((s) => `step.${s}`)]) {
      const s = t(`devices:remove.${k}`)
      assert.notEqual(s, `remove.${k}`, `${lng}: ${k}`)
      assert.ok(s.length > 0)
    }
    assert.match(t("devices:remove.plan.grace", { grace: "10 min" }), /10 min/)
    assert.match(t("devices:remove.title", { name: "studio" }), /studio/)
  }
  // unpair says the Node stays installed; remove says it is deleted
  assert.match(tIn("en")("devices:node.revokeSub"), /stays installed/)
  assert.match(tIn("zh-CN")("devices:node.revokeSub"), /仍装在/)
  assert.match(tIn("en")("devices:remove.sub"), /uninstalls/)
  assert.match(tIn("zh-CN")("devices:remove.sub"), /卸载/)
})
