/* <OwnerChip objectType objectId/> — the 负责人 of a bot, device, persona, connection, account or bot party
 * (ownership.md §4): shows who it is and lets you pick someone else or clear it. Every change is a change
 * record, so the toast offers 撤销. */
import { useEffect, useState, type MouseEvent } from "react"
import { cn } from "cn"
import { CheckIcon, UserRoundXIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { CommandMenu, type CommandGroup } from "@/components/affect/command-menu-clean"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { toast, st } from "@/store"
import { hubBase } from "@/hub"
import { people, type ContactBrief } from "@/api/people"
import { ownership, type Changed, type ObjectType, type Steward } from "@/api/ownership"
import { useHubQuery } from "@/data/use-hub-query"
import { act, joinList, orgLine } from "@/people/common"
import { disambiguateActors, formatActorName, partyActor } from "@/actor"
import { ActorAvatar } from "@/components/actor-avatar"
import { t as tr, useT } from "@/i18n"

/** the toast after a change, with 撤销 when the Hub recorded one */
export function changedToast(r: Changed | undefined, text: string) {
  if (!r) return
  if (!r.undo) return void toast(text)
  const id = r.undo
  toast(text, { action: { label: tr("people:owner.undo"), onClick: () => void act(ownership.undo(id), tr("people:owner.undone")) } })
}

const ownersOf = (xs: Steward[]) => xs.filter((s) => s.role === "owner")

export function OwnerChip({ objectType, objectId, className, bare = false }: { objectType: ObjectType; objectId: string; className?: string; bare?: boolean }) {
  return <Chip key={JSON.stringify([hubBase(), st.workspace?.id, objectType, objectId])} objectType={objectType} objectId={objectId} className={className} bare={bare} />
}

function Chip({ objectType, objectId, className, bare }: { objectType: ObjectType; objectId: string; className?: string; bare: boolean }) {
  const t = useT()
  const L = useHubQuery(["ownership", "of", objectType, objectId], () => ownership.of(objectType, objectId), { events: ["party", "patch"] })
  const [open, setOpen] = useState(false), [q, setQ] = useState(""), [busy, setBusy] = useState(false)
  const [found, setFound] = useState<ContactBrief[] | null>(null)
  useEffect(() => {
    if (!open) return
    let live = true
    const h = setTimeout(() => { people.contacts({ q: q.trim() || undefined, limit: 30 }).then((xs) => { if (live) setFound(xs) }, () => { if (live) setFound([]) }) }, q ? 150 : 0)
    return () => { live = false; clearTimeout(h) }
  }, [open, q])
  if (L.error) return <span className={cn("flex flex-wrap items-center justify-end gap-2 type-callout", className)} role="alert" data-owner-error><span className="text-destructive">{t("people:owner.loadFailed")}</span><Button size="xs" variant="outline" onClick={() => void L.reload()}>{t("common:action.retry")}</Button></span>
  if (!L.data) return <Skeleton className={cn("inline-flex h-6 w-20 rounded-full", className)} aria-hidden />
  const { object, stewards } = L.data
  const owners = ownersOf(stewards), confirmed = owners.filter((s) => s.status === "confirmed"), proposed = owners.filter((s) => s.status !== "confirmed")
  const label = confirmed.length ? joinList(confirmed.map((s) => (s.is_me ? t("people:rel.me") : s.person_name))) : proposed.length ? t("people:owner.proposed", { name: proposed[0].person_name }) : t("people:owner.none")
  const pick = async (c: { id: string; name: string; is_me?: boolean }) => {
    setOpen(false); setQ(""); setBusy(true)
    const r = await act(ownership.set(objectType, objectId, c.id))
    setBusy(false); L.reload()
    changedToast(r, c.is_me ? t("people:owner.changedMe", { name: object.name }) : t("people:owner.changed", { name: object.name, who: c.name }))
  }
  const clear = async () => {
    setOpen(false); setBusy(true)
    const r = await act(ownership.remove(objectType, objectId))
    setBusy(false); L.reload()
    changedToast(r, t("people:owner.cleared", { name: object.name }))
  }
  const stop = (e: MouseEvent) => e.stopPropagation()
  const xs = (found ?? []).filter((c) => c.kind !== "bot" && !(objectType === "party" && c.id === objectId))
  const optDisambig = disambiguateActors(xs.map((c) => ({ key: c.id, name: c.name, subtitle: orgLine(c), fallbackId: c.id })))
  const leadOwner = confirmed[0]
  return (
    <span className={cn("inline-flex", className)} onClick={stop} onKeyDown={(e) => e.stopPropagation()} data-owner-chip={`${objectType}:${objectId}`}>
      <Popover open={open} onOpenChange={(o) => { setOpen(o); if (!o) setQ("") }}>
        <PopoverTrigger render={<Button variant="outline" size="xs" disabled={busy} aria-label={t("people:owner.aria", { name: object.name, owner: label })}
          className={cn("h-6 max-w-[220px] gap-1.5 rounded-full px-2 font-normal", !confirmed.length && "border-dashed text-muted-foreground", proposed.length > 0 && !confirmed.length && "text-attention")} />}>
          {leadOwner ? <ActorAvatar actor={partyActor({ id: leadOwner.person_id, name: leadOwner.person_name, avatar: leadOwner.person_avatar, isMe: leadOwner.is_me })} size={16} tooltip={false} /> : <UserRoundXIcon className="size-3.5 shrink-0" />}
          {!bare && <span className="shrink-0 text-muted-foreground">{t("people:shared.owner")}</span>}
          <span className="truncate" data-owner-name>{label}</span>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-[min(300px,calc(100vw-24px))] gap-0 rounded-2xl p-0" data-owner-menu>
          <div className="border-b px-3 py-2"><div className="type-footnote text-muted-foreground">{t("people:owner.of", { type: object.type_label, name: object.name })}</div>
            {proposed.length > 0 && <div className="mt-0.5 type-footnote text-attention">{t("people:owner.proposedNote", { names: joinList(proposed.map((s) => s.person_name)) })}</div>}</div>
          <CommandMenu
            bare
            autoFocus
            showFooter={false}
            shouldFilter={false}
            onQueryChange={setQ}
            onClose={() => setOpen(false)}
            inputData={{ "data-owner-search": "" }}
            labels={{ placeholder: t("people:owner.search"), empty: found ? t("people:shared.noMatch") : t("people:shared.loading") }}
            groups={[
              ...(xs.length > 0 ? [{
                id: "people",
                label: "",
                commands: xs.map((c) => {
                  const cur = confirmed.some((s) => s.person_id === c.id)
                  const extra = optDisambig.get(c.id)
                  const shown = formatActorName(c.name, extra, (n, ex) => t("common:actor.disambiguated", { name: n, extra: ex }))
                  return {
                    id: c.id,
                    label: c.is_me ? t("people:shared.withMe", { name: shown }) : shown,
                    icon: <ActorAvatar actor={partyActor({ id: c.id, name: c.name, contactKind: c.kind, avatar: c.avatar, subtitle: orgLine(c), isMe: c.is_me })} disambiguator={extra} size={18} tooltip={false} />,
                    trailing: cur ? <CheckIcon className="size-4" /> : undefined,
                    data: { "data-owner-opt": c.name },
                  }
                }),
              }] : []),
              ...(owners.length > 0 ? [{
                id: "clear",
                label: "",
                commands: [{ id: "__clear", label: t("people:owner.clear"), icon: <UserRoundXIcon className="size-4" />, data: { "data-owner-clear": "" } }],
              }] : []),
            ] satisfies CommandGroup[]}
            onRun={(id) => {
              if (id === "__clear") { void clear(); return }
              const c = xs.find((x) => x.id === id)
              if (c && !confirmed.some((s) => s.person_id === c.id)) void pick(c)
            }}
          />
        </PopoverContent>
      </Popover>
    </span>
  )
}
