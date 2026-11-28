/**
 * Synchronous, zero-DOM static SVG renderer for Steve with bounded LRU caching
 * and adaptive Light/Dark SVG favicon export.
 */

import { CHARCOAL_FILTER, GEOMETRY, TIMING, resolveOpticalTier } from "./constants.ts"
import { compileFrame } from "./frame.ts"
import { clamp, hashString } from "./geometry.ts"
import { getMotionClip } from "./motions.ts"
import { PhysicsRig } from "./physics.ts"
import type { CompiledFrame, MascotAppearance, SustainedState, ThemeMode } from "./types.ts"

export interface StaticSteveOptions {
  readonly appearance?: MascotAppearance
  readonly size?: number
  readonly theme?: ThemeMode
  readonly state?: SustainedState
  readonly noCache?: boolean
}

/** Deterministic brushstroke fingerprint seed per character. */
export function appearanceSeed(appearance: MascotAppearance): number {
  if (appearance.kind === "steve") return TIMING.steveSeed
  return (hashString(appearance.id || appearance.color) % 997) + 31
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

function serializeFrameToSvg(
  frame: CompiledFrame,
  appearance: MascotAppearance,
  size: number,
  idPrefix: string,
): string {
  const vh = frame.viewHalf
  const c = frame.colors
  const l = frame.layers
  const sw = frame.strokeWidthScale
  const filterScale = frame.tier === "compact" ? CHARCOAL_FILTER.scaleCompact : CHARCOAL_FILTER.scaleFull
  const accFilterScale =
    frame.tier === "compact" ? CHARCOAL_FILTER.accessoryScaleCompact : CHARCOAL_FILTER.accessoryScaleFull
  const hasAcc =
    frame.accessoryBack.length > 0 || frame.accessoryBody.length > 0 || frame.accessoryFace.length > 0

  const defsParts: string[] = []
  if (frame.enableToothFilter) {
    defsParts.push(
      `<filter id="${idPrefix}-bt" filterUnits="userSpaceOnUse" x="-64" y="-64" width="128" height="128">` +
        `<feTurbulence type="fractalNoise" baseFrequency="${CHARCOAL_FILTER.baseFrequency}" numOctaves="${CHARCOAL_FILTER.numOctaves}" seed="3" result="n"/>` +
        `<feDisplacementMap in="SourceGraphic" in2="n" scale="${filterScale}" xChannelSelector="R" yChannelSelector="G"/>` +
        `</filter>`,
    )
    if (hasAcc) {
      defsParts.push(
        `<filter id="${idPrefix}-at" filterUnits="userSpaceOnUse" x="-64" y="-64" width="128" height="128">` +
          `<feTurbulence type="fractalNoise" baseFrequency="${CHARCOAL_FILTER.baseFrequency}" numOctaves="${CHARCOAL_FILTER.numOctaves}" seed="7" result="n"/>` +
          `<feDisplacementMap in="SourceGraphic" in2="n" scale="${accFilterScale}" xChannelSelector="R" yChannelSelector="G"/>` +
          `</filter>`,
      )
    }
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
  const accBackHtml = renderFxList(frame.accessoryBack)
  const accBodyHtml = renderFxList(frame.accessoryBody)
  const accFaceHtml = renderFxList(frame.accessoryFace)
  const bodyFilterAttr = frame.enableToothFilter ? ` filter="url(#${idPrefix}-bt)"` : ""
  const accFilterAttr = frame.enableToothFilter && hasAcc ? ` filter="url(#${idPrefix}-at)"` : ""

  const kindAttr =
    appearance.kind === "steve"
      ? ` data-steve-avatar="static" data-mascot-kind="steve"`
      : ` data-mascot-avatar="static" data-mascot-kind="bot"`
  const accAttr = frame.accessory ? ` data-mascot-accessory="${esc(frame.accessory)}"` : ""

  return (
    `<svg xmlns="http://www.w3.org/2000/svg"${kindAttr}${accAttr} data-steve-theme="${frame.theme}" viewBox="${-vh} ${-vh} ${vh * 2} ${vh * 2}" width="${size}" height="${size}" overflow="visible">` +
    `<defs>${defsParts.join("")}</defs>` +
    (floorHtml ? `<g>${floorHtml}</g>` : "") +
    (fxBackHtml ? `<g transform="${esc(frame.fxBackTransform)}">${fxBackHtml}</g>` : "") +
    (accBackHtml ? `<g transform="${esc(frame.bodyTransform)}"${accFilterAttr}>${accBackHtml}</g>` : "") +
    `<g transform="${esc(frame.bodyTransform)}"${bodyFilterAttr}>${bodyParts.join("")}</g>` +
    (accBodyHtml ? `<g transform="${esc(frame.bodyTransform)}"${accFilterAttr}>${accBodyHtml}</g>` : "") +
    `<g transform="${esc(frame.bodyTransform)}"><g transform="${esc(f.transform)}">${faceParts.join("")}${accFaceHtml ? `<g${accFilterAttr}>${accFaceHtml}</g>` : ""}</g></g>` +
    (fxFrontHtml ? `<g transform="${esc(frame.fxFrontTransform)}">${fxFrontHtml}</g>` : "") +
    `</svg>`
  )
}

/**
 * Synchronously render a cached static SVG for Steve or any Bot at any size (16..240px) in Light or Dark mode.
 * Works identically in browsers, SSR, unit tests, and icon generation — zero hidden DOM.
 */
export function renderStaticSvg(opts?: StaticSteveOptions): string {
  const appearance: MascotAppearance = opts?.appearance ?? { kind: "steve" }
  const size = opts?.size ?? 32
  const theme: ThemeMode = opts?.theme ?? "light"
  const state: SustainedState = opts?.state ?? "idle"
  const tier = resolveOpticalTier(size)
  const seed = appearanceSeed(appearance)
  const appearanceKey =
    appearance.kind === "steve"
      ? "steve"
      : `bot|${appearance.color.trim().toLowerCase()}|${Math.round(clamp(appearance.round, 0, 100))}|${appearance.accessory ?? "none"}|${seed}`
  const cacheKey = `${appearanceKey}|${size}|${theme}|${state}`

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
    appearance,
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
    seed,
    reducedMotion: true,
    activeMotion: false,
  })

  const idPrefix = `sm${hashString(cacheKey).toString(36)}`
  const markup = serializeFrameToSvg(frame, appearance, size, idPrefix)

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
