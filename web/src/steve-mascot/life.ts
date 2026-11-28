/**
 * Autonomous life signals for live Steve:
 * wall-clock scheduled blinks, idle micro-behaviors, inactivity auto-sleep, and line boil.
 * Wall-clock deadlines allow the shared rAF loop to stop completely between events.
 */

import { TIMING } from "./constants.ts"
import { hashUnit } from "./geometry.ts"
import type { IdleBehaviorId } from "./types.ts"

const IDLE_BEHAVIOR_POOL: readonly IdleBehaviorId[] = [
  "head-tilt",
  "look-around",
  "yawn",
  "wink",
  "hum-note",
  "sip-coffee",
]

export class LifeLayer {
  blinkAmt = 0
  private blinkPhase = 0
  private nextBlinkAtMs: number
  private nextIdleBehaviorAtMs: number
  private rngCursor: number
  private lastActivityMs: number
  private readonly boilOffset: number

  constructor(seed: number, nowMs = Date.now()) {
    this.rngCursor = seed * 1.73 + 11
    this.boilOffset = Math.abs(Math.floor(seed)) % TIMING.boilFrameCount
    this.nextBlinkAtMs = nowMs + (1.8 + this.nextRandom() * 3.0) * 1000
    this.nextIdleBehaviorAtMs =
      nowMs +
      (TIMING.idleBehaviorMinGapSec +
        this.nextRandom() * (TIMING.idleBehaviorMaxGapSec - TIMING.idleBehaviorMinGapSec)) *
        1000
    this.lastActivityMs = nowMs
  }

  private nextRandom(): number {
    this.rngCursor += 1.6180339
    return hashUnit(this.rngCursor)
  }

  noteActivity(nowMs = Date.now()): void {
    this.lastActivityMs = nowMs
  }

  isBlinking(): boolean {
    return this.blinkPhase > 0
  }

  /** Compute deterministic hand-drawn charcoal boil frame index (0..3). */
  getBoilFrame(tSec: number, activeMotion: boolean, reducedMotion: boolean): number {
    if (reducedMotion || !activeMotion || TIMING.boilFps <= 0) return 0
    return (Math.floor(tSec * TIMING.boilFps) + this.boilOffset) % TIMING.boilFrameCount
  }

  /** Advance active blink or trigger when wall-clock deadline arrives. */
  stepBlink(dt: number, nowMs: number, allowBlink: boolean): number {
    if (!allowBlink) {
      this.blinkPhase = 0
      this.blinkAmt = 0
      return 0
    }
    if (this.blinkPhase === 0 && nowMs >= this.nextBlinkAtMs) {
      this.blinkPhase = 0.001
    }
    if (this.blinkPhase > 0) {
      this.blinkPhase += dt * TIMING.blinkSpeedRadPerSec
      if (this.blinkPhase >= Math.PI) {
        this.blinkPhase = 0
        this.blinkAmt = 0
        const gapSec =
          this.nextRandom() < TIMING.doubleBlinkChance
            ? TIMING.doubleBlinkGapSec
            : TIMING.blinkMinGapSec +
              this.nextRandom() * (TIMING.blinkMaxGapSec - TIMING.blinkMinGapSec)
        this.nextBlinkAtMs = nowMs + gapSec * 1000
      } else {
        this.blinkAmt = Math.sin(this.blinkPhase)
      }
    }
    return this.blinkAmt
  }

  /** Check whether inactivity timeout has expired. */
  shouldAutoSleep(nowMs: number, autoSleepMs: number): boolean {
    if (autoSleepMs <= 0) return false
    return nowMs - this.lastActivityMs >= autoSleepMs
  }

  /** Return an autonomous idle micro-behavior when due in calm idle. */
  stepIdleBehavior(nowMs: number, isCalmIdle: boolean): IdleBehaviorId | null {
    if (!isCalmIdle) {
      if (this.nextIdleBehaviorAtMs < nowMs + 3000) {
        this.nextIdleBehaviorAtMs = nowMs + 4500
      }
      return null
    }
    if (nowMs < this.nextIdleBehaviorAtMs) return null

    const gapSec =
      TIMING.idleBehaviorMinGapSec +
      this.nextRandom() * (TIMING.idleBehaviorMaxGapSec - TIMING.idleBehaviorMinGapSec)
    this.nextIdleBehaviorAtMs = nowMs + gapSec * 1000
    const idx = Math.floor(this.nextRandom() * IDLE_BEHAVIOR_POOL.length)
    return IDLE_BEHAVIOR_POOL[idx] ?? "head-tilt"
  }

  /** Milliseconds until the next scheduled idle event (blink, micro-behavior, or auto-sleep). */
  getNextIdleWakeDelayMs(nowMs: number, autoSleepMs: number): number {
    const candidates = [this.nextBlinkAtMs - nowMs, this.nextIdleBehaviorAtMs - nowMs]
    if (autoSleepMs > 0) {
      candidates.push(this.lastActivityMs + autoSleepMs - nowMs)
    }
    let minDelay = Number.POSITIVE_INFINITY
    for (const delay of candidates) {
      if (delay < minDelay) minDelay = delay
    }
    if (!Number.isFinite(minDelay)) return 10_000
    return Math.max(40, Math.ceil(minDelay))
  }
}
