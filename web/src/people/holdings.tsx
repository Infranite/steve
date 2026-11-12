/* 名下 on a person's page (ownership.md §4): their own accounts by provider, and what they are the 负责人 of —
 * bots, mailboxes, devices, connections, personas — with add / remove; plus 没有负责人 for the contacts
 * overview. Hidden on a Hub without these routes. */
import { useState } from "react"
import { AtSignIcon, BotIcon, ChevronRightIcon, PlugIcon, PlusIcon, ServerIcon, UsersIcon, VenetianMaskIcon, XIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Command, CommandEmpty, CommandGroup, CommandItem, CommandList } from "@/components/ui/command"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { api } from "@/api"
import { go, Group, Row } from "@/ui"
import { ownership, objectHref, type Holding, type ObjectInfo, type ObjectType } from "@/api/ownership"
import { Loading, Section, SubMeta, act, joinList, label, useLoad } from "@/people/common"
import { useT } from "@/i18n"
import { OwnerChip, changedToast } from "@/people/owner-chip"

const ICON: Record<ObjectType, typeof BotIcon> = { bot: BotIcon, party: BotIcon, device: ServerIcon, persona: VenetianMaskIcon, connection: PlugIcon, account: AtSignIcon, space: UsersIcon }
const clip = (s: string, n = 24) => (s.length > n ? s.slice(0, n) + "…" : s)
export const ObjIcon = ({ t }: { t: ObjectType }) => { const I = ICON[t] ?? BotIcon; return <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><I className="size-4" strokeWidth={1.5} aria-hidden /></span> }

export function HoldingsSection({ person, name, isMe, isBot }: { person: string; name: string; isMe: boolean; isBot: boolean }) {
  const L = useLoad(() => ownership.holdings(person), [person], ["party", "decision"])
  if (L.err) return null
  const t = useT()
  const h = L.data
  const nAcc = h?.accounts.reduce((n, g) => n + g.accounts.length, 0) ?? 0
  return (
    <Section label={t("people:holdings.title")} right={!isBot ? <AddHolding person={person} name={name} isMe={isMe} /> : undefined}>
      {!h ? <Loading /> : <div className="flex flex-col gap-4" data-holdings>
        {isBot && <Group><Row title={t("people:shared.owner")} sub={t("people:holdings.botOwnerSub")}><OwnerChip objectType="party" objectId={person} bare /></Row></Group>}
        <div>
          <div className="mb-1.5 px-1 type-footnote text-muted-foreground">{t("people:holdings.accounts", { n: nAcc })}</div>
          {nAcc ? <Group>{h.accounts.flatMap((g) => g.accounts.map((a) => (
            <Row key={a.id} title={<span className="font-mono text-[13px]">{a.handle}</span>}
              sub={<SubMeta><span>{g.system}</span><span>{a.kind_label}</span>{a.name && a.name !== a.handle && <span>{a.name}</span>}</SubMeta>}
              onClick={() => go("#/accounts/" + a.id)} data-holding-acct={a.handle}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)))}</Group>
            : <div className="px-1 py-1 type-callout text-muted-foreground">{t("people:holdings.noAccounts")}</div>}
        </div>
        {!isBot && <div>
          <div className="mb-1.5 px-1 type-footnote text-muted-foreground">{t("people:holdings.responsible", { n: h.responsible.length })}</div>
          {h.responsible.length ? <Group>{h.responsible.map((x) => <HoldingRow key={x.object.object_type + x.object.object_id + x.role} x={x} person={person} />)}</Group>
            : <div className="px-1 py-1 type-callout text-muted-foreground">{t("people:holdings.noResponsible")}</div>}
        </div>}
        <div className="px-1 type-footnote text-muted-foreground">{isBot ? t("people:holdings.botNote") : isMe ? t("people:holdings.noteMe") : t("people:holdings.noteOther", { name })}</div>
      </div>}
    </Section>
  )
}

function HoldingRow({ x, person }: { x: Holding; person: string }) {
  const t = useT()
  const href = objectHref(x.object), o = x.object
  const remove = async () => changedToast(await act(ownership.remove(o.object_type, o.object_id, { person_id: person, role: x.role })), t("people:holdings.released", { name: o.name }))
  const others = x.others.filter((s) => s.role === "owner" && s.status === "confirmed").map((s) => s.person_name)
  return (
    <Row icon={<ObjIcon t={o.object_type} />} onClick={href ? () => go(href) : undefined} controls data-holding={o.name}
      title={<span className="inline-flex items-center gap-1.5">{o.name}{x.status !== "confirmed" && <Badge variant="outline" className="font-normal text-attention">{t("people:shared.pending")}</Badge>}</span>}
      sub={<SubMeta><span>{o.type_label}</span><span>{x.role_label}</span>{o.detail && <span>{clip(o.detail)}</span>}<span>{label("source", x.source)}</span>{others.length > 0 && <span>{t("people:holdings.alsoOwners", { names: joinList(others) })}</span>}</SubMeta>}>
      {x.status !== "confirmed" && x.decision_id && <span className="flex gap-1" onClick={(e) => e.stopPropagation()}>
        <Button size="xs" variant="outline" onClick={() => void act(api.decide(x.decision_id!, "yes"), t("people:holdings.confirmed"))} data-holding-yes>{t("common:action.confirm")}</Button>
        <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void act(api.decide(x.decision_id!, "no"), t("people:holdings.ignored"))}>{t("people:shared.wrong")}</Button></span>}
      {x.status === "confirmed" && <Button variant="ghost" size="icon-xs" aria-label={t("people:holdings.releaseAria", { name: o.name })} className="text-muted-foreground" onClick={(e) => { e.stopPropagation(); void remove() }} data-holding-remove={o.name}><XIcon /></Button>}
    </Row>
  )
}

/** 添加: pick one of the objects nobody is responsible for */
function AddHolding({ person, name, isMe }: { person: string; name: string; isMe: boolean }) {
  const t = useT()
  const [open, setOpen] = useState(false), [xs, setXs] = useState<ObjectInfo[] | null>(null)
  const load = () => { setXs(null); ownership.unowned().then(setXs, () => setXs([])) }
  const pick = async (o: ObjectInfo) => { setOpen(false); changedToast(await act(ownership.set(o.object_type, o.object_id, person)), isMe ? t("people:holdings.takenMe", { name: o.name }) : t("people:holdings.takenOther", { who: name, name: o.name })) }
  return (
    <Popover open={open} onOpenChange={(v) => { setOpen(v); if (v) load() }}>
      <PopoverTrigger render={<Button size="xs" variant="ghost" className="text-muted-foreground" data-add-holding />}><PlusIcon />{t("common:action.add")}</PopoverTrigger>
      <PopoverContent align="end" className="w-[min(320px,calc(100vw-24px))] gap-0 rounded-2xl p-0" data-add-holding-menu>
        <div className="border-b px-3 py-2 type-footnote text-muted-foreground">{t("people:holdings.unownedHead")}</div>
        <Command className="rounded-b-2xl"><CommandList className="max-h-[min(320px,50vh)]">
          <CommandEmpty>{xs ? t("people:holdings.allOwned") : t("people:shared.loading")}</CommandEmpty>
          {xs && xs.length > 0 && <CommandGroup>{xs.map((o) => <CommandItem key={o.object_type + o.object_id} value={o.object_type + ":" + o.object_id} onSelect={() => void pick(o)} data-add-holding-opt={o.name}>
            <span className="min-w-0 flex-1 truncate">{o.name}</span><span className="shrink-0 type-footnote text-muted-foreground">{o.type_label}</span></CommandItem>)}</CommandGroup>}
        </CommandList></Command>
      </PopoverContent>
    </Popover>
  )
}

/** 没有负责人 (the contacts overview): what nobody is responsible for yet, each with its chip */
export function UnownedSection() {
  const t = useT()
  const L = useLoad(() => ownership.unowned(), [], ["party", "decision", "bot", "persona", "connection"])
  if (L.err || !L.data || !L.data.length) return null
  return (
    <section className="mt-8" data-unowned><Section label={t("people:holdings.unowned", { n: L.data.length })}>
      <Group>{L.data.map((o) => { const href = objectHref(o); return (
        <Row key={o.object_type + o.object_id} icon={<ObjIcon t={o.object_type} />} title={o.name} sub={<SubMeta><span>{o.type_label}</span>{o.detail && <span>{clip(o.detail)}</span>}</SubMeta>} onClick={href ? () => go(href) : undefined} controls data-unowned-row={o.name}>
          <OwnerChip objectType={o.object_type} objectId={o.object_id} bare /></Row>) })}</Group>
      <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("people:holdings.unownedNote")}</div>
    </Section></section>
  )
}
