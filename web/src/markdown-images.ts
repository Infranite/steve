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
