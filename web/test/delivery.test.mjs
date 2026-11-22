import assert from "node:assert/strict"
import { test } from "node:test"
import { deliveryContent, deliveryForm, deliveryRoute, deliveryTextLang, deliveryWorkspace, loopbackUrl, outputDeliveryId, packageUrl, webUrl } from "../src/delivery.ts"
import { DeliveryHttpError, previewText, sharedDelivery } from "../src/delivery-data.ts"

test("stable paths identify deliveries and tokens without interpreting local outputs as links", () => {
  assert.deepEqual(deliveryRoute("/deliveries/d%20one"), { id: "d one", shared: false })
  assert.deepEqual(deliveryRoute("/shares/opaque-token/"), { id: "opaque-token", shared: true })
  for (const p of ["/deliveries", "/shares/a/b", "/deliveries/%zz", "/home/user/report.md"]) assert.equal(deliveryRoute(p), null)
  assert.equal(outputDeliveryId(["file", "report", "/workspace/report.md", "artifact-1"]), null)
  assert.equal(outputDeliveryId(["file", "report", null, "artifact-1", 1, "result", "delivery-1"]), "delivery-1")
  assert.equal(outputDeliveryId(["file", "report", "/deliveries/d%20one", "artifact-1"]), null)
  assert.equal(outputDeliveryId(["file", "report", "https://other.example/deliveries/foreign", "artifact-1"]), null)
  assert.equal(outputDeliveryId(["file", "report", "https://hub.example/deliveries/old", "artifact-1"]), null)
  assert.equal(outputDeliveryId(["file", "report", "https://hub.example/deliveries/new", "artifact-1", 1, "result", "new"]), "new")
})

test("standalone delivery ownership comes from metadata and must match the link's selector", () => {
  assert.equal(deliveryWorkspace({ workspace_id: "a" }, null), null)
  assert.equal(deliveryWorkspace({ workspace_id: "a" }, "a"), "a")
  assert.equal(deliveryWorkspace({ workspace_id: "a" }, "b"), null)
  assert.equal(deliveryWorkspace({ workspace_id: "a" }, ""), null)
  for (const workspace_id of [undefined, null, "", 42]) assert.equal(deliveryWorkspace({ workspace_id }, "a"), null)
})

test("bundle entrypoints retain authorization queries, nested paths and encoded filenames", () => {
  const d = { content_url: "/api/file-deliveries/a/content?ws=w", entrypoint: "pages/a b/首页.html" }
  assert.equal(deliveryContent(d, "https://hub.example"), "https://hub.example/api/file-deliveries/a/content/pages/a%20b/%E9%A6%96%E9%A1%B5.html?ws=w")
  assert.equal(deliveryContent({ ...d, content_url: "/api/delivery-shares/t/content/pages/index.html" }, "https://hub.example"), "https://hub.example/api/delivery-shares/t/content/pages/index.html")
  for (const entrypoint of ["../index.html", "/index.html", "a/./b.html", "a\\b.html", "a?x.html", "a#x.html"]) {
    assert.equal(deliveryContent({ ...d, entrypoint }, "https://hub.example"), null)
  }
})

test("URL fields reject executable schemes and credentials, and recognize loopback spellings", () => {
  for (const v of ["javascript:alert(1)", "data:text/html,x", "file:///report.pdf", "//foreign.example/a", "https://user:secret@hub.example/a", "workspace/report.pdf"]) assert.equal(webUrl(v, "https://hub.example"), null)
  for (const v of ["http://localhost/a", "http://dev.localhost/a", "http://127.2.3.4/a", "http://[::1]/a", "http://[::ffff:127.0.0.1]/a", "http://0.0.0.0/a", "http://2130706433/a"]) assert.equal(loopbackUrl(v), true, v)
  for (const v of ["https://hub.example", "http://192.168.1.2", "https://localhost.example"]) assert.equal(loopbackUrl(v), false, v)
})

test("HTML bundles take priority over their ZIP mime, while Office and standard media reuse their viewers", () => {
  for (const [name, mime, entrypoint, form] of [
    ["design.zip", "application/zip", "pages/index.html", "html"], ["report.md", "application/octet-stream", null, "text"],
    ["report", "text/markdown; charset=utf-8", null, "text"], ["photo", "image/png", null, "image"],
    ["paper.PDF", "application/octet-stream", null, "pdf"], ["report.docx", "application/octet-stream", null, "office"],
    ["source.zip", "application/zip", null, "binary"],
  ]) assert.equal(deliveryForm({ name, mime, entrypoint }), form)
})

test("preview reads bound unknown-length streams, preserve UTF-8 across chunks and reject errors", async () => {
  const bytes = new TextEncoder().encode("a中b")
  const r = new Response(new ReadableStream({ start(c) { c.enqueue(bytes.subarray(0, 2)); c.enqueue(bytes.subarray(2)); c.close() } }))
  assert.deepEqual(await previewText(r), { text: "a中b", truncated: false })
  assert.deepEqual(await previewText(new Response("abcdef"), 3), { text: "abc", truncated: true })
  assert.deepEqual(await previewText(new Response("abc"), 3), { text: "abc", truncated: false })
  await assert.rejects(() => previewText(new Response("denied", { status: 410 })), (e) => e instanceof DeliveryHttpError && e.status === 410)
})

test("public metadata requests use only the token route without ambient credentials or workspace", async (ctx) => {
  const calls = []
  ctx.mock.method(globalThis, "fetch", async (...args) => { calls.push(args); return Response.json({ id: "d", content_url: "/api/delivery-shares/a/content" }) })
  await sharedDelivery("a/b", new AbortController().signal)
  assert.equal(calls[0][0], "/api/delivery-shares/a%2Fb")
  assert.equal(calls[0][1].credentials, "omit")
  assert.equal(calls[0][1].cache, "no-store")
  assert.equal(calls[0][1].headers, undefined)
  globalThis.fetch = async () => Response.json({ error: "expired" }, { status: 410 })
  await assert.rejects(() => sharedDelivery("expired", new AbortController().signal), (e) => e.status === 410)
})

test("packages preview as their entry, media get players, and Markdown/code are told by extension", () => {
  for (const [name, mime, entrypoint, form] of [
    ["docs.zip", "application/zip", "README.md", "text"], ["paper.zip", "application/zip", "a/paper.pdf", "pdf"],
    ["clip.mp4", "video/mp4", null, "video"], ["clip.MOV", "application/octet-stream", null, "video"],
    ["voice.m4a", "audio/mp4", null, "audio"], ["song.flac", "application/octet-stream", null, "audio"],
    ["DESIGN.md", "text/plain", null, "text"], ["main.rs", "application/octet-stream", null, "text"],
    ["shots.zip", "application/zip", "img/a.png", "image"],
  ]) assert.equal(deliveryForm({ name, mime, entrypoint }), form, name)
  assert.equal(deliveryTextLang({ name: "DESIGN.md", mime: "text/plain" }), "markdown")
  assert.equal(deliveryTextLang({ name: "docs.zip", mime: "application/zip", entrypoint: "guide/intro.markdown" }), "markdown")
  assert.equal(deliveryTextLang({ name: "app.tsx", mime: "text/plain" }), "tsx")
  assert.equal(deliveryTextLang({ name: "notes.txt", mime: "text/plain" }), "text")
})

test("Markdown images resolve inside the package only and keep the workspace selector", () => {
  const entry = "https://hub.example/api/file-deliveries/d/content/docs/guide/README.md?ws=w"
  assert.equal(packageUrl("img/a.png", entry, "docs/guide/README.md"), "https://hub.example/api/file-deliveries/d/content/docs/guide/img/a.png?ws=w")
  assert.equal(packageUrl("../../top.png", entry, "docs/guide/README.md"), "https://hub.example/api/file-deliveries/d/content/top.png?ws=w")
  for (const ref of ["../../../raw", "../../../../../state", "/api/state", "//evil.example/x.png", "https://evil.example/x.png", "javascript:alert(1)", "data:image/png;base64,AA", "#top", ""]) {
    assert.equal(packageUrl(ref, entry, "docs/guide/README.md"), null, ref)
  }
  const shared = "https://hub.example/api/delivery-shares/t/content/README.md"
  assert.equal(packageUrl("a b.png", shared, "README.md"), "https://hub.example/api/delivery-shares/t/content/a%20b.png")
})
