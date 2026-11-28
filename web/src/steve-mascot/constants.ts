/**
 * Named architectural constants for Steve's charcoal mascot.
 * Every color token, optical tier, spring parameter, and timing window is declared here.
 */

import type { MascotAppearance, OpticalTier, PaletteTokens, ThemeMode } from "./types.ts"

/** Single charcoal visual design across Light and Dark — theme switches palette tokens only. */
export const STEVE_PALETTE: Record<ThemeMode, PaletteTokens> = {
  light: {
    ink: "#1c1d22",
    paper: "#fcfaf3",
    wash: "#e8a58c",
    washOpacity: 0.46,
    accent: "#d46a49",
  },
  dark: {
    ink: "#f2efe6",
    paper: "#191b20",
    wash: "#c86d52",
    washOpacity: 0.34,
    accent: "#f08d6c",
  },
}

/** Preset head roundness tiers matching the Bot settings shape selector (0..100). */
export const BOT_ROUND_PRESETS = [15, 45, 70, 95] as const

/** Target WCAG relative luminance for colored Bot charcoal bodies (guarantees >= 4.5:1 eye contrast and clear separation from Steve). */
const BOT_COLOR_CALIBRATION = {
  lightTargetLuminance: 0.165,
  darkTargetLuminance: 0.56,
  lightMinChromaticSat: 44,
  lightMaxChromaticSat: 72,
  darkMinChromaticSat: 52,
  darkMaxChromaticSat: 78,
  /** Below this saturation (e.g. slate/grey), keep natural low saturation rather than forcing vividness. */
  neutralSatThreshold: 16,
  lightEyeCutout: "#fcfaf3",
  darkEyeCutout: "#191b20",
} as const

function parseHexRgb(hex: string): [number, number, number] | null {
  const clean = hex.trim().replace(/^#/, "")
  if (clean.length === 3) {
    const r = Number.parseInt(clean[0]! + clean[0]!, 16)
    const g = Number.parseInt(clean[1]! + clean[1]!, 16)
    const b = Number.parseInt(clean[2]! + clean[2]!, 16)
    return Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b) ? null : [r, g, b]
  }
  if (clean.length === 6) {
    const r = Number.parseInt(clean.slice(0, 2), 16)
    const g = Number.parseInt(clean.slice(2, 4), 16)
    const b = Number.parseInt(clean.slice(4, 6), 16)
    return Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b) ? null : [r, g, b]
  }
  return null
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const rn = r / 255
  const gn = g / 255
  const bn = b / 255
  const max = Math.max(rn, gn, bn)
  const min = Math.min(rn, gn, bn)
  const l = (max + min) / 2
  if (max === min) return [0, 0, l * 100]
  const d = max - min
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min)
  let h = 0
  if (max === rn) h = (gn - bn) / d + (gn < bn ? 6 : 0)
  else if (max === gn) h = (bn - rn) / d + 2
  else h = (rn - gn) / d + 4
  return [h * 60, s * 100, l * 100]
}

function hslToRgb(hDeg: number, sPct: number, lPct: number): [number, number, number] {
  const h = (((hDeg % 360) + 360) % 360) / 360
  const s = Math.max(0, Math.min(1, sPct / 100))
  const l = Math.max(0, Math.min(1, lPct / 100))
  if (s === 0) {
    const v = Math.round(l * 255)
    return [v, v, v]
  }
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s
  const p = 2 * l - q
  const hue2rgb = (t: number) => {
    const u = ((t % 1) + 1) % 1
    if (u < 1 / 6) return p + (q - p) * 6 * u
    if (u < 1 / 2) return q
    if (u < 2 / 3) return p + (q - p) * (2 / 3 - u) * 6
    return p
  }
  return [
    Math.round(hue2rgb(h + 1 / 3) * 255),
    Math.round(hue2rgb(h) * 255),
    Math.round(hue2rgb(h - 1 / 3) * 255),
  ]
}

function rgbToHex(r: number, g: number, b: number): string {
  const to2 = (v: number) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, "0")
  return `#${to2(r)}${to2(g)}${to2(b)}`
}

function relativeLuminance(r: number, g: number, b: number): number {
  const lin = (c: number) => {
    const u = c / 255
    return u <= 0.03928 ? u / 12.92 : Math.pow((u + 0.055) / 1.055, 2.4)
  }
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

function solveHexForLuminance(hDeg: number, sPct: number, targetY: number): string {
  let lo = 6
  let hi = 94
  for (let i = 0; i < 16; i++) {
    const mid = (lo + hi) * 0.5
    const [r, g, b] = hslToRgb(hDeg, sPct, mid)
    if (relativeLuminance(r, g, b) < targetY) lo = mid
    else hi = mid
  }
  const [r, g, b] = hslToRgb(hDeg, sPct, (lo + hi) * 0.5)
  return rgbToHex(r, g, b)
}

const botPaletteCache = new Map<string, PaletteTokens>()

/**
 * Derive a theme-calibrated colored charcoal / chalk palette from any Bot hex color.
 * Preserves the Bot's hue while guaranteeing crisp cutout-eye contrast and clear
 * visual separation from Steve's monochrome black/bone-chalk flagship identity.
 */
export function resolveBotPalette(hexColor: string, theme: ThemeMode): PaletteTokens {
  const key = `${hexColor.trim().toLowerCase()}|${theme}`
  const cached = botPaletteCache.get(key)
  if (cached) return cached

  const rgb = parseHexRgb(hexColor) ?? [122, 167, 199]
  const [h, s] = rgbToHsl(rgb[0], rgb[1], rgb[2])
  const isNeutral = s < BOT_COLOR_CALIBRATION.neutralSatThreshold

  let tokens: PaletteTokens
  if (theme === "light") {
    const sat = isNeutral
      ? Math.max(10, s)
      : Math.max(BOT_COLOR_CALIBRATION.lightMinChromaticSat, Math.min(BOT_COLOR_CALIBRATION.lightMaxChromaticSat, s + 8))
    const ink = solveHexForLuminance(h, sat, BOT_COLOR_CALIBRATION.lightTargetLuminance)
    const [wr, wg, wb] = hslToRgb(h, Math.min(66, sat), 78)
    const [ar, ag, ab] = hslToRgb(h, Math.min(76, sat + 10), 48)
    tokens = {
      ink,
      paper: BOT_COLOR_CALIBRATION.lightEyeCutout,
      wash: rgbToHex(wr, wg, wb),
      washOpacity: 0.44,
      accent: rgbToHex(ar, ag, ab),
    }
  } else {
    const sat = isNeutral
      ? Math.max(14, s + 4)
      : Math.max(BOT_COLOR_CALIBRATION.darkMinChromaticSat, Math.min(BOT_COLOR_CALIBRATION.darkMaxChromaticSat, s + 12))
    const ink = solveHexForLuminance(h, sat, BOT_COLOR_CALIBRATION.darkTargetLuminance)
    const [wr, wg, wb] = hslToRgb(h, Math.min(70, sat), 42)
    const [ar, ag, ab] = hslToRgb(h, Math.min(82, sat + 8), 72)
    tokens = {
      ink,
      paper: BOT_COLOR_CALIBRATION.darkEyeCutout,
      wash: rgbToHex(wr, wg, wb),
      washOpacity: 0.34,
      accent: rgbToHex(ar, ag, ab),
    }
  }

  if (botPaletteCache.size > 256) botPaletteCache.clear()
  botPaletteCache.set(key, tokens)
  return tokens
}

export function resolveAppearancePalette(appearance: MascotAppearance, theme: ThemeMode): PaletteTokens {
  return appearance.kind === "steve" ? STEVE_PALETTE[theme] : resolveBotPalette(appearance.color, theme)
}

/** Fixed theme-aware editorial palette for action props, independent of body tones. */
export const PROP_PALETTE = {
  light: {
    ink: "#1e2026",
    paper: "#fcfaf3",
    woodHandle: "#b56e3f",
    glassTint: "#99cde8",
    sand: "#dda236",
    mugBand: "#c96a48",
    signal: "#d96d49",
    cloudFill: "#fcfaf3",
    success: "#3ea36c",
    love: "#e35b6d",
    failure: "#e05252",
  },
  dark: {
    ink: "#efeae0",
    paper: "#191b20",
    woodHandle: "#d68c58",
    glassTint: "#8dc8eb",
    sand: "#f2bf4e",
    mugBand: "#e6825c",
    signal: "#ef8862",
    cloudFill: "#282b34",
    success: "#4bc080",
    love: "#f27283",
    failure: "#f06868",
  },
} as const

/** Iconic giant eye proportions (classic round + full-face giant cutout eyes). */
export const GIANT_EYES = {
  eyeGap: 12.6,
  eyeRx: 9.2,
  eyeRy: 13.2,
  cutoutScale: 1.02,
} as const

/** Paper-tooth charcoal displacement filter tuning. */
export const CHARCOAL_FILTER = {
  baseFrequency: 0.115,
  numOctaves: 3,
  scaleFull: 2.4,
  scaleCompact: 1.6,
  accessoryScaleFull: 1.15,
  accessoryScaleCompact: 0.72,
} as const

/** Optical tier boundaries calibrated for crisp clarity at 16, 24, 32, 64, and 240px. */
export const OPTICAL_TIERS: Record<
  OpticalTier,
  {
    readonly maxSizePx: number
    readonly viewHalf: number
    readonly strokeWidthScale: number
    readonly eyeSizeBoost: number
    readonly bodyFillOpacity: number
    readonly enableWash: boolean
    readonly enableScribble: boolean
    readonly enableSecondRim: boolean
    readonly enableToothFilter: boolean
    readonly enableFloorShadow: boolean
    /** Render optional Bot role accessories only when pixel budget keeps strokes legible (>= 27px). */
    readonly enableAccessory: boolean
  }
> = {
  micro: {
    maxSizePx: 20,
    viewHalf: 40,
    strokeWidthScale: 1.45,
    eyeSizeBoost: 1.18,
    bodyFillOpacity: 0.98,
    enableWash: false,
    enableScribble: false,
    enableSecondRim: false,
    enableToothFilter: false,
    enableFloorShadow: false,
    enableAccessory: false,
  },
  mini: {
    maxSizePx: 26,
    viewHalf: 42,
    strokeWidthScale: 1.2,
    eyeSizeBoost: 1.1,
    bodyFillOpacity: 0.45,
    enableWash: false,
    enableScribble: true,
    enableSecondRim: false,
    enableToothFilter: false,
    enableFloorShadow: false,
    enableAccessory: false,
  },
  compact: {
    maxSizePx: 52,
    viewHalf: 44,
    strokeWidthScale: 1.0,
    eyeSizeBoost: 1.04,
    bodyFillOpacity: 0.56,
    enableWash: true,
    enableScribble: true,
    enableSecondRim: true,
    enableToothFilter: true,
    enableFloorShadow: false,
    enableAccessory: true,
  },
  full: {
    maxSizePx: Number.POSITIVE_INFINITY,
    viewHalf: 48,
    strokeWidthScale: 1.0,
    eyeSizeBoost: 1.0,
    bodyFillOpacity: 0.56,
    enableWash: true,
    enableScribble: true,
    enableSecondRim: true,
    enableToothFilter: true,
    enableFloorShadow: true,
    enableAccessory: true,
  },
}

/** Minimum rendered pixel size at which Bot accessories are shown (sizes <= 26px omit them for optical clarity). */
export const ACCESSORY_MIN_SIZE_PX = OPTICAL_TIERS.mini.maxSizePx + 1

/** Resolve pixel size into its optical clarity tier. */
export function resolveOpticalTier(sizePx: number): OpticalTier {
  if (sizePx <= OPTICAL_TIERS.micro.maxSizePx) return "micro"
  if (sizePx <= OPTICAL_TIERS.mini.maxSizePx) return "mini"
  if (sizePx <= OPTICAL_TIERS.compact.maxSizePx) return "compact"
  return "full"
}

/** Volume-conserving spring physics constants for live Steve animations. */
export const SPRING = {
  posStiffness: 140,
  posDamping: 13.5,
  squashStiffness: 162,
  squashDamping: 11.5,
  rotStiffness: 115,
  rotDamping: 12.0,
  faceStiffness: 170,
  faceDamping: 14.5,
  lagStiffness: 80,
  lagDamping: 9.0,
  pokeImpulse: 0.3,
  /** Convergence thresholds below which springs snap clean and stop rAF. */
  settlePosEps: 0.035,
  settleVelEps: 0.08,
  settleRotEps: 0.06,
  settleSquashEps: 0.0025,
} as const

/** Simulation, blink, idle behavior, and auto-sleep timing constants. */
export const TIMING = {
  maxFrameDtSec: 0.05,
  maxPhysicsStepSec: 0.033,
  physicsSubSteps: 2,
  boilFps: 6.5,
  boilFrameCount: 4,
  /** Auto-sleep inactivity timeout (ms), matching Steve's existing 90s cadence. */
  autoSleepMs: 90_000,
  /** Duration after last pointer movement before calm idle can go dormant. */
  pointerIdleSettleMs: 360,
  /** Natural blink cadence range (sec) and double-blink chance. */
  blinkMinGapSec: 2.8,
  blinkMaxGapSec: 6.2,
  doubleBlinkChance: 0.18,
  doubleBlinkGapSec: 0.16,
  blinkSpeedRadPerSec: 8.2,
  /** Autonomous idle micro-behavior cadence range (sec). */
  idleBehaviorMinGapSec: 7.5,
  idleBehaviorMaxGapSec: 14.0,
  /** Fixed deterministic seed for Steve's charcoal hand-drawn fingerprint. */
  steveSeed: 17,
} as const

/** Geometry resolution and clamping limits. */
export const GEOMETRY = {
  bodySampleCount: 48,
  circleRx: 32.8,
  circleRy: 31.8,
  minSquash: -0.34,
  maxSquash: 0.42,
  minScaleYDenominator: 0.68,
  squashAnchorFactor: 28,
  pointerRangePx: 200,
  maxStaticCacheEntries: 256,
  opticalTargetArea: 3230,
} as const

/** Behind-the-back prop pull-out (start) and put-away (ending) transition constants. */
export const PROP_TRANSITION = {
  enterDurationSec: 0.44,
  exitDurationSec: 0.36,
  zFlipPhase: 0.48,
  arcSwingX: 40,
  arcLiftY: 13,
  backFadeThreshold: 0.16,
} as const
