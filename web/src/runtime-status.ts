import type { RRuntime } from "@/api"

/** Setup describes installation; a successful local Node probe (`ok`) allows local use, not target-wide admission. */
export const runtimeState = (r?: RRuntime): NonNullable<RRuntime["setup"]>["state"] => r?.setup?.state ?? (r?.ok ? "ready" : r ? "unavailable" : "unknown")
export const runtimeReason = (r?: RRuntime): string | undefined => r?.setup?.reason || r?.setup?.error || r?.error || r?.install_hint || undefined


/** Installation cannot fix a provider/session failure when all component targets are already complete. */
export const canInstallRuntime = (r: RRuntime, requestError?: string): boolean => {
  const s = r.setup
  if (!s?.can_install || (r.ok && s.state === "ready")) return false
  return s.state === "installing" || s.state === "unknown" || !!s.error || !!requestError || s.state === "not_installed"
    || s.components.some(c => c.status !== "installed")
}

/** Steve's canonical permission modes (matching steve_node_api::PermissionMode). */
export const PERMISSION_MODES = ["plan", "ask", "accept_edits", "auto", "full_access"] as const
export type PermissionMode = (typeof PERMISSION_MODES)[number]

export const supportsPermission = (r: RRuntime | undefined, mode: string): boolean => {
  if (!r) return false
  // No session offer yet (`modes` null) declares no restriction; an explicit [] supports none.
  return r.modes == null || r.modes.includes(mode)
}

/** Global configuration has no target probe yet. Keep known restrictions; absent modes are not an admission verdict. */
export const canConfigurePermission = (r: RRuntime | undefined, mode: string): boolean =>
  !r || (!r.ok && !r.modes?.length) || supportsPermission(r, mode)
