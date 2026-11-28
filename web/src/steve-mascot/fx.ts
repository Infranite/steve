/**
 * Expressive cue FX and props used by Steve's live animations.
 * External props use fixed theme-aware colors from PROP_PALETTE;
 * Steve's own anatomy (ear, paws) matches his charcoal body & cutout stroke.
 */

import { PROP_PALETTE } from "./constants.ts"
import { hashNoise } from "./geometry.ts"
import type { FxKind, FxRenderCtx, FxRenderer } from "./types.ts"

function fxTokens(ctx: FxRenderCtx) {
  const isDark = ctx.theme === "dark"
  const pp = isDark ? PROP_PALETTE.dark : PROP_PALETTE.light
  return {
    isDark,
    pp,
    handFill: ctx.colors.ink,
    handStroke: ctx.colors.paper,
    rightEdge: Math.max(29, ctx.bounds.maxX),
    j: (k: number) => hashNoise(ctx.seed + 700 + k * 9.1) * 0.75,
  }
}

const FX_LIST: readonly FxRenderer[] = [
  {
    id: "listen-ear",
    attach: "body",
    render(ctx) {
      const { t, reducedMotion, colors } = ctx
      const { handFill, pp, rightEdge } = fxTokens(ctx)
      const bob = reducedMotion ? 0 : Math.sin(t * 3.0) * 0.8
      const wave1 = reducedMotion ? 0.88 : 0.32 + 0.64 * Math.pow(0.5 + 0.5 * Math.sin(t * 5.0), 1.3)
      const wave2 = reducedMotion ? 0.68 : 0.22 + 0.68 * Math.pow(0.5 + 0.5 * Math.sin(t * 5.0 - 1.1), 1.3)
      const ex = rightEdge - 6.8
      const ey = 0.5 + bob

      const earOuter =
        `M ${(ex - 1.5).toFixed(1)} ${(ey - 11.0).toFixed(1)} ` +
        `C ${(ex + 16.5).toFixed(1)} ${(ey - 14.5).toFixed(1)}, ${(ex + 19.5).toFixed(1)} ${(ey + 7.5).toFixed(1)}, ${(ex + 0.5).toFixed(1)} ${(ey + 12.0).toFixed(1)}`
      const earInner =
        `M ${(ex + 2.5).toFixed(1)} ${(ey - 5.2).toFixed(1)} ` +
        `C ${(ex + 11.2).toFixed(1)} ${(ey - 5.2).toFixed(1)}, ${(ex + 11.5).toFixed(1)} ${(ey + 4.8).toFixed(1)}, ${(ex + 3.5).toFixed(1)} ${(ey + 5.8).toFixed(1)} ` +
        `M ${(ex + 5.2).toFixed(1)} ${(ey - 0.6).toFixed(1)} Q ${(ex + 8.0).toFixed(1)} ${(ey + 1.0).toFixed(1)} ${(ex + 6.0).toFixed(1)} ${(ey + 3.2).toFixed(1)}`
      const wavePath1 = `M ${(ex + 20.5).toFixed(1)} ${(ey - 13.0).toFixed(1)} Q ${(ex + 26.5).toFixed(1)} ${(ey - 1.5).toFixed(1)} ${(ex + 21.5).toFixed(1)} ${(ey + 10.0).toFixed(1)}`
      const wavePath2 = `M ${(ex + 26.5).toFixed(1)} ${(ey - 17.0).toFixed(1)} Q ${(ex + 34.0).toFixed(1)} ${(ey - 1.5).toFixed(1)} ${(ex + 27.5).toFixed(1)} ${(ey + 14.0).toFixed(1)}`

      return {
        back: [],
        front: [
          { d: earOuter, fill: handFill, stroke: colors.ink, strokeWidth: 2.5, opacity: 0.99, transform: "" },
          { d: earInner, stroke: colors.paper, strokeWidth: 1.9, opacity: 0.8, transform: "" },
          { d: wavePath1, stroke: pp.signal, strokeWidth: 2.3, opacity: wave1, transform: "" },
          { d: wavePath2, stroke: pp.signal, strokeWidth: 2.1, opacity: wave2, transform: "" },
        ],
      }
    },
  },
  {
    id: "think-bubbles",
    attach: "stage",
    render(ctx) {
      const { t, reducedMotion } = ctx
      const { pp, handFill, handStroke } = fxTokens(ctx)
      const dy = reducedMotion ? 0 : Math.sin(t * 2.5) * 2.2
      const dotPhase = reducedMotion ? 1 : (t * 2.4) % 3
      const d1Op = reducedMotion ? 0.9 : dotPhase >= 0.15 ? 0.95 : 0.25
      const d2Op = reducedMotion ? 0.9 : dotPhase >= 1.0 ? 0.95 : 0.25
      const d3Op = reducedMotion ? 0.9 : dotPhase >= 1.9 ? 0.95 : 0.25
      const b1 = `M 20.5 ${(-24.0 + dy * 0.4).toFixed(1)} A 2.5 2.5 0 1 0 25.5 ${(-24.0 + dy * 0.4).toFixed(1)} A 2.5 2.5 0 1 0 20.5 ${(-24.0 + dy * 0.4).toFixed(1)}`
      const b2 = `M 27.0 ${(-31.5 + dy * 0.7).toFixed(1)} A 3.8 3.8 0 1 0 34.6 ${(-31.5 + dy * 0.7).toFixed(1)} A 3.8 3.8 0 1 0 27.0 ${(-31.5 + dy * 0.7).toFixed(1)}`
      const cy = -43.5 + dy
      const cloud =
        `M 32.0 ${(cy + 4.5).toFixed(1)} ` +
        `C 26.5 ${(cy + 4.2).toFixed(1)}, 26.5 ${(cy - 4.2).toFixed(1)}, 32.5 ${(cy - 4.5).toFixed(1)} ` +
        `C 34.0 ${(cy - 10.2).toFixed(1)}, 45.0 ${(cy - 10.6).toFixed(1)}, 47.5 ${(cy - 4.8).toFixed(1)} ` +
        `C 54.5 ${(cy - 4.5).toFixed(1)}, 55.0 ${(cy + 4.5).toFixed(1)}, 48.0 ${(cy + 5.2).toFixed(1)} ` +
        `C 45.5 ${(cy + 8.4).toFixed(1)}, 35.5 ${(cy + 8.0).toFixed(1)}, 32.0 ${(cy + 4.5).toFixed(1)} Z`
      const chinPaw = `M -21.5 26.5 A 5.2 4.6 0 1 0 -11.1 26.5 A 5.2 4.6 0 1 0 -21.5 26.5`
      return {
        back: [],
        front: [
          { d: chinPaw, fill: handFill, stroke: handStroke, strokeWidth: 2.1, opacity: 0.96, transform: "" },
          { d: `${b1} ${b2}`, fill: pp.cloudFill, stroke: pp.ink, strokeWidth: 1.9, opacity: 0.96, transform: "" },
          { d: cloud, fill: pp.cloudFill, stroke: pp.ink, strokeWidth: 2.1, opacity: 0.98, transform: "" },
          { d: `M 36.2 ${cy.toFixed(1)} l 0.1 0`, stroke: pp.signal, strokeWidth: 3.2, opacity: d1Op, transform: "" },
          { d: `M 41.2 ${cy.toFixed(1)} l 0.1 0`, stroke: pp.signal, strokeWidth: 3.2, opacity: d2Op, transform: "" },
          { d: `M 46.2 ${cy.toFixed(1)} l 0.1 0`, stroke: pp.signal, strokeWidth: 3.2, opacity: d3Op, transform: "" },
        ],
      }
    },
  },
  {
    id: "inspect-lens",
    attach: "stage",
    stowable: true,
    stowPivotY: 6,
    render(ctx) {
      const { t, reducedMotion, inTransit } = ctx
      const { isDark, pp, handFill, handStroke } = fxTokens(ctx)
      const ox = reducedMotion || inTransit ? 0 : Math.sin(t * 2.2) * 5.2
      const oy = reducedMotion || inTransit ? 0 : Math.cos(t * 1.6) * 2.6
      const cx = 15.5 + ox
      const cy = 2.0 + oy
      const r = 13.2
      const lens = `M ${(cx - r).toFixed(1)} ${cy.toFixed(1)} A ${r} ${r} 0 1 0 ${(cx + r).toFixed(1)} ${cy.toFixed(1)} A ${r} ${r} 0 1 0 ${(cx - r).toFixed(1)} ${cy.toFixed(1)}`
      const rimCol = isDark ? "#1e2026" : "#fcfaf3"
      const hx1 = cx + r * 0.72
      const hy1 = cy + r * 0.72
      const hx2 = hx1 + 11.5
      const hy2 = hy1 + 11.5
      const handle = `M ${hx1.toFixed(1)} ${hy1.toFixed(1)} L ${hx2.toFixed(1)} ${hy2.toFixed(1)}`
      const glint = `M ${(cx - 6.8).toFixed(1)} ${(cy - 2.5).toFixed(1)} Q ${(cx - 4.8).toFixed(1)} ${(cy - 7.8).toFixed(1)} ${(cx + 0.5).toFixed(1)} ${(cy - 7.8).toFixed(1)}`
      const paw = `M ${(hx2 - 6.0).toFixed(1)} ${(hy2 - 1.5).toFixed(1)} A 4.8 4.4 0 1 0 ${(hx2 + 3.6).toFixed(1)} ${(hy2 - 1.5).toFixed(1)} A 4.8 4.4 0 1 0 ${(hx2 - 6.0).toFixed(1)} ${(hy2 - 1.5).toFixed(1)}`
      return {
        back: [],
        front: [
          { d: lens, fill: pp.glassTint, stroke: "none", strokeWidth: 0, opacity: 0.28, transform: "" },
          { d: handle, stroke: pp.woodHandle, strokeWidth: 5.4, opacity: 0.98, transform: "" },
          { d: handle, stroke: pp.ink, strokeWidth: 1.7, opacity: 0.85, transform: "" },
          { d: lens, stroke: rimCol, strokeWidth: 3.1, opacity: 0.98, transform: "" },
          { d: glint, stroke: "#fcfaf3", strokeWidth: 1.9, opacity: 0.88, transform: "" },
          { d: paw, fill: handFill, stroke: handStroke, strokeWidth: 2.0, opacity: 0.98, transform: "" },
        ],
      }
    },
  },
  {
    id: "wait-hourglass",
    attach: "stage",
    render(ctx) {
      const { t, reducedMotion } = ctx
      const { pp, rightEdge } = fxTokens(ctx)
      const rock = reducedMotion ? 6 : Math.sin(t * 2.2) * 9
      const sandPhase = reducedMotion ? 0.45 : (t * 0.55) % 1
      const cx = rightEdge + 10.5
      const cy = -21.0 + (reducedMotion ? 0 : Math.sin(t * 2.0) * 2.0)
      const caps = "M -7.5 -10.5 L 7.5 -10.5 M -7.5 10.5 L 7.5 10.5"
      const glass = "M -6.0 -10.5 L 6.0 -10.5 L 0.6 0 L 6.0 10.5 L -6.0 10.5 L -0.6 0 Z"
      const topFillY = -10.0 + sandPhase * 7.5
      const topHalfW = Math.max(0.8, 5.2 * (1 - sandPhase * 0.78))
      const topSand = `M ${(-topHalfW).toFixed(1)} ${topFillY.toFixed(1)} L ${topHalfW.toFixed(1)} ${topFillY.toFixed(1)} L 0 -0.5 Z`
      const botHalfW = Math.max(1.2, 5.2 * (0.25 + sandPhase * 0.75))
      const botPeakY = 10.0 - (2.0 + sandPhase * 6.5)
      const botSand = `M ${(-botHalfW).toFixed(1)} 10.0 L ${botHalfW.toFixed(1)} 10.0 L 0 ${botPeakY.toFixed(1)} Z`
      const trickle = `M 0 0.8 L 0 ${(botPeakY - 0.5).toFixed(1)}`
      const tf = `translate(${cx.toFixed(1)}, ${cy.toFixed(1)}) rotate(${rock.toFixed(1)})`
      return {
        back: [],
        front: [
          { d: glass, fill: pp.cloudFill, stroke: pp.ink, strokeWidth: 1.8, opacity: 0.95, transform: tf },
          { d: `${topSand} ${botSand}`, fill: pp.sand, stroke: "none", strokeWidth: 0, opacity: 0.94, transform: tf },
          { d: trickle, stroke: pp.sand, strokeWidth: 1.5, opacity: 0.9, transform: tf },
          { d: caps, stroke: pp.woodHandle, strokeWidth: 2.8, opacity: 0.98, transform: tf },
        ],
      }
    },
  },
  {
    id: "speech-ticks",
    attach: "stage",
    render(ctx) {
      const { pp } = fxTokens(ctx)
      const pulse = ctx.reducedMotion ? 1 : 0.55 + 0.45 * Math.sin(ctx.t * 7.2)
      return {
        back: [],
        front: [
          {
            d: "M 38 -20 L 45 -24 M 41 -12 L 49 -13 M 39 -4 L 46 -2",
            stroke: pp.signal,
            strokeWidth: 2.1,
            opacity: pulse * 0.88,
            transform: "",
          },
        ],
      }
    },
  },
  {
    id: "success-burst",
    attach: "stage",
    render(ctx) {
      const { pp } = fxTokens(ctx)
      const sc = ctx.reducedMotion ? 1 : 0.86 + 0.18 * Math.sin(ctx.t * 6)
      return {
        back: [],
        front: [
          {
            d: "M -34 -36 L -42 -44 M -22 -44 L -26 -53 M 26 -43 L 31 -52 M 38 -34 L 46 -41",
            stroke: pp.signal,
            strokeWidth: 2.2,
            opacity: 0.88,
            transform: `scale(${sc.toFixed(2)})`,
          },
          { d: "M 26 -24 L 31 -19 L 40 -30", stroke: pp.success, strokeWidth: 2.8, opacity: 0.96, transform: "" },
        ],
      }
    },
  },
  {
    id: "love-heart",
    attach: "stage",
    render(ctx) {
      const { pp, j } = fxTokens(ctx)
      const fy = ctx.reducedMotion ? -44 : -38 - ((ctx.t * 14) % 16)
      const op = ctx.reducedMotion ? 0.92 : Math.max(0, Math.sin((((ctx.t * 14) % 16) / 16) * Math.PI))
      return {
        back: [],
        front: [
          {
            d: `M 0 ${(4 + j(1)).toFixed(1)} C -7 ${(-2 + j(2)).toFixed(1)}, -8 -9, -3 -10 C 0 -10.5, 0 -6.5, 0 -6.5 C 0 -6.5, 0 -10.5, 3.5 -10 C 8 -9, 7 -2, 0 ${(4 + j(3)).toFixed(1)}`,
            fill: pp.love,
            stroke: pp.love,
            strokeWidth: 2.0,
            opacity: op,
            transform: `translate(31, ${fy.toFixed(1)})`,
          },
        ],
      }
    },
  },
  {
    id: "failure-zigs",
    attach: "stage",
    render(ctx) {
      const { pp } = fxTokens(ctx)
      return {
        back: [],
        front: [
          {
            d: "M -36 -30 L -42 -36 L -37 -41 L -44 -47 M 36 -28 L 43 -34 L 38 -39 L 45 -44",
            stroke: pp.failure,
            strokeWidth: 2.2,
            opacity: 0.92,
            transform: "",
          },
        ],
      }
    },
  },
  {
    id: "sleep-zz",
    attach: "stage",
    render(ctx) {
      const { pp } = fxTokens(ctx)
      return {
        back: [],
        front: [
          {
            d: "M -3.8 -3.8 L 3.8 -3.8 L -3.8 3.8 L 3.8 3.8",
            stroke: pp.ink,
            strokeWidth: 2.0,
            opacity: 0.78,
            transform: "translate(29, -27)",
          },
          {
            d: "M -5.2 -5.2 L 5.2 -5.2 L -5.2 5.2 L 5.2 5.2",
            stroke: pp.ink,
            strokeWidth: 2.3,
            opacity: 0.62,
            transform: "translate(40, -39)",
          },
        ],
      }
    },
  },
  {
    id: "hum-note",
    attach: "stage",
    render(ctx) {
      const { t, reducedMotion } = ctx
      const { pp } = fxTokens(ctx)
      const bob1 = reducedMotion ? 0 : Math.sin(t * 5.2) * 3.5
      const bob2 = reducedMotion ? 0 : Math.cos(t * 5.2) * 3.5
      const n1 = `M 32 ${(-28 + bob1).toFixed(1)} L 32 ${(-40 + bob1).toFixed(1)} L 40 ${(-42 + bob1).toFixed(1)} L 40 ${(-31 + bob1).toFixed(1)}`
      const h1 =
        `M 28.5 ${(-28 + bob1).toFixed(1)} A 3.0 2.3 0 1 0 34.5 ${(-28 + bob1).toFixed(1)} A 3.0 2.3 0 1 0 28.5 ${(-28 + bob1).toFixed(1)} ` +
        `M 36.5 ${(-31 + bob1).toFixed(1)} A 3.0 2.3 0 1 0 42.5 ${(-31 + bob1).toFixed(1)} A 3.0 2.3 0 1 0 36.5 ${(-31 + bob1).toFixed(1)}`
      const n2 = `M 47 ${(-19 + bob2).toFixed(1)} L 47 ${(-29 + bob2).toFixed(1)} Q 52 ${(-28 + bob2).toFixed(1)} 52 ${(-23 + bob2).toFixed(1)}`
      const h2 = `M 44.2 ${(-19 + bob2).toFixed(1)} A 2.6 2.0 0 1 0 49.4 ${(-19 + bob2).toFixed(1)} A 2.6 2.0 0 1 0 44.2 ${(-19 + bob2).toFixed(1)}`
      return {
        back: [],
        front: [
          { d: `${n1} ${n2}`, stroke: pp.signal, strokeWidth: 2.1, opacity: 0.95, transform: "" },
          { d: `${h1} ${h2}`, fill: pp.signal, stroke: pp.signal, strokeWidth: 1.0, opacity: 0.95, transform: "" },
        ],
      }
    },
  },
  {
    id: "sip-coffee",
    attach: "stage",
    render(ctx) {
      const { t, reducedMotion } = ctx
      const { isDark, pp, handFill, handStroke } = fxTokens(ctx)
      const rise = reducedMotion ? 0 : Math.sin(t * 2.5) * 1.6
      const steamShift = reducedMotion ? 0 : Math.sin(t * 4.5) * 2.0
      const mx = 16.0
      const my = 18.0 - rise
      const cup = `M ${(mx - 7.5).toFixed(1)} ${my.toFixed(1)} L ${(mx + 7.5).toFixed(1)} ${my.toFixed(1)} L ${(mx + 6.2).toFixed(1)} ${(my + 13.5).toFixed(1)} Q ${mx.toFixed(1)} ${(my + 15.2).toFixed(1)} ${(mx - 6.2).toFixed(1)} ${(my + 13.5).toFixed(1)} Z`
      const handle = `M ${(mx + 7.2).toFixed(1)} ${(my + 2.5).toFixed(1)} C ${(mx + 13.5).toFixed(1)} ${(my + 2.5).toFixed(1)}, ${(mx + 13.5).toFixed(1)} ${(my + 10.5).toFixed(1)}, ${(mx + 6.5).toFixed(1)} ${(my + 10.5).toFixed(1)}`
      const steam =
        `M ${(mx - 2.8).toFixed(1)} ${(my - 2.5).toFixed(1)} Q ${(mx - 5.2 + steamShift).toFixed(1)} ${(my - 6.5).toFixed(1)} ${(mx - 2.0).toFixed(1)} ${(my - 10.5).toFixed(1)} ` +
        `M ${(mx + 2.8).toFixed(1)} ${(my - 3.0).toFixed(1)} Q ${(mx + 5.2 - steamShift).toFixed(1)} ${(my - 7.2).toFixed(1)} ${(mx + 2.2).toFixed(1)} ${(my - 11.5).toFixed(1)}`
      const paw = `M ${(mx - 10.5).toFixed(1)} ${(my + 7.5).toFixed(1)} A 4.4 4.0 0 1 0 ${(mx - 1.7).toFixed(1)} ${(my + 7.5).toFixed(1)} A 4.4 4.0 0 1 0 ${(mx - 10.5).toFixed(1)} ${(my + 7.5).toFixed(1)}`
      return {
        back: [],
        front: [
          { d: handle, stroke: pp.ink, strokeWidth: 2.2, opacity: 0.96, transform: "" },
          { d: cup, fill: isDark ? "#f2ede2" : "#fcfaf3", stroke: "#1e2026", strokeWidth: 2.1, opacity: 0.98, transform: "" },
          { d: `M ${(mx - 6.8).toFixed(1)} ${(my + 5.2).toFixed(1)} L ${(mx + 6.8).toFixed(1)} ${(my + 5.2).toFixed(1)}`, stroke: pp.mugBand, strokeWidth: 2.9, opacity: 0.95, transform: "" },
          { d: steam, stroke: pp.signal, strokeWidth: 1.8, opacity: 0.85, transform: "" },
          { d: paw, fill: handFill, stroke: handStroke, strokeWidth: 2.0, opacity: 0.98, transform: "" },
        ],
      }
    },
  },
]

const FX_MAP = new Map<FxKind, FxRenderer>(FX_LIST.map((f) => [f.id, f]))

export function getFxRenderer(fx: FxKind): FxRenderer | undefined {
  if (fx === "none") return undefined
  return FX_MAP.get(fx)
}

export function isStowableFx(fx: FxKind): boolean {
  return Boolean(getFxRenderer(fx)?.stowable)
}
