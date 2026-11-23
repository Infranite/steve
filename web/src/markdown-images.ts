export type MarkdownImageSource = { src: string; authenticated: boolean }

/** Called after Markdown's sanitize/harden steps and any delivery-package rebasing.
 * Only Hub API paths are relative to the selected Hub. Never treat a Node's path as a browser resource. */
export function markdownImageSource(ref: string, hub: string): MarkdownImageSource | null {
  // Streamdown owns the placeholder for an image still being written.
  if (ref === "streamdown:incomplete-image") return { src: ref, authenticated: false }
  const api = ref.startsWith("/api/")
  if (!api && !/^https?:\/\//i.test(ref) && !ref.startsWith("//")) return null
  try {
    const base = new URL(hub), url = new URL(ref, base)
    if (!/^https?:$/.test(url.protocol)) return null
    // Do not normalize a traversal out of /api into a new readable resource.
    if (api && (!url.pathname.startsWith("/api/") || /[\\\u0000-\u0020\u007f]/.test(ref))) return null
    return {
      // The source workspace is authoritative: never append the active tab's ?ws= here.
      src: api ? url.href : ref,
      authenticated: url.origin === base.origin && url.pathname.startsWith("/api/"),
    }
  } catch { return null }
}

const UNAVAILABLE = "#steve-file-unavailable-"
// rehype-harden refuses fragments in image src. A private render-only placeholder
// survives that check; Image recognizes it before creating an <img> (no request).
const IMAGE_UNAVAILABLE = "/api/steve-file-unavailable-"
const REASONS = ["unresolved", "scope", "missing", "offline", "limit", "type"] as const
export function replyFileReason(ref?: string) {
  return REASONS.find(reason => ref === UNAVAILABLE + reason || ref === IMAGE_UNAVAILABLE + reason)
}

/** A file path the Hub did not replace with a stable delivery (text written before it was captured, or never
 *  captured) only gets a disabled reference; never request a Node path from the browser. */
export function unresolvedReplyPath(ref: string, image: boolean): boolean {
  if (/^file:/i.test(ref)) return true
  if (!ref || ref.startsWith("#") || ref.startsWith("//") || ref.includes(":")
    || /^\/(?:api|deliveries|shares|auth|assets)\//.test(ref)) return false
  return image || (ref.startsWith("/") && ref !== "/") || /^\.\.?\//.test(ref)
    || /\.(?:md|markdown|pdf|png|jpe?g|gif|webp|txt|log|csv|tsv|jsonl?|ya?ml|xml|toml|docx|xlsx|pptx|mp3|mp4|webm|wav|zip|html?)(?:[?#]|$)/i.test(ref)
}

export function replyImageHint(ref: string): string {
  const reason = ref.startsWith(UNAVAILABLE) && replyFileReason(ref)
  return reason ? IMAGE_UNAVAILABLE + reason : ref
}

export const unresolvedReplyMarker = UNAVAILABLE + "unresolved"

/** Use the Hub in this tab, not the web origin or another saved Hub. Keep the captured workspace. */
export function replyDeliveryUrl(ref: string | undefined, hub: string): string | null {
  if (!ref || !/^\/deliveries\/dlv_[0-9a-f]{32}\?ws=[^&#]+$/.test(ref)) return null
  try { return new URL(ref, hub).href } catch { return null }
}
