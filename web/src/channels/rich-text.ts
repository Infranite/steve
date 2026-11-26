/* Pure rich-text detection and safe Markdown normalization for channel messages (#213).
 * Converts provider plain-link pairs such as `Title (https://example.com)` into Markdown links
 * outside fenced code blocks so CardMarkdown renders rich text cleanly while keeping unsafe
 * schemes inert. */

const FENCE_RE = /^```/
const MD_LINK_RE = /\[[^\]\n]+\]\(https?:\/\/[^\s)]+\)/
const PAREN_URL_RE = /([^\s()[\]\n][^\n()[\]]{0,120}?)\s+\((https?:\/\/[^\s()<>]+)\)/g
const BARE_URL_RE = /\bhttps?:\/\/[^\s<>()]+/
const RICH_PATTERNS = [
  /```[\s\S]+?```/,
  MD_LINK_RE,
  /[^\s()[\]\n]\s+\(https?:\/\/[^\s()<>]+\)/,
  BARE_URL_RE,
  /(?:^|\n)#{1,6}\s+\S/,
  /\*\*[^*\n]+\*\*/,
  /(?:^|\n)(?:[-*+]\s+\S|\d+\.\s+\S|---\s*$)/,
]

/** Whether a channel message body contains rich-text structure worth rendering with CardMarkdown. */
export function isRichText(text: string): boolean {
  if (!text) return false
  return RICH_PATTERNS.some((re) => re.test(text))
}

/** Normalize safe `label (https://url)` pairs (such as Feishu post links) to Markdown links,
 *  leaving code fences and non-http(s) schemes untouched. */
export function formatRichText(text: string): string {
  const lines = text.split("\n")
  let inFence = false
  return lines
    .map((line) => {
      if (FENCE_RE.test(line.trimStart())) {
        inFence = !inFence
        return line
      }
      if (inFence) return line
      return line.replace(PAREN_URL_RE, (full, rawLabel: string, url: string) => {
        const label = rawLabel.trim()
        if (!label || label.endsWith("]")) return full
        const safeLabel = label.replace(/[[\]]/g, "\\$&")
        return `[${safeLabel}](${url})`
      })
    })
    .join("\n")
}
