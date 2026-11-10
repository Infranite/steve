import assert from "node:assert/strict"
import { test } from "node:test"

const qrMod = await import("../src/qr.ts")
const hubMod = await import("../src/hub.ts")

function withMockStorage(fn) {
  const map = new Map()
  const prev = globalThis.localStorage
  globalThis.localStorage = {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, String(v)),
    removeItem: (k) => map.delete(k),
    clear: () => map.clear(),
  }
  try {
    return fn(map)
  } finally {
    globalThis.localStorage = prev
  }
}

test("encodeQr encodes short and long steve://pair URIs with intact finder patterns and SVG paths", () => {
  const shortQr = qrMod.encodeQr("steve://pair?v=1&code=stp_0123456789abcdef0123456789abcdef&lan=http%3A%2F%2F192.168.1.20%3A8440")
  assert.ok(shortQr.version >= 4 && shortQr.version <= 8)
  assert.equal(shortQr.size, shortQr.version * 4 + 17)
  assert.equal(shortQr.modules.length, shortQr.size * shortQr.size)
  // Top-left, top-right, bottom-left finder corners and centers are always dark
  const at = (r, c) => shortQr.modules[r * shortQr.size + c]
  for (const [top, left] of [[0, 0], [0, shortQr.size - 7], [shortQr.size - 7, 0]]) {
    assert.equal(at(top, left), true)
    assert.equal(at(top + 3, left + 3), true)
    assert.equal(at(top + 1, left + 1), false)
  }
  const svg = qrMod.qrSvgPath(shortQr, 2)
  assert.equal(svg.viewBox, `0 0 ${shortQr.size + 4} ${shortQr.size + 4}`)
  assert.match(svg.d, /^M\d+ \d+h\d+v1h-\d+z/)

  // Multi-endpoint payload with UTF-8 device name (version >= 7)
  const longUri =
    "steve://pair?v=1&code=stp_fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210" +
    "&name=" + encodeURIComponent("工作室 Mac Studio") +
    "&pub=" + encodeURIComponent("https://hub.example.com") +
    "&lan=" + encodeURIComponent("http://192.168.1.42:8440") +
    "&lan=" + encodeURIComponent("http://100.64.0.9:8440")
  const longQr = qrMod.encodeQr(longUri)
  assert.ok(longQr.version >= 7 && longQr.version <= 16)
  assert.equal(longQr.modules.length, longQr.size * longQr.size)
})

test("savedHubs remembers, deduplicates, orders by recency, and forgets Hub origins", () => {
  withMockStorage(() => {
    assert.deepEqual(hubMod.savedHubs(), [])
    hubMod.rememberHub("192.168.1.10:8440/", { name: "Office Mac", version: "0.3.0", now: 100 })
    hubMod.rememberHub("https://steve.example.com/", { name: "Cloud Hub", version: "0.3.1", now: 200 })
    assert.deepEqual(
      hubMod.savedHubs().map((h) => [h.url, h.name, h.version]),
      [
        ["https://steve.example.com", "Cloud Hub", "0.3.1"],
        ["http://192.168.1.10:8440", "Office Mac", "0.3.0"],
      ],
    )

    // Updating an existing Hub moves it to the top and preserves existing name if omitted
    hubMod.setHub("http://192.168.1.10:8440", { version: "0.3.2" })
    assert.equal(hubMod.hubBase(), "http://192.168.1.10:8440")
    assert.equal(hubMod.savedHubs()[0].url, "http://192.168.1.10:8440")
    assert.equal(hubMod.savedHubs()[0].name, "Office Mac")
    assert.equal(hubMod.savedHubs()[0].version, "0.3.2")

    hubMod.forgetHub("https://steve.example.com")
    assert.deepEqual(hubMod.savedHubs().map((h) => h.url), ["http://192.168.1.10:8440"])

    hubMod.setHub(null)
    assert.equal(hubMod.hubBase(), "")
  })
})

test("parsePairUri and preferredPairEndpoint prefer public network by default and fall back to LAN", () => {
  const pubUri =
    "steve://pair?v=1&code=stp_00112233445566778899aabbccddeeff" +
    "&name=Home%20Hub&pub=https%3A%2F%2Fsteve.example.com%2F&lan=http%3A%2F%2F192.168.1.25%3A8440&lan=http%3A%2F%2F10.0.0.8%3A8440"
  const parsedPub = hubMod.parsePairUri(pubUri)
  assert.deepEqual(parsedPub, {
    code: "stp_00112233445566778899aabbccddeeff",
    name: "Home Hub",
    publicUrl: "https://steve.example.com",
    lanUrls: ["http://192.168.1.25:8440", "http://10.0.0.8:8440"],
  })
  assert.deepEqual(hubMod.preferredPairEndpoint(parsedPub), {
    url: "https://steve.example.com",
    mode: "public",
  })

  const lanOnlyUri =
    "steve://pair?v=1&code=stp_00112233445566778899aabbccddeeff&lan=http%3A%2F%2F192.168.1.25%3A8440"
  const parsedLan = hubMod.parsePairUri(lanOnlyUri)
  assert.deepEqual(parsedLan, {
    code: "stp_00112233445566778899aabbccddeeff",
    lanUrls: ["http://192.168.1.25:8440"],
  })
  assert.deepEqual(hubMod.preferredPairEndpoint(parsedLan), {
    url: "http://192.168.1.25:8440",
    mode: "lan",
  })

  assert.equal(hubMod.parsePairUri("https://example.com/pair?code=stp_1234567890123456"), null)
  assert.equal(hubMod.parsePairUri("steve://pair?v=1&code=bad"), null)
})
