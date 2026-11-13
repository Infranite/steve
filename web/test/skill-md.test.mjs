import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/skill-md.ts")

test("front matter gives the name and description, the rest is the body", () => {
  const md = "---\nname: pdf-tools\ndescription: \"Fill PDF forms: use when a PDF is mentioned.\"\nlicense: MIT\n---\n\n# PDF\n\nSteps.\n"
  assert.deepEqual(m.parseSkillMd(md), { name: "pdf-tools", description: "Fill PDF forms: use when a PDF is mentioned.", content: "# PDF\n\nSteps." })
})

test("folded and single-quoted values, CRLF and a BOM", () => {
  const md = "\uFEFF---\r\nname: 'review'\r\ndescription: >\r\n  Review a change\r\n  before merging.\r\n---\r\nBody"
  assert.deepEqual(m.parseSkillMd(md), { name: "review", description: "Review a change before merging.", content: "Body" })
})

test("without front matter the whole text is the body", () => {
  assert.deepEqual(m.parseSkillMd("  # Just text\n"), { name: "", description: "", content: "# Just text" })
})

test("names", () => {
  assert.equal(m.skillNameOk("code-review"), true)
  for (const bad of ["Code", "a--b", "-a", "a-", "", "x".repeat(65), "a b"]) assert.equal(m.skillNameOk(bad), false, bad)
  assert.equal(m.skillName("Code Review!"), "code-review")
  assert.equal(m.skillName("--PDF_tools--"), "pdf-tools")
})
