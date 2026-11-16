/* The Hub's `open_url` of a chat, a chat message or a decision about one (“在飞书中打开”): an AppLink that opens it in
 * the platform's own client, the desktop client on a computer and the app on a phone. Only https links are followed;
 * a missing or other one shows nothing. Followed as a link, never window.open, which iOS does not follow. */

/** the link when it can be opened here: an https URL */
export function openable(url?: string | null): string | undefined {
  if (!url) return undefined
  try { return new URL(url).protocol === "https:" ? url : undefined } catch { return undefined }
}

/** follows `url` as a click on a link would, in a new tab (where a browser stays behind the app it opens) */
export function openInApp(url: string, doc: Pick<Document, "createElement"> = document) {
  const href = openable(url)
  if (!href) return
  const a = doc.createElement("a")
  a.href = href; a.target = "_blank"; a.rel = "noopener noreferrer"
  a.click()
}
