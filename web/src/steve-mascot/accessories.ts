/**
 * Curated hand-drawn charcoal accessories for ordinary Bots (10 items).
 * Every accessory uses fixed theme-aware editorial colors independent of Bot body hue,
 * adapts to the Bot's head bounds across roundness 0..100, and fits cleanly inside
 * the shared viewBox without clipping or shrinking head scale.
 *
 * Steve (the flagship main bot) never renders accessories.
 */

import type { BotAccessory } from "../gen/BotAccessory.ts"
import { hashNoise } from "./geometry.ts"
import type { FxStroke, ThemeMode } from "./types.ts"

export const BOT_ACCESSORIES: readonly BotAccessory[] = [
  "headphones",
  "glasses",
  "loupe",
  "book",
  "pencil",
  "beret",
  "hardhat",
  "wrench",
  "net",
  "chart",
] as const

export interface AccessoryRenderCtx {
  readonly accessory: BotAccessory
  readonly theme: ThemeMode
  readonly seed: number
  readonly bounds: {
    readonly minX: number
    readonly maxX: number
    readonly minY: number
    readonly maxY: number
  }
  readonly eyeGap: number
  readonly eyeRx: number
  readonly eyeRy: number
}

export interface CompiledAccessoryLayers {
  /** Drawn behind the charcoal body (inherits body transform). */
  readonly back: readonly FxStroke[]
  /** Drawn in front of the charcoal body, beneath face expressions (inherits body transform). */
  readonly body: readonly FxStroke[]
  /** Drawn in face space so eyewear tracks the giant eyes during live motion. */
  readonly face: readonly FxStroke[]
}

const EMPTY_ACCESSORY_LAYERS: CompiledAccessoryLayers = {
  back: [],
  body: [],
  face: [],
}

/** Fixed theme-aware editorial ink & color tokens for Bot accessories (independent of Bot color). */
const ACC_PALETTE = {
  light: {
    ink: "#1c1e24",
    paper: "#fcfaf3",
    paperEdge: "#e7dfd0",
    band: "#2f3542",
    bandPad: "#d96d49",
    cupFill: "#3d4656",
    cupTrim: "#e07a52",
    frameGold: "#d9922e",
    glassTint: "#8ec6e6",
    glint: "#fcfaf3",
    wood: "#b56c3b",
    brass: "#d49434",
    bookCover: "#2f6b7a",
    bookSpine: "#214e5a",
    ribbon: "#d94f5c",
    pencilBody: "#e8af32",
    pencilWood: "#efd6aa",
    eraser: "#e67e8b",
    ferrule: "#c3cad4",
    beret: "#c73e50",
    beretShade: "#8f2635",
    hardhat: "#eab328",
    hardhatRidge: "#fae082",
    steel: "#aeb9c7",
    steelHi: "#eef2f7",
    netMesh: "#4e837e",
    netFill: "#e4f4f0",
    bar1: "#3e9b8c",
    bar2: "#e39f32",
    bar3: "#d95b52",
  },
  dark: {
    ink: "#1c1e24",
    paper: "#f4efe4",
    paperEdge: "#d6cebe",
    band: "#cfd7e2",
    bandPad: "#f28a63",
    cupFill: "#dce3ed",
    cupTrim: "#f28a63",
    frameGold: "#f5bd52",
    glassTint: "#8dc8eb",
    glint: "#ffffff",
    wood: "#df935b",
    brass: "#f2be57",
    bookCover: "#4aa0b3",
    bookSpine: "#317687",
    ribbon: "#f26b78",
    pencilBody: "#f6c94c",
    pencilWood: "#f5dfb8",
    eraser: "#f2929e",
    ferrule: "#d7dde6",
    beret: "#e65a6c",
    beretShade: "#b23748",
    hardhat: "#f6c942",
    hardhatRidge: "#ffeb99",
    steel: "#cfd8e3",
    steelHi: "#f5f8fc",
    netMesh: "#5ca29a",
    netFill: "#daf2ec",
    bar1: "#50b8a7",
    bar2: "#f2b84b",
    bar3: "#ed6f65",
  },
} as const

function n1(v: number): string {
  return v.toFixed(1)
}

export function renderBotAccessory(ctx: AccessoryRenderCtx): CompiledAccessoryLayers {
  const p = ACC_PALETTE[ctx.theme]
  const { minX, maxX, minY } = ctx.bounds
  const j = (k: number) => hashNoise(ctx.seed + 913 + k * 7.3) * 0.45

  switch (ctx.accessory) {
    case "headphones": {
      const lx = minX - 1.2
      const rx = maxX + 1.2
      const topY = minY - 8.2
      const ey = -1.2
      const band =
        `M ${n1(lx - 2.2)} ${n1(ey - 4.0)} ` +
        `C ${n1(lx - 4.6)} ${n1(topY + 1.8)}, -18.5 ${n1(topY)}, 0 ${n1(topY)} ` +
        `C 18.5 ${n1(topY)}, ${n1(rx + 4.6)} ${n1(topY + 1.8)}, ${n1(rx + 2.2)} ${n1(ey - 4.0)}`
      const pad =
        `M -12.8 ${n1(topY + 1.0)} ` +
        `Q 0 ${n1(topY - 1.2)} 12.8 ${n1(topY + 1.0)}`
      const cupL =
        `M ${n1(lx + 1.6)} ${n1(ey - 9.2)} ` +
        `C ${n1(lx - 7.4)} ${n1(ey - 9.4)}, ${n1(lx - 7.8)} ${n1(ey + 9.6)}, ${n1(lx + 1.6)} ${n1(ey + 9.2)} Z`
      const cupR =
        `M ${n1(rx - 1.6)} ${n1(ey - 9.2)} ` +
        `C ${n1(rx + 7.4)} ${n1(ey - 9.4)}, ${n1(rx + 7.8)} ${n1(ey + 9.6)}, ${n1(rx - 1.6)} ${n1(ey + 9.2)} Z`
      const trimL = `M ${n1(lx - 3.9)} ${n1(ey - 5.4)} Q ${n1(lx - 5.4)} ${n1(ey)} ${n1(lx - 3.9)} ${n1(ey + 5.4)}`
      const trimR = `M ${n1(rx + 3.9)} ${n1(ey - 5.4)} Q ${n1(rx + 5.4)} ${n1(ey)} ${n1(rx + 3.9)} ${n1(ey + 5.4)}`
      return {
        back: [
          { d: band, stroke: p.ink, strokeWidth: 7.8, opacity: 0.98, transform: "" },
          { d: band, stroke: p.band, strokeWidth: 4.5, opacity: 0.99, transform: "" },
        ],
        body: [
          { d: pad, stroke: p.ink, strokeWidth: 7.6, opacity: 0.96, transform: "" },
          { d: pad, stroke: p.bandPad, strokeWidth: 4.6, opacity: 0.99, transform: "" },
          { d: cupL, fill: p.cupFill, stroke: p.ink, strokeWidth: 2.4, opacity: 0.99, transform: "" },
          { d: cupR, fill: p.cupFill, stroke: p.ink, strokeWidth: 2.4, opacity: 0.99, transform: "" },
          { d: trimL, stroke: p.cupTrim, strokeWidth: 2.3, opacity: 0.97, transform: "" },
          { d: trimR, stroke: p.cupTrim, strokeWidth: 2.3, opacity: 0.97, transform: "" },
        ],
        face: [],
      }
    }

    case "glasses": {
      const gap = ctx.eyeGap
      const grx = Math.min(gap - 1.35, ctx.eyeRx + 1.8)
      const gry = Math.min(14.4, ctx.eyeRy + 1.4)
      const oval = (cx: number) =>
        `M ${n1(cx - grx)} 0 ` +
        `A ${n1(grx)} ${n1(gry)} 0 1 0 ${n1(cx + grx)} 0 ` +
        `A ${n1(grx)} ${n1(gry)} 0 1 0 ${n1(cx - grx)} 0 Z`
      const lensL = oval(-gap)
      const lensR = oval(gap)
      const bridge = `M ${n1(-gap + grx - 0.5)} -2.6 Q 0 -5.6 ${n1(gap - grx + 0.5)} -2.6`
      const arms =
        `M ${n1(-gap - grx + 0.4)} -2.8 L ${n1(-gap - grx - 4.8)} -5.0 ` +
        `M ${n1(gap + grx - 0.4)} -2.8 L ${n1(gap + grx + 4.8)} -5.0`
      const glint =
        `M ${n1(-gap - grx * 0.52)} ${n1(-gry * 0.25)} Q ${n1(-gap - grx * 0.3)} ${n1(-gry * 0.62)} ${n1(-gap + grx * 0.14)} ${n1(-gry * 0.62)} ` +
        `M ${n1(gap - grx * 0.52)} ${n1(-gry * 0.25)} Q ${n1(gap - grx * 0.3)} ${n1(-gry * 0.62)} ${n1(gap + grx * 0.14)} ${n1(-gry * 0.62)}`
      return {
        back: [],
        body: [],
        face: [
          { d: `${lensL} ${lensR}`, fill: p.glassTint, stroke: "none", strokeWidth: 0, opacity: 0.16, transform: "" },
          { d: `${arms} ${bridge}`, stroke: p.ink, strokeWidth: 4.8, opacity: 0.96, transform: "" },
          { d: `${lensL} ${lensR}`, stroke: p.ink, strokeWidth: 5.0, opacity: 0.96, transform: "" },
          { d: `${arms} ${bridge}`, stroke: p.frameGold, strokeWidth: 2.4, opacity: 0.98, transform: "" },
          { d: `${lensL} ${lensR}`, stroke: p.frameGold, strokeWidth: 2.6, opacity: 0.98, transform: "" },
          { d: glint, stroke: p.glint, strokeWidth: 1.7, opacity: 0.72, transform: "" },
        ],
      }
    }

    case "loupe": {
      const cx = maxX - 2.6
      const cy = minY + 3.0
      const r = 9.8
      const lens =
        `M ${n1(cx - r)} ${n1(cy)} ` +
        `A ${n1(r)} ${n1(r)} 0 1 0 ${n1(cx + r)} ${n1(cy)} ` +
        `A ${n1(r)} ${n1(r)} 0 1 0 ${n1(cx - r)} ${n1(cy)} Z`
      const hx1 = cx + r * 0.72
      const hy1 = cy + r * 0.72
      const hx2 = hx1 + 9.2
      const hy2 = hy1 + 9.8
      const handle = `M ${n1(hx1)} ${n1(hy1)} L ${n1(hx2)} ${n1(hy2)}`
      const glint = `M ${n1(cx - 5.2)} ${n1(cy - 1.5)} Q ${n1(cx - 3.6)} ${n1(cy - 5.8)} ${n1(cx + 0.8)} ${n1(cy - 5.8)}`
      return {
        back: [],
        body: [
          { d: handle, stroke: p.ink, strokeWidth: 7.4, opacity: 0.96, transform: "" },
          { d: handle, stroke: p.wood, strokeWidth: 4.4, opacity: 0.98, transform: "" },
          { d: lens, fill: p.glassTint, stroke: "none", strokeWidth: 0, opacity: 0.56, transform: "" },
          { d: lens, stroke: p.ink, strokeWidth: 5.2, opacity: 0.96, transform: "" },
          { d: lens, stroke: p.brass, strokeWidth: 2.6, opacity: 0.99, transform: "" },
          { d: glint, stroke: p.glint, strokeWidth: 1.9, opacity: 0.9, transform: "" },
        ],
        face: [],
      }
    }

    case "book": {
      const tf = `translate(${n1(-1.5)}, ${n1(minY - 5.0)}) rotate(-10)`
      const topCover = `M -17.2 -5.4 Q 0 -7.8 17.5 -5.0 L 15.2 2.4 Q 0 0.6 -15.6 2.2 Z`
      const pages = `M -14.8 1.8 L 14.8 2.2 L 13.8 9.2 Q 0 7.6 -13.8 9.0 Z`
      const pageLines = `M -11.4 5.0 Q 0 4.0 11.5 5.2 M -10.8 7.0 Q 0 6.1 10.8 7.2`
      const spine = `M -17.2 -5.4 C -20.0 -1.2, -19.2 6.0, -13.8 9.2`
      const ribbon = `M 6.0 6.2 L 8.2 15.4 L 4.8 13.0 L 1.6 15.8 L 1.4 6.4 Z`
      return {
        back: [],
        body: [
          { d: pages, fill: p.paper, stroke: p.ink, strokeWidth: 2.2, opacity: 0.99, transform: tf },
          { d: pageLines, stroke: p.paperEdge, strokeWidth: 1.4, opacity: 0.95, transform: tf },
          { d: topCover, fill: p.bookCover, stroke: p.ink, strokeWidth: 2.3, opacity: 0.99, transform: tf },
          { d: spine, stroke: p.ink, strokeWidth: 5.2, opacity: 0.96, transform: tf },
          { d: spine, stroke: p.bookSpine, strokeWidth: 2.5, opacity: 0.98, transform: tf },
          { d: ribbon, fill: p.ribbon, stroke: p.ink, strokeWidth: 1.8, opacity: 0.99, transform: tf },
        ],
        face: [],
      }
    }

    case "pencil": {
      const tf = `translate(${n1(maxX - 2.2)}, ${n1(minY + 7.2)}) rotate(-38)`
      const barrel = `M -10.4 -3.9 L 10.4 -3.9 L 10.4 3.9 L -10.4 3.9 Z`
      const barrelLine = `M -9.4 0.3 L 9.4 0.3`
      const ferrule = `M 10.2 -4.0 L 14.6 -4.0 L 14.6 4.0 L 10.2 4.0 Z`
      const eraser = `M 14.4 -3.8 L 19.0 -3.8 Q 22.0 -3.8 22.0 0 Q 22.0 3.8 19.0 3.8 L 14.4 3.8 Z`
      const woodCone = `M -10.2 -3.9 L -18.6 0 L -10.2 3.9 Z`
      const lead = `M -15.2 -1.6 L -19.0 0 L -15.2 1.6 Z`
      return {
        back: [],
        body: [
          { d: woodCone, fill: p.pencilWood, stroke: p.ink, strokeWidth: 2.1, opacity: 0.99, transform: tf },
          { d: lead, fill: p.ink, stroke: p.ink, strokeWidth: 1.2, opacity: 0.99, transform: tf },
          { d: barrel, fill: p.pencilBody, stroke: p.ink, strokeWidth: 2.2, opacity: 0.99, transform: tf },
          { d: barrelLine, stroke: p.ink, strokeWidth: 1.2, opacity: 0.44, transform: tf },
          { d: ferrule, fill: p.ferrule, stroke: p.ink, strokeWidth: 2.1, opacity: 0.99, transform: tf },
          { d: eraser, fill: p.eraser, stroke: p.ink, strokeWidth: 2.1, opacity: 0.99, transform: tf },
        ],
        face: [],
      }
    }

    case "beret": {
      const tf = `translate(${n1(-2.2)}, ${n1(minY + 3.4)}) rotate(-11)`
      const puff =
        `M -20.5 2.0 ` +
        `C -24.2 -5.8, -14.5 -14.2, -1.5 -14.2 ` +
        `C 12.2 -14.2, 23.6 -6.8, 21.8 1.2 ` +
        `C 20.2 5.2, 11.2 4.6, 0.2 3.8 ` +
        `C -9.8 3.2, -18.2 5.2, -20.5 2.0 Z`
      const crease = `M -11.5 -5.8 Q -1.0 -9.8 11.5 -4.4`
      const band = `M -16.8 2.8 Q 0.2 0.2 17.2 2.4`
      const stem = `M -0.8 -14.0 Q 0.4 -18.4 3.4 -18.8`
      return {
        back: [],
        body: [
          { d: stem, stroke: p.ink, strokeWidth: 4.8, opacity: 0.96, transform: tf },
          { d: stem, stroke: p.beret, strokeWidth: 2.2, opacity: 0.98, transform: tf },
          { d: puff, fill: p.beret, stroke: p.ink, strokeWidth: 2.4, opacity: 0.99, transform: tf },
          { d: crease, stroke: p.beretShade, strokeWidth: 2.0, opacity: 0.78, transform: tf },
          { d: band, stroke: p.ink, strokeWidth: 3.0, opacity: 0.92, transform: tf },
        ],
        face: [],
      }
    }

    case "hardhat": {
      const by = minY + 4.4
      const shell =
        `M -21.2 ${n1(by)} ` +
        `C -20.8 ${n1(by - 15.2)}, -10.5 ${n1(by - 17.5)}, 0 ${n1(by - 17.5)} ` +
        `C 10.5 ${n1(by - 17.5)}, 20.8 ${n1(by - 15.2)}, 21.2 ${n1(by)} Z`
      const ridge =
        `M -4.2 ${n1(by - 1.8)} ` +
        `L -3.4 ${n1(by - 18.2)} ` +
        `Q 0 ${n1(by - 19.6)} 3.4 ${n1(by - 18.2)} ` +
        `L 4.2 ${n1(by - 1.8)} Z`
      const brim =
        `M -25.0 ${n1(by + 0.4)} ` +
        `Q 0 ${n1(by - 2.6)} 25.0 ${n1(by + 0.4)} ` +
        `Q 0 ${n1(by + 3.6)} -25.0 ${n1(by + 0.4)} Z`
      return {
        back: [],
        body: [
          { d: shell, fill: p.hardhat, stroke: p.ink, strokeWidth: 2.4, opacity: 0.99, transform: "" },
          { d: ridge, fill: p.hardhatRidge, stroke: p.ink, strokeWidth: 2.0, opacity: 0.98, transform: "" },
          { d: brim, fill: p.hardhat, stroke: p.ink, strokeWidth: 2.3, opacity: 0.99, transform: "" },
        ],
        face: [],
      }
    }

    case "wrench": {
      const tf = `translate(${n1(maxX + 1.8)}, ${n1(minY + 18.5)}) rotate(20)`
      const shape =
        `M -3.2 16.5 ` +
        `L -3.2 -5.2 ` +
        `C -8.6 -7.2, -9.8 -14.8, -5.2 -19.2 ` +
        `L -3.0 -13.0 L 3.0 -13.0 L 5.2 -19.2 ` +
        `C 9.8 -14.8, 8.6 -7.2, 3.2 -5.2 ` +
        `L 3.2 16.5 ` +
        `C 3.2 20.0, -3.2 20.0, -3.2 16.5 Z`
      const groove = `M 0 -4.2 L 0 14.2`
      const hole = `M -1.5 16.2 A 1.5 1.5 0 1 0 1.5 16.2 A 1.5 1.5 0 1 0 -1.5 16.2 Z`
      return {
        back: [],
        body: [
          { d: shape, fill: p.steel, stroke: p.ink, strokeWidth: 2.3, opacity: 0.99, transform: tf },
          { d: groove, stroke: p.steelHi, strokeWidth: 1.8, opacity: 0.85, transform: tf },
          { d: hole, fill: p.ink, stroke: "none", strokeWidth: 0, opacity: 0.82, transform: tf },
        ],
        face: [],
      }
    }

    case "net": {
      const ax = maxX - 1.2
      const ay = minY + 7.2
      const pole = `M ${n1(ax - 7.5)} ${n1(ay + 18.0)} L ${n1(ax + 4.0)} ${n1(ay - 6.2)}`
      const bag =
        `M ${n1(ax - 4.8)} ${n1(ay - 12.6)} ` +
        `C ${n1(ax + 12.0)} ${n1(ay - 17.0)}, ${n1(ax + 20.8)} ${n1(ay - 3.5)}, ${n1(ax + 15.8)} ${n1(ay + 7.6)} ` +
        `C ${n1(ax + 10.6)} ${n1(ay + 12.2)}, ${n1(ax + 2.8)} ${n1(ay + 4.4)}, ${n1(ax + 4.0)} ${n1(ay - 6.0)} Z`
      const mesh =
        `M ${n1(ax + 0.6)} ${n1(ay - 12.0)} Q ${n1(ax + 9.2)} ${n1(ay - 3.6)} ${n1(ax + 10.6)} ${n1(ay + 7.4)} ` +
        `M ${n1(ax + 6.4)} ${n1(ay - 12.2)} Q ${n1(ax + 13.8)} ${n1(ay - 4.4)} ${n1(ax + 15.0)} ${n1(ay + 4.0)} ` +
        `M ${n1(ax + 3.2)} ${n1(ay - 6.2)} Q ${n1(ax + 10.6)} ${n1(ay - 9.0)} ${n1(ax + 16.8)} ${n1(ay - 5.2)} ` +
        `M ${n1(ax + 4.0)} ${n1(ay + 0.4)} Q ${n1(ax + 11.2)} ${n1(ay - 2.4)} ${n1(ax + 16.6)} ${n1(ay + 1.4)}`
      const hoop =
        `M ${n1(ax - 5.0)} ${n1(ay - 12.6)} ` +
        `C ${n1(ax + 0.6)} ${n1(ay - 16.2)}, ${n1(ax + 9.8)} ${n1(ay - 10.2)}, ${n1(ax + 5.2)} ${n1(ay - 5.2)} ` +
        `C ${n1(ax + 0.0)} ${n1(ay - 2.4)}, ${n1(ax - 9.2)} ${n1(ay - 8.2)}, ${n1(ax - 5.0)} ${n1(ay - 12.6)} Z`
      return {
        back: [
          { d: pole, stroke: p.ink, strokeWidth: 6.4, opacity: 0.97, transform: "" },
          { d: pole, stroke: p.wood, strokeWidth: 3.4, opacity: 0.99, transform: "" },
        ],
        body: [
          { d: bag, fill: p.netFill, stroke: p.ink, strokeWidth: 2.2, opacity: 0.94, transform: "" },
          { d: mesh, stroke: p.netMesh, strokeWidth: 1.5, opacity: 0.88, transform: "" },
          { d: hoop, fill: "none", stroke: p.ink, strokeWidth: 3.0, opacity: 0.99, transform: "" },
        ],
        face: [],
      }
    }

    case "chart": {
      const tf = `translate(${n1(maxX - 2.2 + j(1))}, ${n1(minY + 3.8 + j(2))}) rotate(10)`
      const card = `M -10.8 -8.6 Q 0 -9.4 10.8 -8.6 Q 11.6 0 10.8 8.6 Q 0 9.4 -10.8 8.6 Q -11.6 0 -10.8 -8.6 Z`
      const bar1 = `M -7.4 5.8 L -7.4 0.8 L -4.0 0.8 L -4.0 5.8 Z`
      const bar2 = `M -1.7 5.8 L -1.7 -2.0 L 1.7 -2.0 L 1.7 5.8 Z`
      const bar3 = `M 4.0 5.8 L 4.0 -5.2 L 7.4 -5.2 L 7.4 5.8 Z`
      const axis = `M -8.4 6.0 L 8.4 6.0`
      const trend = `M -6.8 -2.2 L -0.4 -5.0 L 6.4 -7.0 M 3.8 -7.4 L 6.6 -7.0 L 5.2 -4.4`
      return {
        back: [],
        body: [
          { d: card, fill: p.paper, stroke: p.ink, strokeWidth: 2.2, opacity: 0.99, transform: tf },
          { d: bar1, fill: p.bar1, stroke: p.ink, strokeWidth: 1.1, opacity: 0.98, transform: tf },
          { d: bar2, fill: p.bar2, stroke: p.ink, strokeWidth: 1.1, opacity: 0.98, transform: tf },
          { d: bar3, fill: p.bar3, stroke: p.ink, strokeWidth: 1.1, opacity: 0.98, transform: tf },
          { d: axis, stroke: p.ink, strokeWidth: 1.5, opacity: 0.88, transform: tf },
          { d: trend, stroke: p.bar3, strokeWidth: 1.7, opacity: 0.96, transform: tf },
        ],
        face: [],
      }
    }
  }
  return EMPTY_ACCESSORY_LAYERS
}
