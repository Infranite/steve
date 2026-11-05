/* The chat composer's @ menu from the Hub (contacts.md §2.8): two groups, Bot (交给它) and 联系人和团队 (关联到这件事).
 * Picked entries are remembered and sent as `mentions` with the message, as long as their “@名字” is still in the text.
 * Without the contacts module the composer keeps its own bot-only menu (`opts === null`). */
import { useEffect, useRef, useState, type ReactNode } from "react"
import { AtSignIcon } from "lucide-react"
import { Robot } from "@/robot"
import { st } from "@/store"
import { available, isAvailable, people, type Mention, type MentionOptions } from "@/api/people"
import { ContactAvatar } from "@/people/avatar"
import { useT } from "@/i18n"

export type MentionOpt = { id: string; label: ReactNode; ins: string; mention: Mention }
const token = (name: string) => "@" + name.replace(/\s+/g, "")

/** `q` = the text after “@” while the @ menu is open, else null */
export function useMentions(q: string | null, initial: [string, Mention][] = []) {
  const t = useT()
  const [res, setRes] = useState<(MentionOptions & { q: string }) | null>(null)
  const picked = useRef(new Map<string, Mention>(initial))
  useEffect(() => {
    if (q === null) return
    let live = true
    const h = setTimeout(() => {
      void available().then((on) => (on ? people.mentionOptions(q) : null)).then((r) => { if (live) setRes(r && { ...r, q }) }, () => { if (live) setRes(null) })
    }, 100)
    return () => { live = false; clearTimeout(h) }
  }, [q])
  /* until the answer for this q arrives, narrow the last one, so Enter never picks an entry the text no longer matches */
  const fits = (n: string) => !res || res.q === q || n.toLowerCase().includes((q ?? "").toLowerCase())
  const opts: MentionOpt[] | null = q === null || !isAvailable() || !res ? null : [
    ...res.bots.filter((b) => !st.bots[b.id]?.main && fits(b.name)).map((b) => ({ id: "bot:" + b.id, ins: token(b.name), mention: { kind: "bot" as const, id: b.id },
      label: <><Robot k={b.id} size={18} /><span className="type-callout">{t("people:mention.bot", { name: b.name })}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{st.bots[b.id]?.role ? t("people:mention.handToRole", { role: st.bots[b.id].role }) : t("people:mention.handTo")}</span></> })),
    ...res.parties.filter((p) => fits(p.name)).map((p) => ({ id: "party:" + p.id, ins: token(p.name), mention: { kind: "party" as const, id: p.id },
      label: <><ContactAvatar name={p.name} type={p.type} size={18} /><span className="type-callout">{p.name}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{p.sub ? t("people:mention.link", { sub: p.sub }) : t("people:mention.linkHere")}</span></> })),
  ]
  return {
    opts,
    /** the menu's two groups (contacts.md §2.8) */
    groups: <T extends { id: string }>(xs: T[]) => [
      { head: <><AtSignIcon className="size-3.5" />{t("people:mention.botsHead")}</>, xs: xs.filter((o) => o.id.startsWith("bot:")) },
      { head: <><AtSignIcon className="size-3.5" />{t("people:mention.partiesHead")}</>, xs: xs.filter((o) => o.id.startsWith("party:")) },
    ],
    /** remember a picked option (no-op for the composer's own options) */
    pick: (o: { ins: string; mention?: Mention }) => { if (o.mention) picked.current.set(o.ins, o.mention) },
    /** the mention picked for this “@名字”, if any (the composer marks it) */
    known: (ins: string): Mention | undefined => picked.current.get(ins),
    snapshot: () => [...picked.current],
    clear: () => picked.current.clear(),
    /** the mentions still present in `text`; retained until sending succeeds */
    take: (text: string): Mention[] => {
      const out = [...picked.current].filter(([ins]) => text.includes(ins)).map(([, m]) => m)
      return out.filter((m, n) => out.findIndex((x) => x.kind === m.kind && x.id === m.id) === n).slice(0, 20)
    },
  }
}
