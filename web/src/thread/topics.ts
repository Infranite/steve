/* ST-50: the conversation's history as topics, and where the reader was. Pure, so node tests can import it. */
import type { Msg } from "../store"

/** a pause this long starts a new topic (the same gap Cindy splits its work groups at) */
export const TOPIC_GAP = 30 * 60_000
export type Topic = { id: string; at: number; end: number; title: string; count: number; items: string[] }

const ITEM = /\b[A-Z]{1,6}-\d+\b/g
const plain = (t: string) => t.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|[\]]/g, "").replace(/\s+/g, " ").trim()
export const clip = (t: string, n: number) => { const x = plain(t); return x.length > n ? x.slice(0, n) + "…" : x }
const dayOf = (ts: number) => new Date(ts).toDateString()

/** the conversation cut at pauses and day changes, newest first; a topic is named by the first thing you said in it */
export function topicsOf(msgs: Msg[]): Topic[] {
  const out: (Topic & { named: boolean })[] = []
  let cur: (Topic & { named: boolean }) | null = null, last = 0
  for (const m of msgs) {
    if (!m.id || !m.created || (m.k !== "me" && m.k !== "steve") || m.notice) continue
    if (!cur || m.created - last > TOPIC_GAP || dayOf(m.created) !== dayOf(last)) {
      if (cur) out.push(cur)
      cur = { id: m.id, at: m.created, end: m.created, title: "", count: 0, items: [], named: false }
    }
    cur.count++; cur.end = m.created; last = m.created
    if (!cur.named && m.t && m.t !== "（附件）") { cur.title = clip(m.t, 40); cur.named = m.k === "me" }
    for (const id of [m.item, m.card?.item, ...(m.k === "me" ? m.t?.match(ITEM) ?? [] : [])]) if (id && !cur.items.includes(id)) cur.items.push(id)
  }
  if (cur) out.push(cur)
  return out.reverse().map(({ named: _, ...t }) => ({ ...t, title: t.title || "（附件）" }))
}

/** a few words of `text` around the first match of `q`, the match marked by its offsets */
export function snippet(text: string, q: string, n = 56): { text: string; at: number; len: number } {
  const t = plain(text), i = q ? t.toLowerCase().indexOf(q.toLowerCase()) : -1
  if (i < 0) return { text: t.length > n ? t.slice(0, n) + "…" : t, at: -1, len: 0 }
  const from = Math.max(0, i - Math.floor((n - q.length) / 3)), s = t.slice(from, from + n)
  const lead = from > 0 ? "…" : ""
  return { text: lead + s + (from + n < t.length ? "…" : ""), at: lead.length + i - from, len: q.length }
}

/** where the reader was: at the latest message, or a message at some offset from the top of the list */
export type ReadPos = { bottom: true } | { bottom: false; id: string; created: number; offset: number }
const POS = "steve:read:v1:"
export function readPos(key: string): ReadPos | null {
  try {
    const v = JSON.parse(localStorage.getItem(POS + key) ?? "null")
    if (v?.bottom === true) return { bottom: true }
    if (v && typeof v.id === "string" && typeof v.created === "number" && typeof v.offset === "number") return { bottom: false, id: v.id, created: v.created, offset: v.offset }
  } catch { /* unreadable: start at the latest */ }
  return null
}
export function savePos(key: string, p: ReadPos) { try { localStorage.setItem(POS + key, JSON.stringify(p)) } catch { /* storage full or off */ } }
