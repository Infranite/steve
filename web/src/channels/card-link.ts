/** Card links are provider-controlled; never allow actions, credentials, or local app routes. */
export function cardLink(value: unknown): string | undefined {
  if (typeof value !== "string" || /[\u0000-\u0020\u007f]/.test(value)) return
  try {
    const u = new URL(value)
    if ((u.protocol === "https:" || u.protocol === "http:") && !u.username && !u.password) return u.href
  } catch { /* an absent or unsafe provider link is rendered as text */ }
}
