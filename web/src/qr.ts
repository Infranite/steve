/* Pure ISO/IEC 18004 QR Code encoder (byte mode, ECC level M, versions 1–16) and SVG path builder.
 * Zero runtime dependencies: renders pairing URIs (`steve://pair?…`) crisply at any size or theme. */

export type QrMatrix = {
  version: number
  size: number
  /** row-major boolean modules (`true` = dark) */
  modules: boolean[]
}

/* [totalCodewords, ecCodewordsPerBlock, group1Blocks, group1DataCodewords, group2Blocks, group2DataCodewords] for ECC M, versions 1..16 */
const ECC_M: [number, number, number, number, number, number][] = [
  [0, 0, 0, 0, 0, 0],
  [26, 10, 1, 16, 0, 0],
  [44, 16, 1, 28, 0, 0],
  [70, 26, 1, 44, 0, 0],
  [100, 18, 2, 32, 0, 0],
  [134, 24, 2, 43, 0, 0],
  [172, 16, 4, 27, 0, 0],
  [196, 18, 4, 31, 0, 0],
  [242, 22, 2, 38, 2, 39],
  [292, 22, 3, 36, 2, 37],
  [346, 26, 4, 43, 1, 44],
  [404, 30, 1, 50, 4, 51],
  [466, 22, 6, 36, 2, 37],
  [532, 22, 8, 37, 1, 38],
  [581, 24, 4, 40, 5, 41],
  [655, 24, 5, 41, 5, 42],
  [733, 28, 7, 45, 3, 46],
]

/* Alignment pattern center coordinates for versions 1..16 */
const ALIGN: number[][] = [
  [],
  [],
  [6, 18],
  [6, 22],
  [6, 26],
  [6, 30],
  [6, 34],
  [6, 22, 38],
  [6, 24, 42],
  [6, 26, 46],
  [6, 28, 50],
  [6, 30, 54],
  [6, 32, 58],
  [6, 34, 62],
  [6, 26, 46, 66],
  [6, 26, 48, 70],
  [6, 26, 50, 74],
]

const GF_EXP = new Uint8Array(512)
const GF_LOG = new Uint8Array(256)
;(() => {
  let x = 1
  for (let i = 0; i < 255; i++) {
    GF_EXP[i] = x
    GF_LOG[x] = i
    x <<= 1
    if (x & 0x100) x ^= 0x11d
  }
  for (let i = 255; i < 512; i++) GF_EXP[i] = GF_EXP[i - 255]
})()

const gfMul = (a: number, b: number): number => (a === 0 || b === 0 ? 0 : GF_EXP[GF_LOG[a] + GF_LOG[b]])

function rsGenerator(degree: number): Uint8Array {
  let poly = new Uint8Array([1])
  for (let i = 0; i < degree; i++) {
    const next = new Uint8Array(poly.length + 1)
    const factor = GF_EXP[i]
    for (let j = 0; j < poly.length; j++) {
      next[j] ^= poly[j]
      next[j + 1] ^= gfMul(poly[j], factor)
    }
    poly = next
  }
  return poly
}

function rsRemainder(data: Uint8Array, degree: number): Uint8Array {
  const gen = rsGenerator(degree)
  const rem = new Uint8Array(degree)
  for (let i = 0; i < data.length; i++) {
    const factor = data[i] ^ rem[0]
    rem.copyWithin(0, 1)
    rem[degree - 1] = 0
    if (factor !== 0) {
      for (let j = 0; j < degree; j++) rem[j] ^= gfMul(gen[j + 1], factor)
    }
  }
  return rem
}

function pickVersion(byteLen: number): number {
  for (let v = 1; v < ECC_M.length; v++) {
    const [, , g1, d1, g2, d2] = ECC_M[v]
    const dataBits = (g1 * d1 + g2 * d2) * 8
    const headerBits = 4 + (v <= 9 ? 8 : 16)
    if (headerBits + byteLen * 8 <= dataBits) return v
  }
  throw new Error("qr-payload-too-long")
}

function buildCodewords(bytes: Uint8Array, version: number): Uint8Array {
  const [totalCw, ecPerBlock, g1, d1, g2, d2] = ECC_M[version]
  const dataCapacity = g1 * d1 + g2 * d2
  const bits: number[] = []
  const pushBits = (val: number, len: number) => {
    for (let i = len - 1; i >= 0; i--) bits.push((val >>> i) & 1)
  }
  pushBits(0b0100, 4) // byte mode
  pushBits(bytes.length, version <= 9 ? 8 : 16)
  for (const b of bytes) pushBits(b, 8)
  const capBits = dataCapacity * 8
  pushBits(0, Math.min(4, capBits - bits.length))
  while (bits.length % 8 !== 0) bits.push(0)

  const data = new Uint8Array(dataCapacity)
  for (let i = 0; i < bits.length / 8; i++) {
    let v = 0
    for (let j = 0; j < 8; j++) v = (v << 1) | bits[i * 8 + j]
    data[i] = v
  }
  for (let i = bits.length / 8, pad = 0; i < dataCapacity; i++, pad ^= 1) {
    data[i] = pad === 0 ? 0xec : 0x11
  }

  const blocks: Uint8Array[] = []
  const eccs: Uint8Array[] = []
  let offset = 0
  for (let i = 0; i < g1 + g2; i++) {
    const len = i < g1 ? d1 : d2
    const chunk = data.subarray(offset, offset + len)
    offset += len
    blocks.push(chunk)
    eccs.push(rsRemainder(chunk, ecPerBlock))
  }

  const out = new Uint8Array(totalCw)
  let pos = 0
  const maxData = Math.max(d1, d2)
  for (let col = 0; col < maxData; col++) {
    for (const blk of blocks) if (col < blk.length) out[pos++] = blk[col]
  }
  for (let col = 0; col < ecPerBlock; col++) {
    for (const ec of eccs) out[pos++] = ec[col]
  }
  return out
}

function formatBits(mask: number): number {
  // ECC level M = 00
  const data = (0b00 << 3) | mask
  let rem = data
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537)
  return ((data << 10) | (rem & 0x3ff)) ^ 0x5412
}

function versionBits(version: number): number {
  let rem = version
  for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25)
  return (version << 12) | (rem & 0xfff)
}

function maskBit(mask: number, r: number, c: number): boolean {
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

function penaltyScore(m: boolean[], size: number): number {
  let score = 0
  const at = (r: number, c: number) => m[r * size + c]
  const finderLine = (line: boolean[]) => {
    let hits = 0
    for (let i = 0; i <= line.length - 11; i++) {
      const a =
        line[i] && !line[i + 1] && line[i + 2] && line[i + 3] && line[i + 4] && !line[i + 5] && line[i + 6] &&
        !line[i + 7] && !line[i + 8] && !line[i + 9] && !line[i + 10]
      const b =
        !line[i] && !line[i + 1] && !line[i + 2] && !line[i + 3] &&
        line[i + 4] && !line[i + 5] && line[i + 6] && line[i + 7] && line[i + 8] && !line[i + 9] && line[i + 10]
      if (a || b) hits++
    }
    return hits * 40
  }
  const runScore = (line: boolean[]) => {
    let s = 0
    let cur = line[0]
    let len = 1
    for (let i = 1; i < line.length; i++) {
      if (line[i] === cur) len++
      else {
        if (len >= 5) s += 3 + (len - 5)
        cur = line[i]
        len = 1
      }
    }
    if (len >= 5) s += 3 + (len - 5)
    return s
  }
  const rowBuf = new Array<boolean>(size)
  const colBuf = new Array<boolean>(size)
  for (let i = 0; i < size; i++) {
    for (let j = 0; j < size; j++) {
      rowBuf[j] = at(i, j)
      colBuf[j] = at(j, i)
    }
    score += runScore(rowBuf) + runScore(colBuf) + finderLine(rowBuf) + finderLine(colBuf)
  }
  for (let r = 0; r < size - 1; r++) {
    for (let c = 0; c < size - 1; c++) {
      const v = at(r, c)
      if (v === at(r, c + 1) && v === at(r + 1, c) && v === at(r + 1, c + 1)) score += 3
    }
  }
  let dark = 0
  for (let i = 0; i < m.length; i++) if (m[i]) dark++
  const k = Math.ceil(Math.abs(dark * 20 - m.length * 10) / m.length) - 1
  if (k > 0) score += k * 10
  return score
}

/** Encodes `text` as UTF-8 into a QR Code matrix (ECC level M). */
export function encodeQr(text: string): QrMatrix {
  const bytes = new TextEncoder().encode(text)
  const version = pickVersion(bytes.length)
  const size = version * 4 + 17
  const modules = new Array<boolean>(size * size).fill(false)
  const isFunc = new Array<boolean>(size * size).fill(false)

  const setFunc = (r: number, c: number, dark: boolean) => {
    if (r < 0 || r >= size || c < 0 || c >= size) return
    modules[r * size + c] = dark
    isFunc[r * size + c] = true
  }

  const placeFinder = (top: number, left: number) => {
    for (let dy = -1; dy <= 7; dy++) {
      for (let dx = -1; dx <= 7; dx++) {
        const dist = Math.max(Math.abs(dx - 3), Math.abs(dy - 3))
        setFunc(top + dy, left + dx, dist !== 2 && dist !== 4)
      }
    }
  }
  placeFinder(0, 0)
  placeFinder(0, size - 7)
  placeFinder(size - 7, 0)

  for (let i = 8; i < size - 8; i++) {
    setFunc(6, i, i % 2 === 0)
    setFunc(i, 6, i % 2 === 0)
  }

  const centers = ALIGN[version]
  for (let i = 0; i < centers.length; i++) {
    for (let j = 0; j < centers.length; j++) {
      if ((i === 0 && j === 0) || (i === 0 && j === centers.length - 1) || (i === centers.length - 1 && j === 0)) continue
      const cr = centers[i]
      const cc = centers[j]
      for (let dy = -2; dy <= 2; dy++) {
        for (let dx = -2; dx <= 2; dx++) {
          setFunc(cr + dy, cc + dx, Math.max(Math.abs(dx), Math.abs(dy)) !== 1)
        }
      }
    }
  }

  // Reserve format information bits + dark module
  for (let i = 0; i <= 8; i++) {
    if (i !== 6) {
      setFunc(8, i, false)
      setFunc(i, 8, false)
    }
  }
  for (let i = 0; i < 8; i++) {
    setFunc(8, size - 1 - i, false)
    setFunc(size - 1 - i, 8, false)
  }
  setFunc(size - 8, 8, true)

  if (version >= 7) {
    const vb = versionBits(version)
    for (let i = 0; i < 18; i++) {
      const bit = ((vb >>> i) & 1) === 1
      const a = size - 11 + (i % 3)
      const b = Math.floor(i / 3)
      setFunc(b, a, bit)
      setFunc(a, b, bit)
    }
  }

  const codewords = buildCodewords(bytes, version)
  let bitIdx = 0
  let upward = true
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5
    for (let vert = 0; vert < size; vert++) {
      const r = upward ? size - 1 - vert : vert
      for (let j = 0; j < 2; j++) {
        const c = right - j
        if (isFunc[r * size + c]) continue
        const dark =
          bitIdx < codewords.length * 8
            ? ((codewords[bitIdx >>> 3] >>> (7 - (bitIdx & 7))) & 1) === 1
            : false
        modules[r * size + c] = dark
        bitIdx++
      }
    }
    upward = !upward
  }

  const writeFormat = (buf: boolean[], mask: number) => {
    const fb = formatBits(mask)
    const put = (r: number, c: number, bitIndex: number) => {
      buf[r * size + c] = ((fb >>> bitIndex) & 1) === 1
    }
    for (let i = 0; i <= 5; i++) put(8, i, 14 - i)
    put(8, 7, 8)
    put(8, 8, 7)
    put(7, 8, 6)
    for (let i = 9; i < 15; i++) put(14 - i, 8, 14 - i)

    for (let i = 0; i < 8; i++) put(8, size - 1 - i, i)
    for (let i = 8; i < 15; i++) put(size - 15 + i, 8, i)
    buf[(size - 8) * size + 8] = true
  }

  let bestMask = 0
  let bestScore = Infinity
  let bestModules = modules
  for (let mask = 0; mask < 8; mask++) {
    const cand = modules.slice()
    for (let r = 0; r < size; r++) {
      for (let c = 0; c < size; c++) {
        if (!isFunc[r * size + c] && maskBit(mask, r, c)) cand[r * size + c] = !cand[r * size + c]
      }
    }
    writeFormat(cand, mask)
    const s = penaltyScore(cand, size)
    if (s < bestScore) {
      bestScore = s
      bestMask = mask
      bestModules = cand
    }
  }
  void bestMask
  return { version, size, modules: bestModules }
}

/** Builds a compact SVG `<path d="…">` with horizontal run-length merging and `margin` quiet-zone modules. */
export function qrSvgPath(qr: QrMatrix, margin = 2): { viewBox: string; d: string } {
  const total = qr.size + margin * 2
  const parts: string[] = []
  for (let r = 0; r < qr.size; r++) {
    let c = 0
    while (c < qr.size) {
      if (!qr.modules[r * qr.size + c]) {
        c++
        continue
      }
      const start = c
      while (c < qr.size && qr.modules[r * qr.size + c]) c++
      const w = c - start
      parts.push(`M${start + margin} ${r + margin}h${w}v1h-${w}z`)
    }
  }
  return { viewBox: `0 0 ${total} ${total}`, d: parts.join("") }
}
