/**
 * Single page-wide demand-driven animation clock + global pointer tracker +
 * IntersectionObserver viewport dormancy + Page Visibility dormancy.
 *
 * Zero idle CPU: when no visible avatar reports `needsFrame() === true`, the
 * `requestAnimationFrame` loop stops completely (0 ticks/sec) until woken by
 * pointer movement, a cue event, a timer deadline, or returning to the viewport.
 */

import { TIMING } from "./constants.ts"
import { getStaticCacheStats } from "./static-svg.ts"
import type { MascotMetrics } from "./types.ts"

export interface ScheduledMascot {
  readonly element: Element
  /** Return true when this instance currently needs continuous frame stepping. */
  needsFrame(): boolean
  /** Advance simulation and patch DOM; return true if another frame is needed. */
  onFrame(dtSec: number, nowSec: number, nowMs: number): boolean
  /** Notified when global pointer moves while this instance is visible. */
  onPointerActivity?(nowMs: number): void
  /** Notified when viewport visibility or document visibility changes. */
  onVisibilityChange?(visible: boolean, nowMs: number): void
}

export interface GlobalPointerState {
  clientX: number
  clientY: number
  active: boolean
  lastMoveMs: number
}

class MascotScheduler {
  private readonly entries = new Set<ScheduledMascot>()
  private readonly inViewport = new WeakMap<Element, boolean>()
  private observer: IntersectionObserver | null = null
  private rafId: number | null = null
  private lastTs = 0
  private listenersBound = false
  private tabVisible = true

  private rafFramesTotal = 0
  private lastFrameMs = 0
  private avgFrameMs = 0

  readonly pointer: GlobalPointerState = {
    clientX: 0,
    clientY: 0,
    active: false,
    lastMoveMs: 0,
  }

  register(entry: ScheduledMascot): () => void {
    this.entries.add(entry)
    this.inViewport.set(entry.element, true)
    this.ensureGlobalListeners()
    this.ensureObserver()
    this.observer?.observe(entry.element)
    if (this.isEntryVisible(entry) && entry.needsFrame()) {
      this.wake()
    }

    return () => {
      this.entries.delete(entry)
      this.observer?.unobserve(entry.element)
      if (!this.hasAnyWork()) {
        this.stopLoop()
      }
    }
  }

  /** Wake the shared rAF clock if any visible entry needs frames. */
  wake(): void {
    if (!this.tabVisible) return
    if (this.rafId !== null || typeof requestAnimationFrame === "undefined") return
    if (!this.hasAnyWork()) return

    this.lastTs = performance.now()
    const tick = (now: number) => {
      this.rafId = null
      if (!this.tabVisible || this.entries.size === 0) return

      const dt = Math.min(TIMING.maxFrameDtSec, Math.max(0.001, (now - this.lastTs) / 1000))
      this.lastTs = now
      const tSec = now / 1000
      const wallNow = Date.now()

      const start = performance.now()
      let anyNeedsMore = false
      for (const entry of this.entries) {
        if (!this.isEntryVisible(entry)) continue
        if (!entry.needsFrame()) continue
        if (entry.onFrame(dt, tSec, wallNow)) {
          anyNeedsMore = true
        }
      }
      const elapsed = performance.now() - start
      this.rafFramesTotal++
      this.lastFrameMs = elapsed
      this.avgFrameMs = this.avgFrameMs === 0 ? elapsed : this.avgFrameMs * 0.9 + elapsed * 0.1

      if (anyNeedsMore && this.tabVisible && this.entries.size > 0) {
        this.rafId = requestAnimationFrame(tick)
      }
    }
    this.rafId = requestAnimationFrame(tick)
  }

  isEntryVisible(entry: ScheduledMascot): boolean {
    return this.tabVisible && this.inViewport.get(entry.element) !== false
  }

  getMetrics(): MascotMetrics {
    let activeLiveInstances = 0
    for (const entry of this.entries) {
      if (this.isEntryVisible(entry) && entry.needsFrame()) activeLiveInstances++
    }
    const cache = getStaticCacheStats()
    return {
      rafRunning: this.rafId !== null,
      rafFramesTotal: this.rafFramesTotal,
      activeLiveInstances,
      registeredLiveInstances: this.entries.size,
      lastFrameMs: this.lastFrameMs,
      avgFrameMs: this.avgFrameMs,
      staticCacheSize: cache.size,
      staticCacheHits: cache.hits,
      staticCacheMisses: cache.misses,
    }
  }

  private hasAnyWork(): boolean {
    if (!this.tabVisible) return false
    for (const entry of this.entries) {
      if (this.isEntryVisible(entry) && entry.needsFrame()) return true
    }
    return false
  }

  private ensureGlobalListeners(): void {
    if (this.listenersBound || typeof window === "undefined") return
    this.listenersBound = true
    this.tabVisible = typeof document === "undefined" || document.visibilityState !== "hidden"

    window.addEventListener(
      "pointermove",
      (e) => {
        const nowMs = Date.now()
        this.pointer.clientX = e.clientX
        this.pointer.clientY = e.clientY
        this.pointer.active = true
        this.pointer.lastMoveMs = nowMs
        for (const entry of this.entries) {
          if (this.isEntryVisible(entry)) {
            entry.onPointerActivity?.(nowMs)
          }
        }
        this.wake()
      },
      { passive: true },
    )

    window.addEventListener("pointerleave", () => {
      const nowMs = Date.now()
      this.pointer.active = false
      this.pointer.lastMoveMs = nowMs
      for (const entry of this.entries) {
        if (this.isEntryVisible(entry)) {
          entry.onPointerActivity?.(nowMs)
        }
      }
      this.wake()
    })

    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", () => {
        const nextVisible = document.visibilityState !== "hidden"
        if (nextVisible === this.tabVisible) return
        this.tabVisible = nextVisible
        const nowMs = Date.now()
        for (const entry of this.entries) {
          entry.onVisibilityChange?.(this.isEntryVisible(entry), nowMs)
        }
        if (nextVisible) {
          this.wake()
        } else {
          this.stopLoop()
        }
      })
    }
  }

  private ensureObserver(): void {
    if (this.observer || typeof IntersectionObserver === "undefined") return
    this.observer = new IntersectionObserver(
      (records) => {
        const nowMs = Date.now()
        for (const rec of records) {
          this.inViewport.set(rec.target, rec.isIntersecting)
          for (const entry of this.entries) {
            if (entry.element === rec.target) {
              entry.onVisibilityChange?.(this.isEntryVisible(entry), nowMs)
            }
          }
        }
        if (this.hasAnyWork()) {
          this.wake()
        } else {
          this.stopLoop()
        }
      },
      { rootMargin: "100px" },
    )
  }

  private stopLoop(): void {
    if (this.rafId !== null && typeof cancelAnimationFrame !== "undefined") {
      cancelAnimationFrame(this.rafId)
    }
    this.rafId = null
  }
}

export const scheduler = new MascotScheduler()

/** Read structured runtime performance & cache telemetry for Steve's mascot subsystem. */
export function getMascotMetrics(): MascotMetrics {
  return scheduler.getMetrics()
}
