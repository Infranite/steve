import assert from "node:assert/strict"
import { readdirSync, readFileSync } from "node:fs"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { CueChoreographer } from "../src/steve-mascot/choreographer.ts"
import { STEVE_PALETTE, TIMING, resolveOpticalTier } from "../src/steve-mascot/constants.ts"
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
