/**
 * Pure deterministic PRNG, classic circle contour, charcoal scribble,
 * hand-drawn rim loops, floor hatch ticks, and sculpted giant eye paths.
 */

import { GEOMETRY } from "./constants.ts"
import type { Vec2 } from "./types.ts"

/** Fast 32-bit FNV-1a string hash. */
export function hashString(input: string): number {
  let h = 2166136261
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

/** Deterministic pseudo-random noise in [-1, 1] for charcoal stroke wobble. */
export function hashNoise(seed: number): number {
  const x = Math.sin(seed * 127.1 + 311.7) * 43758.5453123
  return (x - Math.floor(x)) * 2 - 1
}

/** Deterministic pseudo-random float in [0, 1). */
export function hashUnit(seed: number): number {
  const x = Math.sin(seed * 269.5 + 183.3) * 43758.5453123
  return x - Math.floor(x)
}

/** Clamp a numeric value to [min, max]. */
export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value
}

function angleDiff(a: number, b: number): number {
  let d = a - b
  while (d > Math.PI) d -= Math.PI * 2
  while (d < -Math.PI) d += Math.PI * 2
  return d
}

function gauss(theta: number, center: number, width: number): number {
  const d = angleDiff(theta, center) / width
  return Math.exp(-0.5 * d * d)
}

function resampleClosedByArcLength(rawPts: readonly Vec2[], count: number): Vec2[] {
  const n = rawPts.length
  if (n < 3 || count < 3) return [...rawPts]
  const cumLen = new Float64Array(n + 1)
  for (let i = 0; i < n; i++) {
    const a = rawPts[i]!
    const b = rawPts[(i + 1) % n]!
    cumLen[i + 1] = cumLen[i]! + Math.hypot(b.x - a.x, b.y - a.y)
  }
  const totalLen = cumLen[n]!
  if (totalLen <= 1e-6) return [...rawPts]

  const out: Vec2[] = []
  let seg = 0
  for (let k = 0; k < count; k++) {
    const target = (k / count) * totalLen
    while (seg < n - 1 && cumLen[seg + 1]! < target) seg++
    const segStart = cumLen[seg]!
    const segLen = cumLen[seg + 1]! - segStart
    const t = segLen > 1e-9 ? (target - segStart) / segLen : 0
    const a = rawPts[seg]!
    const b = rawPts[(seg + 1) % n]!
    out.push({ x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t })
  }
  return out
}

function sampleContourAt(contour: readonly Vec2[], u: number): Vec2 {
  const n = contour.length
  if (n === 0) return { x: 0, y: 0 }
  const wrapped = ((u % 1) + 1) % 1
  const f = wrapped * n
  const i0 = Math.floor(f) % n
  const i1 = (i0 + 1) % n
  const t = f - Math.floor(f)
  const p0 = contour[i0]!
  const p1 = contour[i1]!
  return { x: p0.x + (p1.x - p0.x) * t, y: p0.y + (p1.y - p0.y) * t }
}

let cachedCircleContour: readonly Vec2[] | null = null
const cachedBotContours = new Map<number, readonly Vec2[]>()

function polygonArea(pts: readonly Vec2[]): number {
  let twice = 0
  const n = pts.length
  for (let i = 0; i < n; i++) {
    const a = pts[i]!
    const b = pts[(i + 1) % n]!
    twice += a.x * b.y - b.x * a.y
  }
  return Math.abs(twice * 0.5)
}

function getClassicCircleContour(): readonly Vec2[] {
  if (cachedCircleContour) return cachedCircleContour
  const raw: Vec2[] = []
  for (let i = 0; i < 128; i++) {
    const a = (i / 128) * Math.PI * 2 - Math.PI / 2
    const s = Math.sin(a)
    // Gentle grounded flattening along the bottom curve so Steve sits naturally on the baseline.
    const ground = s > 0 ? 1 - 0.022 * Math.pow(s, 2.4) : 1
    raw.push({
      x: Math.cos(a) * GEOMETRY.circleRx * ground,
      y: Math.sin(a) * GEOMETRY.circleRy * ground,
    })
  }
  cachedCircleContour = resampleClosedByArcLength(raw, 192)
  return cachedCircleContour
}

/**
 * Optical-area-normalized superellipse contour for ordinary Bots parameterized by `round` (0..100):
 * 0..15 = crisp architectural square, 45 = Bauhaus squircle, 70 = soft cushion, 95..100 = round.
 */
function getBotShapeContour(roundInput: number): readonly Vec2[] {
  const round = Math.round(clamp(roundInput, 0, 100))
  const cached = cachedBotContours.get(round)
  if (cached) return cached

  const exponent = 2.0 + 5.2 * Math.pow(1 - round / 100, 1.35)
  const raw: Vec2[] = []
  for (let i = 0; i < 160; i++) {
    const a = (i / 160) * Math.PI * 2 - Math.PI / 2
    const c = Math.cos(a)
    const s = Math.sin(a)
    const r = 1 / Math.pow(Math.pow(Math.abs(c), exponent) + Math.pow(Math.abs(s), exponent), 1 / exponent)
    raw.push({ x: c * 32.0 * r, y: s * 31.2 * r })
  }
  const area = polygonArea(raw)
  const scale = area > 1e-3 ? Math.sqrt(GEOMETRY.opticalTargetArea / area) : 1
  const normalized = resampleClosedByArcLength(
    raw.map((p) => ({ x: p.x * scale, y: p.y * scale })),
    192,
  )
  cachedBotContours.set(round, normalized)
  return normalized
}

function applyContourMorphs(
  base: Vec2,
  params: {
    theta: number
    t: number
    sleepAmt: number
    loveAmt: number
    shiverAmp: number
    reducedMotion: boolean
    activeMotion: boolean
  },
): Vec2 {
  const { theta, t, sleepAmt, loveAmt, shiverAmp, reducedMotion, activeMotion } = params
  const s = Math.sin(theta)
  const bottomPad = s > 0 ? Math.pow(s, 2.2) : 0
  let x = base.x
  let y = base.y
  let rm = 1

  if (!reducedMotion && activeMotion) {
    rm += 0.008 * Math.sin(theta * 3 + t * 2.0)
    if (shiverAmp > 0.01) {
      rm += 0.022 * shiverAmp * Math.sin(theta * 6 + t * 24)
    }
  }

  if (loveAmt > 0.01) {
    rm -= 0.14 * loveAmt * gauss(theta, -Math.PI / 2, 0.3)
    rm += 0.1 * loveAmt * gauss(theta, -Math.PI / 2 - 0.55, 0.36)
    rm += 0.1 * loveAmt * gauss(theta, -Math.PI / 2 + 0.55, 0.36)
    rm += 0.08 * loveAmt * gauss(theta, Math.PI / 2, 0.42)
  }

  x *= rm
  y *= rm

  if (sleepAmt > 0.001) {
    x *= 1 + 0.22 * sleepAmt
    y *= 1 - 0.3 * sleepAmt
    if (s < 0) y += Math.abs(s) * 4 * sleepAmt
    y += bottomPad * 1.5 * sleepAmt
  }

  return { x, y: y + sleepAmt * 10.5 }
}

/** Sample Steve's classic circle silhouette or a Bot's roundness-derived contour with organic morphs. */
export function sampleMascotContour(params: {
  round: number | null
  theta: number
  t: number
  sleepAmt: number
  loveAmt: number
  shiverAmp: number
  reducedMotion: boolean
  activeMotion: boolean
}): Vec2 {
  const u = (params.theta + Math.PI / 2) / (Math.PI * 2)
  const contour = params.round === null ? getClassicCircleContour() : getBotShapeContour(params.round)
  return applyContourMorphs(sampleContourAt(contour, u), params)
}

function smoothHandWobble(u: number, seed: number): number {
  const p1 = hashNoise(seed + 1.1) * Math.PI
  const p2 = hashNoise(seed + 7.3) * Math.PI
  const p3 = hashNoise(seed + 19.7) * Math.PI
  const p4 = hashNoise(seed + 37.9) * Math.PI
  return (
    Math.sin(u * 2 + p1) * 0.44 +
    Math.sin(u * 3 + p2) * 0.3 +
    Math.sin(u * 5 + p3) * 0.17 +
    Math.sin(u * 7 + p4) * 0.09
  )
}

/** Open Catmull-Rom spline converted to SVG cubic Bezier path. */
export function openSmoothPath(pts: readonly Vec2[]): string {
  const n = pts.length
  if (n < 2) return ""
  let d = `M ${pts[0]!.x.toFixed(2)} ${pts[0]!.y.toFixed(2)}`
  for (let i = 0; i < n - 1; i++) {
    const p0 = pts[Math.max(0, i - 1)]!
    const p1 = pts[i]!
    const p2 = pts[i + 1]!
    const p3 = pts[Math.min(n - 1, i + 2)]!
    const c1x = p1.x + (p2.x - p0.x) / 6
    const c1y = p1.y + (p2.y - p0.y) / 6
    const c2x = p2.x - (p3.x - p1.x) / 6
    const c2y = p2.y - (p3.y - p1.y) / 6
    d += ` C ${c1x.toFixed(2)} ${c1y.toFixed(2)}, ${c2x.toFixed(2)} ${c2y.toFixed(2)}, ${p2.x.toFixed(2)} ${p2.y.toFixed(2)}`
  }
  return d
}

/** Closed Catmull-Rom spline converted to SVG cubic Bezier path. */
export function closedSmoothPath(pts: readonly Vec2[]): string {
  const n = pts.length
  if (n < 3) return ""
  let d = `M ${pts[0]!.x.toFixed(2)} ${pts[0]!.y.toFixed(2)}`
  for (let i = 0; i < n; i++) {
    const p0 = pts[(i - 1 + n) % n]!
    const p1 = pts[i]!
    const p2 = pts[(i + 1) % n]!
    const p3 = pts[(i + 2) % n]!
    const c1x = p1.x + (p2.x - p0.x) / 6
    const c1y = p1.y + (p2.y - p0.y) / 6
    const c2x = p2.x - (p3.x - p1.x) / 6
    const c2y = p2.y - (p3.y - p1.y) / 6
    d += ` C ${c1x.toFixed(2)} ${c1y.toFixed(2)}, ${c2x.toFixed(2)} ${c2y.toFixed(2)}, ${p2.x.toFixed(2)} ${p2.y.toFixed(2)}`
  }
  return `${d} Z`
}

/** Hand-drawn charcoal contour loop that overshoots its start slightly like real dry chalk. */
export function buildLooseLoop(
  basePts: readonly Vec2[],
  seedBase: number,
  jitterAmp: number,
  startShift: number,
  overshootCount: number,
  radialBias = 0,
): string {
  const n = basePts.length
  if (n < 3) return ""
  const pts: Vec2[] = []
  const shift = ((startShift % n) + n) % n
  const scaledOvershoot = Math.max(1, Math.round((overshootCount * n) / 18))
  const total = n + scaledOvershoot
  for (let k = 0; k <= total; k++) {
    const idx = (k + shift) % n
    const p = basePts[idx]!
    const rad = Math.hypot(p.x, p.y) || 1
    const u = (k / n) * Math.PI * 2
    const w1 = smoothHandWobble(u, seedBase)
    const w2 = smoothHandWobble(u + 1.3, seedBase + 91.3)
    const tailDrift = k === 0 || k === total ? 1.5 : 0
    const dr = radialBias + w1 * jitterAmp + tailDrift * w2
    const dt = w2 * (jitterAmp * 0.55)
    const ux = p.x / rad
    const uy = p.y / rad
    pts.push({
      x: p.x + ux * dr - uy * dt,
      y: p.y + uy * dr + ux * dt,
    })
  }
  return openSmoothPath(pts)
}

/** Expressive diagonal 6B charcoal scribble fill clipped inside Steve's circle contour. */
export function buildCharcoalScribble(
  seedBase: number,
  basePts: readonly Vec2[],
  rows = 20,
): string {
  let minX = Number.POSITIVE_INFINITY
  let maxX = Number.NEGATIVE_INFINITY
  let minY = Number.POSITIVE_INFINITY
  let maxY = Number.NEGATIVE_INFINITY
  for (const p of basePts) {
    if (p.x < minX) minX = p.x
    if (p.x > maxX) maxX = p.x
    if (p.y < minY) minY = p.y
    if (p.y > maxY) maxY = p.y
  }
  // Overshoot beyond the clip boundary so Catmull-Rom turnarounds never leave edge gaps.
  minX -= 16
  maxX += 16
  minY -= 9
  maxY += 9

  const pts: Vec2[] = []
  const cx = (minX + maxX) / 2
  const halfSpanX = (maxX - minX) / 2
  const spanY = maxY - minY
  for (let i = 0; i <= rows; i++) {
    const t = i / rows
    const f = t * 2 - 1
    const y = minY + t * spanY
    const dir = i % 2 === 0 ? 1 : -1
    const slant = f * -4.2
    const jitterX = hashNoise(seedBase + i * 11.3) * 3.4
    const jitterY = hashNoise(seedBase + i * 7.9 + 50) * 2.0
    pts.push({ x: cx - dir * halfSpanX + slant + jitterX, y: y + jitterY })
    pts.push({
      x: cx + dir * halfSpanX + slant + hashNoise(seedBase + i * 19.1) * 3.4,
      y: y + hashNoise(seedBase + i * 5.3 + 90) * 2.1,
    })
  }
  return openSmoothPath(pts)
}

/** Hand-sketched floor contact ticks beneath full-size Steve. */
export function buildFloorTicks(
  centerX: number,
  width: number,
  seed: number,
  baseY = 39.5,
): readonly string[] {
  const paths: string[] = []
  for (let idx = 0; idx < 3; idx++) {
    const y = baseY + idx * 3.1
    const w = width * (1 - idx * 0.22)
    const j1 = hashNoise(seed + idx * 19.3) * 1.3
    const j2 = hashNoise(seed + idx * 19.3 + 7) * 1.3
    paths.push(
      `M ${(centerX - w + j1).toFixed(1)} ${(y + j2).toFixed(1)} Q ${centerX.toFixed(1)} ${(y + 1.2 - j1).toFixed(1)} ${(centerX + w - j2).toFixed(1)} ${(y + j1).toFixed(1)}`,
    )
  }
  return paths
}

/**
 * Analytic closed-form SVG path for a giant oval cutout eye with optional flat/slanted
 * upper lid trim (`topCut` in [0, 0.58], `slantRad` for focused / pensive expressions).
 */
export function buildSculptedEyePath(
  cx: number,
  cy: number,
  rx: number,
  ry: number,
  topCut = 0,
  slantRad = 0,
): string {
  const safeRx = Math.max(0.6, rx)
  const safeRy = Math.max(0.6, ry)
  if (topCut <= 0.01) {
    return (
      `M ${(cx - safeRx).toFixed(2)} ${cy.toFixed(2)} ` +
      `A ${safeRx.toFixed(2)} ${safeRy.toFixed(2)} 0 1 0 ${(cx + safeRx).toFixed(2)} ${cy.toFixed(2)} ` +
      `A ${safeRx.toFixed(2)} ${safeRy.toFixed(2)} 0 1 0 ${(cx - safeRx).toFixed(2)} ${cy.toFixed(2)} Z`
    )
  }
  const cut = Math.min(0.58, topCut)
  const y0 = -safeRy + cut * 2 * safeRy
  const m = Math.tan(slantRad)
  const R = Math.hypot(safeRy, m * safeRx)
  const phi = Math.atan2(m * safeRx, safeRy)
  const ratio = Math.max(-0.95, Math.min(0.95, y0 / R))
  const beta = Math.asin(ratio)
  const aRight = phi + beta
  const aLeft = phi + Math.PI - beta
  const xL = cx + Math.cos(aLeft) * safeRx
  const yL = cy + Math.sin(aLeft) * safeRy
  const xR = cx + Math.cos(aRight) * safeRx
  const yR = cy + Math.sin(aRight) * safeRy
  const largeArc = y0 < 0 ? 1 : 0
  return (
    `M ${xL.toFixed(2)} ${yL.toFixed(2)} ` +
    `L ${xR.toFixed(2)} ${yR.toFixed(2)} ` +
    `A ${safeRx.toFixed(2)} ${safeRy.toFixed(2)} 0 ${largeArc} 1 ${xL.toFixed(2)} ${yL.toFixed(2)} Z`
  )
}
