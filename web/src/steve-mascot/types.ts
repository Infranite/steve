/**
 * Strongly typed contracts for Steve's finalized charcoal mascot:
 * classic circle silhouette + charcoal scribble + giant solid-cutout eyes,
 * theme-only Light/Dark palette switching, zero accessories or orbiters.
 */

import type { BotAccessory } from "../gen/BotAccessory.ts"

export type { BotAccessory }

export type ThemeMode = "light" | "dark"

export type OpticalTier = "micro" | "mini" | "compact" | "full"

/** Visual identity: Steve is always the monochrome classic circle with no accessories; ordinary Bots derive color, roundness, and optional accessory. */
export type MascotAppearance =
  | { readonly kind: "steve" }
  | {
      readonly kind: "bot"
      readonly id: string
      readonly color: string
      readonly round: number
      readonly accessory?: BotAccessory | null
    }

export type SustainedState = "idle" | "listening" | "thinking" | "sleeping"

export type ReactionId =
  | "reply"
  | "inspect"
  | "wary"
  | "success"
  | "affirm"
  | "failure"
  | "love"
  | "poke"
  | "wake"

export type IdleBehaviorId =
  | "head-tilt"
  | "look-around"
  | "yawn"
  | "wink"
  | "hum-note"
  | "sip-coffee"

export type MotionPriority = "low" | "normal" | "high" | "interrupt"

export type ExpressionKind =
  | "normal"
  | "curious"
  | "surprised-wide"
  | "think-squint"
  | "inspect-focus"
  | "wary-half"
  | "happy-arc"
  | "sleep-arc"
  | "dizzy-x"
  | "yawn"
  | "wink"

export type MouthKind = "none" | "speak" | "wavy" | "smile" | "yawn"

export type FxKind =
  | "none"
  | "listen-ear"
  | "think-bubbles"
  | "inspect-lens"
  | "wait-hourglass"
  | "speech-ticks"
  | "success-burst"
  | "love-heart"
  | "failure-zigs"
  | "sleep-zz"
  | "hum-note"
  | "sip-coffee"

export interface Vec2 {
  x: number
  y: number
}

export interface PaletteTokens {
  readonly ink: string
  readonly paper: string
  readonly wash: string
  readonly washOpacity: number
  readonly accent: string
}

export type PropTransitionStage = "entering" | "active" | "exiting"

export interface PropTransitionState {
  readonly fx: FxKind
  /** 0 = hidden behind back, 1 = fully presented in front. */
  readonly phase: number
  readonly stage: PropTransitionStage
  /** Pull-out / put-away flank side (+1 right, -1 left). */
  readonly side: 1 | -1
}

export interface MotionPose {
  readonly tx: number
  readonly ty: number
  readonly tsq: number
  readonly trot: number
  readonly faceOffsetX: number
  readonly faceOffsetY: number
  readonly expression: ExpressionKind
  readonly mouth: MouthKind
  readonly mouthOpen: number
  readonly mouthWidth: number
  readonly blushOpacity: number
  readonly fx: FxKind
  readonly sleepWeight: number
  readonly loveWeight: number
  readonly shiverAmp: number
  readonly propTransition?: PropTransitionState
}

export interface MotionSampleCtx {
  readonly t: number
  readonly elapsedSec: number
  readonly lookX: number
  readonly lookY: number
  readonly reducedMotion: boolean
}

export interface MotionImpulse {
  readonly vy?: number
  readonly vsq?: number
  readonly vrot?: number
  readonly pokeStrength?: number
  readonly pokeAngle?: number
}

export interface MotionClip {
  readonly id: SustainedState | ReactionId | IdleBehaviorId
  readonly kind: "sustained" | "oneshot" | "idle-behavior"
  readonly durationMs?: number
  readonly defaultPriority?: MotionPriority
  readonly impulse?: MotionImpulse
  sample(ctx: MotionSampleCtx): MotionPose
}

export interface FxStroke {
  readonly d: string
  readonly fill?: string
  readonly stroke?: string
  readonly strokeWidth: number
  readonly opacity: number
  readonly transform: string
}

export interface FxRenderCtx {
  readonly t: number
  readonly seed: number
  readonly theme: ThemeMode
  readonly reducedMotion: boolean
  readonly inTransit: boolean
  readonly colors: PaletteTokens
  readonly bounds: { readonly minX: number; readonly maxX: number; readonly minY: number; readonly maxY: number }
}

export interface FxRenderer {
  readonly id: Exclude<FxKind, "none">
  readonly attach: "body" | "stage"
  readonly stowable?: boolean
  readonly stowPivotY?: number
  render(ctx: FxRenderCtx): { readonly back: readonly FxStroke[]; readonly front: readonly FxStroke[] }
}

export interface CompiledCharcoalLayers {
  readonly washTransform: string
  readonly bodyFillOpacity: number
  readonly scribblePath: string
  readonly stroke1: string
  readonly stroke2: string
}

export interface CompiledFace {
  readonly transform: string
  readonly showOvalEyes: boolean
  readonly eyeLPath: string
  readonly eyeRPath: string
  readonly arcL: string
  readonly arcR: string
  readonly arcStrokeWidth: number
  readonly winkRightArc: string
  readonly mouthPath: string
  readonly mouthOpacity: number
  readonly mouthFilled: boolean
  readonly blushL: string
  readonly blushR: string
  readonly blushOpacity: number
}

export interface CompiledFrame {
  readonly viewHalf: number
  readonly tier: OpticalTier
  readonly theme: ThemeMode
  readonly colors: PaletteTokens
  readonly bodyTransform: string
  readonly cleanBodyPath: string
  readonly layers: CompiledCharcoalLayers
  readonly strokeWidthScale: number
  readonly enableToothFilter: boolean
  readonly floorTicks: readonly { readonly d: string; readonly opacity: number }[]
  readonly face: CompiledFace
  readonly accessory: BotAccessory | null
  readonly accessoryBack: readonly FxStroke[]
  readonly accessoryBody: readonly FxStroke[]
  readonly accessoryFace: readonly FxStroke[]
  readonly fxBack: readonly FxStroke[]
  readonly fxBackTransform: string
  readonly fxFront: readonly FxStroke[]
  readonly fxFrontTransform: string
}

export interface MascotMetrics {
  readonly rafRunning: boolean
  readonly rafFramesTotal: number
  readonly activeLiveInstances: number
  readonly registeredLiveInstances: number
  readonly lastFrameMs: number
  readonly avgFrameMs: number
  readonly staticCacheSize: number
  readonly staticCacheHits: number
  readonly staticCacheMisses: number
}
