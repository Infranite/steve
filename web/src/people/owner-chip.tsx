/* <OwnerChip objectType objectId/> — the 负责人 of a bot, device, persona, connection, account or bot party
 * (ownership.md §4): shows who it is and lets you pick someone else or clear it. Every change is a change
 * record, so the toast offers 撤销. Renders nothing on a Hub without the contacts module. */
import { useEffect, useState, type MouseEvent } from "react"
import { cn } from "cn"
import { CheckIcon, UserRoundIcon, UserRoundXIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList, CommandSeparator } from "@/components/ui/command"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { toast } from "@/store"
import { people, type ContactBrief } from "@/api/people"
import { ownership, type Changed, type ObjectType, type Steward } from "@/api/ownership"
import { act, useLoad, usePeopleOn } from "@/people/common"

/** the toast after a change, with 撤销 when the Hub recorded one */
export function changedToast(r: Changed | undefined, text: string) {
  if (!r) return
  if (!r.undo) return void toast(text)
  const id = r.undo
  toast(text, { action: { label: "撤销", onClick: () => void act(ownership.undo(id), "已撤销") } })
}

const ownersOf = (xs: Steward[]) => xs.filter((s) => s.role === "owner")

export function OwnerChip({ objectType, objectId, className, bare = false }: { objectType: ObjectType; objectId: string; className?: string; bare?: boolean }) {
  const on = usePeopleOn()
  if (!on) return null
  return <Chip objectType={objectType} objectId={objectId} className={className} bare={bare} />
}

function Chip({ objectType, objectId, className, bare }: { objectType: ObjectType; objectId: string; className?: string; bare: boolean }) {
  const L = useLoad(() => ownership.of(objectType, objectId), [objectType, objectId], ["party", "decision"])
  const [open, setOpen] = useState(false), [q, setQ] = useState(""), [busy, setBusy] = useState(false)
  const [found, setFound] = useState<ContactBrief[] | null>(null)
  useEffect(() => {
    if (!open) return
    let live = true
    const t = setTimeout(() => { people.contacts({ q: q.trim() || undefined, limit: 30 }).then((xs) => { if (live) setFound(xs) }, () => { if (live) setFound([]) }) }, q ? 150 : 0)
    return () => { live = false; clearTimeout(t) }
  }, [open, q])
  if (L.err || !L.data) return L.err ? null : <span className={cn("inline-flex h-6 w-20 animate-pulse rounded-full bg-muted", className)} aria-hidden />
  const { object, stewards } = L.data
  const owners = ownersOf(stewards), confirmed = owners.filter((s) => s.status === "confirmed"), proposed = owners.filter((s) => s.status !== "confirmed")
  const label = confirmed.length ? confirmed.map((s) => (s.is_me ? "我" : s.person_name)).join("、") : proposed.length ? `${proposed[0].person_name}？` : "没有负责人"
  const pick = async (c: { id: string; name: string; is_me?: boolean }) => {
    setOpen(false); setQ(""); setBusy(true)
    const r = await act(ownership.set(objectType, objectId, c.id))
    setBusy(false); L.reload()
    changedToast(r, `「${object.name}」的负责人改成了 ${c.is_me ? "你" : c.name}`)
  }
  const clear = async () => {
    setOpen(false); setBusy(true)
    const r = await act(ownership.remove(objectType, objectId))
    setBusy(false); L.reload()
    changedToast(r, `「${object.name}」现在没有负责人`)
  }
  const stop = (e: MouseEvent) => e.stopPropagation()
  const xs = (found ?? []).filter((c) => c.kind !== "bot" && !(objectType === "party" && c.id === objectId))
  return (
    <span className={cn("inline-flex", className)} onClick={stop} onKeyDown={(e) => e.stopPropagation()} data-owner-chip={`${objectType}:${objectId}`}>
      <Popover open={open} onOpenChange={(o) => { setOpen(o); if (!o) setQ("") }}>
        <PopoverTrigger render={<Button variant="outline" size="xs" disabled={busy} aria-label={`${object.name} 的负责人：${label}，点按修改`}
          className={cn("h-6 max-w-[220px] gap-1 rounded-full px-2 font-normal", !confirmed.length && "border-dashed text-muted-foreground", proposed.length > 0 && !confirmed.length && "text-attention")} />}>
          {confirmed.length ? <UserRoundIcon className="size-3.5 shrink-0 text-muted-foreground" /> : <UserRoundXIcon className="size-3.5 shrink-0" />}
          {!bare && <span className="shrink-0 text-muted-foreground">负责人</span>}
          <span className="truncate" data-owner-name>{label}</span>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-[min(300px,calc(100vw-24px))] gap-0 rounded-2xl p-0" data-owner-menu>
          <div className="border-b px-3 py-2"><div className="type-footnote text-muted-foreground">{object.type_label}「{object.name}」的负责人</div>
            {proposed.length > 0 && <div className="mt-0.5 type-footnote text-attention">{proposed.map((s) => s.person_name).join("、")} 待确认（对话里提到的）</div>}</div>
          <Command className="rounded-b-2xl" shouldFilter={false}>
            <CommandInput placeholder="搜索联系人…" value={q} onValueChange={setQ} data-owner-search />
            <CommandList className="max-h-[min(320px,50vh)]">
              <CommandEmpty>{found ? "没有匹配的联系人" : "载入中…"}</CommandEmpty>
              {xs.length > 0 && <CommandGroup>{xs.map((c) => { const cur = confirmed.some((s) => s.person_id === c.id); return (
                <CommandItem key={c.id} value={c.id} onSelect={() => { if (!cur) void pick(c) }} data-owner-opt={c.name}>
                  <span className="min-w-0 flex-1 truncate">{c.is_me ? `${c.name}（我）` : c.name}</span>{cur && <CheckIcon className="size-4" />}</CommandItem>) })}</CommandGroup>}
              {owners.length > 0 && <><CommandSeparator /><CommandGroup><CommandItem value="__clear" onSelect={() => void clear()} className="text-muted-foreground" data-owner-clear><UserRoundXIcon />去掉负责人</CommandItem></CommandGroup></>}
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
    </span>
  )
}
