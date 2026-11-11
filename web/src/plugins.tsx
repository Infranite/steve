/* 插件 (#/plugins, reached from 设置): the plugin libraries this Hub reads, what each one offers, what is installed, and
 * connecting an installed plugin as yourself. Libraries, installing and uninstalling are the instance owner's; connecting
 * is anyone's who is signed in. A connected plugin is an ordinary external-channel connection: it is managed and
 * disconnected in 账号, and disconnecting it there removes the plugin instance.
 * Data: GET /plugins (hub-rs steve-hub plugins/api.rs); the caller's accounts only name and link their connections. */
import { useEffect, useState } from "react"
import { ChevronRightIcon, ExternalLinkIcon, LockIcon, PlugIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { go, Page, Group, Row, SectionLabel, Empty, StatusDot } from "@/ui"
import { modReq, Unauthenticated } from "@/api"
import { accountsApi, type RAccount } from "@/api/accounts"
import { vaultApi } from "@/api/vault"
import { useLoad, useBusy, report, Dots } from "@/accounts/load"
import { toast } from "@/store"
import { useT, lang } from "@/i18n"
import { ago } from "@/i18n/format"

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
  /** the library has a build for the Hub's machine */
  available: boolean
  installed: { version: string; installed: number; config: ConfigField[] } | null; update: boolean
  /** the caller's own */
  connections: Instance[]
}
/** `address` and `error` are the owner's to see; anyone else gets `failed` */
type Store = { id: string; name: Text | null; checked: number | null; official: boolean; plugins: Plugin[]; address?: string; error?: string | null; failed?: boolean }
type View = { target: string; owner: boolean; stores: Store[] }

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
const Ico = ({ I }: { I: any }) => <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground" aria-hidden><I className="size-4" strokeWidth={1.5} /></span>
/** where a connection is managed: its account's page, or the accounts list when the account is not among the caller's */
const accountOf = (mine: RAccount[], connection: string | null) => (connection ? mine.find((a) => a.connection?.id === connection) : undefined)
const accountHref = (a: RAccount | undefined) => (a ? "#/accounts/" + a.id : "#/accounts")

type Dlg = { k: "connect"; s: Store; p: Plugin } | { k: "connected"; connection: string }

export function PluginsView() {
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
  const title = t("pages:plugins.title"), back: [string, string] = ["#/settings", t("manage:settings.title")]
  if (L.s === "loading") return <Page title={title} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s !== "ok") return (
    <Page title={title} back={back}>
      <div data-plugins-error><Empty title={t("pages:plugins.loadFailed")} sub={L.s === "error" ? L.err : t("pages:plugins.absent")} icon={<PlugIcon />} />
        <div className="-mt-8 flex justify-center"><Button size="sm" variant="outline" onClick={() => void L.reload()} data-plugins-retry>{t("common:action.retry")}</Button></div></div>
    </Page>
  )
  const { v, mine } = L.data
  return (
    <Page title={title} sub={t("pages:plugins.sub")} back={back} max={720}>
      <div className="flex flex-col gap-8" data-plugins data-plugins-owner={v.owner}>
        {!v.stores.length && <div data-plugins-empty><Empty title={t("pages:plugins.empty")} sub={t(v.owner ? "pages:plugins.emptyOwner" : "pages:plugins.emptyOther")} icon={<PlugIcon />} /></div>}
        {v.stores.map((s) => <StoreSection key={s.id} s={s} v={v} mine={mine} reload={L.reload} onConnect={(p) => setDlg({ k: "connect", s, p })} />)}
        {v.owner ? <AddStore reload={L.reload} /> : v.stores.length > 0 && <p className="px-1 type-footnote text-muted-foreground" data-plugins-owner-only>{t("pages:plugins.ownerOnly")}</p>}
      </div>
      {dlg?.k === "connect" && <ConnectDialog s={dlg.s} p={dlg.p} onClose={() => setDlg(null)} done={async (connection) => { await L.reload(); setDlg({ k: "connected", connection }) }} />}
      {dlg?.k === "connected" && <ConnectedDialog href={accountHref(accountOf(mine, dlg.connection))} onClose={() => setDlg(null)} />}
    </Page>
  )
}

/** one library: its name and when it was last read, then what it offers */
function StoreSection({ s, v, mine, reload, onConnect }: { s: Store; v: View; mine: RAccount[]; reload: () => Promise<void>; onConnect: (p: Plugin) => void }) {
  const t = useT()
  const [busy, act] = useBusy(), [rm, setRm] = useState(false)
  const name = say(s.name) || s.address || s.id
  const unread = s.checked == null
  const refresh = async () => {
    const r = await act(() => pluginsApi.refresh(s.id))
    if (!r) return
    toast(r.error ? t("pages:plugins.readFailed", { error: r.error }) : t("pages:plugins.readDone"))
    await reload()
  }
  const remove = async () => { if (await act(() => pluginsApi.removeStore(s.id), t("pages:plugins.storeRemoved"))) { setRm(false); await reload() } }
  return (
    <section data-plugin-store={s.id} data-store-unread={unread}>
      <SectionLabel right={v.owner && <span className="flex shrink-0 items-center gap-1">
        {/* a library that was never read shows nothing yet: reading it is the one thing to do */}
        <Button size="sm" variant={unread ? "default" : "outline"} disabled={busy} onClick={() => void refresh()} data-store-refresh>{busy ? <Spinner className="size-3.5" /> : <RefreshCwIcon className="size-3.5" />}{unread ? t("pages:plugins.read") : t("common:action.refresh")}</Button>
        <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => setRm(true)} data-store-remove>{t("common:action.remove")}</Button></span>}>
        <span className="inline-flex flex-wrap items-center gap-1.5" data-store-name>{name}{s.official && <Badge variant="outline" className="font-normal" data-store-official>{t("pages:plugins.official")}</Badge>}</span>
      </SectionLabel>
      <div className="mb-2 px-1 type-footnote text-muted-foreground"><Dots>
        {s.address && <span className="break-all font-mono" data-store-address>{s.address}</span>}
        <span data-store-checked>{unread ? t("pages:plugins.neverRead") : t("pages:plugins.readAt", { when: ago(s.checked!) })}</span></Dots></div>
      {(s.error || s.failed) && <p className="mb-2 whitespace-pre-wrap break-words px-1 type-footnote text-destructive" data-store-error>{s.error ? t("pages:plugins.readFailed", { error: s.error }) : t("pages:plugins.readFailedBefore")}</p>}
      {s.plugins.length ? <Group>{s.plugins.map((p) => <PluginRow key={p.id} s={s} p={p} v={v} mine={mine} reload={reload} onConnect={() => onConnect(p)} />)}</Group>
        : <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground" data-store-empty>{t(unread ? "pages:plugins.storeUnread" : "pages:plugins.storeEmpty")}</div>}
      <AlertDialog open={rm} onOpenChange={setRm}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("pages:plugins.removeStoreTitle", { name })}</AlertDialogTitle><AlertDialogDescription>{t("pages:plugins.removeStoreSub")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={busy} onClick={() => void remove()} data-store-remove-confirm>{t("common:action.remove")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </section>
  )
}

/** one plugin of a library, then the caller's own connections through it */
function PluginRow({ s, p, v, mine, reload, onConnect }: { s: Store; p: Plugin; v: View; mine: RAccount[]; reload: () => Promise<void>; onConnect: () => void }) {
  const t = useT()
  const [busy, act] = useBusy(), [rm, setRm] = useState(false)
  const name = say(p.name) || p.id, inst = p.installed
  /* what the library names now can be installed here */
  const fits = p.listed && !p.unsupported && p.available
  const why = p.unsupported ? t("pages:plugins.needsNewer") : !p.listed ? t("pages:plugins.unlisted") : !p.available ? t("pages:plugins.noBuild", { target: v.target }) : ""
  const install = async () => { if (await act(() => pluginsApi.install(s.id, p.id), t(inst ? "pages:plugins.updated" : "pages:plugins.installed", { name }))) await reload() }
  const uninstall = async () => { if (await act(() => pluginsApi.uninstall(s.id, p.id), t("pages:plugins.uninstalled", { name }))) { setRm(false); await reload() } }
  return <>
    <Row icon={<Ico I={PlugIcon} />} data-plugin={p.id} data-plugin-installed={inst?.version ?? ""} data-plugin-update={p.update}
      title={<span className="inline-flex flex-wrap items-center gap-1.5">{name}
        <span className="type-footnote text-muted-foreground"><span className="font-mono" data-plugin-version>{inst?.version ?? p.version}</span> · {p.kind === "channel" ? t("pages:plugins.kindChannel") : p.kind}</span>
        {inst && <Badge variant="outline" className="font-normal">{t("pages:plugins.installedBadge")}</Badge>}
        {p.update && <Badge variant="outline" className="font-normal text-blue">{t("pages:plugins.updateBadge", { version: p.version })}</Badge>}
        {why && <Badge variant="outline" className="h-auto whitespace-normal font-normal text-attention" data-plugin-why={p.unsupported ? "unsupported" : !p.listed ? "unlisted" : "unavailable"}>{why}</Badge>}</span>}
      sub={<Dots><span>{say(p.summary)}</span>
        {p.homepage && /^https?:\/\//.test(p.homepage) && <a href={p.homepage} target="_blank" rel="noreferrer" className="inline-flex items-center gap-0.5 hover:underline" data-plugin-homepage>{t("pages:plugins.homepage")}<ExternalLinkIcon className="size-3" aria-hidden /></a>}</Dots>}>
      <div className="flex flex-wrap items-center justify-end gap-2">
        {v.owner && inst && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => setRm(true)} data-plugin-uninstall>{t("pages:plugins.uninstall")}</Button>}
        {v.owner && fits && (inst ? p.update : true) && <Button size="sm" variant={inst ? "outline" : "default"} disabled={busy} onClick={() => void install()} data-plugin-install>
          {busy && <Spinner className="size-3.5" />}{t(inst ? (busy ? "pages:plugins.updating" : "pages:plugins.update") : busy ? "pages:plugins.installing" : "pages:plugins.install")}</Button>}
        {inst && <Button size="sm" disabled={busy} onClick={onConnect} data-plugin-connect>{t("pages:plugins.connect")}</Button>}
      </div>
    </Row>
    {p.connections.map((c) => { const a = accountOf(mine, c.connection); return (
      <div key={c.instance} data-plugin-connection={c.instance} data-connection-state={c.status.state}>
        <Row className="pl-12" onClick={c.connection ? () => go(accountHref(a)) : undefined}
          title={a ? a.display_name || a.handle : t("pages:plugins.myConnection")}
          sub={<span className="inline-flex items-center gap-1.5"><StatusDot s={DOT[c.status.state] ?? "idle"} />{t(`pages:plugins.state.${DOT[c.status.state] ? c.status.state : "stopped"}`)}</span>}>
          {c.connection && <ChevronRightIcon className="size-4 text-tertiary" />}</Row>
        {c.status.error && <div className="-mt-1.5 max-h-40 overflow-auto whitespace-pre-wrap break-words pb-2.5 pl-12 pr-2 type-footnote text-attention" data-connection-error>{c.status.error}</div>}
      </div>) })}
    <AlertDialog open={rm} onOpenChange={setRm}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("pages:plugins.uninstallTitle", { name })}</AlertDialogTitle><AlertDialogDescription>{t("pages:plugins.uninstallSub")}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={busy} onClick={() => void uninstall()} data-plugin-uninstall-confirm>{t("pages:plugins.uninstall")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </>
}

/** owner only: a library is added by its address; the Hub reads its index first and refuses one it cannot read */
function AddStore({ reload }: { reload: () => Promise<void> }) {
  const t = useT()
  const [address, setAddress] = useState(""), [busy, act] = useBusy()
  const add = async () => { if (await act(() => pluginsApi.addStore(address.trim()), t("pages:plugins.added"))) { setAddress(""); await reload() } }
  return (
    <section data-plugin-add><SectionLabel>{t("pages:plugins.add")}</SectionLabel>
      <form className="flex gap-2" onSubmit={(ev) => { ev.preventDefault(); if (address.trim() && !busy) void add() }}>
        <Input className="rounded-lg font-mono" value={address} maxLength={2000} onChange={(ev) => setAddress(ev.target.value)} placeholder={t("pages:plugins.addPlaceholder")} aria-label={t("pages:plugins.add")} autoComplete="off" spellCheck={false} data-plugin-add-address />
        <Button type="submit" disabled={busy || !address.trim()} data-plugin-add-go>{busy ? <Spinner className="size-4" /> : t("common:action.add")}</Button>
      </form>
      <p className="mt-2 px-1 type-footnote text-muted-foreground">{t("pages:plugins.addSub")}</p>
    </section>
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
  const title = t("pages:plugins.connectTitle", { name: say(p.name) || p.id })
  if (noVault) return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[440px]" data-plugin-connect-dialog="no-vault">
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle><DialogDescription className="type-callout">{t("accounts:connect.noVault")}</DialogDescription></DialogHeader>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button onClick={() => { onClose(); go("#/vault") }} data-go-enable-vault>{t("accounts:connect.enableVault")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
  return (
    <Dialog open onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-plugin-connect-dialog={p.id}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle><DialogDescription className="type-callout">{t("pages:plugins.connectSub")}</DialogDescription></DialogHeader>
        <form className="contents" autoComplete="off" onSubmit={(ev) => { ev.preventDefault(); if (!busy && filled) void submit() }}>
          <FieldGroup className="gap-4">{fields.map((f) => (
            <Field key={f.key}>
              <FieldLabel htmlFor={"pf-" + f.key}>{say(f.label) || f.key}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label={t("accounts:connect.secret")} />}{f.required === false && <span className="ml-1 font-normal text-muted-foreground">{t("accounts:connect.optional")}</span>}</FieldLabel>
              <Input id={"pf-" + f.key} className="rounded-lg font-mono" type={f.secret ? "password" : "text"} autoComplete="off" spellCheck={false} required={f.required !== false} disabled={busy}
                value={vals[f.key] ?? ""} onChange={(ev) => setVals({ ...vals, [f.key]: ev.target.value })} data-plugin-field={f.key} />
              {f.hint && <FieldDescription>{say(f.hint)}</FieldDescription>}
            </Field>))}</FieldGroup>
          <button type="submit" hidden />
        </form>
        {busy && <p className="type-footnote text-muted-foreground" data-plugin-connecting>{t("pages:plugins.connecting")}</p>}
        {err && <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-destructive/10 px-3 py-2 font-sans type-footnote text-destructive" role="alert" data-plugin-connect-error>{err}</pre>}
        <DialogFooter className="gap-2"><Button variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button>
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
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[440px]" data-plugin-connected>
        <DialogHeader><DialogTitle className="type-headline">{t("pages:plugins.connectedTitle")}</DialogTitle><DialogDescription className="type-callout">{t("pages:plugins.connectedSub")}</DialogDescription></DialogHeader>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("pages:plugins.done")}</Button><Button onClick={() => { onClose(); go(href) }} data-plugin-to-accounts>{t("pages:plugins.toAccounts")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
