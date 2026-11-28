/**
 * Retained live DOM SVG renderer for Steve's charcoal mascot.
 * Builds minimal charcoal SVG nodes once and patches attributes in-place — zero innerHTML churn.
 */

import { CHARCOAL_FILTER } from "./constants.ts"
import type { CompiledFrame, FxStroke, OpticalTier, PaletteTokens } from "./types.ts"

const SVG_NS = "http://www.w3.org/2000/svg"

function svgEl<K extends keyof SVGElementTagNameMap>(
  tag: K,
  attrs?: Record<string, string | number>,
): SVGElementTagNameMap[K] {
  const node = document.createElementNS(SVG_NS, tag)
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      node.setAttribute(k, String(v))
    }
  }
  return node
}

export class SteveSvgRenderer {
  readonly svg: SVGSVGElement
  private readonly uid: string
  private viewHalf = 48
  private lastStyleKey = ""

  private bodyDisp: SVGFEDisplacementMapElement
  private accDisp: SVGFEDisplacementMapElement
  private clipPath: SVGPathElement
  private shadowGroup: SVGGElement
  private shadowLines: SVGPathElement[]

  private fxBackGroup: SVGGElement
  private fxBackPaths: SVGPathElement[]

  private accBackGroup: SVGGElement
  private accBackPaths: SVGPathElement[]

  private bodyRig: SVGGElement
  private washPath: SVGPathElement
  private bodyFillPath: SVGPathElement
  private scribblePath: SVGPathElement
  private stroke1: SVGPathElement
  private stroke2: SVGPathElement

  private accBodyGroup: SVGGElement
  private accBodyPaths: SVGPathElement[]

  private faceRig: SVGGElement
  private faceGroup: SVGGElement
  private blushL: SVGPathElement
  private blushR: SVGPathElement
  private eyeL: SVGPathElement
  private eyeR: SVGPathElement
  private arcL: SVGPathElement
  private arcR: SVGPathElement
  private winkRightArc: SVGPathElement
  private mouth: SVGPathElement
  private accFaceGroup: SVGGElement
  private accFacePaths: SVGPathElement[]

  private fxFrontGroup: SVGGElement
  private fxFrontPaths: SVGPathElement[]

  constructor(uid: string, sizePx: number, kind: "steve" | "bot" = "steve") {
    this.uid = uid
    this.svg = svgEl("svg", {
      ...(kind === "steve" ? { "data-steve-avatar": "live" } : { "data-mascot-avatar": "live" }),
      "data-mascot-kind": kind,
      viewBox: "-48 -48 96 96",
      width: sizePx,
      height: sizePx,
      overflow: "visible",
    })
    this.svg.style.display = "block"
    this.svg.style.overflow = "visible"

    const defs = svgEl("defs")
    const filter = svgEl("filter", {
      id: `${uid}-bt`,
      filterUnits: "userSpaceOnUse",
      x: -64,
      y: -64,
      width: 128,
      height: 128,
    })
    filter.append(
      svgEl("feTurbulence", {
        type: "fractalNoise",
        baseFrequency: CHARCOAL_FILTER.baseFrequency,
        numOctaves: CHARCOAL_FILTER.numOctaves,
        seed: 3,
        result: "n",
      }),
    )
    this.bodyDisp = svgEl("feDisplacementMap", {
      in: "SourceGraphic",
      in2: "n",
      scale: CHARCOAL_FILTER.scaleFull,
      xChannelSelector: "R",
      yChannelSelector: "G",
    })
    filter.append(this.bodyDisp)

    const accFilter = svgEl("filter", {
      id: `${uid}-at`,
      filterUnits: "userSpaceOnUse",
      x: -64,
      y: -64,
      width: 128,
      height: 128,
    })
    accFilter.append(
      svgEl("feTurbulence", {
        type: "fractalNoise",
        baseFrequency: CHARCOAL_FILTER.baseFrequency,
        numOctaves: CHARCOAL_FILTER.numOctaves,
        seed: 7,
        result: "n",
      }),
    )
    this.accDisp = svgEl("feDisplacementMap", {
      in: "SourceGraphic",
      in2: "n",
      scale: CHARCOAL_FILTER.accessoryScaleFull,
      xChannelSelector: "R",
      yChannelSelector: "G",
    })
    accFilter.append(this.accDisp)

    const clip = svgEl("clipPath", { id: `${uid}-cl` })
    this.clipPath = svgEl("path")
    clip.append(this.clipPath)
    defs.append(filter, accFilter, clip)
    this.svg.append(defs)

    this.shadowGroup = svgEl("g")
    this.shadowLines = [0, 1, 2].map(() =>
      svgEl("path", { fill: "none", "stroke-linecap": "round", "stroke-width": 1.5 }),
    )
    this.shadowGroup.append(...this.shadowLines)
    this.svg.append(this.shadowGroup)

    this.fxBackGroup = svgEl("g")
    this.fxBackPaths = []
    this.svg.append(this.fxBackGroup)

    this.accBackGroup = svgEl("g")
    this.accBackPaths = []
    this.svg.append(this.accBackGroup)

    this.bodyRig = svgEl("g")
    this.washPath = svgEl("path", { opacity: 0 })
    this.bodyFillPath = svgEl("path")
    const shadeGroup = svgEl("g", { "clip-path": `url(#${uid}-cl)` })
    this.scribblePath = svgEl("path", {
      fill: "none",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      "stroke-width": 6.8,
      opacity: 0,
    })
    shadeGroup.append(this.scribblePath)

    this.stroke1 = svgEl("path", {
      fill: "none",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      opacity: 0,
    })
    this.stroke2 = svgEl("path", {
      fill: "none",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      opacity: 0,
    })
    this.bodyRig.append(this.washPath, this.bodyFillPath, shadeGroup, this.stroke1, this.stroke2)
    this.svg.append(this.bodyRig)

    this.accBodyGroup = svgEl("g")
    this.accBodyPaths = []
    this.svg.append(this.accBodyGroup)

    this.faceRig = svgEl("g")
    this.faceGroup = svgEl("g")
    this.blushL = svgEl("path", { fill: "none", "stroke-linecap": "round", "stroke-width": 1.6, opacity: 0 })
    this.blushR = svgEl("path", { fill: "none", "stroke-linecap": "round", "stroke-width": 1.6, opacity: 0 })
    this.eyeL = svgEl("path", { "data-steve-eye": "left", "stroke-linejoin": "round" })
    this.eyeR = svgEl("path", { "data-steve-eye": "right", "stroke-linejoin": "round" })
    this.arcL = svgEl("path", { fill: "none", "stroke-linecap": "round", "stroke-linejoin": "round", opacity: 0 })
    this.arcR = svgEl("path", { fill: "none", "stroke-linecap": "round", "stroke-linejoin": "round", opacity: 0 })
    this.winkRightArc = svgEl("path", {
      fill: "none",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      opacity: 0,
    })
    this.mouth = svgEl("path", {
      fill: "none",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      "stroke-width": 2.2,
      opacity: 0,
    })
    this.accFaceGroup = svgEl("g")
    this.accFacePaths = []
    this.faceGroup.append(
      this.blushL,
      this.blushR,
      this.eyeL,
      this.eyeR,
      this.arcL,
      this.arcR,
      this.winkRightArc,
      this.mouth,
      this.accFaceGroup,
    )
    this.faceRig.append(this.faceGroup)
    this.svg.append(this.faceRig)

    this.fxFrontGroup = svgEl("g")
    this.fxFrontPaths = []
    this.svg.append(this.fxFrontGroup)
  }

  setSize(sizePx: number): void {
    this.svg.setAttribute("width", String(sizePx))
    this.svg.setAttribute("height", String(sizePx))
  }

  private syncThemeStyles(
    colors: PaletteTokens,
    tier: OpticalTier,
    sw: number,
    enableToothFilter: boolean,
  ): void {
    const key = `${colors.ink}|${colors.paper}|${tier}|${sw}|${enableToothFilter ? 1 : 0}`
    if (this.lastStyleKey === key) return
    this.lastStyleKey = key

    if (enableToothFilter) {
      this.bodyDisp.setAttribute(
        "scale",
        String(tier === "compact" ? CHARCOAL_FILTER.scaleCompact : CHARCOAL_FILTER.scaleFull),
      )
      this.accDisp.setAttribute(
        "scale",
        String(tier === "compact" ? CHARCOAL_FILTER.accessoryScaleCompact : CHARCOAL_FILTER.accessoryScaleFull),
      )
      this.bodyRig.setAttribute("filter", `url(#${this.uid}-bt)`)
      this.accBackGroup.setAttribute("filter", `url(#${this.uid}-at)`)
      this.accBodyGroup.setAttribute("filter", `url(#${this.uid}-at)`)
      this.accFaceGroup.setAttribute("filter", `url(#${this.uid}-at)`)
    } else {
      this.bodyRig.removeAttribute("filter")
      this.accBackGroup.removeAttribute("filter")
      this.accBodyGroup.removeAttribute("filter")
      this.accFaceGroup.removeAttribute("filter")
    }

    this.washPath.setAttribute("fill", colors.wash)
    this.bodyFillPath.setAttribute("fill", colors.ink)
    this.scribblePath.setAttribute("stroke", colors.ink)
    this.stroke1.setAttribute("stroke", colors.ink)
    this.stroke1.setAttribute("stroke-width", (3.8 * sw).toFixed(2))
    this.stroke2.setAttribute("stroke", colors.ink)
    this.stroke2.setAttribute("stroke-width", (2.0 * sw).toFixed(2))

    for (const ln of this.shadowLines) {
      ln.setAttribute("stroke", colors.ink)
    }

    this.eyeL.setAttribute("fill", colors.paper)
    this.eyeR.setAttribute("fill", colors.paper)
    this.arcL.setAttribute("stroke", colors.paper)
    this.arcR.setAttribute("stroke", colors.paper)
    this.winkRightArc.setAttribute("stroke", colors.paper)
    this.mouth.setAttribute("stroke", colors.paper)
    this.blushL.setAttribute("stroke", colors.accent)
    this.blushR.setAttribute("stroke", colors.accent)
  }

  render(frame: CompiledFrame): void {
    if (this.viewHalf !== frame.viewHalf) {
      this.viewHalf = frame.viewHalf
      this.svg.setAttribute(
        "viewBox",
        `${-frame.viewHalf} ${-frame.viewHalf} ${frame.viewHalf * 2} ${frame.viewHalf * 2}`,
      )
    }
    this.svg.setAttribute("data-steve-theme", frame.theme)
    if (frame.accessory) this.svg.setAttribute("data-mascot-accessory", frame.accessory)
    else this.svg.removeAttribute("data-mascot-accessory")

    const c = frame.colors
    const l = frame.layers
    this.syncThemeStyles(c, frame.tier, frame.strokeWidthScale, frame.enableToothFilter)

    this.bodyRig.setAttribute("transform", frame.bodyTransform)
    this.faceRig.setAttribute("transform", frame.bodyTransform)
    this.clipPath.setAttribute("d", frame.cleanBodyPath)
    this.bodyFillPath.setAttribute("d", frame.cleanBodyPath)
    this.bodyFillPath.setAttribute("opacity", l.bodyFillOpacity.toFixed(2))

    if (l.washTransform) {
      this.washPath.setAttribute("d", frame.cleanBodyPath)
      this.washPath.setAttribute("transform", l.washTransform)
      this.washPath.setAttribute("opacity", (c.washOpacity * 0.78).toFixed(2))
    } else {
      this.washPath.setAttribute("opacity", "0")
    }

    this.scribblePath.setAttribute("d", l.scribblePath)
    this.scribblePath.setAttribute("opacity", l.scribblePath ? "0.92" : "0")
    this.stroke1.setAttribute("d", l.stroke1)
    this.stroke1.setAttribute("opacity", l.stroke1 ? "0.96" : "0")
    this.stroke2.setAttribute("d", l.stroke2)
    this.stroke2.setAttribute("opacity", l.stroke2 ? "0.62" : "0")

    if (frame.floorTicks.length > 0) {
      this.shadowGroup.setAttribute("opacity", "1")
      for (let i = 0; i < this.shadowLines.length; i++) {
        const tick = frame.floorTicks[i]
        const ln = this.shadowLines[i]!
        if (tick) {
          ln.setAttribute("d", tick.d)
          ln.setAttribute("opacity", tick.opacity.toFixed(2))
        } else {
          ln.setAttribute("opacity", "0")
        }
      }
    } else {
      this.shadowGroup.setAttribute("opacity", "0")
    }

    const f = frame.face
    this.faceGroup.setAttribute("transform", f.transform)
    this.blushL.setAttribute("d", f.blushL)
    this.blushR.setAttribute("d", f.blushR)
    this.blushL.setAttribute("opacity", f.blushOpacity.toFixed(2))
    this.blushR.setAttribute("opacity", f.blushOpacity.toFixed(2))

    if (f.showOvalEyes) {
      this.eyeL.setAttribute("opacity", "1")
      this.eyeL.setAttribute("d", f.eyeLPath)
      if (f.winkRightArc) {
        this.eyeR.setAttribute("opacity", "0")
        this.winkRightArc.setAttribute("opacity", "1")
        this.winkRightArc.setAttribute("d", f.winkRightArc)
        this.winkRightArc.setAttribute("stroke-width", f.arcStrokeWidth.toFixed(2))
      } else {
        this.winkRightArc.setAttribute("opacity", "0")
        this.eyeR.setAttribute("opacity", "1")
        this.eyeR.setAttribute("d", f.eyeRPath)
      }
      this.arcL.setAttribute("opacity", "0")
      this.arcR.setAttribute("opacity", "0")
    } else {
      this.eyeL.setAttribute("opacity", "0")
      this.eyeR.setAttribute("opacity", "0")
      this.winkRightArc.setAttribute("opacity", "0")
      this.arcL.setAttribute("opacity", "1")
      this.arcR.setAttribute("opacity", "1")
      this.arcL.setAttribute("d", f.arcL)
      this.arcR.setAttribute("d", f.arcR)
      this.arcL.setAttribute("stroke-width", f.arcStrokeWidth.toFixed(2))
      this.arcR.setAttribute("stroke-width", f.arcStrokeWidth.toFixed(2))
    }

    this.mouth.setAttribute("d", f.mouthPath)
    this.mouth.setAttribute("fill", f.mouthFilled ? c.paper : "none")
    this.mouth.setAttribute("opacity", f.mouthOpacity.toFixed(2))

    this.syncFxLayer(this.accBackGroup, this.accBackPaths, frame.accessoryBack, frame.bodyTransform, c.accent)
    this.syncFxLayer(this.accBodyGroup, this.accBodyPaths, frame.accessoryBody, frame.bodyTransform, c.accent)
    this.syncFxLayer(this.accFaceGroup, this.accFacePaths, frame.accessoryFace, "", c.accent)
    this.syncFxLayer(this.fxBackGroup, this.fxBackPaths, frame.fxBack, frame.fxBackTransform, c.accent)
    this.syncFxLayer(this.fxFrontGroup, this.fxFrontPaths, frame.fxFront, frame.fxFrontTransform, c.accent)
  }

  private syncFxLayer(
    group: SVGGElement,
    pool: SVGPathElement[],
    strokes: readonly FxStroke[],
    groupTransform: string,
    defaultStroke: string,
  ): void {
    group.setAttribute("transform", groupTransform)
    while (pool.length < strokes.length) {
      const p = svgEl("path", {
        fill: "none",
        "stroke-linecap": "round",
        "stroke-linejoin": "round",
        opacity: 0,
      })
      group.append(p)
      pool.push(p)
    }
    for (let i = 0; i < pool.length; i++) {
      const el = pool[i]!
      const s = strokes[i]
      if (!s) {
        el.setAttribute("opacity", "0")
        continue
      }
      el.setAttribute("d", s.d)
      el.setAttribute("fill", s.fill ?? "none")
      el.setAttribute("stroke", s.stroke ?? defaultStroke)
      el.setAttribute("stroke-width", s.strokeWidth.toFixed(2))
      el.setAttribute("opacity", s.opacity.toFixed(2))
      if (s.transform) el.setAttribute("transform", s.transform)
      else el.removeAttribute("transform")
    }
  }
}
