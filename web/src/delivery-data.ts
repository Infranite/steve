import type { Delivery } from "./delivery"

export class DeliveryHttpError extends Error {
  status: number
  constructor(status: number, message: string) { super(message); this.status = status }
}

/** A share is bound to the issuing URL, not the Hub saved in this browser. Never send workspace or login credentials. */
export async function sharedDelivery(token: string, signal: AbortSignal): Promise<Delivery> {
  const r = await fetch(`/api/delivery-shares/${encodeURIComponent(token)}`, { credentials: "omit", cache: "no-store", signal })
  const body = await r.json().catch(() => null)
  if (!r.ok) throw new DeliveryHttpError(r.status, body?.error ?? "")
  if (!body || typeof body.id !== "string" || typeof body.content_url !== "string") throw new DeliveryHttpError(502, "")
  return body as Delivery
}

/** Bound preview memory and work even when a text download has no Content-Length. */
export async function previewText(r: Response, limit = 256_000): Promise<{ text: string; truncated: boolean }> {
  if (!r.ok) throw new DeliveryHttpError(r.status, "")
  const reader = r.body?.getReader()
  if (!reader) return { text: "", truncated: false }
  const decoder = new TextDecoder(), chunks: string[] = []
  let read = 0, truncated = false
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      const take = value.subarray(0, Math.max(0, limit - read))
      chunks.push(decoder.decode(take, { stream: true })); read += take.length
      if (take.length !== value.length) { truncated = true; await reader.cancel(); break }
    }
    chunks.push(decoder.decode())
  } finally { reader.releaseLock() }
  return { text: chunks.join(""), truncated }
}
