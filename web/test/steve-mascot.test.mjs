import assert from "node:assert/strict"
import { readdirSync, readFileSync } from "node:fs"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { BOT_ACCESSORIES } from "../src/steve-mascot/accessories.ts"
import { CueChoreographer } from "../src/steve-mascot/choreographer.ts"
import {
  ACCESSORY_MIN_SIZE_PX,
  BOT_ROUND_PRESETS,
  STEVE_PALETTE,
  TIMING,
  resolveBotPalette,
  resolveOpticalTier,
} from "../src/steve-mascot/constants.ts"
import { compileFrame } from "../src/steve-mascot/frame.ts"
import { LifeLayer } from "../src/steve-mascot/life.ts"
import { PhysicsRig } from "../src/steve-mascot/physics.ts"
import {
  clearStaticCache,
  getStaticCacheStats,
  renderAdaptiveFaviconSvg,
  renderStaticSvg,
} from "../src/steve-mascot/static-svg.ts"

const HERO_CUES = [
  "input",
  "think",
  "success",
  "affirm",
  "failure",
  "wary",
  "inspect",
  "love",
  "reply",
  "idle",
]

test("finalized Steve static SVGs render single charcoal style cleanly at 16, 24, 32, 64, and 240px across Light & Dark", () => {
  clearStaticCache()
  const sizes = [16, 24, 32, 64, 240]
  const seenClipIds = new Set()

  for (const size of sizes) {
    for (const theme of ["light", "dark"]) {
      const svg = renderStaticSvg({ size, theme })
      const palette = STEVE_PALETTE[theme]

      assert.match(svg, /^<svg\b[^>]*data-steve-avatar="static"/)
      assert.match(svg, new RegExp(`data-steve-theme="${theme}"`))
      assert.match(svg, new RegExp(`width="${size}" height="${size}"`))
      assert.match(svg, /data-steve-eye="left"/)
      assert.match(svg, /data-steve-eye="right"/)
      assert.ok(svg.includes(`fill="${palette.ink}"`), `${size}px ${theme}: charcoal body uses ${palette.ink}`)
      assert.ok(svg.includes(`fill="${palette.paper}"`), `${size}px ${theme}: giant cutout eyes use ${palette.paper}`)
      assert.doesNotMatch(svg, /<ellipse\b|<circle\b/, `${size}px ${theme}: no inner ring pupils or floating beads`)

      const clipMatch = svg.match(/<clipPath id="([^"]+)"/)
      assert.ok(clipMatch, `${size}px ${theme}: has clipPath`)
      assert.ok(!seenClipIds.has(clipMatch[1]), `${size}px ${theme}: unique clipPath id across size/theme`)
      seenClipIds.add(clipMatch[1])
    }
  }

  assert.equal(resolveOpticalTier(16), "micro")
  assert.equal(resolveOpticalTier(24), "mini")
  assert.equal(resolveOpticalTier(32), "compact")
  assert.equal(resolveOpticalTier(64), "full")
  assert.equal(resolveOpticalTier(240), "full")

  // Cache hit verification: re-rendering same specs must hit cache with zero new misses
  const before = getStaticCacheStats()
  assert.equal(before.size, sizes.length * 2)
  assert.equal(before.misses, sizes.length * 2)
  assert.equal(before.hits, 0)
  for (const size of sizes) {
    renderStaticSvg({ size, theme: "light" })
  }
  const after = getStaticCacheStats()
  assert.equal(after.hits, sizes.length)
  assert.equal(after.misses, before.misses)
})

test("adaptive SVG favicon switches Light and Dark charcoal palettes via prefers-color-scheme", () => {
  const favicon = renderAdaptiveFaviconSvg(32)
  assert.match(favicon, /@media\(prefers-color-scheme:dark\)/)
  assert.ok(favicon.includes(STEVE_PALETTE.light.ink))
  assert.ok(favicon.includes(STEVE_PALETTE.light.paper))
  assert.ok(favicon.includes(STEVE_PALETTE.dark.ink))
  assert.ok(favicon.includes(STEVE_PALETTE.dark.paper))
  assert.doesNotMatch(favicon, /<circle\b|<ellipse\b/)
})

test("CueChoreographer handles all 10 HeroCue values, behind-the-back prop transitions, and settles cleanly to 0-CPU idle", () => {
  const impulses = []
  const choreo = new CueChoreographer((imp) => impulses.push(imp))

  // Initially calm idle -> does not require continuous rAF frames
  assert.equal(choreo.getState(), "idle")
  assert.equal(choreo.isCalmIdle(), true)
  assert.equal(choreo.needsContinuousFrame(), false)

  // Verify every HeroCue maps deterministically
  choreo.applyHeroCue("input")
  let pose = choreo.step(0.016, 1.0, 0, 0, false)
  assert.equal(choreo.getState(), "listening")
  assert.equal(pose.fx, "listen-ear")
  assert.ok(pose.trot < -12, "listening tilts head noticeably")
  assert.ok(pose.faceOffsetX < -7, "listening turns face left to present right ear")
  assert.equal(choreo.needsContinuousFrame(), true)

  choreo.applyHeroCue("think")
  pose = choreo.step(0.016, 2.0, 0, 0, false)
  assert.equal(choreo.getState(), "thinking")
  assert.equal(pose.fx, "think-bubbles")
  assert.equal(pose.expression, "think-squint")

  // Bounded reply reaction speaks with syllable mouth + speech ticks, then settles back to calm idle
  choreo.applyHeroCue("reply")
  pose = choreo.step(0.016, 3.0, 0, 0, false)
  assert.equal(choreo.getState(), "idle")
  assert.equal(choreo.getActiveOneShotId(), "reply")
  assert.equal(pose.mouth, "speak")
  assert.equal(pose.fx, "speech-ticks")
  pose = choreo.step(0.016, 5.5, 0, 0, false)
  assert.equal(choreo.getActiveOneShotId(), null)
  assert.equal(choreo.isCalmIdle(), true)

  // Inspect reaction pulls magnifying glass out from behind back (back -> front Z-layer), then stows back behind back
  choreo.applyHeroCue("inspect")
  pose = choreo.step(0.08, 6.0, 0, 0, false)
  assert.equal(pose.propTransition?.fx, "inspect-lens")
  assert.equal(pose.propTransition?.stage, "entering")
  const enterFrame = compileFrame({
    theme: "light",
    tier: "full",
    live: true,
    pose,
    physics: new PhysicsRig(),
    sleepAmt: 0,
    loveAmt: 0,
    blinkAmt: 0,
    boilFrame: 0,
    tSec: 6.0,
    seed: TIMING.steveSeed,
    reducedMotion: false,
    activeMotion: true,
  })
  assert.ok(enterFrame.fxBack.length > 0, "entering prop starts behind Steve's body")
  assert.equal(enterFrame.fxFront.length, 0)

  // Advance to settled active front layer
  let t = 6.0
  for (let i = 0; i < 12; i++) {
    t += 0.05
    pose = choreo.step(0.05, t, 0, 0, false)
  }
  assert.equal(pose.propTransition?.stage, "active")

  // End inspect -> stows behind back smoothly until propTransition clears
  choreo.applyHeroCue("idle")
  pose = choreo.step(0.08, t + 0.08, 0, 0, false)
  assert.equal(pose.propTransition?.stage, "exiting")
  for (let i = 0; i < 10; i++) {
    t += 0.06
    pose = choreo.step(0.06, t, 0, 0, false)
  }
  assert.equal(choreo.getPropTransition(), null)
  assert.equal(choreo.needsContinuousFrame(), false)

  // Remaining HeroCues: wary, success, affirm, failure, love, idle
  for (const [cue, expectedExpr, expectedFx] of [
    ["wary", "wary-half", "wait-hourglass"],
    ["success", "happy-arc", "success-burst"],
    ["affirm", "normal", "none"],
    ["failure", "dizzy-x", "failure-zigs"],
    ["love", "happy-arc", "love-heart"],
  ]) {
    t += 3.0
    choreo.applyHeroCue(cue)
    pose = choreo.step(0.016, t, 0, 0, false)
    assert.equal(choreo.getActiveOneShotId(), cue, `cue ${cue} starts one-shot`)
    assert.equal(pose.expression, expectedExpr, `cue ${cue} expression`)
    assert.equal(pose.fx, expectedFx, `cue ${cue} fx`)
  }

  // Ensure our test covered all 10 HeroCue names
  assert.equal(HERO_CUES.length, 10)
  choreo.applyHeroCue("idle")
  for (let i = 0; i < 40; i++) {
    t += 0.05
    choreo.step(0.05, t, 0, 0, false)
  }
  assert.equal(choreo.needsContinuousFrame(), false, "settles back to 0-CPU idle")
  assert.ok(impulses.length > 0, "reactions emit physics impulses")
})

test("PhysicsRig converges and reports settled for zero-CPU dormancy, and LifeLayer triggers auto-sleep & idle behaviors", () => {
  const rig = new PhysicsRig()
  const target = { tx: 2.0, ty: 1.0, tsq: 0, trot: 2.4, faceX: 4.1, faceY: 2.7 }
  assert.equal(rig.isSettled(target), false)
  for (let i = 0; i < 120; i++) {
    rig.step(1 / 60, target)
  }
  assert.equal(rig.isSettled(target), true, "springs settle cleanly after pointer stops")

  const life = new LifeLayer(TIMING.steveSeed, 1000)
  assert.equal(life.shouldAutoSleep(1000 + TIMING.autoSleepMs - 1, TIMING.autoSleepMs), false)
  assert.equal(life.shouldAutoSleep(1000 + TIMING.autoSleepMs, TIMING.autoSleepMs), true)
  assert.ok(life.getNextIdleWakeDelayMs(1000, TIMING.autoSleepMs) >= 40)

  // Autonomous idle micro-behavior fires after scheduled deadline
  const behavior = life.stepIdleBehavior(1000 + 20_000, true)
  assert.ok(
    ["head-tilt", "look-around", "yawn", "wink", "hum-note", "sip-coffee"].includes(behavior),
    `unexpected idle behavior: ${behavior}`,
  )
})

test("ordinary Bots share the charcoal family while staying distinct from Steve and each other by color & roundness", () => {
  clearStaticCache()
  const colors = ["#7aa7c7", "#8c7ae6", "#5fb39a", "#e0a458", "#d9777f", "#6c7a89", "#3d4a5c", "#b08968", "#ffff00", "#000000"]

  const parseRgb = (hex) => [
    Number.parseInt(hex.slice(1, 3), 16),
    Number.parseInt(hex.slice(3, 5), 16),
    Number.parseInt(hex.slice(5, 7), 16),
  ]
  const lum = (hex) => {
    const [r, g, b] = parseRgb(hex).map((v) => {
      const u = v / 255
      return u <= 0.03928 ? u / 12.92 : Math.pow((u + 0.055) / 1.055, 2.4)
    })
    return 0.2126 * r + 0.7152 * g + 0.0722 * b
  }
  const contrast = (a, b) => {
    const la = lum(a)
    const lb = lum(b)
    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05)
  }

  for (const hex of colors) {
    for (const theme of ["light", "dark"]) {
      const pal = resolveBotPalette(hex, theme)
      assert.notEqual(pal.ink, STEVE_PALETTE[theme].ink, `${hex} ${theme}: Bot stays distinct from Steve monochrome`)
      assert.ok(
        contrast(pal.ink, pal.paper) >= 4.4,
        `${hex} ${theme}: giant cutout eyes maintain >= 4.4:1 contrast (got ${contrast(pal.ink, pal.paper).toFixed(2)})`,
      )
    }
  }

  // Distinct roundness values produce distinct silhouette paths and unique clipPath IDs
  const byRound = new Map()
  for (const round of BOT_ROUND_PRESETS) {
    const svg = renderStaticSvg({
      appearance: { kind: "bot", id: `bot_${round}`, color: "#8c7ae6", round },
      size: 64,
      theme: "light",
    })
    assert.match(svg, /data-mascot-avatar="static"/)
    assert.match(svg, /data-mascot-kind="bot"/)
    assert.doesNotMatch(svg, /data-steve-avatar/)
    const clipBody = svg.match(/<clipPath id="[^"]+"><path d="([^"]+)"/)?.[1]
    assert.ok(clipBody, `round=${round}: has clipPath body`)
    for (const [prevRound, prevBody] of byRound) {
      assert.notEqual(clipBody, prevBody, `round ${round} contour differs from round ${prevRound}`)
    }
    byRound.set(round, clipBody)
  }
  assert.equal(BOT_ACCESSORIES.length, 10)
  assert.equal(ACCESSORY_MIN_SIZE_PX, 27)
})

test("all 10 Bot accessories render cleanly at >= 27px, omit at <= 26px, stay cache-isolated, and never appear on Steve", () => {
  clearStaticCache()

  const bare64 = renderStaticSvg({
    appearance: { kind: "bot", id: "bot_bare", color: "#7aa7c7", round: 45, accessory: null },
    size: 64,
    theme: "light",
  })
  assert.doesNotMatch(bare64, /data-mascot-accessory=/)

  for (const accessory of BOT_ACCESSORIES) {
    for (const theme of ["light", "dark"]) {
      // Compact (32px) and full (64px) render the accessory
      for (const size of [32, 64]) {
        const svg = renderStaticSvg({
          appearance: { kind: "bot", id: "bot_acc", color: "#8c7ae6", round: 45, accessory },
          size,
          theme,
        })
        assert.match(svg, new RegExp(`data-mascot-accessory="${accessory}"`), `${accessory} ${size}px ${theme} renders`)
        if (size === 64 && theme === "light") {
          assert.notEqual(svg, bare64, `${accessory} changes rendered SVG markup`)
        }
      }
      // Micro (16px) and mini (24px) omit the accessory for optical clarity
      for (const size of [16, 24]) {
        const svg = renderStaticSvg({
          appearance: { kind: "bot", id: "bot_acc", color: "#8c7ae6", round: 45, accessory },
          size,
          theme,
        })
        assert.doesNotMatch(svg, /data-mascot-accessory=/, `${accessory} omitted at ${size}px ${theme}`)
      }
    }
  }

  // Steve never renders accessories at any size
  for (const size of [16, 24, 32, 64, 240]) {
    const steveSvg = renderStaticSvg({ appearance: { kind: "steve" }, size, theme: "light" })
    assert.doesNotMatch(steveSvg, /data-mascot-accessory=/)
  }

  // Changing Bot body color must not alter fixed accessory fill/stroke colors
  const beretBlue = renderStaticSvg({
    appearance: { kind: "bot", id: "same", color: "#7aa7c7", round: 45, accessory: "beret" },
    size: 64,
    theme: "light",
  })
  const beretAmber = renderStaticSvg({
    appearance: { kind: "bot", id: "same", color: "#e0a458", round: 45, accessory: "beret" },
    size: 64,
    theme: "light",
  })
  assert.ok(beretBlue.includes("#c73e50") && beretAmber.includes("#c73e50"), "beret color stays fixed regardless of Bot body color")
})

test("steve-mascot module contains zero traces of exploratory schemes, beads, or protocol adapters", () => {
  const dir = fileURLToPath(new URL("../src/steve-mascot", import.meta.url))
  const files = readdirSync(dir).filter((f) => f.endsWith(".ts") || f.endsWith(".tsx"))
  assert.ok(files.length >= 10)

  const forbidden = /\b(wireCoil|wireGhost|buildContinuousCoil|buildCrossHatch|bead-jiggle|charcoal-beads|spark-crumbs|C wire|B chalk|a2a|acp|mcp|orbiter)\b/i
  for (const name of files) {
    const content = readFileSync(new URL(`../src/steve-mascot/${name}`, import.meta.url), "utf8")
    assert.doesNotMatch(content, forbidden, `${name} must not retain exploratory scheme or protocol terms`)
  }
})
