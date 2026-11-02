// The fixture's TS Hub build (ops/test-release.sh): dist/main.js loads Steve's real TS Hub from the checkout under test
// (TEST_ROOT) through tsx, so ops/run.sh can start it as `node dist/main.js` like a real release.
import { mkdirSync, writeFileSync } from "node:fs"
const root = process.env.TEST_ROOT
if (!root) { console.error("TEST_ROOT is not set"); process.exit(1) }
mkdirSync("dist", { recursive: true })
writeFileSync("dist/main.js", `const { register } = await import(${JSON.stringify(root + "/node_modules/tsx/dist/esm/api/index.mjs")})
register()
await import(${JSON.stringify(root + "/hub/src/main.ts")})
`)
console.log("fake hub build: dist/main.js -> " + root + "/hub/src/main.ts")
