/* What the library's three kinds share: loading a list, the Bots that use an entry (chips, and a menu to choose them
 * from the library's side), and the error text of a failed call. */
import { useEffect, useState } from "react"
import { BotIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuGroup, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { isNotWired } from "@/api"
import { st, toast, useHubEvents, useStore } from "@/store"
import { BotAvatar } from "@/ui"
import { capsApi, type CapKind, type CapUser } from "@/api/caps"

export const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

export type Load<T> = { s: "loading" } | { s: "unwired" } | { s: "error"; e: string } | { s: "ok"; v: T }

/** a list from the Hub, again when a Bot changes (an assignment publishes the Bot's event) */
export function useList<T>(f: () => Promise<T>, deps: unknown[]): [Load<T>, () => Promise<void>, (v: T) => void] {
  const [d, setD] = useState<Load<T>>({ s: "loading" })
  const load = () => f().then((v) => setD({ s: "ok", v }), (e) => setD(isNotWired(e) ? { s: "unwired" } : { s: "error", e: errText(e) }))
  useEffect(() => { setD({ s: "loading" }); void load() }, deps)
  useHubEvents(["bot"], () => void load())
  return [d, load, (v: T) => setD({ s: "ok", v })]
}

/** every Bot of this workspace, Steve's own first */
export function useBots() {
  useStore()
  return Object.values(st.bots).sort((a, b) => Number(!!b.main) - Number(!!a.main) || a.n.localeCompare(b.n))
}

/** the Bots that use an entry: small avatars and names; a switched-off one is dimmed */
export function UsedBy({ users }: { users: CapUser[] }) {
  const t = useT()
  useStore()
  if (!users.length) return <span className="type-footnote text-muted-foreground" data-used-by="">{t("caps:use.none")}</span>
  return (
    <span className="flex flex-wrap items-center gap-1.5" data-used-by={users.map((u) => u.id).join(",")}>
      {users.map((u) => {
        const b = st.bots[u.id]
        return (
          <a key={u.id} href={`#/bots/${encodeURIComponent(u.id)}`} title={u.enabled ? undefined : t("caps:use.offFor", { bot: b?.n ?? u.id })}
            className={"inline-flex items-center gap-1 rounded-full bg-muted/70 py-0.5 pr-2 pl-0.5 type-footnote hover:bg-muted " + (u.enabled ? "text-foreground" : "text-muted-foreground line-through decoration-muted-foreground/60")} data-used-bot={u.id}>
            <BotAvatar k={u.id} size={16} />{b?.n ?? u.id}
          </a>
        )
      })}
    </span>
  )
}

/** from the library's side: which Bots use this entry. Ticking one assigns it (switched on), unticking takes it off */
export function AssignMenu({ kind, id, name, users, onChange }: { kind: CapKind; id: string; name: string; users: CapUser[]; onChange: () => void }) {
  const t = useT()
  const bots = useBots()
  const has = new Set(users.map((u) => u.id))
  const flip = async (bot: string, on: boolean) => {
    const n = st.bots[bot]?.n ?? bot
    try {
      if (on) await capsApi.assign(bot, kind, id); else await capsApi.unassign(bot, kind, id)
      toast(t(on ? "caps:use.assigned" : "caps:use.unassigned", { bot: n, name }))
    } catch (e) { toast(errText(e)) }
    onChange()
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button size="xs" variant="ghost" className="text-muted-foreground" data-assign={name} aria-label={t("caps:use.assign")} />}><BotIcon /><span className="max-sm:hidden">{t("caps:use.assign")}</span></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-80 min-w-48 overflow-y-auto">
        <DropdownMenuGroup>
        <DropdownMenuLabel>{t("caps:use.bots")}</DropdownMenuLabel>
        {bots.map((b) => (
          <DropdownMenuCheckboxItem key={b.id} checked={has.has(b.id)} onCheckedChange={(on) => void flip(b.id, !!on)} data-assign-bot={b.id}>
            <span className="flex min-w-0 items-center gap-2"><BotAvatar k={b.id} size={18} /><span className="truncate">{b.n}</span></span>
          </DropdownMenuCheckboxItem>
        ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

