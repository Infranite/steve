/**
 * Named architectural constants for Steve's charcoal mascot.
 * Every color token, optical tier, spring parameter, and timing window is declared here.
 */

import type { OpticalTier, PaletteTokens, ThemeMode } from "./types.ts"

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
  },
  compact: {
    maxSizePx: 52,
    viewHalf: 44,
    strokeWidthScale: 1.0,
    eyeSizeBoost: 1.04,
    bodyFillOpacity: 0.3,
    enableWash: true,
    enableScribble: true,
    enableSecondRim: true,
    enableToothFilter: true,
    enableFloorShadow: false,
  },
  full: {
    maxSizePx: Number.POSITIVE_INFINITY,
    viewHalf: 48,
    strokeWidthScale: 1.0,
    eyeSizeBoost: 1.0,
    bodyFillOpacity: 0.28,
    enableWash: true,
    enableScribble: true,
    enableSecondRim: true,
    enableToothFilter: true,
    enableFloorShadow: true,
  },
}

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
  maxStaticCacheEntries: 128,
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
