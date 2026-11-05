import { t } from "../i18n/index.ts"

/** Public registration view only: never store application secrets or device codes in the browser. */
export type RegistrationView = { state: string; error?: string; expires_at: number }

export function registrationPending(r: RegistrationView): boolean {
  return r.state === "pending" || r.state === "verifying" || (r.state === "credentials_saved" && !r.error)
}

export function registrationLabel(r: RegistrationView): string {
  switch (r.state) {
    case "pending": return t("integrations:reg.state.pending")
    case "credentials_saved": return r.error ? t("integrations:reg.state.savedError") : t("integrations:reg.state.saved")
    case "verifying": return t("integrations:reg.state.verifying")
    case "succeeded": return t("integrations:reg.state.succeeded")
    case "denied": return t("integrations:reg.state.denied")
    case "expired": return t("integrations:reg.state.expired")
    case "cancelled": return t("integrations:reg.state.cancelled")
    default: return t("integrations:reg.state.other")
  }
}

/** Do not guess a deployment URL or accept javascript:/third-party confirmation pages. */
export function officialConfirmationUrl(value?: string): string | undefined {
  if (!value) return undefined
  try {
    const u = new URL(value)
    const hosts = ["open.feishu.cn", "open.larksuite.com", "accounts.feishu.cn", "accounts.larksuite.com"]
    return u.protocol === "https:" && !u.username && !u.password && !u.port && hosts.includes(u.hostname) ? u.href : undefined
  } catch { return undefined }
}

export function remainingSeconds(expiresAt: number, now: number): number {
  return Math.max(0, Math.ceil((expiresAt - now) / 1000))
}
