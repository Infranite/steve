import assert from "node:assert/strict"
import { test } from "node:test"
import {
  compareVersionsDesc,
  filterModelGroups,
  OTHER_FAMILY,
  parseModelId,
  structureHarnessModels,
} from "../src/harness-model-list.ts"

test("parseModelId extracts family, source prefixes, version, and date snapshot deterministically", () => {
  const claudeSnap = parseModelId("claude-opus-4-5-20251101")
  assert.equal(claudeSnap.family, "claude")
  assert.equal(claudeSnap.familyLabel, "Claude")
  assert.equal(claudeSnap.source, "direct")
  assert.equal(claudeSnap.baseId, "claude-opus-4-5")
  assert.equal(claudeSnap.snapshotDate, "20251101")
  assert.deepEqual(claudeSnap.version, [4, 5])

  const gptDashDate = parseModelId("gpt-4o-2024-11-20")
  assert.equal(gptDashDate.family, "gpt")
  assert.equal(gptDashDate.familyLabel, "GPT")
  assert.equal(gptDashDate.baseId, "gpt-4o")
  assert.equal(gptDashDate.snapshotDate, "20241120")
  assert.deepEqual(gptDashDate.version, [4])

  const geminiPreview = parseModelId("gemini-2.5-pro-preview-06-05")
  assert.equal(geminiPreview.family, "gemini")
  assert.equal(geminiPreview.familyLabel, "Gemini")
  assert.equal(geminiPreview.baseId, "gemini-2.5-pro-preview")
  assert.equal(geminiPreview.snapshotDate, "0605")
  assert.equal(geminiPreview.isPreview, true)
  assert.deepEqual(geminiPreview.version, [2, 5])

  const kimi = parseModelId("kimi-k2-0905-preview")
  assert.equal(kimi.family, "kimi")
  assert.equal(kimi.familyLabel, "Kimi")
  assert.equal(kimi.baseId, "kimi-k2-preview")
  assert.equal(kimi.snapshotDate, "0905")
  assert.deepEqual(kimi.version, [2])

  const passthroughGlm = parseModelId("passthrough/glm-4.7")
  assert.equal(passthroughGlm.family, "glm")
  assert.equal(passthroughGlm.familyLabel, "GLM")
  assert.equal(passthroughGlm.source, "passthrough")
  assert.equal(passthroughGlm.baseId, "passthrough/glm-4.7")
  assert.deepEqual(passthroughGlm.version, [4, 7])

  const passthroughLocal = parseModelId("passthrough/local/qwen3-32b")
  assert.equal(passthroughLocal.family, "qwen")
  assert.equal(passthroughLocal.familyLabel, "Qwen")
  assert.equal(passthroughLocal.source, "passthrough_local")
  assert.deepEqual(passthroughLocal.version, [3])

  const localLlama = parseModelId("local/llama-3.3-70b-instruct")
  assert.equal(localLlama.family, "llama")
  assert.equal(localLlama.familyLabel, "Llama")
  assert.equal(localLlama.source, "local")
  assert.deepEqual(localLlama.version, [3, 3])

  const longCtx = parseModelId("claude-opus-4-6[1m]")
  assert.equal(longCtx.family, "claude")
  assert.equal(longCtx.baseId, "claude-opus-4-6[1m]")
  assert.deepEqual(longCtx.version, [4, 6])

  const namespacedSnap = parseModelId("passthrough/anthropic/claude-opus-4-6-20260205")
  assert.equal(namespacedSnap.family, "claude")
  assert.equal(namespacedSnap.source, "passthrough")
  assert.equal(namespacedSnap.namespace, "anthropic")
  assert.equal(namespacedSnap.baseId, "passthrough/anthropic/claude-opus-4-6")
  assert.equal(namespacedSnap.snapshotDate, "20260205")
  assert.deepEqual(namespacedSnap.version, [4, 6])
})

test("parseModelId auto-groups brand-new future families without a hardcoded model list", () => {
  const futureA = parseModelId("zephyr-3.2-pro")
  assert.equal(futureA.family, "zephyr")
  assert.equal(futureA.familyLabel, "Zephyr")
  assert.deepEqual(futureA.version, [3, 2])

  const futureB = parseModelId("passthrough/helios-v2.1-fast")
  assert.equal(futureB.family, "helios")
  assert.equal(futureB.familyLabel, "Helios")
  assert.equal(futureB.source, "passthrough")
  assert.deepEqual(futureB.version, [2, 1])

  const deepseek = parseModelId("deepseek-v3.2")
  assert.equal(deepseek.family, "deepseek")
  assert.equal(deepseek.familyLabel, "DeepSeek")
  assert.deepEqual(deepseek.version, [3, 2])
})

test("parseModelId routes unrecognized control/router IDs to 'other' instead of guessing", () => {
  for (const id of [
    "default",
    "auto",
    "auto-mode-classifier",
    "remote/model",
    "local/model",
    "custom-router-vnext",
    "foo-bar",
    "embedding-small",
    "",
  ]) {
    const parsed = parseModelId(id)
    assert.equal(parsed.family, OTHER_FAMILY, `expected "${id}" in other, got "${parsed.family}"`)
    assert.equal(parsed.familyLabel, null)
  }
  assert.equal(parseModelId("local/model").source, "local")
})

test("structureHarnessModels collapses date snapshots with aliases, sorts newest-to-oldest, and builds summary chips", () => {
  const raw = [
    "default",
    "auto-mode-classifier",
    "claude-3-5-sonnet-20241022",
    "claude-opus-4-5-20251101",
    "claude-opus-4-5",
    "claude-opus-4-5-20251015",
    "claude-opus-4-6",
    "claude-sonnet-4-5-20250929",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
    "gpt-4o",
    "gpt-4o-2024-11-20",
    "gpt-5.4",
    "gpt-5.3-codex",
    "gemini-2.5-pro-preview-06-05",
    "gemini-2.5-pro",
    "gemini-3.0-pro",
    "kimi-k2",
    "kimi-k2.5",
    "passthrough/glm-4.5",
    "passthrough/glm-4.7",
    "passthrough/glm-4.7-20251110",
    "passthrough/local/qwen3-32b",
    "passthrough/local/qwen2.5-72b",
  ]
  const structured = structureHarnessModels(raw, "claude-opus-4-6")

  assert.equal(structured.totalRaw, raw.length)
  assert.equal(structured.defaultModel, "claude-opus-4-6")

  // Default family comes first, then direct families alphabetically, then passthrough/local, then other last.
  assert.deepEqual(
    structured.groups.map((g) => g.family),
    ["claude", "gemini", "gpt", "kimi", "glm", "qwen", "other"],
  )

  // Summary chips exclude "other" when named families exist, showing each family's latest model.
  assert.deepEqual(
    structured.familyChips.map((c) => [c.family, c.latestId, c.source]),
    [
      ["claude", "claude-opus-4-6", "direct"],
      ["gemini", "gemini-3.0-pro", "direct"],
      ["gpt", "gpt-5.4", "direct"],
      ["kimi", "kimi-k2.5", "direct"],
      ["glm", "passthrough/glm-4.7", "passthrough"],
      ["qwen", "passthrough/local/qwen3-32b", "passthrough_local"],
    ],
  )

  // Claude entries sorted newest-to-oldest, with date snapshots collapsed under their alias.
  const claudeGroup = structured.groups[0]
  assert.equal(claudeGroup.rawCount, 8)
  assert.deepEqual(
    claudeGroup.entries.map((e) => e.id),
    ["claude-opus-4-6", "claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5", "claude-3-5-sonnet-20241022"],
  )
  assert.equal(claudeGroup.entries[0].isDefault, true)
  assert.deepEqual(
    claudeGroup.entries[1].snapshots,
    [
      { id: "claude-opus-4-5-20251101", date: "20251101" },
      { id: "claude-opus-4-5-20251015", date: "20251015" },
    ],
  )

  // Passthrough GLM snapshot collapsed under passthrough/glm-4.7, not mixed with direct models.
  const glmGroup = structured.groups.find((g) => g.family === "glm")
  assert.equal(glmGroup?.entries[0].id, "passthrough/glm-4.7")
  assert.deepEqual(glmGroup?.entries[0].snapshots, [{ id: "passthrough/glm-4.7-20251110", date: "20251110" }])

  // Search matches snapshot IDs, families, and source prefixes.
  const byDate = filterModelGroups(structured.groups, "20251101")
  assert.equal(byDate.groups.length, 1)
  assert.equal(byDate.groups[0].entries[0].id, "claude-opus-4-5")

  const byPassthrough = filterModelGroups(structured.groups, "passthrough")
  assert.deepEqual(byPassthrough.groups.map((g) => g.family), ["glm", "qwen"])
  assert.equal(byPassthrough.matchedRawCount, 5)
})

test("compareVersionsDesc treats missing minor as 0 and places versioned entries before unversioned", () => {
  assert.ok(compareVersionsDesc([5, 4], [5, 3]) < 0)
  assert.ok(compareVersionsDesc([4, 1], [4]) < 0)
  assert.equal(compareVersionsDesc([3, 0], [3]), 0)
  assert.ok(compareVersionsDesc([2], []) < 0)
})
