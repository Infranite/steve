import assert from "node:assert/strict"
import { test } from "node:test"

const qrMod = await import("../src/qr.ts")
const hubMod = await import("../src/hub.ts")

function withMockStorage(fn) {
  const map = new Map()
  const desc = Object.getOwnPropertyDescriptor(globalThis, "localStorage")
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    writable: true,
    value: {
      getItem: (k) => (map.has(k) ? map.get(k) : null),
      setItem: (k, v) => map.set(k, String(v)),
      removeItem: (k) => map.delete(k),
      clear: () => map.clear(),
    },
  })
  try {
    return fn(map)
  } finally {
    if (desc) Object.defineProperty(globalThis, "localStorage", desc)
    else delete globalThis.localStorage
  }
}

/** Independent ISO/IEC 18004 reader (versions 1–16, ECC M) to verify `encodeQr` round-trips real text and valid RS syndromes. */
function decodeQrMatrix(qr) {
  const { version, size, modules } = qr
  const at = (r, c) => modules[r * size + c]

  // Read top-left format info bits (unmasked with 0x5412)
  let fmt = 0
  const readFmtBit = (r, c) => (at(r, c) ? 1 : 0)
  for (let i = 0; i <= 5; i++) fmt = (fmt << 1) | readFmtBit(8, i)
  fmt = (fmt << 1) | readFmtBit(8, 7)
  fmt = (fmt << 1) | readFmtBit(8, 8)
  fmt = (fmt << 1) | readFmtBit(7, 8)
  for (let i = 9; i < 15; i++) fmt = (fmt << 1) | readFmtBit(14 - i, 8)
  const unmaskedFmt = fmt ^ 0x5412
  const eccBits = (unmaskedFmt >>> 13) & 0b11
  assert.equal(eccBits, 0b00, "ECC level M must be 00")
  const mask = (unmaskedFmt >>> 10) & 0b111

  // Verify second copy of format bits matches
  let fmt2 = 0
  for (let i = 14; i >= 8; i--) fmt2 = (fmt2 << 1) | readFmtBit(size - 15 + i, 8)
  for (let i = 7; i >= 0; i--) fmt2 = (fmt2 << 1) | readFmtBit(8, size - 1 - i)
  assert.equal(fmt2, fmt, "both format info copies must match")

  // Reconstruct function module map
  const isFunc = new Array(size * size).fill(false)
  const mark = (r, c) => {
    if (r >= 0 && r < size && c >= 0 && c < size) isFunc[r * size + c] = true
  }
  for (const [top, left] of [[0, 0], [0, size - 7], [size - 7, 0]]) {
    for (let dy = -1; dy <= 7; dy++) for (let dx = -1; dx <= 7; dx++) mark(top + dy, left + dx)
  }
  for (let i = 8; i < size - 8; i++) {
    mark(6, i)
    mark(i, 6)
  }
  const alignTable = [
    [], [], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34], [6, 22, 38], [6, 24, 42],
    [6, 26, 46], [6, 28, 50], [6, 30, 54], [6, 32, 58], [6, 34, 62], [6, 26, 46, 66], [6, 26, 48, 70], [6, 26, 50, 74],
  ]
  const centers = alignTable[version]
  for (let i = 0; i < centers.length; i++) {
    for (let j = 0; j < centers.length; j++) {
      if ((i === 0 && j === 0) || (i === 0 && j === centers.length - 1) || (i === centers.length - 1 && j === 0)) continue
      for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) mark(centers[i] + dy, centers[j] + dx)
    }
  }
  for (let i = 0; i <= 8; i++) { mark(8, i); mark(i, 8) }
  for (let i = 0; i < 8; i++) { mark(8, size - 1 - i); mark(size - 1 - i, 8) }
  mark(size - 8, 8)
  if (version >= 7) {
    for (let i = 0; i < 18; i++) {
      const a = size - 11 + (i % 3)
      const b = Math.floor(i / 3)
      mark(b, a)
      mark(a, b)
    }
  }

  const maskFn = (r, c) => {
    switch (mask) {
      case 0: return (r + c) % 2 === 0
      case 1: return r % 2 === 0
      case 2: return c % 3 === 0
      case 3: return (r + c) % 3 === 0
      case 4: return (Math.floor(r / 2) + Math.floor(c / 3)) % 2 === 0
      case 5: return ((r * c) % 2) + ((r * c) % 3) === 0
      case 6: return (((r * c) % 2) + ((r * c) % 3)) % 2 === 0
      default: return (((r + c) % 2) + ((r * c) % 3)) % 2 === 0
    }
  }

  // Extract raw bit stream in zig-zag order and unmask
  const rawBits = []
  let upward = true
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5
    for (let vert = 0; vert < size; vert++) {
      const r = upward ? size - 1 - vert : vert
      for (let j = 0; j < 2; j++) {
        const c = right - j
        if (isFunc[r * size + c]) continue
        const bit = at(r, c) !== maskFn(r, c) ? 1 : 0
        rawBits.push(bit)
      }
    }
    upward = !upward
  }

  const eccSpec = [
    [0, 0, 0, 0, 0, 0],
    [26, 10, 1, 16, 0, 0], [44, 16, 1, 28, 0, 0], [70, 26, 1, 44, 0, 0], [100, 18, 2, 32, 0, 0],
    [134, 24, 2, 43, 0, 0], [172, 16, 4, 27, 0, 0], [196, 18, 4, 31, 0, 0], [242, 22, 2, 38, 2, 39],
    [292, 22, 3, 36, 2, 37], [346, 26, 4, 43, 1, 44], [404, 30, 1, 50, 4, 51], [466, 22, 6, 36, 2, 37],
    [532, 22, 8, 37, 1, 38], [581, 24, 4, 40, 5, 41], [655, 24, 5, 41, 5, 42], [733, 28, 7, 45, 3, 46],
  ][version]
  const [totalCw, ecPerBlock, g1, d1, g2, d2] = eccSpec
  const codewords = new Uint8Array(totalCw)
  for (let i = 0; i < totalCw; i++) {
    let b = 0
    for (let j = 0; j < 8; j++) b = (b << 1) | rawBits[i * 8 + j]
    codewords[i] = b
  }

  // De-interleave blocks and verify Reed-Solomon syndromes over GF(256)
  const blocks = Array.from({ length: g1 + g2 }, (_, idx) => new Uint8Array((idx < g1 ? d1 : d2) + ecPerBlock))
  let pos = 0
  const maxData = Math.max(d1, d2)
  for (let col = 0; col < maxData; col++) {
    for (let b = 0; b < blocks.length; b++) {
      const dataLen = b < g1 ? d1 : d2
      if (col < dataLen) blocks[b][col] = codewords[pos++]
    }
  }
  for (let col = 0; col < ecPerBlock; col++) {
    for (let b = 0; b < blocks.length; b++) {
      const dataLen = b < g1 ? d1 : d2
      blocks[b][dataLen + col] = codewords[pos++]
    }
  }

  const gfExp = new Uint8Array(512)
  const gfLog = new Uint8Array(256)
  for (let i = 0, x = 1; i < 255; i++) {
    gfExp[i] = x
    gfLog[x] = i
    x = (x << 1) ^ (x & 0x80 ? 0x11d : 0)
  }
  for (let i = 255; i < 512; i++) gfExp[i] = gfExp[i - 255]
  const gfMul = (a, b) => (a === 0 || b === 0 ? 0 : gfExp[gfLog[a] + gfLog[b]])

  const dataBytes = []
  for (let b = 0; b < blocks.length; b++) {
    const blk = blocks[b]
    for (let s = 0; s < ecPerBlock; s++) {
      let syn = 0
      const root = gfExp[s]
      for (const byte of blk) syn = gfMul(syn, root) ^ byte
      assert.equal(syn, 0, `Reed-Solomon syndrome ${s} of block ${b} must be zero`)
    }
    const dataLen = b < g1 ? d1 : d2
    for (let i = 0; i < dataLen; i++) dataBytes.push(blk[i])
  }

  // Parse byte-mode segment
  const dataBits = []
  for (const byte of dataBytes) for (let j = 7; j >= 0; j--) dataBits.push((byte >>> j) & 1)
  const readBits = (offset, len) => {
    let v = 0
    for (let i = 0; i < len; i++) v = (v << 1) | dataBits[offset + i]
    return v
  }
  assert.equal(readBits(0, 4), 0b0100, "byte mode indicator")
  const lenBits = version <= 9 ? 8 : 16
  const byteCount = readBits(4, lenBits)
  const out = new Uint8Array(byteCount)
  for (let i = 0; i < byteCount; i++) out[i] = readBits(4 + lenBits + i * 8, 8)
  return new TextDecoder().decode(out)
}

test("encodeQr round-trips a pairing link as a phone's camera reads it, short and with several addresses and a UTF-8 name", () => {
  const short = "http://192.168.1.20:8440/#/pair?v=1&code=stp_0123456789abcdef0123456789abcdef&name=192.168.1.20&lan=http%3A%2F%2F192.168.1.20%3A8440"
  const shortQr = qrMod.encodeQr(short)
  assert.ok(shortQr.version >= 4 && shortQr.version <= 9)
  assert.equal(shortQr.size, shortQr.version * 4 + 17)
  assert.equal(shortQr.modules.length, shortQr.size * shortQr.size)
  assert.equal(decodeQrMatrix(shortQr), short)

  const svg = qrMod.qrSvgPath(shortQr, 2)
  assert.equal(svg.viewBox, `0 0 ${shortQr.size + 4} ${shortQr.size + 4}`)
  assert.match(svg.d, /^M\d+ \d+h\d+v1h-\d+z/)

  // several addresses and a UTF-8 name (version >= 7: two groups of blocks, version information)
  const long =
    "https://hub.example.com/#/pair?" +
    hubMod.pairQuery({ code: "stp_fedcba9876543210fedcba9876543210", name: "工作室 Mac Studio", publicUrl: "https://hub.example.com", lanUrls: ["http://192.168.1.42:8440", "http://100.64.0.9:8440"] })
  const longQr = qrMod.encodeQr(long)
  assert.ok(longQr.version >= 7 && longQr.version <= 16)
  assert.equal(decodeQrMatrix(longQr), long)
})

test("savedHubs remembers, deduplicates, orders by recency, keeps the 20 most recent, and forgets", () => {
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

    hubMod.setHub("http://192.168.1.10:8440", { version: "0.3.2" })
    assert.equal(hubMod.hubBase(), "http://192.168.1.10:8440")
    assert.equal(hubMod.currentHub(), "http://192.168.1.10:8440")
    assert.equal(hubMod.savedHubs()[0].url, "http://192.168.1.10:8440")
    assert.equal(hubMod.savedHubs()[0].name, "Office Mac")
    assert.equal(hubMod.savedHubs()[0].version, "0.3.2")

    hubMod.forgetHub("https://steve.example.com")
    assert.deepEqual(hubMod.savedHubs().map((h) => h.url), ["http://192.168.1.10:8440"])

    hubMod.setHub(null)
    assert.equal(hubMod.hubBase(), "")
  })
  assert.equal(hubMod.normalize(" 192.168.1.10:8440/ "), "http://192.168.1.10:8440")
  assert.equal(hubMod.normalize("HTTPS://Steve.Example.com/x?y#z"), "https://steve.example.com")
  for (const bad of ["", "ftp://192.168.1.10", "steve://pair?code=x", "javascript:alert(1)", "http://"]) assert.equal(hubMod.normalize(bad), null, bad)
  withMockStorage(() => {
    /* an old one remembered late does not push a newer one out */
    for (let n = 1; n <= 20; n++) hubMod.rememberHub(`http://10.0.0.${n}:8440`, { now: 1000 + n })
    hubMod.rememberHub("http://10.0.9.9:8440", { now: 5 })
    const urls = hubMod.savedHubs().map((h) => h.url)
    assert.equal(urls.length, 20)
    assert.equal(urls[0], "http://10.0.0.20:8440")
    assert.ok(!urls.includes("http://10.0.9.9:8440"))
  })
})

test("reach: a page calls its own Hub, the same host on another port and any https Hub; it goes to the page of the rest", () => {
  const lan = "http://192.168.1.5:8440"
  assert.equal(hubMod.reach(lan, lan), "call")
  assert.equal(hubMod.reach("http://192.168.1.5:8440", "http://192.168.1.5:8441"), "call")
  assert.equal(hubMod.reach("http://127.0.0.1:18540", "http://127.0.0.1:8440"), "call")
  assert.equal(hubMod.reach("https://steve.example.com", lan), "call")
  assert.equal(hubMod.reach("https://steve.example.com", "https://other.example.com"), "call")
  /* a cookie from plain http on another host does not stick */
  assert.equal(hubMod.reach("http://192.168.1.10:8440", lan), "open")
  /* an https page may not call plain http, on its own host either */
  assert.equal(hubMod.reach("http://192.168.1.10:8440", "https://steve.example.com"), "open")
  assert.equal(hubMod.reach("http://steve.example.com:8440", "https://steve.example.com"), "open")
  assert.equal(hubMod.reach("not a url", lan), "open")
})

test("openUrl carries the list in the fragment and parseCarried reads only what openUrl writes", () => {
  assert.equal(hubMod.openUrl("http://192.168.1.10:8440"), "http://192.168.1.10:8440/#/")
  const url = hubMod.openUrl("http://192.168.1.10:8440", [{ url: "http://192.168.1.5:8440", name: "书房 Mac" }, { url: "https://steve.example.com" }])
  const u = new URL(url)
  assert.equal(u.origin + u.pathname, "http://192.168.1.10:8440/")
  assert.equal(u.search, "", "nothing of the list is in what a server is sent")
  const arrival = hubMod.readArrival(u.hash)
  assert.deepEqual(arrival, { hubs: [{ url: "http://192.168.1.5:8440", name: "书房 Mac" }, { url: "https://steve.example.com" }], rest: "#/" })

  assert.deepEqual(hubMod.parseCarried("nonsense"), [])
  assert.deepEqual(hubMod.parseCarried('{"a":1}'), [])
  assert.deepEqual(hubMod.parseCarried('[["javascript:alert(1)"],["ftp://x"],[42],"http://a",["http://a:1/path?q#f","  n  "],["http://a:1"]]'), [{ url: "http://a:1", name: "n" }])
  assert.equal(hubMod.parseCarried(JSON.stringify(Array.from({ length: 50 }, (_, n) => [`http://10.0.0.${n}:8440`]))).length, 20)
})

test("parsePairLink reads the link a browser opens and the one the app opens, and pairQuery writes what it reads", () => {
  const query =
    "v=1&code=stp_00112233445566778899aabbccddeeff" +
    "&name=Home%20Hub&pub=https%3A%2F%2Fsteve.example.com%2F&lan=http%3A%2F%2F192.168.1.25%3A8440,http%3A%2F%2F10.0.0.8%3A8440"
  const want = {
    code: "stp_00112233445566778899aabbccddeeff",
    name: "Home Hub",
    publicUrl: "https://steve.example.com",
    lanUrls: ["http://192.168.1.25:8440", "http://10.0.0.8:8440"],
  }
  assert.deepEqual(hubMod.parsePairLink("steve://pair?" + query), want)
  assert.deepEqual(hubMod.parsePairLink("https://steve.example.com/#/pair?" + query), want)
  /* the link's own host is where the page is, not an address of the Hub */
  assert.deepEqual(hubMod.parsePairLink("  http://192.168.1.25:8441/#/pair?" + query + "\n"), want)
  /* `lan` given once per address is read too */
  assert.deepEqual(hubMod.parsePairLink("steve://pair?v=1&code=stp_00112233445566778899aabbccddeeff&lan=http%3A%2F%2F192.168.1.25%3A8440&lan=http%3A%2F%2F10.0.0.8%3A8440").lanUrls, want.lanUrls)
  assert.deepEqual(hubMod.pairAddresses(want), ["https://steve.example.com", "http://192.168.1.25:8440", "http://10.0.0.8:8440"])

  const named = { code: "stp_00112233445566778899aabbccddeeff", name: "工作室 Mac & Studio", lanUrls: ["http://192.168.1.25:8440"] }
  assert.ok(!hubMod.pairQuery(named).includes("+"), "a space is %20: not every reader of a query takes + for one")
  assert.deepEqual(hubMod.parsePairLink("steve://pair?" + hubMod.pairQuery(named)), named)
  assert.deepEqual(hubMod.parsePairLink(hubMod.appPairLink(want)), want)

  assert.equal(hubMod.parsePairLink("https://example.com/pair?code=stp_1234567890123456"), null, "not in the fragment")
  assert.equal(hubMod.parsePairLink("steve://pair?v=1&code=bad"), null)
  assert.equal(hubMod.parsePairLink("steve://pair?v=1&code=stp_short"), null)
  assert.equal(hubMod.parsePairLink("http://192.168.1.25:8440"), null, "an address is not a pairing link")
  assert.equal(hubMod.parsePairLink("https://steve.example.com/#/items/pair?code=stp_00112233445566778899aabbccddeeff"), null)
})

test("pairPlan: a page claims the code only at its own host or its own Hub, and opens any other Hub's link there", () => {
  const code = "stp_00112233445566778899aabbccddeeff"
  const both = { code, publicUrl: "https://steve.example.com", lanUrls: ["http://192.168.1.25:8440", "http://10.0.0.8:8440"] }
  /* the phone opened the LAN link: the LAN address on this host answers it */
  assert.deepEqual(hubMod.pairPlan(both, "http://192.168.1.25:8441", ""), { call: ["http://192.168.1.25:8440"], open: null })
  assert.deepEqual(hubMod.pairPlan(both, "https://steve.example.com", ""), { call: ["https://steve.example.com"], open: null })
  /* pasted into another Hub's page: that Hub's own page takes it, even where this page could call it */
  const there = hubMod.pairPlan(both, "http://192.168.1.99:8440", "http://192.168.1.99:8440")
  assert.deepEqual(there.call, [])
  assert.equal(there.open, "https://steve.example.com/#/pair?" + hubMod.pairQuery(both))
  /* a page elsewhere that already talks to this Hub claims at it */
  assert.deepEqual(hubMod.pairPlan(both, "https://app.example.com", "https://steve.example.com"), { call: ["https://steve.example.com"], open: null })
  const lan = { code, name: "Home Hub", lanUrls: ["http://192.168.1.25:8440"] }
  const plan = hubMod.pairPlan(lan, "http://192.168.1.99:8440", "")
  assert.deepEqual(plan.call, [])
  assert.equal(plan.open, "http://192.168.1.25:8440/#/pair?" + hubMod.pairQuery(lan))
  assert.deepEqual(hubMod.parsePairLink(plan.open), lan)
  assert.deepEqual(hubMod.pairPlan({ code, lanUrls: [] }, "http://192.168.1.99:8440", ""), { call: [], open: null })
  /* an https page cannot call its own host over plain http: the link goes to that page */
  assert.deepEqual(hubMod.pairPlan(lan, "https://192.168.1.25", "").call, [])

  /* a made-up link that names this page's host beside another Hub: the code is never claimed at the other one */
  const forged = { code, publicUrl: "https://evil.example", lanUrls: ["http://hub.example:1"] }
  assert.deepEqual(hubMod.pairPlan(forged, "http://hub.example:8441", "").call, ["http://hub.example:1"])
  const onTls = hubMod.pairPlan(forged, "https://hub.example", "")
  assert.deepEqual(onTls.call, [])
  assert.ok(onTls.open.startsWith("https://evil.example/#/pair?"), "opened there, under its own address, with a warning first")
})

test("readArrival takes a pairing or a carried list out of the fragment and leaves every other route alone", () => {
  const query = "v=1&code=stp_00112233445566778899aabbccddeeff&lan=http%3A%2F%2F192.168.1.25%3A8440"
  const href = "http://192.168.1.25:8441/#/pair?" + query
  assert.deepEqual(hubMod.readArrival("#/pair?" + query, href), {
    pair: { link: { code: "stp_00112233445566778899aabbccddeeff", lanUrls: ["http://192.168.1.25:8440"] }, text: href },
    rest: "#/",
  })
  /* said "pair" and could not be read: still taken out of the address, and told as such */
  assert.deepEqual(hubMod.readArrival("#/pair?code=nope", "http://h/#/pair?code=nope"), { pair: { link: null, text: "http://h/#/pair?code=nope" }, rest: "#/" })
  assert.deepEqual(hubMod.readArrival("#/pair"), { pair: { link: null, text: "steve://pair" }, rest: "#/" })
  assert.deepEqual(hubMod.readArrival("#/?hubs=%5B%5B%22http%3A%2F%2Fa%3A1%22%5D%5D"), { hubs: [{ url: "http://a:1" }], rest: "#/" })
  assert.deepEqual(hubMod.readArrival("#/?hubs=garbage"), { hubs: [], rest: "#/" })
  for (const h of ["", "#/", "#/items/ST-1", "#/pairing", "#/settings/access", "#/?x=1", "#/hub/pair"]) assert.equal(hubMod.readArrival(h), null, h)
})

test("browserEntries lists the saved Hubs with the one in use among them, in a steady order", () => {
  withMockStorage(() => {
    assert.deepEqual(hubMod.browserEntries(), [])
    hubMod.rememberHub("https://steve.example.com", { name: "Cloud", version: "0.3.1", now: 300 })
    hubMod.rememberHub("http://192.168.1.10:8440", { now: 100 })
    hubMod.setHub("http://192.168.1.10:8440")
    assert.deepEqual(hubMod.browserEntries(), [
      { id: "http://192.168.1.10:8440", name: "192.168.1.10:8440", address: "http://192.168.1.10:8440", current: true, removable: false },
      { id: "https://steve.example.com", name: "Cloud", address: "https://steve.example.com", current: false, version: "0.3.1", removable: true },
    ])
    /* the Hub in use is listed before it was ever saved */
    hubMod.forgetHub("http://192.168.1.10:8440")
    assert.deepEqual(hubMod.browserEntries().map((h) => [h.address, h.current]), [["http://192.168.1.10:8440", true], ["https://steve.example.com", false]])
  })
})

test("the Hubs another page's list named are offered once, and added only on a yes", () => {
  withMockStorage(() => {
    const session = new Map()
    const desc = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage")
    Object.defineProperty(globalThis, "sessionStorage", { configurable: true, writable: true, value: { getItem: (k) => (session.has(k) ? session.get(k) : null), setItem: (k, v) => session.set(k, String(v)), removeItem: (k) => session.delete(k) } })
    try {
      const carried = JSON.stringify([["http://192.168.1.10:8440", "Home"], ["https://steve.example.com", "Cloud"], ["http://10.0.0.2:8440"]])
      hubMod.setHub("http://192.168.1.10:8440")
      /* a no leaves the list as it was */
      session.set("steve.hubs.offer", carried)
      /* everything a yes would change is in the offer: the two unknown Hubs, and the name for the one in use */
      assert.deepEqual(hubMod.hubsOffer().map((h) => h.url), ["http://192.168.1.10:8440", "https://steve.example.com", "http://10.0.0.2:8440"])
      hubMod.answerOffer(false)
      assert.deepEqual(hubMod.hubsOffer(), [])
      assert.deepEqual(hubMod.savedHubs().map((h) => h.url), ["http://192.168.1.10:8440"])
      /* a yes adds the unknown ones behind the Hub in use, which takes the name the other page had for it */
      session.set("steve.hubs.offer", carried)
      hubMod.answerOffer(true)
      assert.deepEqual(hubMod.savedHubs().map((h) => [h.url, h.name]), [["http://192.168.1.10:8440", "Home"], ["https://steve.example.com", "Cloud"], ["http://10.0.0.2:8440", undefined]])
      assert.equal(session.has("steve.hubs.offer"), false)
      /* a name the person gave stays, and a list that would change nothing is no offer */
      hubMod.rememberHub("https://steve.example.com", { name: "Mine" })
      session.set("steve.hubs.offer", carried)
      assert.deepEqual(hubMod.hubsOffer(), [])
      hubMod.answerOffer(true)
      assert.equal(hubMod.savedHubs().find((h) => h.url === "https://steve.example.com").name, "Mine")
    } finally {
      if (desc) Object.defineProperty(globalThis, "sessionStorage", desc)
      else delete globalThis.sessionStorage
    }
  })
})
