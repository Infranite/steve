/* Devices · Clients (#/devices/clients, formerly 设置 · 访问与客户端): this client, the other clients signed in to the
 * Hub (the instance owner sees every member's) with the route each last came by, the signed-out ones folded away,
 * and the access tokens command lines and scripts sign in with. A token's text is shown once, when it is made. */
import { useEffect, useState, type ReactNode } from "react"
import { GlobeIcon, KeyRoundIcon, LogOutIcon, MonitorIcon, MonitorSmartphoneIcon, QrCodeIcon, SmartphoneIcon, SquareTerminalIcon } from "lucide-react"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { TextField } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { api } from "@/api"
import { ActorAvatar } from "@/components/actor-avatar"
import { useMe } from "@/mine"
import { access, type AccessClient, type AccessToken, type AccessView } from "@/access-api"
import { toast } from "@/store"
import { Loading, Notice, openSheet } from "@/ui"
import { ageOf, agoText, clientKind, routeLabel, type ClientKind } from "@/device-model"
import { clientKey, clientOnline } from "@/device-map-model"
import { useDevices } from "@/devices-data"
import { Chip, DCard, DRow } from "@/devices-ui"
import { cn } from "cn"

const ICON: Record<ClientKind, typeof GlobeIcon> = { browser: GlobeIcon, phone: SmartphoneIcon, mac: MonitorIcon, cli: SquareTerminalIcon, other: MonitorSmartphoneIcon }
const DAY = 86_400_000

/** the sign-ins of one client: the same name, kind, platform, route and person (a browser signed in again and again);
 *  this client always alone. Signing a group out signs out every sign-in in it. */
type Group = { key: string; clients: AccessClient[]; head: AccessClient }
function groups(v: AccessView, live: boolean): Group[] {
  const m = new Map<string, AccessClient[]>()
  for (const c of [...v.clients].filter((c) => !!c.revoked !== live).sort((a, b) => (b.last_seen ?? b.created) - (a.last_seen ?? a.created))) {
    if (c.id === v.current) continue
    const key = clientKey(c)
    m.set(key, [...(m.get(key) ?? []), c])
  }
  return [...m].map(([key, clients]) => ({ key, clients, head: clients[0] }))
}

/** `focus`: a client to bring into view (from the map), its row lit for a moment */
export function ClientsTab({ focus }: { focus?: string }) {
  const t = useT(), { access: v, accessError, reloadAccess } = useDevices(), currentUser = useMe()
  const [confirm, setConfirm] = useState<Group | null>(null)
  const ready = !!v
  useEffect(() => {
    if (!focus || !ready) return
    const el = document.querySelector<HTMLElement>(`[data-client="${CSS.escape(focus)}"], [data-client-current="${CSS.escape(focus)}"]`)
    if (!el) return
    el.scrollIntoView({ block: "center", behavior: "smooth" })
    el.setAttribute("data-client-focus", "")
    const timer = setTimeout(() => el.removeAttribute("data-client-focus"), 1600)
    return () => { clearTimeout(timer); el.removeAttribute("data-client-focus") }
  }, [focus, ready])
  if (!v) return accessError ? <Notice tone="destructive">{t("devices:loadFailed", { error: accessError })}</Notice> : <Loading />
  const now = Date.now()
  const tokenName = (id?: string | null) => (id ? v.tokens.find((x) => x.id === id)?.name : undefined)
  const mine = (c: AccessClient) => !v.all || !c.user_id || c.user_id === v.me
  const me = v.clients.find((c) => c.id === v.current)
  const live = groups(v, true), gone = groups(v, false)
  const fresh = live.filter((g) => ageOf(g.head.last_seen, g.head.created, now) === "fresh"), stale = live.filter((g) => !fresh.includes(g))
  const facts = (c: AccessClient, extra?: ReactNode) => <>
    <span>{[t(`devices:clients.kind.${clientKind(c.kind, c.platform)}`), c.platform].filter(Boolean).join(" · ")}</span>
    {c.last_route && <span>{t("devices:clients.via", { route: routeLabel(c.last_route) })}</span>}
    {extra}
    {mine(c) && tokenName(c.token_id) && <span>{t("devices:clients.token", { name: tokenName(c.token_id) })}</span>}
  </>
  /* online: it holds the live connection to the Hub now */
  const when = (c: AccessClient, on: boolean) => <span title={c.last_seen ? date(c.last_seen, { dateStyle: "medium", timeStyle: "short" }) : undefined}>{on ? t("devices:node.now") : c.last_seen ? agoText(c.last_seen, now) : t("devices:tokens.neverUsed")}</span>
  const onBadge = <Badge tone="success" size="sm" dot className="font-normal" data-client-online>{t("devices:status.online")}</Badge>
  const row = (g: Group) => {
    const c = g.head, on = g.clients.some((x) => clientOnline(x)), age = on ? "fresh" : ageOf(c.last_seen, c.created, now), Icon = ICON[clientKind(c.kind, c.platform)]
    return <DRow key={g.key} icon={<Icon />} className={cn("group/client", age !== "fresh" && "[&>span:nth-child(-n+2)]:opacity-70")} data-client={c.id}
      title={<><span className="truncate">{c.name}</span>
        {on && onBadge}
        {g.clients.length > 1 && <span className="type-footnote tabular-nums text-muted-foreground" data-client-count={g.clients.length}>{t("devices:clients.sameN", { count: g.clients.length })}</span>}
        {!mine(c) && c.user_id && <Chip><span className="inline-flex items-center gap-1"><ActorAvatar actorId={c.user_id} name={v.users?.[c.user_id]} size={16} />{t("devices:clients.of", { name: v.users?.[c.user_id] ?? c.user_id })}</span></Chip>}
        {age === "unused" && <Badge tone="warning" size="sm" data-client-unused>{t("devices:clients.unused30")}</Badge>}</>}
      sub={facts(c, when(c, on))}>
      <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setConfirm(g)} data-revoke-client={c.id}>{t("devices:clients.revoke")}</Button>
    </DRow>
  }
  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)]" data-access-pane>
      <div className="flex min-w-0 flex-col gap-4">
        <DCard title={t("devices:clients.this")} sub={t("devices:clients.thisSub")} data-access-hub>
          <div className="border-t border-border/70">
            <DRow icon={me ? (() => { const I = ICON[clientKind(me.kind, me.platform)]; return <I /> })() : <GlobeIcon />} data-client-current={me?.id ?? ""}
              title={<><span className="truncate">{me?.name ?? t("devices:clients.browserSession")}</span><Chip tone="info">{t("devices:clients.you")}</Chip></>}
              sub={<>{currentUser?.login && <span className="font-mono" data-login-handle>{t("devices:clients.loginAs", { login: currentUser.login })}</span>}{me ? facts(me, me.version ? <span>{me.version}</span> : null) : v.route ? <span>{t("devices:clients.via", { route: routeLabel(v.route) })}</span> : undefined}</>}>
              <Button size="sm" variant="outline" onClick={async () => { await api.logout().catch(() => {}); location.reload() }} data-sign-out><LogOutIcon />{t("devices:clients.signOut")}</Button>
            </DRow>
          </div>
        </DCard>
        <DCard title={t("devices:clients.others", { n: live.length })} sub={t(v.all && Object.keys(v.users ?? {}).some((id) => id !== v.me) ? "devices:clients.othersSubAll" : "devices:clients.othersSub")} data-access-clients
          action={<Button size="sm" variant="outline" onClick={() => openSheet("addDevice", { kind: "client" })} data-open-pair-qr><QrCodeIcon />{t("devices:clients.add")}</Button>}>
          <div className="border-t border-border/70">
            {fresh.map(row)}
            {!live.length && <p className="px-5 py-4 type-callout text-muted-foreground" data-no-clients>{t("devices:clients.none")}</p>}
            {stale.length > 0 && <><div className="px-5 pt-3 pb-1 type-label text-tertiary" data-clients-stale>{t("devices:clients.stale")}</div>{stale.map(row)}</>}
          </div>
          {gone.length > 0 && <details className="border-t border-border/70 px-5 py-2" data-clients-revoked>
            <summary className="cursor-pointer py-1.5 type-footnote text-muted-foreground hover:text-foreground">{t("devices:clients.revokedN", { n: gone.length })}</summary>
            <div className="-mx-5">{gone.map((g) => { const c = g.head, Icon = ICON[clientKind(c.kind, c.platform)]; return <DRow key={g.key} icon={<Icon />} className="opacity-70" data-client-revoked={c.id}
              title={<span className="truncate line-through">{c.name}</span>}
              sub={<><span>{t("devices:clients.revokedAgo", { ago: agoText(c.revoked!, now) })}</span>{c.last_seen && <span>{t("devices:clients.lastVisit", { ago: agoText(c.last_seen, now) })}</span>}{c.last_route && <span>{routeLabel(c.last_route)}</span>}</>} /> })}</div>
          </details>}
        </DCard>
      </div>
      <div className="flex min-w-0 flex-col gap-4">
        <Tokens v={v} reload={reloadAccess} />
        <p className="px-1 type-footnote text-muted-foreground" data-clients-note>{t("devices:clients.onlineNote")}</p>
      </div>
      {confirm && <RevokeClient v={v} group={confirm} onClose={() => setConfirm(null)} onDone={reloadAccess} />}
    </div>
  )
}

/** "Sign out X?": it is signed out at once; the token it signed in with can go too, ticked when nothing else uses it */
function RevokeClient({ v, group, onClose, onDone }: { v: AccessView; group: Group; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const c = group.head, ids = new Set(group.clients.map((x) => x.id))
  const token = c.token_id ? v.tokens.find((x) => x.id === c.token_id && !x.revoked) : undefined
  const shared = token ? v.clients.some((x) => x.token_id === token.id && !x.revoked && !ids.has(x.id)) : false
  const [withToken, setWithToken] = useState(!!token && !shared)
  const [busy, setBusy] = useState(false)
  const go = async () => {
    setBusy(true)
    try {
      await Promise.all(group.clients.map((x) => access.revokeClient(x.id)))
      if (token && withToken) await access.revokeToken(token.id)
      toast(t("devices:clients.revokeDone", { name: c.name })); onClose(); onDone()
    } catch (e) { toast((e as Error).message); setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="sm:max-w-md" data-revoke-dialog>
        <DialogHeader><DialogTitle>{t("devices:clients.revokeTitle", { name: c.name })}</DialogTitle><DialogDescription>{t("devices:clients.revokeBody")}</DialogDescription></DialogHeader>
        {token && <div className="flex flex-col gap-3 type-callout">
          <p className="text-muted-foreground">{t("devices:clients.revokeTokenNote", { token: token.name })}</p>
          <Checkbox checked={withToken} onCheckedChange={(x) => setWithToken(!!x)} label={t("devices:clients.revokeToken", { token: token.name })} data-revoke-with-token />
        </div>}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button variant="destructive" disabled={busy} onClick={() => void go()} data-revoke-confirm>{busy ? <Spinner className="size-4" /> : t("devices:clients.revoke")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** access tokens: how many clients use each, when it was used, when it expires; made here and shown once */
function Tokens({ v, reload }: { v: AccessView; reload: () => void }) {
  const t = useT()
  const [name, setName] = useState(""), [fresh, setFresh] = useState<string | null>(null), [busy, setBusy] = useState(false)
  const now = Date.now()
  const create = async () => {
    setBusy(true)
    try { const r = await access.create(name.trim()); setFresh(r.token); setName(""); reload() }
    catch (e) { toast((e as Error).message) }
    setBusy(false)
  }
  const revoke = async (x: AccessToken) => {
    try { await access.revokeToken(x.id); toast(t("devices:tokens.revoked", { name: x.name })); reload() } catch (e) { toast((e as Error).message) }
  }
  const live = v.tokens.filter((x) => !x.revoked)
  return (
    <DCard icon={<KeyRoundIcon />} title={t("devices:tokens.title")} sub={t("devices:tokens.sub")} data-access-tokens>
      {fresh && <div className="mx-5 mb-3 rounded-xl bg-success/10 px-4 py-3 anim-in" data-fresh-token>
        <div className="type-footnote text-muted-foreground">{t("pages:access.shownOnce")}</div>
        <code className="mt-1 block break-all type-code text-foreground">{fresh}</code>
        <div className="mt-2 flex gap-2"><Button size="sm" variant="outline" onClick={() => { void navigator.clipboard?.writeText(fresh); toast(t("pages:access.copied")) }}>{t("pages:access.copy")}</Button><Button size="sm" variant="ghost" onClick={() => setFresh(null)}>{t("pages:access.done")}</Button></div>
      </div>}
      <div className="border-t border-border/70">
        {live.map((x) => {
          const users = v.clients.filter((c) => c.token_id === x.id && !c.revoked).length
          const expired = x.expires != null && x.expires <= now
          return <DRow key={x.id} data-token={x.id}
            title={<><span className="truncate">{x.name}</span>{ageOf(x.last_used, x.created, now) === "unused" && <Badge tone="warning" size="sm">{t("devices:clients.unused30")}</Badge>}</>}
            sub={<>
              <span>{t("devices:tokens.created", { ago: agoText(x.created, now) })}</span>
              <span>{x.last_used ? t("devices:tokens.used", { ago: agoText(x.last_used, now) }) : t("devices:tokens.neverUsed")}</span>
              <span>{users ? t("devices:tokens.usedBy", { count: users }) : t("devices:tokens.noClients")}</span>
              <span className={expired ? "text-destructive" : undefined}>{x.expires == null ? t("devices:tokens.noExpiry") : expired ? t("devices:tokens.expired") : t("devices:tokens.expiresIn", { count: Math.max(1, Math.round((x.expires - now) / DAY)) })}</span>
            </>}>
            <Button size="sm" variant="ghost" className="text-destructive" onClick={() => void revoke(x)} data-revoke-token={x.id}>{t("devices:tokens.revoke")}</Button>
          </DRow>
        })}
        {!live.length && <p className="px-5 py-3 type-callout text-muted-foreground">{t("devices:tokens.none")}</p>}
        <form className="flex items-center gap-2 border-t border-border/70 px-5 py-3" onSubmit={(e) => { e.preventDefault(); void create() }}>
          <TextField size="sm" className="min-w-0 flex-1" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("devices:tokens.name")} aria-label={t("devices:tokens.name")} data-token-name />
          <Button size="sm" variant="outline" type="submit" disabled={busy} data-new-token>{busy ? <Spinner className="size-4" /> : t("devices:tokens.create")}</Button>
        </form>
      </div>
    </DCard>
  )
}
