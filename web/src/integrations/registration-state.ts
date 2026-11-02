/** Public registration view only: never store application secrets or device codes in the browser. */
export type RegistrationView = { state: string; error?: string; expires_at: number }

export function registrationPending(r: RegistrationView): boolean {
  return r.state === "pending" || r.state === "verifying" || (r.state === "credentials_saved" && !r.error)
}

export function registrationLabel(r: RegistrationView): string {
  switch (r.state) {
    case "pending": return "等待你在官方页面确认"
    case "credentials_saved": return r.error ? "凭据已保存，连接检查未完成" : "凭据已安全保存，正在检查连接"
    case "verifying": return "正在检查连接"
    case "succeeded": return "应用已接入"
    case "denied": return "你没有同意接入"
    case "expired": return "确认链接已过期"
    case "cancelled": return "已取消本次接入"
    default: return "接入未完成"
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
