/* 设置 · 插件 (#/settings/plugins): what this Hub has installed and whether it runs, what the plugin libraries offer,
 * and the libraries themselves. It opens on what the owner comes for: anything broken first (a connection that is not
 * running, a library that could not be read), then what is installed, then what can be installed, then the libraries
 * and adding one.
 *
 * Plugins live under 设置 rather than beside 能力: 能力 is the workspace's library of what Bots use (MCP, skills, A2A),
 * chosen per Bot, while a plugin is the Hub's own: installed once by the instance owner for the whole instance, run beside
 * the Hub (a channel) or read by the client (a theme), never handed to a Bot or a harness.
 *
 * Libraries, installing and uninstalling are the instance owner's; connecting a channel plugin is anyone's who is signed
 * in, and makes an ordinary external-channel connection of theirs (managed and disconnected in 账号). A theme plugin is a
 * manifest the client registers (plugin-themes.ts) and then a choice in 外观.
 * Data: GET /plugins (hub-rs steve-hub plugins/api.rs); the caller's accounts only name and link their connections. */
import { useEffect, useState } from "react"
import { CircleAlertIcon, DownloadIcon, ExternalLinkIcon, LockIcon, MoreHorizontalIcon, PaletteIcon, PlugIcon, RefreshCwIcon, TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { IntegrationRow, IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { go, Section, Panel, Row, Tile, Empty, Blank, Notice, Pill, Meta, StatusDot, Loading, Chevron } from "@/ui"
import { modReq, Unauthenticated } from "@/api"
import { accountsApi, type RAccount } from "@/api/accounts"
import { vaultApi } from "@/api/vault"
import { useLoad, useBusy, report } from "@/accounts/load"
import { toast } from "@/store"
import { t as tr, useT, lang } from "@/i18n"
import { ago } from "@/i18n/format"
import { syncPluginThemes, usePluginTheme } from "@/plugin-themes"

/* the label and description are read when the settings list renders, so they follow the UI language */
export const PLUGINS_CAT = Object.defineProperties(["plugins", "", PlugIcon, ""], {
  1: { get: () => tr("manage:settings.catPlugins"), enumerable: true },
  3: { get: () => tr("manage:settings.catPluginsSub"), enumerable: true },
}) as [string, string, any, string]

/** a library's or a plugin's words: one string, or one per language */
type Text = string | Record<string, string>
type ConfigField = { key: string; label: Text; hint?: Text; secret: boolean; required: boolean }
type State = "starting" | "running" | "waiting" | "stopped"
type Instance = { instance: string; connection: string | null; status: { state: State; pid?: number; error?: string } }
type Plugin = {
  id: string; name: Text; summary: Text; kind: string; version: string; homepage: string | null; config: ConfigField[]
  /** false: installed, and the library no longer lists it */
  listed: boolean
  /** this Hub cannot run it: its kind or its contract is from a newer Steve */
  unsupported: "kind" | "contract" | null
  /** the library has a build for the Hub's machine (or one for any machine) */
  available: boolean
  installed: { version: string; installed: number; config: ConfigField[] } | null; update: boolean
  /** the caller's own */
  connections: Instance[]
}
/** `address` and `error` are the owner's to see; anyone else gets `failed` */
type Store = { id: string; name: Text | null; checked: number | null; official: boolean; plugins: Plugin[]; address?: string; error?: string | null; failed?: boolean }
type View = { target: string; owner: boolean; stores: Store[] }
type Entry = { s: Store; p: Plugin }

const e = encodeURIComponent
const pluginsApi = {
  list: () => modReq<View>("GET", "/plugins"),
  addStore: (address: string) => modReq<{ id: string }>("POST", "/plugins/stores", { address }),
  /** `error`: the index could not be read this time; what was read before stays */
  refresh: (store: string) => modReq<{ id: string; checked: number | null; error: string | null }>("POST", `/plugins/stores/${e(store)}/refresh`, {}),
  removeStore: (store: string) => modReq<{ ok: true }>("DELETE", `/plugins/stores/${e(store)}`),
  /** also the update: installs the version the library names now */
  install: (store: string, plugin: string) => modReq<{ version: string; installed: number }>("POST", `/plugins/installed/${e(store)}/${e(plugin)}`, {}),
  uninstall: (store: string, plugin: string) => modReq<{ ok: true }>("DELETE", `/plugins/installed/${e(store)}/${e(plugin)}`),
  /** the settings go straight into the body, never anywhere else */
  connect: (store: string, plugin: string, config: Record<string, string>) => modReq<{ instance: string; connection: string }>("POST", `/plugins/installed/${e(store)}/${e(plugin)}/connect`, { config }),
}

/** in the reader's language, else English, else whatever there is */
const say = (x: Text | null | undefined): string => (typeof x === "string" ? x : x ? x[lang()] ?? x.en ?? Object.values(x)[0] ?? "" : "")
const DOT: Record<State, string> = { starting: "run", running: "done", waiting: "wait", stopped: "idle" }
const isTheme = (p: Plugin) => p.kind === "theme"
const kindIcon = (p: Plugin) => (isTheme(p) ? PaletteIcon : PlugIcon)
const kindKey = (p: Plugin) => (isTheme(p) ? "pages:plugins.kindTheme" : p.kind === "connector" ? "pages:plugins.kindConnector" : p.kind === "channel" ? "pages:plugins.kindChannel" : "pages:plugins.kindOther")
const nameOf = (p: Plugin) => say(p.name) || p.id
/** a library that was never read has no name of its own yet: the official one is called that, any other by its id */
const storeName = (s: Store) => say(s.name) || (s.official ? tr("pages:plugins.officialName") : s.id)
/** where a connection is managed: its account's page, or the accounts list when the account is not among the caller's */
const accountOf = (mine: RAccount[], connection: string | null) => (connection ? mine.find((a) => a.connection?.id === connection) : undefined)
const accountHref = (a: RAccount | undefined) => (a ? "#/accounts/" + a.id : "#/accounts")
/** a connection that is not doing its job: its process is down */
const troubled = (c: Instance) => c.status.state === "waiting" || c.status.state === "stopped"

type Dlg = { k: "connect"; s: Store; p: Plugin } | { k: "connected"; connection: string }

export function PluginsSettingsPane() {
  const t = useT()
  const L = useLoad(async () => {
    const [v, mine] = await Promise.all([pluginsApi.list(), accountsApi.accounts({ scope: "mine" }).catch(() => [] as RAccount[])])
    return { v, mine }
  }, [], ["connection"])
  const [dlg, setDlg] = useState<Dlg | null>(null)
  /* a plugin process's state has no event of its own: while one of the caller's is starting or waiting, look again */
  const unsettled = L.s === "ok" && L.data.v.stores.some((s) => s.plugins.some((p) => p.connections.some((c) => c.status.state === "starting" || c.status.state === "waiting")))
  const { reload } = L
  useEffect(() => { if (!unsettled) return; const h = setInterval(() => void reload(), 5000); return () => clearInterval(h) }, [unsettled, reload])
  /* what is installed changed: the theme plugins are read again, so a new skin is a choice in 外观 at once (and on
   * opening the pane, so a theme's state below is this client's own) */
  const changed = async () => { await reload(); await syncPluginThemes() }
  useEffect(() => { void syncPluginThemes() }, [])
  if (L.s === "loading") return <Loading />
  if (L.s !== "ok") return (
    <div data-plugins-error><Empty compact className="bg-muted/50" icon={<PlugIcon />} title={t("pages:plugins.loadFailed")} sub={L.s === "error" ? L.err : t("pages:plugins.absent")}
      action={<Button size="sm" variant="outline" onClick={() => void L.reload()} data-plugins-retry>{t("common:action.retry")}</Button>} /></div>
  )
  const { v, mine } = L.data
  const all: Entry[] = v.stores.flatMap((s) => s.plugins.map((p) => ({ s, p })))
  const installed = all.filter((x) => x.p.installed)
  const offered = all.filter((x) => !x.p.installed)
  const unread = v.stores.filter((s) => s.checked == null)
  const failedStores = v.stores.filter((s) => s.error || s.failed)
  const down = installed.flatMap(({ s, p }) => p.connections.filter(troubled).map((c) => ({ s, p, c })))
  return (
    <div className="flex flex-col gap-8" data-plugins data-plugins-owner={v.owner}>
      {(down.length > 0 || failedStores.length > 0) && <div className="flex flex-col gap-2" data-plugins-attention>
        {down.map(({ p, c }) => { const a = accountOf(mine, c.connection); return (
          <Notice key={c.instance} tone="attention" data-plugin-down={c.instance}
            action={<Button size="xs" variant="ghost" onClick={() => go(accountHref(a))}>{t("pages:plugins.toAccount")}</Button>}>
            <span className="font-medium">{a ? `${nameOf(p)} · ${a.display_name || a.handle}` : nameOf(p)}</span>{t("pages:plugins.colon")}{t(`pages:plugins.state.${c.status.state}`)}
          </Notice>) })}
        {failedStores.map((s) => (
          <Notice key={s.id} tone="attention" data-plugin-store-failed={s.id}>{t("pages:plugins.storeFailed", { name: storeName(s) })}</Notice>))}
      </div>}

      <Section label={t("pages:plugins.installedSection")} data-plugins-installed>
        {installed.length ? <div className="flex flex-col gap-3">{installed.map(({ s, p }) => (
          <InstalledCard key={s.id + "/" + p.id} s={s} p={p} v={v} mine={mine} changed={changed} onConnect={() => setDlg({ k: "connect", s, p })} />))}</div>
          : <Blank data-plugins-none>{t(v.owner ? "pages:plugins.noneOwner" : "pages:plugins.noneOther")}</Blank>}
      </Section>

      <Section label={t("pages:plugins.offeredSection")} note={v.owner ? undefined : t("pages:plugins.ownerOnly")} data-plugins-offered>
        {offered.length > 0 && <Panel>{offered.map(({ s, p }) => <OfferedRow key={s.id + "/" + p.id} s={s} p={p} v={v} changed={changed} />)}</Panel>}
        {unread.length > 0 && <div className={offered.length ? "mt-2 flex flex-col gap-2" : "flex flex-col gap-2"}>{unread.map((s) => <UnreadStore key={s.id} s={s} owner={v.owner} reload={reload} />)}</div>}
        {!offered.length && !unread.length && <Blank data-plugins-offered-none>{t(v.stores.length ? "pages:plugins.offeredNone" : v.owner ? "pages:plugins.emptyOwner" : "pages:plugins.emptyOther")}</Blank>}
      </Section>

      <Section label={t("pages:plugins.librariesSection")} note={v.owner ? t("pages:plugins.addSub") : undefined} data-plugins-libraries>
        <div className="flex flex-col gap-3">
          {v.stores.length > 0 && <Panel>{v.stores.map((s) => <StoreRow key={s.id} s={s} owner={v.owner} reload={reload} />)}</Panel>}
          {v.owner && <AddStore reload={reload} />}
          {!v.owner && !v.stores.length && <Blank>{t("pages:plugins.empty")}</Blank>}
        </div>
      </Section>
      {dlg?.k === "connect" && <ConnectDialog s={dlg.s} p={dlg.p} onClose={() => setDlg(null)} done={async (connection) => { await reload(); setDlg({ k: "connected", connection }) }} />}
      {dlg?.k === "connected" && <ConnectedDialog href={accountHref(accountOf(mine, dlg.connection))} onClose={() => setDlg(null)} />}
    </div>
  )
}

/** an installed plugin: what it is, where it came from, and – a channel – the caller's connections through it and
 *  connecting another; – a theme – whether the client took its manifest and where it is chosen */
function InstalledCard({ s, p, v, mine, changed, onConnect }: { s: Store; p: Plugin; v: View; mine: RAccount[]; changed: () => Promise<void>; onConnect: () => void }) {
  const t = useT()
  const [busy, act] = useBusy(), [rm, setRm] = useState(false)
  const name = nameOf(p), inst = p.installed!, I = kindIcon(p)
  const canUpdate = v.owner && p.update && p.listed && !p.unsupported && p.available
  const update = async () => { if (await act(() => pluginsApi.install(s.id, p.id), t("pages:plugins.updated", { name }))) await changed() }
  const uninstall = async () => { if (await act(() => pluginsApi.uninstall(s.id, p.id), t("pages:plugins.uninstalled", { name }))) { setRm(false); await changed() } }
  return (
    <>
      <IntegrationSettings name={name} logo={<I className="size-5" />} data-plugin={p.id} data-plugin-installed={inst.version} data-plugin-update={p.update} data-plugin-kind={p.kind}
        badge={<Badge variant="secondary">{t(kindKey(p), { kind: p.kind })}</Badge>}
        meta={<Meta one><span className="font-mono" data-plugin-version>{inst.version}</span><span className="truncate">{t("pages:plugins.from", { name: storeName(s) })}</span>
          {p.homepage && /^https?:\/\//.test(p.homepage) && <a href={p.homepage} target="_blank" rel="noreferrer" className="inline-flex items-center gap-0.5 hover:text-foreground hover:underline" data-plugin-homepage>{t("pages:plugins.homepage")}<ExternalLinkIcon className="size-3" aria-hidden /></a>}</Meta>}
        aside={<span className="flex shrink-0 items-center gap-1">
          {(p.kind === "channel" || p.kind === "connector") && !p.unsupported && <Button size="sm" disabled={busy} onClick={onConnect} data-plugin-connect>{t("pages:plugins.connect")}</Button>}
          {v.owner && <DropdownMenu>
            <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("pages:plugins.more", { name })} data-plugin-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-36"><DropdownMenuItem variant="destructive" onClick={() => setRm(true)} data-plugin-uninstall><TrashIcon />{t("pages:plugins.uninstall")}</DropdownMenuItem></DropdownMenuContent>
          </DropdownMenu>}
        </span>}>
        <IntegrationSection>
          <p className="type-callout text-muted-foreground" data-plugin-summary>{say(p.summary)}</p>
          {!p.listed && <p className="type-footnote text-attention" data-plugin-why="unlisted">{t("pages:plugins.unlisted")}</p>}
          {canUpdate && <IntegrationRow title={t("pages:plugins.updateTo", { version: p.version })} hint={t("pages:plugins.updateFrom", { version: inst.version })} data-plugin-update-row>
            <Button size="sm" variant="outline" disabled={busy} onClick={() => void update()} data-plugin-install>{busy ? <Spinner className="size-3.5" /> : <DownloadIcon />}{t("pages:plugins.update")}</Button>
          </IntegrationRow>}
          {isTheme(p) ? <ThemeState s={s} p={p} /> : <Connections p={p} mine={mine} />}
        </IntegrationSection>
      </IntegrationSettings>
      <AlertDialog open={rm} onOpenChange={setRm}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("pages:plugins.uninstallTitle", { name })}</AlertDialogTitle><AlertDialogDescription>{t(isTheme(p) ? "pages:plugins.uninstallThemeSub" : "pages:plugins.uninstallSub")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={busy} onClick={() => void uninstall()} data-plugin-uninstall-confirm>{t("pages:plugins.uninstall")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </>
  )
}

/** the caller's own connections through a channel plugin: whose account, whether its process runs, what it last said */
function Connections({ p, mine }: { p: Plugin; mine: RAccount[] }) {
  const t = useT()
  if (!p.connections.length) return <IntegrationRow title={t("pages:plugins.noConnection")} hint={t("pages:plugins.noConnectionSub")} data-plugin-connections="0" />
  return <>{p.connections.map((c) => {
    const a = accountOf(mine, c.connection)
    return (
      <div key={c.instance} className="flex flex-col gap-1" data-plugin-connection={c.instance} data-connection-state={c.status.state}>
        <IntegrationRow title={a ? a.display_name || a.handle : t("pages:plugins.myConnection")}
          hint={<span className="inline-flex items-center gap-1.5"><StatusDot s={DOT[c.status.state] ?? "idle"} />{t(`pages:plugins.state.${DOT[c.status.state] ? c.status.state : "stopped"}`)}</span>}>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go(accountHref(a))} data-plugin-connection-open>{t("pages:plugins.manage")}<Chevron /></Button>
        </IntegrationRow>
        {c.status.error && <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-attention/10 px-3 py-2 font-mono type-footnote text-foreground/85" data-connection-error>{c.status.error}</pre>}
      </div>)
  })}</>
}

/** a theme plugin on this client: registered (and chosen in 外观), or refused with the reasons */
function ThemeState({ s, p }: { s: Store; p: Plugin }) {
  const t = useT()
  const st = usePluginTheme(s.id, p.id)
  if (st?.errors) return (
    <div className="flex items-start gap-2 rounded-lg bg-attention/10 px-3 py-2 type-footnote" data-plugin-theme="refused">
      <CircleAlertIcon className="mt-0.5 size-3.5 shrink-0 text-attention" aria-hidden />
      <span className="min-w-0"><span className="font-medium">{t("pages:plugins.themeRefused")}</span> <span className="break-words text-muted-foreground">{st.errors.slice(0, 3).join("; ")}</span></span>
    </div>)
  return (
    <IntegrationRow title={t("pages:plugins.themeReady")} hint={t("pages:plugins.themeReadySub")} data-plugin-theme={st?.id ?? "pending"}>
      <Button size="sm" variant="outline" onClick={() => go("#/settings/look")} data-plugin-theme-open>{t("pages:plugins.themeOpen")}</Button>
    </IntegrationRow>
  )
}

/** a plugin a library offers that is not installed: install it, or why it cannot be installed here */
function OfferedRow({ s, p, v, changed }: { s: Store; p: Plugin; v: View; changed: () => Promise<void> }) {
  const t = useT()
  const [busy, act] = useBusy()
  const name = nameOf(p)
  const why = p.unsupported ? t("pages:plugins.needsNewer") : !p.available ? t("pages:plugins.noBuild", { target: v.target }) : ""
  const install = async () => { if (await act(() => pluginsApi.install(s.id, p.id), t("pages:plugins.installed", { name }))) await changed() }
  return (
    <Row icon={<Tile I={kindIcon(p)} />} data-plugin={p.id} data-plugin-installed="" data-plugin-kind={p.kind}
      title={<span className="inline-flex flex-wrap items-center gap-1.5">{name}<Pill>{t(kindKey(p), { kind: p.kind })}</Pill></span>}
      sub={<span className="flex flex-col gap-0.5"><span>{say(p.summary)}</span>
        <Meta inline><span className="font-mono" data-plugin-version>{p.version}</span><span>{storeName(s)}</span>
          {why && <span className="text-attention" data-plugin-why={p.unsupported ? "unsupported" : "unavailable"}>{why}</span>}</Meta></span>}>
      {v.owner && !why && <Button size="sm" variant="outline" disabled={busy} onClick={() => void install()} data-plugin-install>
        {busy ? <Spinner className="size-3.5" /> : <DownloadIcon />}{t(busy ? "pages:plugins.installing" : "pages:plugins.install")}</Button>}
    </Row>
  )
}

/** a library that was never read: it offers nothing yet, and reading it is the one thing to do (the official one is
 *  listed on a new installation and nothing is fetched until the owner asks) */
function UnreadStore({ s, owner, reload }: { s: Store; owner: boolean; reload: () => Promise<void> }) {
  const t = useT()
  const [busy, act] = useBusy()
  const read = async () => { const r = await act(() => pluginsApi.refresh(s.id)); if (!r) return; toast(r.error ? t("pages:plugins.readFailed", { error: r.error }) : t("pages:plugins.readDone")); await reload() }
  return (
    <Notice tone="info" data-store-unread-notice={s.id} action={owner && <Button size="sm" variant="outline" disabled={busy} onClick={() => void read()} data-store-read>{busy ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}{t("pages:plugins.read")}</Button>}>
      {t(owner ? "pages:plugins.unreadOwner" : "pages:plugins.unreadOther", { name: storeName(s) })}
    </Notice>
  )
}

/** one library: its name, where it is (the owner sees the address), when it was read, and reading it again or removing it */
function StoreRow({ s, owner, reload }: { s: Store; owner: boolean; reload: () => Promise<void> }) {
  const t = useT()
  const [busy, act] = useBusy(), [rm, setRm] = useState(false)
  const name = storeName(s), unread = s.checked == null
  const refresh = async () => {
    const r = await act(() => pluginsApi.refresh(s.id))
    if (!r) return
    toast(r.error ? t("pages:plugins.readFailed", { error: r.error }) : t("pages:plugins.readDone"))
    await reload()
  }
  const remove = async () => { if (await act(() => pluginsApi.removeStore(s.id), t("pages:plugins.storeRemoved"))) { setRm(false); await reload() } }
  return (
    <div data-plugin-store={s.id} data-store-unread={unread}>
      <Row title={<span className="inline-flex flex-wrap items-center gap-1.5" data-store-name>{name}{s.official && <Pill tone="info" data-store-official>{t("pages:plugins.official")}</Pill>}</span>}
        sub={<span className="flex flex-col gap-0.5">
          {s.address && <span className="break-all font-mono" data-store-address>{s.address}</span>}
          <Meta inline><span data-store-checked>{unread ? t("pages:plugins.neverRead") : t("pages:plugins.readAt", { when: ago(s.checked!) })}</span>
            {!unread && <span>{t("pages:plugins.count", { count: s.plugins.length })}</span>}</Meta>
          {(s.error || s.failed) && <span className="whitespace-pre-wrap break-words text-destructive" data-store-error>{s.error ? t("pages:plugins.readFailed", { error: s.error }) : t("pages:plugins.readFailedBefore")}</span>}
        </span>}>
        {owner && <span className="flex shrink-0 items-center gap-0.5">
          <Button size="icon-sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void refresh()} aria-label={t("pages:plugins.readAgain", { name })} title={t("pages:plugins.readAgain", { name })} data-store-refresh>{busy ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}</Button>
          <Button size="icon-sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => setRm(true)} aria-label={t("pages:plugins.removeStore", { name })} title={t("pages:plugins.removeStore", { name })} data-store-remove><TrashIcon /></Button>
        </span>}
      </Row>
      <AlertDialog open={rm} onOpenChange={setRm}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("pages:plugins.removeStoreTitle", { name })}</AlertDialogTitle><AlertDialogDescription>{t("pages:plugins.removeStoreSub")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={busy} onClick={() => void remove()} data-store-remove-confirm>{t("common:action.remove")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </div>
  )
}

/** owner only: a library is added by its address; the Hub reads its index first and refuses one it cannot read */
function AddStore({ reload }: { reload: () => Promise<void> }) {
  const t = useT()
  const [address, setAddress] = useState(""), [busy, act] = useBusy()
  const add = async () => { if (await act(() => pluginsApi.addStore(address.trim()), t("pages:plugins.added"))) { setAddress(""); await reload() } }
  return (
    <form className="flex gap-2" data-plugin-add onSubmit={(ev) => { ev.preventDefault(); if (address.trim() && !busy) void add() }}>
      <Input className="min-w-0 flex-1 font-mono" value={address} maxLength={2000} onChange={(ev) => setAddress(ev.target.value)} placeholder={t("pages:plugins.addPlaceholder")} aria-label={t("pages:plugins.add")} autoComplete="off" spellCheck={false} data-plugin-add-address />
      <Button type="submit" variant="outline" disabled={busy || !address.trim()} data-plugin-add-go>{busy ? <Spinner className="size-4" /> : t("pages:plugins.add")}</Button>
    </form>
  )
}

/** the settings the installed plugin asks for; secret ones are password fields, and no value is kept once sent.
 *  The Hub starts the plugin before it answers, which can take some twenty seconds; its refusal may carry the end of
 *  the plugin's log, so the line breaks are kept. */
function ConnectDialog({ s, p, onClose, done }: { s: Store; p: Plugin; onClose: () => void; done: (connection: string) => void | Promise<void> }) {
  const t = useT()
  const fields = p.installed?.config ?? []
  const [vals, setVals] = useState<Record<string, string>>({}), [busy, setBusy] = useState(false), [err, setErr] = useState(""), [noVault, setNoVault] = useState(false)
  /* the connection's secrets live in the system vault: connecting needs an enabled vault */
  useEffect(() => { vaultApi.status().then((x) => setNoVault(!x.initialized), () => {}) }, [])
  const filled = fields.every((f) => f.required === false || (vals[f.key] ?? "").trim())
  const submit = async () => {
    setBusy(true); setErr("")
    const config: Record<string, string> = {}
    for (const f of fields) if ((vals[f.key] ?? "").trim()) config[f.key] = vals[f.key]
    try { const r = await pluginsApi.connect(s.id, p.id, config); setVals({}); await done(r.connection) }
    catch (x) { if (x instanceof Unauthenticated) report(x); else setErr((x as Error).message) }
    finally { setBusy(false) }
  }
  const title = t("pages:plugins.connectTitle", { name: nameOf(p) })
  if (noVault) return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[440px]" data-plugin-connect-dialog="no-vault">
        <DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{t("accounts:connect.noVault")}</DialogDescription></DialogHeader>
        <DialogFooter><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button onClick={() => { onClose(); go("#/vault") }} data-go-enable-vault>{t("accounts:connect.enableVault")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
  return (
    <Dialog open onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent className="sm:max-w-[480px]" data-plugin-connect-dialog={p.id}>
        <DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{t("pages:plugins.connectSub")}</DialogDescription></DialogHeader>
        <form className="contents" autoComplete="off" onSubmit={(ev) => { ev.preventDefault(); if (!busy && filled) void submit() }}>
          <FieldGroup className="gap-4">{fields.map((f) => (
            <Field key={f.key}>
              <FieldLabel htmlFor={"pf-" + f.key}>{say(f.label) || f.key}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label={t("accounts:connect.secret")} />}{f.required === false && <span className="ml-1 font-normal text-muted-foreground">{t("accounts:connect.optional")}</span>}</FieldLabel>
              <Input id={"pf-" + f.key} className="font-mono" type={f.secret ? "password" : "text"} autoComplete="off" spellCheck={false} required={f.required !== false} disabled={busy}
                value={vals[f.key] ?? ""} onChange={(ev) => setVals({ ...vals, [f.key]: ev.target.value })} data-plugin-field={f.key} />
              {f.hint && <FieldDescription>{say(f.hint)}</FieldDescription>}
            </Field>))}</FieldGroup>
          <button type="submit" hidden />
        </form>
        {busy && <p className="type-footnote text-muted-foreground" data-plugin-connecting>{t("pages:plugins.connecting")}</p>}
        {err && <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-destructive/10 px-3 py-2 font-sans type-footnote text-destructive" role="alert" data-plugin-connect-error>{err}</pre>}
        <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button disabled={busy || !filled} onClick={() => void submit()} data-plugin-connect-go>{busy && <Spinner />}{t("pages:plugins.connectGo")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** connected: from here on it is a connection like any other, and 账号 is where it is looked after */
function ConnectedDialog({ href, onClose }: { href: string; onClose: () => void }) {
  const t = useT()
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[440px]" data-plugin-connected>
        <DialogHeader><DialogTitle>{t("pages:plugins.connectedTitle")}</DialogTitle><DialogDescription>{t("pages:plugins.connectedSub")}</DialogDescription></DialogHeader>
        <DialogFooter><Button variant="outline" onClick={onClose}>{t("pages:plugins.done")}</Button><Button onClick={() => { onClose(); go(href) }} data-plugin-to-accounts>{t("pages:plugins.toAccounts")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
