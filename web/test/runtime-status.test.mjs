import assert from "node:assert/strict"
import test from "node:test"
import { runtimeState, runtimeReason, supportsPermission, canConfigurePermission, canInstallRuntime } from "../src/runtime-status.ts"

test("setup distinguishes unknown, partial installation and probe failure from usability", () => {
  const runtime = (state, ok = false) => ({ id: "sample", ok, setup: { state } })
  for (const state of ["unknown", "not_installed", "installing", "ready", "unavailable"]) assert.equal(runtimeState(runtime(state)), state)
  assert.equal(runtimeState(), "unknown")
  assert.equal(runtimeState({ ok: true }), "ready", "a harness without managed components is usable without setup")
  assert.equal(runtimeState({ ok: false }), "unavailable")
  assert.equal(runtimeReason({ error: "probe failed", setup: { reason: "provider missing", error: "install failed" } }), "provider missing")
  assert.equal(runtimeReason({ error: "probe failed" }), "probe failed")
})

test("installation actions follow component gaps and installer failures, not provider failures", () => {
  const r = (state, components, extra = {}) => ({ ok: state === "ready", setup: { state, components, can_install: true, ...extra } })
  const complete = [{ status: "installed" }, { status: "installed" }]
  assert.equal(canInstallRuntime(r("ready", complete)), false)
  assert.equal(canInstallRuntime(r("ready", complete, { error: "old installer error" }), "old request error"), false)
  assert.equal(canInstallRuntime({ ...r("unavailable", complete), error: "Provider credentials missing" }), false)
  assert.equal(canInstallRuntime({ ...r("ready", complete), ok: false, error: "Session failed" }), false)
  assert.equal(canInstallRuntime(r("unavailable", complete, { error: "npm offline" })), true)
  assert.equal(canInstallRuntime(r("unavailable", complete), "HTTP request failed"), true)
  assert.equal(canInstallRuntime(r("unknown", complete)), true, "interrupted install may need a retry")
  assert.equal(canInstallRuntime(r("unavailable", [{ status: "missing" }])), true)
  assert.equal(canInstallRuntime(r("unavailable", [{ status: "unknown" }])), true)
  assert.equal(canInstallRuntime(r("unavailable", [{ status: "installed" }])), false)
  assert.equal(canInstallRuntime(r("not_installed", [], { can_install: false })), false)
  assert.equal(canInstallRuntime({ ok: true }), false, "a runtime without setup has nothing to install")
})

test("permission support is registry driven, with explicit empty modes and an absent offer kept distinct", () => {
  const limited = { id: "arbitrary-adapter", modes: ["full_access"] }
  assert.equal(supportsPermission(limited, "default"), false)
  assert.equal(supportsPermission(limited, "plan"), false)
  assert.equal(supportsPermission(limited, "bypassPermissions"), true)
  assert.equal(supportsPermission({ modes: [] }, "default"), false)
  assert.equal(supportsPermission({ modes: null }, "default"), true)
  assert.equal(supportsPermission({}, "default"), true)
  assert.equal(supportsPermission(undefined, "default"), false)
  assert.equal(supportsPermission({ modes: ["accept_edits"] }, "acceptEdits"), true)
})


test("global permission configuration keeps declared restrictions but unknown targets editable", () => {
  for (const ok of [false, true]) {
    assert.equal(canConfigurePermission({ ok, modes: ["full_access"] }, "default"), false)
    assert.equal(canConfigurePermission({ ok, modes: ["full_access"] }, "bypassPermissions"), true)
  }
  for (const mode of ["default", "plan", "bypassPermissions"]) {
    assert.equal(canConfigurePermission({ ok: false, modes: [] }, mode), true)
    assert.equal(canConfigurePermission(undefined, mode), true)
    assert.equal(canConfigurePermission({ ok: true, modes: [] }, mode), false, "verified empty contract remains distinct")
  }
})
