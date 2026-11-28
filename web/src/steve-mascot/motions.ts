/**
 * Steve's finalized motion catalog — strictly the states, cue reactions,
 * and idle micro-behaviors used by Steve's web product.
 */

import type { MotionClip, MotionPose } from "./types.ts"

const BASE_POSE: MotionPose = {
  tx: 0,
  ty: 0,
  tsq: 0,
  trot: 0,
  faceOffsetX: 0,
  faceOffsetY: 0,
  expression: "normal",
  mouth: "none",
  mouthOpen: 0,
  mouthWidth: 1,
  blushOpacity: 0,
  fx: "none",
  sleepWeight: 0,
  loveWeight: 0,
  shiverAmp: 0,
}

function pose(overrides: Partial<MotionPose>): MotionPose {
  return { ...BASE_POSE, ...overrides }
}

function sampleSpeechEnvelope(t: number, reducedMotion: boolean) {
  if (reducedMotion) {
    return { open: 5.2, width: 1.0, norm: 0.55 }
  }
  const fastSyllable = Math.sin(t * 13.5)
  const midSyllable = Math.sin(t * 6.4 + 0.9)
  const phrase = 0.55 + 0.45 * Math.sin(t * 2.9 + 0.3)
  const rawOpen = Math.max(0, (fastSyllable * 0.62 + midSyllable * 0.38 + 0.22) * phrase)
  const norm = Math.min(1, rawOpen * 1.22)
  const width = 0.72 + 0.46 * (0.5 + 0.5 * Math.sin(t * 8.2 + 1.4))
  return { open: norm * 8.8, width, norm }
}

const CLIPS: readonly MotionClip[] = [
  /* ─────────────────────────── Sustained States ─────────────────────────── */
  {
    id: "idle",
    kind: "sustained",
    sample(ctx) {
      if (ctx.reducedMotion) return pose({})
      // Pure pointer-tracking rest pose so physics settles cleanly to 0-CPU dormancy when idle.
      return pose({
        tx: ctx.lookX * 4.0,
        ty: ctx.lookY * 2.2,
        trot: ctx.lookX * 4.8,
        faceOffsetX: ctx.lookX * 8.2,
        faceOffsetY: ctx.lookY * 5.4,
      })
    },
  },
  {
    id: "listening",
    kind: "sustained",
    impulse: { pokeStrength: 0.28, pokeAngle: -Math.PI / 2 },
    sample(ctx) {
      // Turned 3/4 head tilt presenting Steve's big right ear toward incoming input.
      if (ctx.reducedMotion) {
        return pose({
          tx: -4.0,
          trot: -16.5,
          faceOffsetX: -9.5,
          faceOffsetY: 0.5,
          expression: "curious",
          fx: "listen-ear",
        })
      }
      const nod = Math.sin(ctx.t * 2.6)
      return pose({
        tx: -4.2 + ctx.lookX * 2.0,
        ty: 1.2 + nod * 1.1,
        tsq: 0.035 + nod * 0.02,
        trot: -16.8 + nod * 2.4,
        faceOffsetX: -9.5 + nod * 0.8,
        faceOffsetY: 0.6 + ctx.lookY * 2.2,
        expression: "curious",
        fx: "listen-ear",
      })
    },
  },
  {
    id: "thinking",
    kind: "sustained",
    impulse: { vy: -32, vsq: -0.05 },
    sample(ctx) {
      if (ctx.reducedMotion) {
        return pose({
          trot: -5.5,
          faceOffsetX: 3.5,
          faceOffsetY: -2.8,
          expression: "think-squint",
          fx: "think-bubbles",
        })
      }
      return pose({
        tx: Math.cos(ctx.t * 1.4) * 2.0,
        ty: -2.8 + Math.sin(ctx.t * 2.4) * 2.0,
        tsq: Math.sin(ctx.t * 2.4 + 0.5) * 0.04,
        trot: -5.5 + Math.sin(ctx.t * 1.4) * 3.6,
        faceOffsetX: 3.2 + Math.sin(ctx.t * 1.8) * 3.4,
        faceOffsetY: -3.2 + Math.cos(ctx.t * 1.8) * 0.8,
        expression: "think-squint",
        fx: "think-bubbles",
      })
    },
  },
  {
    id: "sleeping",
    kind: "sustained",
    sample() {
      // Cozy melted resting posture; settles completely to 0-CPU dormancy once melted.
      return pose({
        ty: 3.4,
        tsq: 0.055,
        expression: "sleep-arc",
        sleepWeight: 1,
        fx: "sleep-zz",
      })
    },
  },

  /* ─────────────────────────── One-Shot Reactions ─────────────────────────── */
  {
    id: "reply",
    kind: "oneshot",
    durationMs: 2100,
    defaultPriority: "normal",
    impulse: { vy: -48, vsq: -0.06 },
    sample(ctx) {
      const sp = sampleSpeechEnvelope(ctx.elapsedSec, ctx.reducedMotion)
      if (ctx.reducedMotion) {
        return pose({
          mouth: "speak",
          mouthOpen: sp.open,
          mouthWidth: sp.width,
          fx: "speech-ticks",
        })
      }
      const env = Math.sin(Math.min(1, ctx.elapsedSec / 2.1) * Math.PI)
      return pose({
        tx: ctx.lookX * 2.6,
        ty: -sp.norm * 4.0 * env,
        tsq: (0.35 - sp.norm) * 0.12 * env,
        trot: Math.sin(ctx.elapsedSec * 4.4) * 4.2 * env,
        faceOffsetX: ctx.lookX * 5.8,
        faceOffsetY: ctx.lookY * 3.2 - sp.norm * 0.8 * env,
        mouth: "speak",
        mouthOpen: sp.open * Math.max(0.25, env),
        mouthWidth: sp.width,
        fx: "speech-ticks",
      })
    },
  },
  {
    id: "inspect",
    kind: "oneshot",
    durationMs: 2600,
    defaultPriority: "normal",
    sample(ctx) {
      // Magnifying glass pulls out from behind back, sweeps, then stows smoothly on exit.
      if (ctx.reducedMotion) {
        return pose({
          tx: -2.0,
          trot: 6.0,
          faceOffsetX: 4.5,
          faceOffsetY: -1.0,
          expression: "inspect-focus",
          fx: "inspect-lens",
        })
      }
      const sweep = Math.sin(ctx.elapsedSec * 2.2)
      return pose({
        tx: -2.4 + sweep * 2.6,
        ty: -1.6 + Math.cos(ctx.elapsedSec * 1.6) * 1.3,
        tsq: -0.03 + Math.sin(ctx.elapsedSec * 2.2) * 0.02,
        trot: 6.2 + sweep * 3.2,
        faceOffsetX: 4.6 + sweep * 3.0,
        faceOffsetY: -1.0 + Math.cos(ctx.elapsedSec * 1.6) * 1.4,
        expression: "inspect-focus",
        fx: "inspect-lens",
      })
    },
  },
  {
    id: "wary",
    kind: "oneshot",
    durationMs: 2600,
    defaultPriority: "normal",
    sample(ctx) {
      if (ctx.reducedMotion) {
        return pose({
          expression: "wary-half",
          fx: "wait-hourglass",
        })
      }
      const scan = Math.sin(ctx.elapsedSec * 2.2)
      return pose({
        tx: scan * 2.5,
        ty: 1.5 + Math.abs(Math.sin(ctx.elapsedSec * 3.2)) * 0.9,
        tsq: 0.03,
        trot: -scan * 4.0,
        faceOffsetX: scan * 6.0,
        faceOffsetY: -0.8,
        expression: "wary-half",
        fx: "wait-hourglass",
      })
    },
  },
  {
    id: "success",
    kind: "oneshot",
    durationMs: 1850,
    defaultPriority: "normal",
    impulse: { vy: -125, vsq: -0.14 },
    sample(ctx) {
      if (ctx.reducedMotion) {
        return pose({
          expression: "happy-arc",
          mouth: "smile",
          blushOpacity: 0.45,
          fx: "success-burst",
        })
      }
      const phase = (ctx.elapsedSec * 2.35) % 1
      const jump = Math.sin(phase * Math.PI)
      return pose({
        ty: -jump * 11.5,
        tsq: phase < 0.16 ? 0.19 : phase > 0.84 ? 0.17 : -jump * 0.12,
        trot: Math.sin(ctx.elapsedSec * 4.7) * 6,
        expression: "happy-arc",
        mouth: "smile",
        blushOpacity: 0.48,
        fx: "success-burst",
      })
    },
  },
  {
    id: "affirm",
    kind: "oneshot",
    durationMs: 1150,
    defaultPriority: "normal",
    impulse: { vy: 45, vsq: 0.09 },
    sample(ctx) {
      if (ctx.reducedMotion) {
        return pose({ expression: "happy-arc" })
      }
      const cycle = Math.min(1, ctx.elapsedSec / 0.85)
      const nod = Math.sin(cycle * Math.PI * 4) * (1 - cycle * 0.3)
      return pose({
        ty: Math.max(0, nod) * 6.0,
        tsq: Math.max(0, nod) * 0.12,
        trot: nod * 2.6,
        faceOffsetY: Math.max(0, nod) * 3.5,
        expression: cycle > 0.15 && cycle < 0.65 ? "happy-arc" : "normal",
      })
    },
  },
  {
    id: "failure",
    kind: "oneshot",
    durationMs: 1800,
    defaultPriority: "high",
    impulse: { pokeStrength: 0.95, pokeAngle: 0 },
    sample(ctx) {
      if (ctx.reducedMotion) {
        return pose({ expression: "dizzy-x", mouth: "wavy", fx: "failure-zigs" })
      }
      const decay = Math.max(0.25, 1 - ctx.elapsedSec / 1.8)
      return pose({
        tx: Math.sin(ctx.elapsedSec * 38) * 3.4 * decay,
        ty: 2.5,
        tsq: 0.07 + Math.sin(ctx.elapsedSec * 26) * 0.035 * decay,
        trot: Math.sin(ctx.elapsedSec * 32) * 3.8 * decay,
        expression: "dizzy-x",
        mouth: "wavy",
        fx: "failure-zigs",
        shiverAmp: decay,
      })
    },
  },
  {
    id: "love",
    kind: "oneshot",
    durationMs: 2400,
    defaultPriority: "high",
    impulse: { vy: -115, vsq: -0.12 },
    sample(ctx) {
      if (ctx.reducedMotion) {
        return pose({
          expression: "happy-arc",
          blushOpacity: 0.82,
          loveWeight: 1,
          fx: "love-heart",
        })
      }
      return pose({
        ty: -Math.abs(Math.sin(ctx.elapsedSec * 4.1)) * 5.2,
        tsq: Math.sin(ctx.elapsedSec * 8.2) * 0.085,
        trot: Math.sin(ctx.elapsedSec * 4.1) * 7.0,
        expression: "happy-arc",
        blushOpacity: 0.82,
        loveWeight: 1,
        fx: "love-heart",
      })
    },
  },
  {
    id: "poke",
    kind: "oneshot",
    durationMs: 650,
    defaultPriority: "low",
    impulse: { pokeStrength: 1.0, pokeAngle: -Math.PI / 2 },
    sample(ctx) {
      return pose({
        expression: ctx.elapsedSec < 0.28 ? "surprised-wide" : "normal",
      })
    },
  },
  {
    id: "wake",
    kind: "oneshot",
    durationMs: 1150,
    defaultPriority: "high",
    impulse: { vy: -80, vsq: -0.14 },
    sample(ctx) {
      if (ctx.reducedMotion) return pose({ expression: "curious" })
      const p = Math.min(1, ctx.elapsedSec / 0.9)
      const stretch = Math.sin(p * Math.PI)
      return pose({
        ty: -stretch * 6.5,
        tsq: -stretch * 0.14,
        expression: p < 0.35 ? "yawn" : "curious",
        mouth: p < 0.42 ? "yawn" : "none",
      })
    },
  },

  /* ─────────────────────────── Idle Micro-Behaviors ─────────────────────────── */
  {
    id: "head-tilt",
    kind: "idle-behavior",
    durationMs: 2100,
    sample(ctx) {
      const env = Math.sin(Math.min(1, ctx.elapsedSec / 2.1) * Math.PI)
      return pose({
        trot: env * 12.5,
        ty: env * 1.4,
        faceOffsetX: env * 2.5,
        expression: "curious",
      })
    },
  },
  {
    id: "look-around",
    kind: "idle-behavior",
    durationMs: 2400,
    sample(ctx) {
      const p = Math.min(1, ctx.elapsedSec / 2.4)
      const look = Math.sin(p * Math.PI * 2) * 8.5
      return pose({
        tx: look * 0.35,
        trot: look * 0.65,
        faceOffsetX: look,
        faceOffsetY: -1.8,
      })
    },
  },
  {
    id: "yawn",
    kind: "idle-behavior",
    durationMs: 2000,
    sample(ctx) {
      const env = Math.sin(Math.min(1, ctx.elapsedSec / 2.0) * Math.PI)
      return pose({
        ty: -env * 3.2,
        tsq: -env * 0.1,
        expression: env > 0.25 ? "yawn" : "normal",
        mouth: env > 0.2 ? "yawn" : "none",
      })
    },
  },
  {
    id: "wink",
    kind: "idle-behavior",
    durationMs: 1100,
    sample(ctx) {
      const env = Math.sin(Math.min(1, ctx.elapsedSec / 1.1) * Math.PI)
      return pose({
        trot: env * -6.0,
        ty: -env * 2.2,
        expression: env > 0.2 ? "wink" : "normal",
        blushOpacity: env * 0.32,
      })
    },
  },
  {
    id: "hum-note",
    kind: "idle-behavior",
    durationMs: 2600,
    sample(ctx) {
      const sway = Math.sin(ctx.elapsedSec * 5.2)
      return pose({
        tx: sway * 2.2,
        ty: -Math.abs(sway) * 2.4,
        trot: sway * 7.5,
        expression: "happy-arc",
        mouth: "smile",
        blushOpacity: 0.35,
        fx: "hum-note",
      })
    },
  },
  {
    id: "sip-coffee",
    kind: "idle-behavior",
    durationMs: 2800,
    sample(ctx) {
      const env = Math.sin(Math.min(1, ctx.elapsedSec / 2.8) * Math.PI)
      return pose({
        ty: env * 1.5,
        tsq: env * 0.04,
        trot: -env * 4.5,
        expression: env > 0.28 ? "happy-arc" : "normal",
        blushOpacity: env * 0.38,
        fx: "sip-coffee",
      })
    },
  },
]

const CLIP_MAP = new Map<string, MotionClip>(CLIPS.map((c) => [c.id, c]))

export function getMotionClip(id: string): MotionClip | undefined {
  return CLIP_MAP.get(id)
}
