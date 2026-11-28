/**
 * Volume-conserving spring physics rig for live Steve animations.
 * Supports smooth settling detection so the shared rAF clock stops completely at rest.
 */

import { GEOMETRY, SPRING, TIMING } from "./constants.ts"
import { clamp } from "./geometry.ts"
import type { MotionImpulse } from "./types.ts"

export interface PhysicsTargets {
  readonly tx: number
  readonly ty: number
  readonly tsq: number
  readonly trot: number
  readonly faceX: number
  readonly faceY: number
}

export class PhysicsRig {
  px = 0
  py = 0
  vx = 0
  vy = 0

  sq = 0
  vsq = 0

  rot = 0
  vrot = 0

  fx = 0
  fy = 0
  vfx = 0
  vfy = 0

  lagX = 0
  lagY = 0
  vlagX = 0
  vlagY = 0

  /** Apply a directional tap/poke squash impulse. */
  poke(angle: number, strength: number): void {
    const k = SPRING.pokeImpulse * strength
    this.vsq += k * 14.5
    this.vy += k * 56
    this.vrot += Math.sin(angle * 3.1) * 4.5 * k
    this.vlagX += Math.cos(angle) * 68 * k
    this.vlagY += Math.sin(angle) * 68 * k
  }

  /** Apply a declarative impulse when entering a cue state or reaction. */
  applyImpulse(imp: MotionImpulse): void {
    if (imp.vy) this.vy += imp.vy
    if (imp.vsq) this.vsq += imp.vsq
    if (imp.vrot) this.vrot += imp.vrot
    if (imp.pokeStrength) {
      this.poke(imp.pokeAngle ?? -Math.PI / 2, imp.pokeStrength)
    }
  }

  /** Snap all springs directly to targets (static SVG, reduced motion, or clean rest). */
  snapTo(targets: PhysicsTargets): void {
    this.px = targets.tx
    this.py = targets.ty
    this.vx = 0
    this.vy = 0
    this.sq = clamp(targets.tsq, GEOMETRY.minSquash, GEOMETRY.maxSquash)
    this.vsq = 0
    this.rot = targets.trot
    this.vrot = 0
    this.fx = targets.faceX
    this.fy = targets.faceY
    this.vfx = 0
    this.vfy = 0
    this.lagX = this.px * 0.45
    this.lagY = this.py * 0.45
    this.vlagX = 0
    this.vlagY = 0
  }

  /** Advance springs by dt seconds using sub-stepped semi-implicit Euler integration. */
  step(dt: number, targets: PhysicsTargets): void {
    const clampedDt = Math.min(dt, TIMING.maxPhysicsStepSec)
    const sdt = clampedDt / TIMING.physicsSubSteps

    for (let s = 0; s < TIMING.physicsSubSteps; s++) {
      this.vx += ((targets.tx - this.px) * SPRING.posStiffness - this.vx * SPRING.posDamping) * sdt
      this.vy += ((targets.ty - this.py) * SPRING.posStiffness - this.vy * SPRING.posDamping) * sdt
      this.px += this.vx * sdt
      this.py += this.vy * sdt

      this.vsq += ((targets.tsq - this.sq) * SPRING.squashStiffness - this.vsq * SPRING.squashDamping) * sdt
      this.sq = clamp(this.sq + this.vsq * sdt, GEOMETRY.minSquash, GEOMETRY.maxSquash)

      this.vrot += ((targets.trot - this.rot) * SPRING.rotStiffness - this.vrot * SPRING.rotDamping) * sdt
      this.rot += this.vrot * sdt

      this.vfx += ((targets.faceX - this.fx) * SPRING.faceStiffness - this.vfx * SPRING.faceDamping) * sdt
      this.vfy += ((targets.faceY - this.fy) * SPRING.faceStiffness - this.vfy * SPRING.faceDamping) * sdt
      this.fx += this.vfx * sdt
      this.fy += this.vfy * sdt

      const targetLagX = this.px * 0.45
      const targetLagY = this.py * 0.45
      this.vlagX += ((targetLagX - this.lagX) * SPRING.lagStiffness - this.vlagX * SPRING.lagDamping) * sdt
      this.vlagY += ((targetLagY - this.lagY) * SPRING.lagStiffness - this.vlagY * SPRING.lagDamping) * sdt
      this.lagX += this.vlagX * sdt
      this.lagY += this.vlagY * sdt
    }
  }

  /** Return true when all springs have converged within rest tolerances (safe to pause rAF). */
  isSettled(targets: PhysicsTargets): boolean {
    const targetLagX = targets.tx * 0.45
    const targetLagY = targets.ty * 0.45
    return (
      Math.abs(this.px - targets.tx) < SPRING.settlePosEps &&
      Math.abs(this.py - targets.ty) < SPRING.settlePosEps &&
      Math.abs(this.vx) < SPRING.settleVelEps &&
      Math.abs(this.vy) < SPRING.settleVelEps &&
      Math.abs(this.sq - targets.tsq) < SPRING.settleSquashEps &&
      Math.abs(this.vsq) < SPRING.settleVelEps &&
      Math.abs(this.rot - targets.trot) < SPRING.settleRotEps &&
      Math.abs(this.vrot) < SPRING.settleVelEps &&
      Math.abs(this.fx - targets.faceX) < SPRING.settlePosEps &&
      Math.abs(this.fy - targets.faceY) < SPRING.settlePosEps &&
      Math.abs(this.vfx) < SPRING.settleVelEps &&
      Math.abs(this.vfy) < SPRING.settleVelEps &&
      Math.abs(this.lagX - targetLagX) < SPRING.settlePosEps &&
      Math.abs(this.lagY - targetLagY) < SPRING.settlePosEps &&
      Math.abs(this.vlagX) < SPRING.settleVelEps &&
      Math.abs(this.vlagY) < SPRING.settleVelEps
    )
  }

  /** Volume-conserving horizontal/vertical scales and grounded anchor offset. */
  getBodyTransform(): { sx: number; sy: number; anchorOffsetY: number; lagDeltaX: number; lagDeltaY: number } {
    const sx = 1 + this.sq
    const sy = 1 / Math.max(GEOMETRY.minScaleYDenominator, sx)
    return {
      sx,
      sy,
      anchorOffsetY: (1 - sy) * GEOMETRY.squashAnchorFactor,
      lagDeltaX: (this.lagX - this.px * 0.45) * 1.4,
      lagDeltaY: (this.lagY - this.py * 0.45) * 1.4,
    }
  }
}
