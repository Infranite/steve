/**
 * Motion choreographer for Steve's web cue bus (`web/src/bus.ts`).
 * Maps all 10 HeroCue signals cleanly onto sustained states, bounded reactions,
 * idle micro-behaviors, sleep/wake, and behind-the-back prop transitions.
 */

import type { HeroCue } from "../bus.ts"
import { PROP_TRANSITION } from "./constants.ts"
import { isStowableFx } from "./fx.ts"
import { getMotionClip } from "./motions.ts"
import type {
  FxKind,
  IdleBehaviorId,
  MotionClip,
  MotionImpulse,
  MotionPose,
  MotionPriority,
  PropTransitionState,
  ReactionId,
  SustainedState,
} from "./types.ts"

const PRIORITY_RANK: Record<MotionPriority, number> = {
  low: 1,
  normal: 2,
  high: 3,
  interrupt: 4,
}

interface ActiveOneShot {
  readonly id: ReactionId | IdleBehaviorId
  readonly clip: MotionClip
  readonly isIdleBehavior: boolean
  readonly priority: MotionPriority
  startedAtSec: number | null
  readonly durationSec: number
}

export class CueChoreographer {
  private state: SustainedState = "idle"
  private stateStartedAtSec: number | null = null
  private activeOneShot: ActiveOneShot | null = null

  sleepAmt = 0
  loveAmt = 0
  private propState: PropTransitionState | null = null
  private nextPropSide: 1 | -1 = 1
  private readonly onImpulse: (imp: MotionImpulse) => void

  constructor(onImpulse: (imp: MotionImpulse) => void) {
    this.onImpulse = onImpulse
  }

  getState(): SustainedState {
    return this.state
  }

  getActiveOneShotId(): ReactionId | IdleBehaviorId | null {
    return this.activeOneShot ? this.activeOneShot.id : null
  }

  getPropTransition(): PropTransitionState | null {
    return this.propState ? { ...this.propState } : null
  }

  isCalmIdle(): boolean {
    return this.state === "idle" && this.activeOneShot === null && this.propState === null
  }

  /** Whether the choreographer itself requires continuous frame stepping. */
  needsContinuousFrame(): boolean {
    if (this.activeOneShot !== null) return true
    if (this.state === "listening" || this.state === "thinking") return true
    if (this.propState !== null && this.propState.stage !== "active") return true
    const targetSleep = this.state === "sleeping" ? 1 : 0
    if (Math.abs(this.sleepAmt - targetSleep) > 0.005) return true
    if (this.loveAmt > 0.005) return true
    return false
  }

  /** Set a sustained state (`idle`, `listening`, `thinking`, `sleeping`). */
  setState(next: SustainedState): void {
    const prev = this.state
    this.state = next
    if (prev !== next) {
      this.stateStartedAtSec = null
    }
    if (this.activeOneShot?.isIdleBehavior) {
      this.activeOneShot = null
    }
    if (next === "sleeping" && this.activeOneShot) {
      this.activeOneShot = null
    }
    if (prev !== next) {
      const clip = getMotionClip(next)
      if (clip?.impulse) this.onImpulse(clip.impulse)
    }
  }

  /** Trigger a prioritized one-shot reaction. */
  react(reaction: ReactionId, priorityOverride?: MotionPriority): boolean {
    const clip = getMotionClip(reaction)
    if (!clip) return false
    const priority = priorityOverride ?? clip.defaultPriority ?? "normal"

    if (this.activeOneShot && !this.activeOneShot.isIdleBehavior) {
      if (PRIORITY_RANK[priority] < PRIORITY_RANK[this.activeOneShot.priority]) {
        return false
      }
    }

    this.activeOneShot = {
      id: reaction,
      clip,
      isIdleBehavior: false,
      priority,
      startedAtSec: null,
      durationSec: (clip.durationMs ?? 1500) / 1000,
    }
    if (clip.impulse) this.onImpulse(clip.impulse)
    return true
  }

  /** Trigger a low-priority autonomous idle micro-behavior when calm. */
  triggerIdleBehavior(behavior: IdleBehaviorId): boolean {
    if (!this.isCalmIdle()) return false
    const clip = getMotionClip(behavior)
    if (!clip) return false
    this.activeOneShot = {
      id: behavior,
      clip,
      isIdleBehavior: true,
      priority: "low",
      startedAtSec: null,
      durationSec: (clip.durationMs ?? 1800) / 1000,
    }
    if (clip.impulse) this.onImpulse(clip.impulse)
    return true
  }

  /**
   * Exhaustive mapping of all 10 Steve web cue bus events (`web/src/bus.ts`).
   * Sustained cues (`input`, `think`) stay active until replaced or `idle`;
   * bounded cues return smoothly to calm `idle` so Steve never gets stuck mid-emote.
   */
  applyHeroCue(cue: HeroCue): void {
    // Explicit web bus cues always supersede any prior cue reaction.
    this.activeOneShot = null
    switch (cue) {
      case "input":
        this.setState("listening")
        return
      case "think":
        this.setState("thinking")
        return
      case "idle":
        this.setState("idle")
        return
      case "reply":
        this.setState("idle")
        this.react("reply", "high")
        return
      case "inspect":
        this.setState("idle")
        this.react("inspect", "high")
        return
      case "wary":
        this.setState("idle")
        this.react("wary", "high")
        return
      case "success":
        this.setState("idle")
        this.react("success", "high")
        return
      case "affirm":
        this.setState("idle")
        this.react("affirm", "high")
        return
      case "failure":
        this.setState("idle")
        this.react("failure", "interrupt")
        return
      case "love":
        this.setState("idle")
        this.react("love", "high")
        return
    }
  }

  /** Advance one-shots, morph weights, and behind-the-back prop transitions, returning the sampled pose. */
  step(dt: number, tSec: number, lookX: number, lookY: number, reducedMotion: boolean): MotionPose {
    if (this.stateStartedAtSec === null) {
      this.stateStartedAtSec = tSec
    }

    if (this.activeOneShot) {
      if (this.activeOneShot.startedAtSec === null) {
        this.activeOneShot.startedAtSec = tSec
      }
      if (tSec - this.activeOneShot.startedAtSec >= this.activeOneShot.durationSec) {
        this.activeOneShot = null
      }
    }

    const active = this.activeOneShot
    const clip = active ? active.clip : (getMotionClip(this.state) ?? getMotionClip("idle")!)
    const elapsedSec = active
      ? tSec - (active.startedAtSec ?? tSec)
      : tSec - this.stateStartedAtSec

    const pose = clip.sample({
      t: tSec,
      elapsedSec: Math.max(0, elapsedSec),
      lookX,
      lookY,
      reducedMotion,
    })

    const blend = reducedMotion ? 1 : Math.min(1, dt * 6.5)
    this.sleepAmt += (pose.sleepWeight - this.sleepAmt) * blend
    if (Math.abs(pose.sleepWeight - this.sleepAmt) < 0.002) this.sleepAmt = pose.sleepWeight
    this.loveAmt += (pose.loveWeight - this.loveAmt) * blend
    if (Math.abs(pose.loveWeight - this.loveAmt) < 0.002) this.loveAmt = pose.loveWeight

    const desiredProp: FxKind | null = pose.fx !== "none" && isStowableFx(pose.fx) ? pose.fx : null
    const propTransition = this.stepPropTransition(dt, desiredProp, reducedMotion)
    if (!propTransition) {
      return pose
    }

    let extraTrot = 0
    let extraTx = 0
    let extraTsq = 0
    if (propTransition.stage === "entering" || propTransition.stage === "exiting") {
      const arcEnv = Math.sin(propTransition.phase * Math.PI)
      extraTrot = -propTransition.side * arcEnv * 8.5
      extraTx = -propTransition.side * arcEnv * 2.6
      if (propTransition.stage === "entering" && propTransition.phase > 0.8) {
        extraTsq = Math.sin(((propTransition.phase - 0.8) / 0.2) * Math.PI) * 0.085
      }
    }

    const suppressIncoming = desiredProp !== null && propTransition.fx !== desiredProp
    return {
      ...pose,
      tx: pose.tx + extraTx,
      tsq: pose.tsq + extraTsq,
      trot: pose.trot + extraTrot,
      fx: suppressIncoming ? "none" : pose.fx,
      propTransition: { ...propTransition },
    }
  }

  private stepPropTransition(
    dt: number,
    desiredProp: FxKind | null,
    reducedMotion: boolean,
  ): PropTransitionState | null {
    if (reducedMotion) {
      this.propState = desiredProp ? { fx: desiredProp, phase: 1, stage: "active", side: 1 } : null
      return this.propState
    }

    const enterStep = dt / PROP_TRANSITION.enterDurationSec
    const exitStep = dt / PROP_TRANSITION.exitDurationSec

    if (!this.propState) {
      if (desiredProp) {
        const side = this.nextPropSide
        this.nextPropSide = side === 1 ? -1 : 1
        this.propState = {
          fx: desiredProp,
          phase: Math.min(1, enterStep),
          stage: enterStep >= 1 ? "active" : "entering",
          side,
        }
      }
      return this.propState
    }

    if (desiredProp === this.propState.fx) {
      if (this.propState.stage !== "active") {
        const nextPhase = Math.min(1, this.propState.phase + enterStep)
        this.propState = {
          ...this.propState,
          phase: nextPhase,
          stage: nextPhase >= 1 ? "active" : "entering",
        }
      }
      return this.propState
    }

    const nextPhase = Math.max(0, this.propState.phase - exitStep)
    if (nextPhase <= 0) {
      if (desiredProp) {
        const side = this.nextPropSide
        this.nextPropSide = side === 1 ? -1 : 1
        this.propState = {
          fx: desiredProp,
          phase: Math.min(1, enterStep),
          stage: enterStep >= 1 ? "active" : "entering",
          side,
        }
      } else {
        this.propState = null
      }
      return this.propState
    }

    this.propState = {
      ...this.propState,
      phase: nextPhase,
      stage: "exiting",
    }
    return this.propState
  }
}
