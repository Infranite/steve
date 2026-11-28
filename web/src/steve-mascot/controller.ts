/**
 * Live interactive controller for Steve's charcoal mascot.
 * Combines demand-driven 60fps physics, cue choreography, pointer following,
 * idle micro-behaviors, auto-sleep/wake, reduced-motion compliance, and
 * lightweight timer wakeups so idle CPU stays at 0%.
 */

import type { HeroCue } from "../bus.ts"
import { CueChoreographer } from "./choreographer.ts"
import { GEOMETRY, TIMING, resolveOpticalTier } from "./constants.ts"
import { compileFrame } from "./frame.ts"
import { clamp } from "./geometry.ts"
import { LifeLayer } from "./life.ts"
import { PhysicsRig, type PhysicsTargets } from "./physics.ts"
import { scheduler, type ScheduledMascot } from "./scheduler.ts"
import { appearanceSeed } from "./static-svg.ts"
import { SteveSvgRenderer } from "./svg-renderer.ts"
import type { MascotAppearance, ThemeMode } from "./types.ts"

let nextUid = 1

export class SteveMascotController implements ScheduledMascot {
  readonly element: HTMLElement
  private readonly renderer: SteveSvgRenderer
  private readonly physics = new PhysicsRig()
  private readonly life: LifeLayer
  private readonly choreographer: CueChoreographer
  private readonly unregister: () => void

  private appearance: MascotAppearance
  private seed: number
  private sizePx: number
  private theme: ThemeMode
  private reducedMotion: boolean
  private dirty = true
  private wakeTimer: ReturnType<typeof setTimeout> | null = null
  private disposed = false

  constructor(opts: {
    container: HTMLElement
    appearance?: MascotAppearance
    size: number
    theme: ThemeMode
    reducedMotion: boolean
  }) {
    this.element = opts.container
    this.appearance = opts.appearance ?? { kind: "steve" }
    this.seed = appearanceSeed(this.appearance)
    this.sizePx = opts.size
    this.theme = opts.theme
    this.reducedMotion = opts.reducedMotion

    const nowMs = Date.now()
    this.life = new LifeLayer(this.seed, nowMs)
    this.choreographer = new CueChoreographer((imp) => {
      if (!this.reducedMotion) this.physics.applyImpulse(imp)
    })

    const uid = `sml${nextUid++}`
    this.renderer = new SteveSvgRenderer(uid, this.sizePx, this.appearance.kind)
    this.element.replaceChildren(this.renderer.svg)
    this.element.addEventListener("pointerdown", this.handlePointerDown)

    this.renderImmediate(nowMs)
    this.unregister = scheduler.register(this)
    this.scheduleNextIdleTimer(nowMs)
  }

  setAppearance(next: MascotAppearance): void {
    const prev = this.appearance
    if (
      prev.kind === next.kind &&
      (prev.kind === "steve" ||
        (next.kind === "bot" &&
          prev.id === next.id &&
          prev.color === next.color &&
          prev.round === next.round &&
          (prev.accessory ?? null) === (next.accessory ?? null)))
    ) {
      return
    }
    this.appearance = next
    this.seed = appearanceSeed(next)
    if (this.reducedMotion) {
      this.renderImmediate(Date.now())
    } else {
      this.markDirtyAndWake()
    }
  }

  setSize(sizePx: number): void {
    if (this.sizePx === sizePx) return
    this.sizePx = sizePx
    this.renderer.setSize(sizePx)
    this.markDirtyAndWake()
  }

  setTheme(theme: ThemeMode): void {
    if (this.theme === theme) return
    this.theme = theme
    this.markDirtyAndWake()
  }

  setReducedMotion(reduced: boolean): void {
    if (this.reducedMotion === reduced) return
    this.reducedMotion = reduced
    if (reduced) {
      this.clearIdleTimer()
      this.renderImmediate(Date.now())
    } else {
      this.markDirtyAndWake()
    }
  }

  /** Handle an event from Steve's web cue bus (`web/src/bus.ts`). */
  applyCue(cue: HeroCue): void {
    const nowMs = Date.now()
    this.life.noteActivity(nowMs)
    this.choreographer.applyHeroCue(cue)
    if (this.reducedMotion) {
      this.renderImmediate(nowMs)
      return
    }
    this.markDirtyAndWake()
  }

  needsFrame(): boolean {
    if (this.disposed) return false
    if (this.dirty) return true
    if (this.reducedMotion) return false
    if (this.choreographer.needsContinuousFrame()) return true
    if (this.life.isBlinking()) return true
    const look = this.computeLookTarget()
    const restTargets: PhysicsTargets =
      this.choreographer.getState() === "sleeping"
        ? { tx: 0, ty: 3.4, tsq: 0.055, trot: 0, faceX: 0, faceY: 0 }
        : {
            tx: look.x * 4.0,
            ty: look.y * 2.2,
            tsq: 0,
            trot: look.x * 4.8,
            faceX: look.x * 8.2,
            faceY: look.y * 5.4,
          }
    if (!this.physics.isSettled(restTargets)) return true
    if (
      this.choreographer.getState() === "idle" &&
      scheduler.pointer.active &&
      Date.now() - scheduler.pointer.lastMoveMs < TIMING.pointerIdleSettleMs
    ) {
      return true
    }
    return false
  }

  onPointerActivity(nowMs: number): void {
    if (this.disposed || this.reducedMotion) return
    this.life.noteActivity(nowMs)
    if (this.choreographer.getState() === "sleeping") {
      this.choreographer.setState("idle")
      this.choreographer.react("wake", "high")
    }
    this.dirty = true
  }

  onVisibilityChange(visible: boolean, nowMs: number): void {
    if (this.disposed) return
    if (!visible) {
      this.clearIdleTimer()
      return
    }
    this.life.noteActivity(nowMs)
    this.scheduleNextIdleTimer(nowMs)
  }

  onFrame(dtSec: number, nowSec: number, nowMs: number): boolean {
    if (this.disposed) return false
    this.dirty = false

    // Auto-sleep check while awake and calm
    if (
      !this.reducedMotion &&
      this.choreographer.isCalmIdle() &&
      this.life.shouldAutoSleep(nowMs, TIMING.autoSleepMs)
    ) {
      this.choreographer.setState("sleeping")
    }

    // Autonomous idle micro-behavior check when calm
    if (!this.reducedMotion && this.choreographer.isCalmIdle()) {
      const behavior = this.life.stepIdleBehavior(nowMs, true)
      if (behavior) {
        this.choreographer.triggerIdleBehavior(behavior)
      }
    }

    const look = this.reducedMotion ? { x: 0, y: 0 } : this.computeLookTarget()
    const pose = this.choreographer.step(dtSec, nowSec, look.x, look.y, this.reducedMotion)
    const targets: PhysicsTargets = {
      tx: pose.tx,
      ty: pose.ty,
      tsq: pose.tsq,
      trot: pose.trot,
      faceX: pose.faceOffsetX,
      faceY: pose.faceOffsetY,
    }

    if (this.reducedMotion) {
      this.physics.snapTo(targets)
    } else {
      this.physics.step(dtSec, targets)
    }

    const allowBlink =
      !this.reducedMotion &&
      pose.expression !== "happy-arc" &&
      pose.expression !== "sleep-arc" &&
      pose.expression !== "dizzy-x" &&
      pose.expression !== "yawn" &&
      pose.expression !== "wink"
    const blinkAmt = this.life.stepBlink(dtSec, nowMs, allowBlink)

    const stillMoving = !this.reducedMotion && (this.choreographer.needsContinuousFrame() || this.life.isBlinking() || !this.physics.isSettled(targets))
    if (!stillMoving && !this.reducedMotion) {
      // Snap sub-pixel tail cleanly before entering 0-CPU dormancy.
      this.physics.snapTo(targets)
    }

    const boilFrame = this.life.getBoilFrame(nowSec, stillMoving, this.reducedMotion)
    const frame = compileFrame({
      appearance: this.appearance,
      theme: this.theme,
      tier: resolveOpticalTier(this.sizePx),
      live: true,
      pose,
      physics: this.physics,
      sleepAmt: this.choreographer.sleepAmt,
      loveAmt: this.choreographer.loveAmt,
      blinkAmt,
      boilFrame,
      tSec: nowSec,
      seed: this.seed,
      reducedMotion: this.reducedMotion,
      activeMotion: stillMoving,
    })
    this.renderer.render(frame)

    const needsMore =
      stillMoving ||
      (!this.reducedMotion &&
        this.choreographer.getState() === "idle" &&
        scheduler.pointer.active &&
        nowMs - scheduler.pointer.lastMoveMs < TIMING.pointerIdleSettleMs)

    if (!needsMore) {
      this.scheduleNextIdleTimer(nowMs)
    } else {
      this.clearIdleTimer()
    }
    return needsMore
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.clearIdleTimer()
    this.element.removeEventListener("pointerdown", this.handlePointerDown)
    this.unregister()
    this.renderer.svg.remove()
  }

  private readonly handlePointerDown = (e: PointerEvent): void => {
    const nowMs = Date.now()
    this.life.noteActivity(nowMs)
    if (this.choreographer.getState() === "sleeping") {
      this.choreographer.setState("idle")
      this.choreographer.react("wake", "high")
    } else if (!this.reducedMotion) {
      const r = this.renderer.svg.getBoundingClientRect()
      const angle = Math.atan2(e.clientY - (r.top + r.height / 2), e.clientX - (r.left + r.width / 2))
      this.physics.poke(angle, 0.9)
      this.choreographer.react("poke", "low")
    }
    if (this.reducedMotion) {
      this.renderImmediate(nowMs)
    } else {
      this.markDirtyAndWake()
    }
  }

  private computeLookTarget(): { x: number; y: number } {
    if (!scheduler.pointer.active) return { x: 0, y: 0 }
    const rect = this.renderer.svg.getBoundingClientRect()
    if (rect.width === 0 || rect.height === 0) return { x: 0, y: 0 }
    const cx = rect.left + rect.width / 2
    const cy = rect.top + rect.height / 2
    return {
      x: clamp((scheduler.pointer.clientX - cx) / GEOMETRY.pointerRangePx, -1, 1),
      y: clamp((scheduler.pointer.clientY - cy) / GEOMETRY.pointerRangePx, -1, 1),
    }
  }

  private renderImmediate(nowMs: number): void {
    const nowSec = nowMs / 1000
    const pose = this.choreographer.step(0, nowSec, 0, 0, true)
    this.physics.snapTo({
      tx: pose.tx,
      ty: pose.ty,
      tsq: pose.tsq,
      trot: pose.trot,
      faceX: pose.faceOffsetX,
      faceY: pose.faceOffsetY,
    })
    const frame = compileFrame({
      appearance: this.appearance,
      theme: this.theme,
      tier: resolveOpticalTier(this.sizePx),
      live: true,
      pose,
      physics: this.physics,
      sleepAmt: this.choreographer.sleepAmt,
      loveAmt: this.choreographer.loveAmt,
      blinkAmt: 0,
      boilFrame: 0,
      tSec: nowSec,
      seed: this.seed,
      reducedMotion: this.reducedMotion,
      activeMotion: false,
    })
    this.renderer.render(frame)
    this.dirty = false
  }

  private markDirtyAndWake(): void {
    this.dirty = true
    this.clearIdleTimer()
    if (scheduler.isEntryVisible(this)) {
      scheduler.wake()
    }
  }

  private scheduleNextIdleTimer(nowMs: number): void {
    this.clearIdleTimer()
    if (this.disposed || this.reducedMotion) return
    if (!scheduler.isEntryVisible(this)) return
    if (!this.choreographer.isCalmIdle()) return

    const delayMs = this.life.getNextIdleWakeDelayMs(nowMs, TIMING.autoSleepMs)
    this.wakeTimer = setTimeout(() => {
      this.wakeTimer = null
      if (this.disposed || this.reducedMotion || !scheduler.isEntryVisible(this)) return
      this.dirty = true
      scheduler.wake()
    }, delayMs)
  }

  private clearIdleTimer(): void {
    if (this.wakeTimer !== null) {
      clearTimeout(this.wakeTimer)
      this.wakeTimer = null
    }
  }
}
