/* 事项详情属性栏的“联系人” (prototype people.tsx ItemPeople): people, organizations and teams linked to the item */
import { useState } from "react"
import { XIcon } from "lucide-react"
import { Input } from "@/components/ui/input"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { people } from "@/api/people"
import { act, label, useLoad } from "@/people/common"
import { disambiguateActors, formatActorName, partyActor } from "@/actor"
import { ActorAvatar } from "@/components/actor-avatar"
import { useT } from "@/i18n"

const href = (type: string, id: string) => (type === "team" ? "#/teams/" : type === "org" ? "#/orgs/" : "#/contacts/") + id

/** the 联系人 row of the item page */
export function ItemPeopleProp({ id, Prop }: { id: string; Prop: (p: { label: string; children: React.ReactNode }) => React.ReactNode }) {
  const t = useT()
  return <>{Prop({ label: t("people:shared.contacts"), children: <ItemPeople id={id} /> })}</>
}

export function ItemPeople({ id }: { id: string }) {
  const t = useT()
  const { data } = useLoad(() => people.itemPeople(id), [id], ["party", "item"])
  const [open, setOpen] = useState(false), [q, setQ] = useState("")
  const opts = useLoad(() => (open ? people.mentionOptions(q.trim()) : Promise.resolve(null)), [open, q], [])
  const xs = data ?? []
  const cands = (opts.data?.parties ?? []).filter((p) => !xs.some((x) => x.party_id === p.id))
  const disambig = disambiguateActors(xs.map((x) => ({ key: x.party_id, name: x.name, subtitle: label("itemRole", x.role), fallbackId: x.party_id })))
  return (
    <div className="flex flex-col gap-1 pt-1" data-item-people>
      {xs.map((x) => { const extra = disambig.get(x.party_id); return (
        <div key={x.party_id + x.role} className="group/p flex items-center gap-2" data-item-person={x.name}><ActorAvatar actor={partyActor({ id: x.party_id, name: x.name, type: x.type, avatar: x.avatar })} disambiguator={extra} size={18} />
          <a href={href(x.type, x.party_id)} className="min-w-0 flex-1 truncate hover:underline">{formatActorName(x.name, extra, (n, ex) => t("common:actor.disambiguated", { name: n, extra: ex }))}<span className="ml-1 type-footnote text-muted-foreground">{x.confidence < 1 ? t("people:shared.roleInferred", { role: label("itemRole", x.role) }) : label("itemRole", x.role)}</span></a>
          <button type="button" aria-label={t("people:itemPeople.remove", { name: x.name })} onClick={() => void act(people.removeItemPerson(id, x.party_id, x.role))} data-rm-person={x.name}
            className="text-tertiary opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/p:opacity-100 [@media(hover:none)]:opacity-100"><XIcon className="size-3.5" /></button></div>) })}
      <DropdownMenu open={open} onOpenChange={(o) => { setOpen(o); if (!o) setQ("") }}>
        <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-person />}>{xs.length ? t("people:itemPeople.add") : t("people:itemPeople.noneAdd")}</DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="max-h-80 min-w-60">
          <div className="p-1" onKeyDown={(e) => e.stopPropagation()}><Input autoFocus className="h-8 rounded-lg md:type-callout" aria-label={t("people:itemPeople.searchAria")} placeholder={t("people:itemPeople.searchPh")} value={q} onChange={(e) => setQ(e.target.value)} data-person-search /></div>
          {!opts.data ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">{t("people:shared.loading")}</div> : !cands.length ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">{t("people:shared.noMatch")}</div>
            : cands.map((p) => <DropdownMenuItem key={p.id} onClick={() => void act(people.addItemPerson(id, p.id), t("people:itemPeople.linked", { name: p.name }))} data-person-opt={p.name}><ActorAvatar actor={partyActor({ id: p.id, name: p.name, type: p.type, avatar: p.avatar })} size={18} tooltip={false} />{p.name}<span className="ml-auto max-w-32 truncate type-footnote text-muted-foreground">{p.sub}</span></DropdownMenuItem>)}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}
