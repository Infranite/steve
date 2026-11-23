import assert from "node:assert/strict"
import test from "node:test"
import { readFileSync } from "node:fs"
import { orderedUnits, stopPending } from "../src/workflow.ts"

const unit = (id, depends_on = [], status = "pending", required = true) => ({ id, depends_on, status, required })

test("dependency ordering retains independent work and puts review before integration", () => {
  const units = [unit("integrate", ["review"]), unit("review", ["a", "b"]), unit("a"), unit("b")]
  assert.deepEqual(orderedUnits(units).map((u) => u.id), ["a", "b", "review", "integrate"])
  assert.equal(units[0].id, "integrate", "display order never mutates the stored graph")
})

test("malformed historical dependencies remain visible and cannot hang the item page", () => {
  const units = [unit("a", ["b"]), unit("b", ["a"]), unit("missing", ["old"])]
  assert.equal(orderedUnits(units).length, 3)
  assert.equal(orderedUnits(units)[0].id, "missing")
})

test("stop pending shows only on a stopped unit, never on one that succeeded", () => {
  assert.equal(stopPending({ status: "cancelled", stop_pending: true }), true)
  assert.equal(stopPending({ status: "failed", stop_pending: true }), true)
  assert.equal(stopPending({ status: "succeeded", stop_pending: true }), false, "an older Hub flagged finished units whose Node was not attached")
  assert.equal(stopPending({ status: "cancelled", stop_pending: false }), false)
  assert.equal(stopPending({ status: "cancelled" }), false)
})

const panel = readFileSync(new URL("../src/agent-tree-view.tsx", import.meta.url), "utf8")

test("disabled workflows block every retry/resume/rework entry, including an already open confirmation", () => {
  assert.match(panel, /const disabled = workflow \? !workflow\.enabled : false/, "the Hub's flag decides")
  assert.match(panel, /if \(disabled && \(action === "resume" \|\| action === "retry"\)\)/, "action handler also guards stale controls")
  assert.match(panel, /workflow\.paused && \(at\.disabled \|\| at\.unavailable \|\| at\.old\)/, "only resume is disabled, not pause")
  assert.match(panel, /!at\.old && !at\.disabled && !at\.unavailable && u\.attempt/, "compact step retry cannot bypass the flag")
  for (const marker of ['data-workflow-retry={x.node.unit.id}', 'data-workflow-retry={u!.id}', 'data-workflow-rework={u!.id}']) {
    const control = panel.split("\n").find((line) => line.includes(marker))
    assert.match(control, /disabled=\{at\.busy \|\| at\.disabled/, marker)
  }
  assert.match(panel, /disabled=\{busy \|\| disabled && !activeAffected/, "an open rework confirmation cannot retry, but can still stop affected work")
  const cancel = panel.split("\n").find((line) => line.includes('data-workflow-cancel={u!.id}'))
  assert.doesNotMatch(cancel, /at\.disabled/, "stopping remains available")
  assert.match(panel, /DropdownMenuItem disabled=\{at\.busy\} onClick=\{at\.askSingle\}/, "explicit single-session takeover retains its safeguards")
})

test("disabled explanation replaces resume promises and locales honestly explain review waiver", () => {
  assert.match(panel, /at\.disabled && !at\.single && <N mark="workflow-disabled">/)
  assert.match(panel, /at\.unavailable && !at\.disabled/)
  assert.match(panel, /!at\.single && !at\.disabled && !at\.unavailable && <N>/)
  for (const lang of ["en", "zh-CN"]) {
    const locale = readFileSync(new URL(`../src/i18n/locales/${lang}/views.ts`, import.meta.url), "utf8")
    assert.match(locale, /disabled: "/)
    assert.match(locale, lang === "en" ? /explicitly waives the remaining required graph reviews/ : /明确豁免本轮剩余的工作图必需评审/)
  }
})
