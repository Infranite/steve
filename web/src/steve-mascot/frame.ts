/**
 * Pure, DOM-free Frame Compiler for Steve's finalized charcoal mascot.
 * Live DOM rendering and synchronous static SVG export share this single source of truth.
 */

import { renderBotAccessory } from "./accessories.ts"
import { GIANT_EYES, GEOMETRY, OPTICAL_TIERS, PROP_TRANSITION, resolveAppearancePalette } from "./constants.ts"
import { getFxRenderer } from "./fx.ts"
import {
  buildCharcoalScribble,
  buildFloorTicks,
  buildLooseLoop,
  buildSculptedEyePath,
  clamp,
  closedSmoothPath,
  hashNoise,
  sampleMascotContour,
} from "./geometry.ts"
import type { PhysicsRig } from "./physics.ts"
import type {
  CompiledCharcoalLayers,
  CompiledFace,
  CompiledFrame,
  FxKind,
  FxStroke,
  MascotAppearance,
  MotionPose,
  OpticalTier,
  PaletteTokens,
  ThemeMode,
  Vec2,
} from "./types.ts"

function computePropArcTransform(
  phase: number,
  side: 1 | -1,
  pivotY: number,
): { layer: "back" | "front"; transform: string; opacity: number; settled: boolean } {
  const p = clamp(phase, 0, 1)
  if (p >= 0.999) {
    return { layer: "front", transform: "", opacity: 1, settled: true }
  }
  const layer: "back" | "front" = p < PROP_TRANSITION.zFlipPhase ? "back" : "front"
  const swing = Math.sin(p * Math.PI)
  const dx = side * swing * PROP_TRANSITION.arcSwingX
  const dy = -swing * PROP_TRANSITION.arcLiftY - (1 - p) * 7.5
  const scale = 0.56 + 0.44 * Math.pow(p, 0.72)
  const rot = side * swing * 18.0
  const opacity = p < PROP_TRANSITION.backFadeThreshold ? p / PROP_TRANSITION.backFadeThreshold : 1
  const transform =
    `translate(${dx.toFixed(2)}, ${dy.toFixed(2)}) ` +
    `translate(0, ${pivotY.toFixed(1)}) ` +
    `rotate(${rot.toFixed(2)}) ` +
    `scale(${scale.toFixed(3)}) ` +
    `translate(0, ${(-pivotY).toFixed(1)})`
  return { layer, transform, opacity, settled: p >= 0.9 }
}

function wrapPropStrokesWithArc(
  strokes: readonly FxStroke[],
  arc: ReturnType<typeof computePropArcTransform>,
): readonly FxStroke[] {
  if (!arc.transform && arc.opacity >= 0.999) return strokes
  return strokes.map((s) => ({
    ...s,
    opacity: s.opacity * arc.opacity,
    transform: s.transform ? `${arc.transform} ${s.transform}` : arc.transform,
  }))
}

function renderFx(params: {
  fx: FxKind
  t: number
  seed: number
  theme: ThemeMode
  reducedMotion: boolean
  inTransit: boolean
  enabled: boolean
  colors: PaletteTokens
  bounds: { readonly minX: number; readonly maxX: number; readonly minY: number; readonly maxY: number }
}): { back: readonly FxStroke[]; front: readonly FxStroke[]; attach: "body" | "stage" } {
  if (!params.enabled || params.fx === "none") {
    return { back: [], front: [], attach: "stage" }
  }
  const renderer = getFxRenderer(params.fx)
  if (!renderer) {
    return { back: [], front: [], attach: "stage" }
  }
  const layers = renderer.render({
    t: params.t,
    seed: params.seed,
    theme: params.theme,
    reducedMotion: params.reducedMotion,
    inTransit: params.inTransit,
    colors: params.colors,
    bounds: params.bounds,
  })
  return { attach: renderer.attach, back: layers.back, front: layers.front }
}

export function compileFrame(params: {
  appearance?: MascotAppearance
  theme: ThemeMode
  tier: OpticalTier
  live: boolean
  pose: MotionPose
  physics: PhysicsRig
  sleepAmt: number
  loveAmt: number
  blinkAmt: number
  boilFrame: number
  tSec: number
  seed: number
  reducedMotion: boolean
  activeMotion: boolean
}): CompiledFrame {
  const {
    theme,
    tier,
    live,
    pose,
    physics,
    sleepAmt,
    loveAmt,
    blinkAmt,
    boilFrame,
    tSec,
    reducedMotion,
    activeMotion,
  } = params
  const appearance: MascotAppearance = params.appearance ?? { kind: "steve" }
  const tierCfg = OPTICAL_TIERS[tier]
  const colors = resolveAppearancePalette(appearance, theme)
  const frameSeed = boilFrame * 37.7 + params.seed

  const bt = physics.getBodyTransform()
  const bodyTransform = `translate(${physics.px.toFixed(2)}, ${(physics.py + 2 + bt.anchorOffsetY).toFixed(2)}) rotate(${physics.rot.toFixed(2)}) scale(${bt.sx.toFixed(3)}, ${bt.sy.toFixed(3)})`

  // 1. Sample Steve's classic circle or Bot's roundness-derived contour
  const contourRound = appearance.kind === "bot" ? appearance.round : null
  const basePts: Vec2[] = []
  let minX = Number.POSITIVE_INFINITY
  let maxX = Number.NEGATIVE_INFINITY
  let minY = Number.POSITIVE_INFINITY
  let maxY = Number.NEGATIVE_INFINITY
  for (let i = 0; i < GEOMETRY.bodySampleCount; i++) {
    const theta = (i / GEOMETRY.bodySampleCount) * Math.PI * 2 - Math.PI / 2
    const pt = sampleMascotContour({
      round: contourRound,
      theta,
      t: reducedMotion ? 0 : tSec,
      sleepAmt,
      loveAmt,
      shiverAmp: pose.shiverAmp,
      reducedMotion,
      activeMotion,
    })
    basePts.push(pt)
    if (pt.x < minX) minX = pt.x
    if (pt.x > maxX) maxX = pt.x
    if (pt.y < minY) minY = pt.y
    if (pt.y > maxY) maxY = pt.y
  }
  const cleanBodyPath = closedSmoothPath(basePts)
  const bounds = { minX, maxX, minY, maxY }

  // 2. Build charcoal surface layers (single finalized charcoal style in both Light & Dark)
  const layers: CompiledCharcoalLayers = {
    washTransform: tierCfg.enableWash
      ? `translate(${(3.4 + bt.lagDeltaX * 0.65).toFixed(2)}, ${(3.8 + bt.lagDeltaY * 0.65).toFixed(2)})`
      : "",
    bodyFillOpacity: tierCfg.bodyFillOpacity,
    scribblePath: tierCfg.enableScribble ? buildCharcoalScribble(frameSeed + 200, basePts) : "",
    stroke1: buildLooseLoop(basePts, frameSeed + 7, 1.55, 4, 2, 0),
    stroke2: tierCfg.enableSecondRim ? buildLooseLoop(basePts, frameSeed + 53, 2.35, 12, 3, 1.15) : "",
  }

  // 3. Floor contact hatch ticks (full tier only)
  const floorTicks: { d: string; opacity: number }[] = []
  if (tierCfg.enableFloorShadow) {
    const lift = Math.max(0, -physics.py)
    const shW = Math.max(12, (24 * bt.sx + sleepAmt * 10) * (1 - lift * 0.02))
    const tickPaths = buildFloorTicks(physics.px * 0.72, shW, frameSeed)
    for (let idx = 0; idx < tickPaths.length; idx++) {
      floorTicks.push({
        d: tickPaths[idx]!,
        opacity: Math.max(0.06, (0.28 - idx * 0.06) * (1 - lift * 0.035)),
      })
    }
  }

  // 4. Giant solid-cutout eyes & expressions (no inner pupils)
  const boost = tierCfg.eyeSizeBoost
  const turn = reducedMotion ? 0 : clamp(physics.fx / 11, -1, 1)
  const eyeGap = GIANT_EYES.eyeGap * (1 - 0.12 * turn * turn)
  const baseRx = GIANT_EYES.eyeRx * boost
  const baseRy = GIANT_EYES.eyeRy * boost
  const faceTransform = `translate(${physics.fx.toFixed(2)}, ${(2.0 + physics.fy + sleepAmt * 9.5).toFixed(2)})`

  const expr = pose.expression
  const isArcExpr = expr === "happy-arc" || expr === "sleep-arc" || expr === "dizzy-x" || expr === "yawn"
  const boilDx = activeMotion && !reducedMotion ? hashNoise(frameSeed + 501) * 0.32 : 0
  const boilDy = activeMotion && !reducedMotion ? hashNoise(frameSeed + 509) * 0.32 : 0

  let rxL = baseRx * (1 - Math.max(0, -turn) * 0.15)
  let rxR = baseRx * (1 - Math.max(0, turn) * 0.15)
  let ryL = baseRy
  let ryR = baseRy
  let topCutL = 0
  let topCutR = 0
  let slantL = 0
  let slantR = 0

  if (expr === "curious") {
    rxL *= 1.12
    ryL *= 1.12
    rxR *= 1.12
    ryR *= 1.12
  } else if (expr === "surprised-wide") {
    rxL *= 1.24
    ryL *= 1.26
    rxR *= 1.24
    ryR *= 1.26
  } else if (expr === "think-squint") {
    topCutL = 0.38
    topCutR = 0.16
    slantL = 0.12
  } else if (expr === "inspect-focus") {
    rxL *= 0.98
    rxR *= 1.14
    ryR *= 1.14
    topCutL = 0.38
    topCutR = 0.14
    slantL = 0.22
    slantR = -0.1
  } else if (expr === "wary-half") {
    rxL *= 1.08
    rxR *= 1.08
    topCutL = 0.42
    topCutR = 0.42
  } else if (pose.mouth === "speak" && !reducedMotion) {
    const b = 1 + Math.abs(Math.sin(tSec * 7.2)) * 0.12
    ryL *= b
    ryR *= b
  }

  const blinkScale = 1 - blinkAmt * 0.88
  ryL = Math.max(0.9, ryL * blinkScale)
  ryR = Math.max(0.9, ryR * blinkScale)
  if (blinkAmt > 0.25) {
    topCutL *= 1 - blinkAmt
    topCutR *= 1 - blinkAmt
  }

  let arcL = ""
  let arcR = ""
  let winkRightArc = ""
  const giantScale = Math.min(1.85, Math.max(1.0, baseRx / 4.2))
  const arcStrokeWidth = (tier === "micro" ? 3.8 : 3.2) * 1.22

  if (expr === "happy-arc") {
    const r = 5.2 * giantScale
    arcL = `M ${(-eyeGap - r).toFixed(1)} 2 Q ${(-eyeGap).toFixed(1)} ${(-r - 2.2).toFixed(1)} ${(-eyeGap + r).toFixed(1)} 2`
    arcR = `M ${(eyeGap - r).toFixed(1)} 2 Q ${eyeGap.toFixed(1)} ${(-r - 2.2).toFixed(1)} ${(eyeGap + r).toFixed(1)} 2`
  } else if (expr === "sleep-arc" || expr === "yawn") {
    const r = 5.0 * giantScale
    arcL = `M ${(-eyeGap - r).toFixed(1)} -1 Q ${(-eyeGap).toFixed(1)} ${(r + 1.6).toFixed(1)} ${(-eyeGap + r).toFixed(1)} -1`
    arcR = `M ${(eyeGap - r).toFixed(1)} -1 Q ${eyeGap.toFixed(1)} ${(r + 1.6).toFixed(1)} ${(eyeGap + r).toFixed(1)} -1`
  } else if (expr === "dizzy-x") {
    const r = 4.2 * giantScale
    arcL = `M ${(-eyeGap - r).toFixed(1)} ${(-r).toFixed(1)} L ${(-eyeGap + r).toFixed(1)} ${r.toFixed(1)} M ${(-eyeGap + r).toFixed(1)} ${(-r).toFixed(1)} L ${(-eyeGap - r).toFixed(1)} ${r.toFixed(1)}`
    arcR = `M ${(eyeGap - r).toFixed(1)} ${(-r).toFixed(1)} L ${(eyeGap + r).toFixed(1)} ${r.toFixed(1)} M ${(eyeGap + r).toFixed(1)} ${(-r).toFixed(1)} L ${(eyeGap - r).toFixed(1)} ${r.toFixed(1)}`
  } else if (expr === "wink") {
    const r = 5.0 * giantScale
    winkRightArc = `M ${(eyeGap - r).toFixed(1)} 1.5 Q ${eyeGap.toFixed(1)} ${(-r - 1.5).toFixed(1)} ${(eyeGap + r).toFixed(1)} 1.5`
  }

  let mouthPath = ""
  let mouthOpacity = 0
  let mouthFilled = false
  const mouthY = Math.min(bounds.maxY - 10.5, Math.max(9.6, baseRy + 2.4))
  if (live || tier === "full" || tier === "compact") {
    if (pose.mouth === "speak") {
      const open = Math.min(7.8, Math.max(0, pose.mouthOpen))
      const hw = 5.2 * pose.mouthWidth
      if (open < 1.1) {
        mouthPath = `M ${(-hw * 0.92).toFixed(1)} ${(mouthY + 1.2).toFixed(1)} Q 0 ${(mouthY + 2.2).toFixed(1)} ${(hw * 0.92).toFixed(1)} ${(mouthY + 1.2).toFixed(1)}`
        mouthFilled = false
      } else {
        const topLipY = mouthY - open * 0.14
        const botLipY = mouthY + open * 1.22
        mouthPath =
          `M ${(-hw).toFixed(1)} ${topLipY.toFixed(1)} ` +
          `Q 0 ${(topLipY - 0.9).toFixed(1)} ${hw.toFixed(1)} ${topLipY.toFixed(1)} ` +
          `C ${(hw * 0.94).toFixed(1)} ${botLipY.toFixed(1)}, ${(-hw * 0.94).toFixed(1)} ${botLipY.toFixed(1)}, ${(-hw).toFixed(1)} ${topLipY.toFixed(1)} Z`
        mouthFilled = true
      }
      mouthOpacity = 0.96
    } else if (pose.mouth === "wavy") {
      mouthPath = `M -5.2 ${(mouthY + 1.4).toFixed(1)} Q -2.6 ${(mouthY - 1.4).toFixed(1)} 0 ${(mouthY + 1.4).toFixed(1)} Q 2.6 ${(mouthY + 4.2).toFixed(1)} 5.2 ${(mouthY + 1.4).toFixed(1)}`
      mouthOpacity = 0.9
    } else if (pose.mouth === "yawn") {
      mouthPath = `M -3.6 ${(mouthY + 1.2).toFixed(1)} A 3.6 4.4 0 1 0 3.6 ${(mouthY + 1.2).toFixed(1)} A 3.6 4.4 0 1 0 -3.6 ${(mouthY + 1.2).toFixed(1)}`
      mouthOpacity = 0.9
      mouthFilled = true
    } else if (pose.mouth === "smile") {
      mouthPath = `M -4.2 ${(mouthY - 0.4).toFixed(1)} Q 0 ${(mouthY + 4.2).toFixed(1)} 4.2 ${(mouthY - 0.4).toFixed(1)}`
      mouthOpacity = 0.9
    }
  }

  const eyeRxL = rxL * GIANT_EYES.cutoutScale
  const eyeRyL = Math.max(1.5, ryL * GIANT_EYES.cutoutScale)
  const eyeRxR = rxR * GIANT_EYES.cutoutScale
  const eyeRyR = Math.max(1.5, ryR * GIANT_EYES.cutoutScale)
  const showBlush = (live || tier === "full") && pose.blushOpacity > 0.01

  const face: CompiledFace = {
    transform: faceTransform,
    showOvalEyes: !isArcExpr,
    eyeLPath: buildSculptedEyePath(-eyeGap + boilDx, boilDy, eyeRxL, eyeRyL, topCutL, slantL),
    eyeRPath: buildSculptedEyePath(eyeGap - boilDx, -boilDy, eyeRxR, eyeRyR, topCutR, slantR),
    arcL,
    arcR,
    arcStrokeWidth,
    winkRightArc,
    mouthPath,
    mouthOpacity,
    mouthFilled,
    blushL: showBlush ? "M -23 11 L -20 6 M -19 11.5 L -16 6.5 M -15 11 L -12 6.5" : "",
    blushR: showBlush ? "M 12 11 L 15 6.5 M 16 11.5 L 19 6.5 M 20 11 L 23 6.5" : "",
    blushOpacity: showBlush ? pose.blushOpacity : 0,
  }

  // 4b. Optional Bot role accessory (Steve never wears accessories; small <= 26px sizes omit them for optical clarity)
  const activeAccessory =
    tierCfg.enableAccessory && appearance.kind === "bot" && appearance.accessory ? appearance.accessory : null
  const accLayers = activeAccessory
    ? renderBotAccessory({
        accessory: activeAccessory,
        theme,
        seed: frameSeed,
        bounds,
        eyeGap,
        eyeRx: baseRx,
        eyeRy: baseRy,
      })
    : { back: [], body: [], face: [] }

  // 5. Cue FX & stowable props (enabled on live instances and full-size static states)
  const enableFx = live || tier === "full"
  const resolveAttach = (attach: "body" | "stage") =>
    attach === "body"
      ? bodyTransform
      : `translate(${(physics.px * 0.82).toFixed(2)}, ${(physics.py * 0.82 + 2).toFixed(2)})`

  const fxBack: FxStroke[] = []
  const fxFront: FxStroke[] = []
  let fxBackTransform = "translate(0, 2)"
  let fxFrontTransform = `translate(${(physics.px * 0.82).toFixed(2)}, ${(physics.py * 0.82 + 2).toFixed(2)})`

  const propTr = pose.propTransition
  const propRenderer = propTr ? getFxRenderer(propTr.fx) : undefined
  if (propTr && propRenderer?.stowable && enableFx) {
    const arc = computePropArcTransform(propTr.phase, propTr.side, propRenderer.stowPivotY ?? 12)
    const built = renderFx({
      fx: propTr.fx,
      t: tSec,
      seed: frameSeed,
      theme,
      reducedMotion: reducedMotion || !arc.settled,
      inTransit: !arc.settled,
      enabled: true,
      colors,
      bounds,
    })
    const wrapped = wrapPropStrokesWithArc(built.front, arc)
    const tf = resolveAttach(built.attach)
    if (arc.layer === "back") {
      fxBack.push(...wrapped)
      fxBackTransform = tf
    } else {
      fxFront.push(...wrapped)
      fxFrontTransform = tf
    }
  }

  if (pose.fx !== "none" && (!propTr || pose.fx !== propTr.fx)) {
    const built = renderFx({
      fx: pose.fx,
      t: tSec,
      seed: frameSeed,
      theme,
      reducedMotion,
      inTransit: false,
      enabled: enableFx,
      colors,
      bounds,
    })
    const tf = resolveAttach(built.attach)
    if (built.back.length > 0) {
      fxBack.push(...built.back)
      fxBackTransform = tf
    }
    if (built.front.length > 0) {
      fxFront.push(...built.front)
      fxFrontTransform = tf
    }
  }

  return {
    viewHalf: tierCfg.viewHalf,
    tier,
    theme,
    colors,
    bodyTransform,
    cleanBodyPath,
    layers,
    strokeWidthScale: tierCfg.strokeWidthScale,
    enableToothFilter: tierCfg.enableToothFilter,
    floorTicks,
    face,
    accessory: activeAccessory,
    accessoryBack: accLayers.back,
    accessoryBody: accLayers.body,
    accessoryFace: accLayers.face,
    fxBack,
    fxBackTransform,
    fxFront,
    fxFrontTransform,
  }
}
