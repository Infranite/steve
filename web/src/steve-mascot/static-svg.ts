/**
 * Synchronous, zero-DOM static SVG renderer for Steve with bounded LRU caching
 * and adaptive Light/Dark SVG favicon export.
 */

import { CHARCOAL_FILTER, GEOMETRY, TIMING, resolveOpticalTier } from "./constants.ts"
import { compileFrame } from "./frame.ts"
import { hashString } from "./geometry.ts"
import { getMotionClip } from "./motions.ts"
import { PhysicsRig } from "./physics.ts"
import type { CompiledFrame, SustainedState, ThemeMode } from "./types.ts"

export interface StaticSteveOptions {
  readonly size?: number
  readonly theme?: ThemeMode
  readonly state?: SustainedState
  readonly noCache?: boolean
}

const staticCache = new Map<string, string>()
let cacheHits = 0
let cacheMisses = 0

export function getStaticCacheStats(): { size: number; hits: number; misses: number } {
  return { size: staticCache.size, hits: cacheHits, misses: cacheMisses }
}

export function clearStaticCache(): void {
  staticCache.clear()
  cacheHits = 0
  cacheMisses = 0
}

function esc(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;")
}

function serializeFrameToSvg(frame: CompiledFrame, size: number, idPrefix: string): string {
  const vh = frame.viewHalf
  const c = frame.colors
  const l = frame.layers
  const sw = frame.strokeWidthScale
  const filterScale = frame.tier === "compact" ? CHARCOAL_FILTER.scaleCompact : CHARCOAL_FILTER.scaleFull

  const defsParts: string[] = []
  if (frame.enableToothFilter) {
    defsParts.push(
      `<filter id="${idPrefix}-bt" filterUnits="userSpaceOnUse" x="-64" y="-64" width="128" height="128">` +
        `<feTurbulence type="fractalNoise" baseFrequency="${CHARCOAL_FILTER.baseFrequency}" numOctaves="${CHARCOAL_FILTER.numOctaves}" seed="3" result="n"/>` +
        `<feDisplacementMap in="SourceGraphic" in2="n" scale="${filterScale}" xChannelSelector="R" yChannelSelector="G"/>` +
        `</filter>`,
    )
  }
  defsParts.push(`<clipPath id="${idPrefix}-cl"><path d="${esc(frame.cleanBodyPath)}"/></clipPath>`)

  const floorHtml = frame.floorTicks
    .map(
      (t) =>
        `<path d="${esc(t.d)}" fill="none" stroke="${esc(c.ink)}" stroke-width="1.5" stroke-linecap="round" opacity="${t.opacity.toFixed(2)}"/>`,
    )
    .join("")

  const bodyParts: string[] = []
  if (l.washTransform) {
    bodyParts.push(
      `<path d="${esc(frame.cleanBodyPath)}" fill="${esc(c.wash)}" opacity="${(c.washOpacity * 0.78).toFixed(2)}" transform="${esc(l.washTransform)}"/>`,
    )
  }
  bodyParts.push(
    `<path d="${esc(frame.cleanBodyPath)}" fill="${esc(c.ink)}" opacity="${l.bodyFillOpacity.toFixed(2)}"/>`,
  )
  if (l.scribblePath) {
    bodyParts.push(
      `<g clip-path="url(#${idPrefix}-cl)"><path d="${esc(l.scribblePath)}" fill="none" stroke="${esc(c.ink)}" stroke-width="6.8" stroke-linecap="round" stroke-linejoin="round" opacity="0.92"/></g>`,
    )
  }
  if (l.stroke1) {
    bodyParts.push(
      `<path d="${esc(l.stroke1)}" fill="none" stroke="${esc(c.ink)}" stroke-width="${(3.8 * sw).toFixed(2)}" stroke-linecap="round" stroke-linejoin="round" opacity="0.96"/>`,
    )
  }
  if (l.stroke2) {
    bodyParts.push(
      `<path d="${esc(l.stroke2)}" fill="none" stroke="${esc(c.ink)}" stroke-width="${(2.0 * sw).toFixed(2)}" stroke-linecap="round" stroke-linejoin="round" opacity="0.62"/>`,
    )
  }

  const f = frame.face
  const faceParts: string[] = []
  if (f.blushOpacity > 0) {
    faceParts.push(
      `<path d="${esc(f.blushL)}" fill="none" stroke="${esc(c.accent)}" stroke-width="1.6" stroke-linecap="round" opacity="${f.blushOpacity.toFixed(2)}"/>`,
      `<path d="${esc(f.blushR)}" fill="none" stroke="${esc(c.accent)}" stroke-width="1.6" stroke-linecap="round" opacity="${f.blushOpacity.toFixed(2)}"/>`,
    )
  }
  if (f.showOvalEyes) {
    faceParts.push(`<path data-steve-eye="left" d="${esc(f.eyeLPath)}" fill="${esc(c.paper)}"/>`)
    if (f.winkRightArc) {
      faceParts.push(
        `<path data-steve-eye="right" d="${esc(f.winkRightArc)}" fill="none" stroke="${esc(c.paper)}" stroke-width="${f.arcStrokeWidth.toFixed(2)}" stroke-linecap="round"/>`,
      )
    } else {
      faceParts.push(`<path data-steve-eye="right" d="${esc(f.eyeRPath)}" fill="${esc(c.paper)}"/>`)
    }
  } else {
    faceParts.push(
      `<path data-steve-eye="left" d="${esc(f.arcL)}" fill="none" stroke="${esc(c.paper)}" stroke-width="${f.arcStrokeWidth.toFixed(2)}" stroke-linecap="round" stroke-linejoin="round"/>`,
      `<path data-steve-eye="right" d="${esc(f.arcR)}" fill="none" stroke="${esc(c.paper)}" stroke-width="${f.arcStrokeWidth.toFixed(2)}" stroke-linecap="round" stroke-linejoin="round"/>`,
    )
  }
  if (f.mouthOpacity > 0 && f.mouthPath) {
    faceParts.push(
      `<path d="${esc(f.mouthPath)}" fill="${f.mouthFilled ? esc(c.paper) : "none"}" stroke="${esc(c.paper)}" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" opacity="${f.mouthOpacity.toFixed(2)}"/>`,
    )
  }

  const renderFxList = (list: CompiledFrame["fxFront"]) =>
    list
      .map(
        (s) =>
          `<path d="${esc(s.d)}" fill="${esc(s.fill ?? "none")}" stroke="${esc(s.stroke ?? c.accent)}" stroke-width="${s.strokeWidth.toFixed(2)}" stroke-linecap="round" stroke-linejoin="round" opacity="${s.opacity.toFixed(2)}"${s.transform ? ` transform="${esc(s.transform)}"` : ""}/>`,
      )
      .join("")
  const fxBackHtml = renderFxList(frame.fxBack)
  const fxFrontHtml = renderFxList(frame.fxFront)
  const bodyFilterAttr = frame.enableToothFilter ? ` filter="url(#${idPrefix}-bt)"` : ""

  return (
    `<svg xmlns="http://www.w3.org/2000/svg" data-steve-avatar="static" data-steve-theme="${frame.theme}" viewBox="${-vh} ${-vh} ${vh * 2} ${vh * 2}" width="${size}" height="${size}" overflow="visible">` +
    `<defs>${defsParts.join("")}</defs>` +
    (floorHtml ? `<g>${floorHtml}</g>` : "") +
    (fxBackHtml ? `<g transform="${esc(frame.fxBackTransform)}">${fxBackHtml}</g>` : "") +
    `<g transform="${esc(frame.bodyTransform)}"${bodyFilterAttr}>${bodyParts.join("")}</g>` +
    `<g transform="${esc(frame.bodyTransform)}"><g transform="${esc(f.transform)}">${faceParts.join("")}</g></g>` +
    (fxFrontHtml ? `<g transform="${esc(frame.fxFrontTransform)}">${fxFrontHtml}</g>` : "") +
    `</svg>`
  )
}

/**
 * Synchronously render a cached static SVG for Steve at any size (16..240px) in Light or Dark mode.
 * Works identically in browsers, SSR, unit tests, and icon generation — zero hidden DOM.
 */
export function renderStaticSvg(opts?: StaticSteveOptions): string {
  const size = opts?.size ?? 32
  const theme: ThemeMode = opts?.theme ?? "light"
  const state: SustainedState = opts?.state ?? "idle"
  const tier = resolveOpticalTier(size)
  const cacheKey = `${size}|${theme}|${state}`

  if (!opts?.noCache) {
    const hit = staticCache.get(cacheKey)
    if (hit !== undefined) {
      cacheHits++
      return hit
    }
    cacheMisses++
  }

  const clip = getMotionClip(state) ?? getMotionClip("idle")!
  const pose = clip.sample({
    t: 0,
    elapsedSec: 0,
    lookX: 0,
    lookY: 0,
    reducedMotion: true,
  })

  const physics = new PhysicsRig()
  physics.snapTo({
    tx: pose.tx,
    ty: pose.ty,
    tsq: pose.tsq,
    trot: pose.trot,
    faceX: pose.faceOffsetX,
    faceY: pose.faceOffsetY,
  })

  const frame = compileFrame({
    theme,
    tier,
    live: false,
    pose,
    physics,
    sleepAmt: pose.sleepWeight,
    loveAmt: pose.loveWeight,
    blinkAmt: 0,
    boilFrame: 0,
    tSec: 0,
    seed: TIMING.steveSeed,
    reducedMotion: true,
    activeMotion: false,
  })

  const idPrefix = `sm${hashString(cacheKey).toString(36)}`
  const markup = serializeFrameToSvg(frame, size, idPrefix)

  if (!opts?.noCache) {
    if (staticCache.size >= GEOMETRY.maxStaticCacheEntries) {
      const oldest = staticCache.keys().next().value
      if (oldest !== undefined) staticCache.delete(oldest)
    }
    staticCache.set(cacheKey, markup)
  }
  return markup
}

/**
 * Generate a self-contained adaptive SVG favicon (same charcoal design, switches Light/Dark
 * colors automatically via prefers-color-scheme).
 */
export function renderAdaptiveFaviconSvg(size = 32): string {
  // Micro-tier crisp charcoal silhouette so browser tabs at 16px and bookmarks at 32px stay bold.
  const lightSvg = renderStaticSvg({ size: 20, theme: "light", noCache: true })
  const darkSvg = renderStaticSvg({ size: 20, theme: "dark", noCache: true })
  const stripRoot = (s: string) => s.replace(/^<svg[^>]*>/, "").replace(/<\/svg>$/, "")
  const vbMatch = lightSvg.match(/viewBox="([^"]+)"/)
  const viewBox = vbMatch ? vbMatch[1] : "-40 -40 80 80"

  return (
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}" width="${size}" height="${size}">` +
    `<style>.d{display:none}@media(prefers-color-scheme:dark){.l{display:none}.d{display:inline}}</style>` +
    `<g class="l">${stripRoot(lightSvg)}</g>` +
    `<g class="d">${stripRoot(darkSvg)}</g>` +
    `</svg>`
  )
}
