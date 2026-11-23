import assert from "node:assert/strict"
import { test } from "node:test"
import { unresolvedReplyMarker, unresolvedReplyPath, markdownImageSource, replyDeliveryUrl, replyFileReason, replyImageHint } from "../src/markdown-images.ts"

const hub = "https://hub.example.test"
test("Hub Markdown images use the current Hub and retain the exact source workspace/query/hash", () => {
  for (const path of ["/api/files/fil_sample?ws=source%2Fone&other=a%20b#preview", "/api/files/fil_sample", "/api/file-deliveries/d/content/a.png?ws=source", "/api/items/ST-1/artifacts/a/raw?path=a.png&ws=source"]) {
    assert.deepEqual(markdownImageSource(path, hub), { src: hub + path, authenticated: true })
  }
  const explicit = hub + "/api/files/fil_sample?ws=source&ws=second"
  assert.deepEqual(markdownImageSource(explicit, hub), { src: explicit, authenticated: true })
  assert.deepEqual(markdownImageSource("//hub.example.test/api/files/f?ws=source", hub), { src: "//hub.example.test/api/files/f?ws=source", authenticated: true })
})

test("external images retain their URL without opting into Hub credentialed CORS", () => {
  for (const ref of ["https://images.example.test/p.png", "https://other.example.test/api/files/f?ws=foreign", "//images.example.test/p.png"]) {
    assert.deepEqual(markdownImageSource(ref, hub), { src: ref, authenticated: false })
  }
  const bundle = hub + "/api/file-deliveries/d/content/docs/img.png?ws=source"
  assert.deepEqual(markdownImageSource(bundle, hub), { src: bundle, authenticated: true })
})

test("local paths and unsafe or unresolvable image sources never become browser requests", () => {
  for (const ref of ["", "/private/tmp/sample.png", "/tmp/sample.png", "/home/user/sample.png", "C:\\Temp\\sample.png", "C:/Temp/sample.png", "sample.png", "../sample.png", "file:///tmp/sample.png", "javascript:alert(1)", "vbscript:alert(1)", "data:image/svg+xml,<svg/>", "/api/../../private/file.png", "/api/\\file.png", "/api/files/\nfile"]) assert.equal(markdownImageSource(ref, hub), null, ref)
  assert.equal(markdownImageSource("/api/files/f", "invalid Hub"), null)
  assert.deepEqual(markdownImageSource("streamdown:incomplete-image", hub), { src: "streamdown:incomplete-image", authenticated: false })
})

test("unresolved reply paths never become browser resource requests", () => {
  for (const path of ["file:///workspace/my%20report.md", "/workspace/report.md", "./report.md", "reports/report.md"]) {
    assert.equal(unresolvedReplyPath(path, false), true, path)
    assert.equal(markdownImageSource(path, hub), null, path)
  }
  for (const path of ["https://example.test/report.md", "//cdn.example.test/a.png", "#/items/ST-1", "/api/file-deliveries/d/content/a.png?ws=original", "/deliveries/dlv_x?ws=original"]) {
    assert.equal(unresolvedReplyPath(path, false), false, path)
  }
  assert.equal(replyFileReason(unresolvedReplyMarker), "unresolved")
  assert.equal(replyFileReason("#steve-file-unavailable-scope"), "scope")
  assert.equal(replyFileReason(replyImageHint(unresolvedReplyMarker)), "unresolved")
  assert.equal(replyImageHint("/api/files/old"), "/api/files/old")
  assert.equal(replyFileReason("#something-else"), undefined)
})

test("captured reply delivery routes use the current Hub and original workspace", () => {
  const path = "/deliveries/dlv_1234567890abcdef1234567890abcdef?ws=other"
  assert.equal(replyDeliveryUrl(path, hub), hub + path)
  for (const ref of ["/Users/example/report.md", "//example.test" + path, path.replace("?ws=other", ""), path.replace("/dlv_", "/../dlv_")]) {
    assert.equal(replyDeliveryUrl(ref, hub), null)
  }
})
