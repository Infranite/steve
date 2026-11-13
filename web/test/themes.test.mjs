import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"
import { ACCENTS, BUILTIN, MONOS, NEUTRALS, RADIUS, SANS } from "../src/themes/builtin.ts"
import { combine, contrast, radiusVars, toCss, tokenValue, validateManifest } from "../src/themes/manifest.ts"

const css = readFileSync(new URL("../src/index.css", import.meta.url), "utf8")
/** the declarations of the first `<sel> {` block of index.css */
function block(sel) {
  const i = css.indexOf(`\n${sel} {`)
  const body = css.slice(css.indexOf("{", i) + 1, css.indexOf("\n}", i))
  return Object.fromEntries([...body.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]))
}
const base = { light: block(":root"), dark: block(".dark") }
const themeVars = block("@theme")

test("every built-in theme passes its own check", () => {
  for (const m of BUILTIN) assert.deepEqual(validateManifest(m), { ok: true, manifest: m }, m.id)
})

test("the default of each kind is exactly what index.css holds", () => {
  for (const mode of ["light", "dark"]) {
    for (const m of [ACCENTS[0], NEUTRALS[0]]) for (const [k, v] of Object.entries(m.tokens[mode])) assert.equal(v, base[mode][k], `${m.id} ${mode} --${k}`)
  }
  assert.equal(SANS[0].fonts.sans, themeVars["font-sans"])
  assert.equal(SANS[0].fonts.sans, themeVars["font-heading"])
  assert.equal(MONOS[0].fonts.mono, themeVars["font-mono"])
  assert.equal(RADIUS[0].radius, 1)
  for (const [k, v] of Object.entries(radiusVars(1))) if (k !== "radius") assert.equal(v, themeVars[k], k)
})

test("a manifest with anything outside the whitelist or the syntax is refused, with the reasons", () => {
  const ok = { id: "ocean", name: { "zh-CN": "海洋", en: "Ocean" }, kind: "accent", tokens: { light: { primary: "#0b6bc0", "--ring": "oklch(0.55 0.15 250)", blue: "rgb(11 107 192 / 90%)" }, dark: { primary: "var(--foreground)" } } }
  const c = validateManifest(ok)
  assert.equal(c.ok, true)
  assert.equal(c.manifest.tokens.light.ring, "oklch(0.55 0.15 250)", "a leading -- is accepted and dropped")
  const bad = (patch, why) => { const r = validateManifest({ ...ok, ...patch }); assert.equal(r.ok, false, why); return r.errors.join(" | ") }
  assert.match(bad({ tokens: { light: { background: "#fff" } } }), /not a token a accent theme may set/, "an accent may not touch the neutrals")
  assert.match(bad({ tokens: { light: { "font-size": "#fff" } } }), /font-size/, "a token outside the whitelist")
  assert.match(bad({ tokens: { light: { primary: "red; } body { display: none" } } }), /not a colour/, "CSS injection")
  assert.match(bad({ tokens: { light: { primary: "url(https://x/y.png)" } } }), /not a colour/)
  assert.match(bad({ tokens: { light: { primary: "var(--primary)" } } }), /not a colour/, "a token pointing at itself")
  assert.match(bad({ tokens: { light: { primary: "var(--evil)" } } }), /not a colour/, "a reference outside the whitelist")
  assert.match(bad({ tokens: { sepia: {} } }), /only light and dark/)
  assert.match(bad({ kind: "skin" }), /kind/)
  assert.match(bad({ id: "Bad Id" }), /id/)
  assert.match(bad({ name: {} }), /name/)
  assert.match(bad({ css: "body{}" }), /not a manifest field/)
  assert.match(bad({ radius: 1 }), /radius: not something a accent theme may set/)
  assert.match(bad({ kind: "radius", tokens: undefined, radius: 9 }), /0 to 2/)
  assert.match(bad({ kind: "sans", tokens: undefined, fonts: { sans: 'x; } * { content: url("a")' } }), /font-family/)
  assert.equal(validateManifest(null).ok, false)
  assert.equal(validateManifest("x").ok, false)
  /* a full skin may set shadows, statuses, fonts and radius; a shadow has to be a shadow */
  const skin = { id: "paper", name: "Paper", kind: "full", radius: 0.5, fonts: { sans: '"Iowan Old Style", serif', mono: "ui-monospace, monospace" },
    tokens: { light: { background: "#f7f3ea", success: "#2b7a4b", lift: "0 1px 2px rgb(0 0 0 / 0.1), inset 0 0 0 1px rgb(0 0 0 / 0.05)" } } }
  assert.equal(validateManifest(skin).ok, true)
  assert.equal(validateManifest({ ...skin, tokens: { light: { lift: "0 1px 2px url(x)" } } }).ok, false)
  assert.equal(validateManifest({ ...skin, tokens: { light: { lift: "red" } } }).ok, false)
})

test("the chosen manifests lay over each other in order, per mode, onto <html>", () => {
  const r = combine([NEUTRALS[2], ACCENTS[3], SANS[1], RADIUS[1]])
  assert.equal(r.light.background, "#fcfcfb")
  assert.equal(r.dark.primary, "#00786e")
  assert.equal(r.common["font-sans"], SANS[1].fonts.sans)
  assert.equal(r.common["font-heading"], SANS[1].fonts.sans)
  assert.equal(r.common["radius-xl"], "8px")
  const later = combine([ACCENTS[1], { id: "x", name: "x", kind: "full", tokens: { light: { primary: "#111111" } } }])
  assert.equal(later.light.primary, "#111111", "a later layer wins")
  assert.equal(later.dark.primary, "#0b6bc0", "and only in the mode it sets")
  const out = toCss(r)
  assert.match(out, /^:root:root \{ --font-sans: /m)
  assert.match(out, /^:root:root:not\(\.dark\) \{ .*--background: #fcfcfb;/m)
  assert.match(out, /^:root:root\.dark \{ .*--primary: #00786e;/m)
  assert.equal(toCss(combine([])), "")
})

test("every accent on every neutral meets WCAG AA in both modes", () => {
  for (const n of NEUTRALS) for (const a of ACCENTS) for (const mode of ["light", "dark"]) {
    const r = combine([n, a]), v = (k) => tokenValue(k, mode, r, base)
    const at = `${a.id} on ${n.id}, ${mode}`
    assert.ok(contrast(v("primary-foreground"), v("primary")) >= 4.5, `text on primary, ${at}: ${contrast(v("primary-foreground"), v("primary")).toFixed(2)}`)
    for (const s of ["background", "card"]) {
      assert.ok(contrast(v("blue"), v(s)) >= 4.5, `blue text on ${s}, ${at}: ${contrast(v("blue"), v(s)).toFixed(2)}`)
      assert.ok(contrast(v("foreground"), v(s)) >= 7, `ink on ${s}, ${at}`)
      assert.ok(contrast(v("muted-foreground"), v(s)) >= 4.5, `secondary text on ${s}, ${at}`)
    }
    assert.ok(contrast(v("primary"), v("background")) >= 3, `primary against the page (a control's edge), ${at}`)
  }
})

test("themes from elsewhere register through a check and never break what is chosen", async () => {
  const T = await import("../src/themes/index.ts")
  const warn = console.warn; console.warn = () => {}
  try {
    const before = T.themesFor("accent").map((m) => m.id)
    assert.deepEqual(before, ACCENTS.map((m) => m.id))
    assert.equal(T.registerTheme({ id: "evil", name: "Evil", kind: "accent", tokens: { light: { primary: "red;}" } } }).ok, false)
    assert.equal(T.registerTheme({ ...ACCENTS[1], name: { en: "Mine" } }).ok, false, "a built-in cannot be replaced")
    assert.deepEqual(T.themesFor("accent").map((m) => m.id), before)
    const ocean = { id: "plugin.ocean", name: { "zh-CN": "海洋", en: "Ocean" }, kind: "accent", tokens: { light: { primary: "#0b5e8a" }, dark: { primary: "#0b5e8a" } } }
    assert.equal(T.registerTheme(ocean).ok, true)
    assert.deepEqual(T.themesFor("accent").map((m) => m.id), [...before, "plugin.ocean"])
    assert.equal(T.themeName(T.getTheme("plugin.ocean"), "zh-CN"), "海洋")
    assert.equal(T.themeName(T.getTheme("plugin.ocean"), "fr"), "Ocean")
    /* a stored choice of a theme that is not there shows the default, and comes back with it */
    T.setAppearance({ accent: "plugin.ocean" })
    assert.match(T.appearanceCss(), /--primary: #0b5e8a/)
    T.unregisterTheme("plugin.ocean")
    assert.equal(T.chosen("accent").id, "indigo")
    assert.equal(T.appearanceCss(), "", "the defaults add nothing")
    T.registerTheme(ocean)
    assert.equal(T.chosen("accent").id, "plugin.ocean")
    /* a skin only fills the skin slot, an accent never does */
    T.setAppearance({ skin: "plugin.ocean" })
    assert.equal(T.chosen("skin"), undefined)
    T.resetAppearance()
    assert.deepEqual(T.getAppearance(), T.DEFAULT_APPEARANCE)
  } finally { console.warn = warn }
})
