import assert from "node:assert/strict"
import { test } from "node:test"

const entries = new Map()
Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
  getItem: (k) => entries.get(k) ?? null,
  setItem: (k, v) => entries.set(k, v),
  removeItem: (k) => entries.delete(k),
} })
const { readDraft, saveDraft, clearSentDraft, draftKey, draftToSend } = await import("../src/composer-draft.ts")
const draft = { text: "尚未发送 @开发", files: [{ id: "file-1", name: "图.png", mime: "image/png", size: 123 }], mentions: [["@开发", { kind: "bot", id: "dev" }]] }

test("drafts survive reload with attachments and picked mentions, isolated by workspace and destination", async () => {
  const key = draftKey("personal", "chat")
  saveDraft(key, draft)
  const reloaded = await import("../src/composer-draft.ts?reload")
  assert.deepEqual(reloaded.readDraft(key), draft)
  assert.equal(readDraft(draftKey("work", "chat")).text, "")
  assert.equal(readDraft(draftKey("personal", "session", "one")).text, "")
  assert.equal(readDraft(draftKey("personal", "item", "one")).text, "")
})

test("only successful sends clear matching drafts; a previous send cannot clear a newer draft", () => {
  const key = draftKey("personal", "session", "one")
  saveDraft(key, draft)
  assert.deepEqual(readDraft(key), draft) // a failed send performs no clearing
  const next = { ...draft, text: "新的补充" }
  saveDraft(key, next)
  clearSentDraft(key, draft)
  assert.deepEqual(readDraft(key), next)
  clearSentDraft(key, next)
  assert.equal(readDraft(key).text, "")
  assert.equal(entries.has("steve:composer:v1:" + key), false)
})

test("unavailable storage still preserves drafts across navigation", () => {
  const previous = globalThis.localStorage
  Object.defineProperty(globalThis, "localStorage", { configurable: true, get: () => { throw new Error("denied") } })
  const key = draftKey("personal", "item", "ST-21")
  assert.doesNotThrow(() => saveDraft(key, draft))
  assert.deepEqual(readDraft(key), draft)
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: previous })
})

test("malformed persisted drafts are ignored and malformed entries are filtered", async () => {
  const key = draftKey("corrupt", "chat")
  entries.set("steve:composer:v1:" + key, "{broken")
  const reloaded = await import("../src/composer-draft.ts?corrupt")
  assert.equal(reloaded.readDraft(key).text, "")
  entries.set("steve:composer:v1:" + key, JSON.stringify({ ...draft, files: [null, ...draft.files], mentions: [null, ["@坏", null], ...draft.mentions] }))
  assert.deepEqual(reloaded.readDraft(key), draft)
})

test("a draft keeps one send id across retries and reloads; editing it makes a new one", async () => {
  const key = draftKey("personal", "chat", "idem")
  const first = draftToSend(key, draft)
  assert.ok(first.clientId)
  // the composer autosaves without the id (typing, a reload's first render): an unchanged draft keeps it
  saveDraft(key, { ...draft })
  assert.equal(readDraft(key).clientId, first.clientId)
  const reloaded = await import("../src/composer-draft.ts?idem")
  assert.equal(reloaded.readDraft(key).clientId, first.clientId)
  assert.equal(draftToSend(key, { ...draft }).clientId, first.clientId)
  // a changed text or attachment is another message
  saveDraft(key, { ...draft, text: draft.text + "！" })
  assert.equal(readDraft(key).clientId, undefined)
  const edited = draftToSend(key, { ...draft, text: draft.text + "！" })
  assert.notEqual(edited.clientId, first.clientId)
  assert.notEqual(draftToSend(key, { ...draft, text: draft.text + "！", files: [] }).clientId, edited.clientId)
  // a successful send clears it, and the same words typed again later are a new send
  const sent = draftToSend(key, draft)
  assert.ok(clearSentDraft(key, sent))
  assert.equal(readDraft(key).clientId, undefined)
  assert.notEqual(draftToSend(key, draft).clientId, sent.clientId)
  // without a draft key every send is new
  assert.notEqual(draftToSend(undefined, draft).clientId, draftToSend(undefined, draft).clientId)
})
