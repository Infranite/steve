/* An account's spaces, grouped so a long list stays readable: the new ones first (with one action for all of
 * them), then those with messages in the last week, newest first; everything else and the blocked ones fold. A
 * search over the names once there are more than a handful. */
import { useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { go, Group, Row, SectionLabel, Pill, Blank, Disclosure } from "@/ui"
import { toast } from "@/store"
import { channelsApi, attentionOf, spaceTitle, type ChannelAccount, type Space, type SpacePatch } from "@/api/channels"
import { Name, SpaceAvatar, doingLine } from "@/channels/space-bits"
import { useT } from "@/i18n"

/** messages within this many days count as recent */
export const RECENT_DAYS = 7
/** new spaces shown before the rest of them fold */
const NEW_SHOWN = 5
/** a search box once there are more spaces than this */
const SEARCH_FROM = 8
/** spaces changed at once by a batch action */
const BATCH_PARALLEL = 6

/** the groups of `spaces` at `now` (ms): new (not set up yet), recent, the rest, blocked */
export function groupSpaces(spaces: Space[], now: number) {
  const since = now - RECENT_DAYS * 86_400_000
  const blocked = spaces.filter((s) => attentionOf(s) === "blocked")
  const open = spaces.filter((s) => attentionOf(s) !== "blocked")
  const fresh = open.filter((s) => !s.configured)
  const seen = open.filter((s) => s.configured)
  const byLast = (a: Space, b: Space) => (b.last_message ?? 0) - (a.last_message ?? 0)
  const recent = seen.filter((s) => (s.last_message ?? 0) >= since).sort(byLast)
  const rest = seen.filter((s) => (s.last_message ?? 0) < since).sort(byLast)
  return { fresh: fresh.sort(byLast), recent, rest, blocked: blocked.sort(byLast) }
}

export function GroupedSpaces({ a, empty, onChanged }: { a: ChannelAccount; empty: string; onChanged: () => void }) {
  const t = useT()
  const [q, setQ] = useState(""), [busy, setBusy] = useState(false)
  if (!a.spaces.length) return <Blank>{empty}</Blank>
  const query = q.trim().toLowerCase()
  const shown = query ? a.spaces.filter((s) => spaceTitle(s).toLowerCase().includes(query)) : a.spaces
  const g = groupSpaces(shown, Date.now())
  const row = (s: Space) => (
    <Row key={s.id} icon={<SpaceAvatar s={s} size={28} />} onClick={() => go("#/channels/space/" + s.id)} data-space-row={spaceTitle(s)}
      title={<span className="flex min-w-0 items-center gap-1.5"><Name className={cn(attentionOf(s) !== "realtime" && "text-muted-foreground")}>{spaceTitle(s)}</Name>{s.has_external && <Pill className="font-normal">{t("channels:word.external")}</Pill>}{!s.configured && <Pill tone="attention">{t("channels:word.new")}</Pill>}</span>}
      sub={<span className="block truncate">{[doingLine(s, a), s.duty].filter(Boolean).join(" · ")}</span>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)
  /* one action for every new space: each becomes set up (any change does that) */
  const batch = async (b: SpacePatch, done: string) => {
    setBusy(true)
    const todo = [...g.fresh]
    let failed = 0
    while (todo.length) {
      const part = todo.splice(0, BATCH_PARALLEL)
      const r = await Promise.allSettled(part.map((s) => channelsApi.patchSpace(s.id, b)))
      failed += r.filter((x) => x.status === "rejected").length
    }
    setBusy(false)
    toast(failed ? t("channels:groups.batchPartly", { count: failed }) : done)
    onChanged()
  }
  return (
    <div className="flex flex-col gap-5" data-grouped-spaces>
      {a.spaces.length > SEARCH_FROM && <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("channels:groups.search")} aria-label={t("channels:groups.search")} data-spaces-search />}
      {g.fresh.length > 0 && <div data-spaces-group="new">
        <SectionLabel>{t("channels:groups.new", { n: g.fresh.length })}</SectionLabel>
        <div className="mb-2 flex flex-wrap items-center gap-2" data-spaces-batch>
          <span className="type-footnote text-muted-foreground">{t("channels:groups.batchHint")}</span>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void batch({ muted: true }, t("channels:groups.batchRecordDone", { count: g.fresh.length }))} data-batch="record">{t("channels:groups.batchRecord")}</Button>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void batch({ attention: "quiet" }, t("channels:groups.batchQuietDone", { count: g.fresh.length }))} data-batch="quiet">{t("channels:groups.batchQuiet")}</Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => void batch({ muted: false }, t("channels:groups.batchKeepDone", { count: g.fresh.length }))} data-batch="keep">{t("channels:groups.batchKeep")}</Button>
        </div>
        <Group>{g.fresh.slice(0, NEW_SHOWN).map(row)}</Group>
        {g.fresh.length > NEW_SHOWN && <Disclosure label={t("channels:groups.moreNew", { count: g.fresh.length - NEW_SHOWN })} className="mt-2"><Group className="mt-2">{g.fresh.slice(NEW_SHOWN).map(row)}</Group></Disclosure>}
      </div>}
      {g.recent.length > 0 && <div data-spaces-group="recent"><SectionLabel>{t("channels:groups.recent", { n: g.recent.length, days: RECENT_DAYS })}</SectionLabel><Group>{g.recent.map(row)}</Group></div>}
      {g.rest.length > 0 && <Disclosure label={t("channels:groups.rest", { count: g.rest.length })} defaultOpen={!g.fresh.length && !g.recent.length} data-spaces-group="rest"><Group className="mt-2">{g.rest.map(row)}</Group></Disclosure>}
      {g.blocked.length > 0 && <Disclosure label={t("channels:groups.blocked", { count: g.blocked.length })} data-spaces-group="blocked"><Group className="mt-2">{g.blocked.map(row)}</Group></Disclosure>}
      {query && !shown.length && <Blank>{t("channels:groups.noMatch")}</Blank>}
    </div>
  )
}
