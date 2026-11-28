import assert from "node:assert/strict"
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { test } from "node:test"
import { fileURLToPath } from "node:url"
import { STEVE_LOGO_ENSO, STEVE_LOGO_LAYOUT, STEVE_WORDMARK_GLYPHS } from "../src/components/steve-logo-paths.ts"

const ROOT = fileURLToPath(new URL("../..", import.meta.url))
const BRAND_DIR = join(ROOT, "docs/assets/brand")

const CORE_SVGS = [
  "steve-mark-light.svg",
  "steve-mark-dark.svg",
  "steve-mark-mono.svg",
  "steve-logo-horizontal-light.svg",
  "steve-logo-horizontal-dark.svg",
  "steve-logo-horizontal-mono.svg",
  "steve-logo-stacked-light.svg",
  "steve-logo-stacked-dark.svg",
  "steve-logo-stacked-mono.svg",
]

function parsePngDimensions(buf) {
  const PNG_SIG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  assert.ok(buf.length >= 24, "PNG buffer too small")
  assert.ok(buf.subarray(0, 8).equals(PNG_SIG), "Invalid PNG signature")
  assert.equal(buf.subarray(12, 16).toString("ascii"), "IHDR", "Missing IHDR chunk")
  return {
    width: buf.readUInt32BE(16),
    height: buf.readUInt32BE(20),
  }
}

test("brand SVG assets exist, use pure outlines without <text>, and honor theme tokens", () => {
  for (const name of CORE_SVGS) {
    const file = join(BRAND_DIR, name)
    assert.ok(existsSync(file), `Missing brand asset ${name}`)
    const svg = readFileSync(file, "utf8")
    assert.match(svg, /^<svg\s[^>]*xmlns="http:\/\/www\.w3\.org\/2000\/svg"/, `${name}: valid SVG root`)
    assert.match(svg, /viewBox="0 0 \d+ \d+"/, `${name}: valid viewBox`)
    assert.equal(svg.includes("<text"), false, `${name}: must outline wordmark as <path>, never <text>`)

    if (name.endsWith("-mono.svg")) {
      assert.ok(svg.includes("currentColor"), `${name}: monochrome variant uses currentColor`)
      assert.equal(svg.includes("<filter"), false, `${name}: monochrome variant omits displacement filters`)
      assert.equal(svg.includes("#e8a58c"), false, `${name}: monochrome variant omits light wash`)
      assert.equal(svg.includes("#c86d52"), false, `${name}: monochrome variant omits dark wash`)
    } else if (name.endsWith("-light.svg")) {
      assert.ok(svg.includes("#1c1d22") && svg.includes("#d46a49"), `${name}: uses Steve light charcoal palette`)
    } else if (name.endsWith("-dark.svg")) {
      assert.ok(svg.includes("#f2efe6") && svg.includes("#f08d6c"), `${name}: uses Steve dark charcoal palette`)
    }
  }
})

test("GitHub social preview SVGs and rasterized PNGs match exact 1280x640 dimensions", () => {
  for (const theme of ["light", "dark"]) {
    const svgFile = join(BRAND_DIR, `social-preview-${theme}.svg`)
    const pngFile = join(BRAND_DIR, `social-preview-${theme}.png`)
    assert.ok(existsSync(svgFile), `Missing social-preview-${theme}.svg`)
    assert.ok(existsSync(pngFile), `Missing social-preview-${theme}.png`)

    const svg = readFileSync(svgFile, "utf8")
    assert.match(svg, /viewBox="0 0 1280 640"/, `social-preview-${theme}.svg must be 1280x640`)
    assert.equal(svg.includes("ST-229"), false, `social-preview-${theme}.svg should use evergreen sample IDs`)

    const dims = parsePngDimensions(readFileSync(pngFile))
    assert.deepEqual(dims, { width: 1280, height: 640 }, `social-preview-${theme}.png must be 1280x640`)
  }
})

test("steve-logo-paths exports valid single-source layout and vector geometry", () => {
  assert.deepEqual(Object.keys(STEVE_LOGO_LAYOUT).sort(), ["horizontal", "mark", "stacked", "wordmark"])
  for (const [variant, cfg] of Object.entries(STEVE_LOGO_LAYOUT)) {
    assert.match(cfg.viewBox, /^0 0 \d+ \d+$/, `${variant} viewBox`)
  }
  assert.ok(STEVE_LOGO_ENSO.full.ensoPath.length > 40)
  assert.ok(STEVE_LOGO_ENSO.micro.ensoPath.length > 40)
  assert.equal(STEVE_WORDMARK_GLYPHS.items.length, 5, "Steve wordmark has 5 outlined glyphs")
})
