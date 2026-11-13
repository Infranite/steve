/* 设置 · 访问与客户端 (A4): this page's Hub and client, the list of Hubs, pairing another device, access tokens, and
 * every signed-in client of yours with the route it last came by. A token's text is shown once, when it is made. */
import { ago } from "@/i18n/format"
import { t as tr, useT } from "@/i18n"
import { useEffect, useMemo, useState, type ReactNode } from "react"
import { CheckCircle2Icon, KeyRoundIcon, QrCodeIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { api, modReq } from "@/api"
import { HubChooser, useHubAddress } from "@/hub-ui"
import { encodeQr, qrSvgPath } from "@/qr"
import { toast } from "@/store"
import { closeSheet, Row } from "@/ui"
import { Panel, Section, Card, Notice, Pill, Loading, Tile, Chevron, FieldRow, LIFT } from "@/settings-kit"
import { cn } from "cn"

export const ACCESS_CAT = Object.defineProperties(["access", "", KeyRoundIcon, ""], {
  1: { get: () => tr("pages:access.cat.title"), enumerable: true },
  3: { get: () => tr("pages:access.cat.desc"), enumerable: true },
}) as [string, string, any, string]

type Token = { id: string; name: string; created: number; expires: number | null; last_used: number | null; revoked: number | null }
type Client = { id: string; kind: string; name: string; platform: string; version: string; origin: string | null; created: number; last_seen: number | null; last_route: string | null; last_ip: string | null; revoked: number | null }
type View = { tokens: Token[]; clients: Client[]; current: string | null; route: string | null; hub: { version: string } }
type Oidc = { source: "env" | "settings" | "none"; issuer: string | null; client_id: string | null; has_secret: boolean; callback: string; enabled: boolean }
export type ClientPairing = {
  id: string
  code: string
  name: string
  expires: number
  public_url: string | null
  lan_urls: string[]
  loopback_only: boolean
  /** one pairing link per address of the Hub, the public one first */
  links: { url: string; kind: "public" | "lan"; link: string }[]
}
type PairingStatus = {
  id: string
  status: "pending" | "claimed" | "expired"
  expires: number
  client: { id: string; name: string; kind: string; platform: string } | null
}

const access = {
  get: () => modReq<View>("GET", "/access"),
  create: (name: string) => modReq<{ token: string; item: Token }>("POST", "/access/tokens", { name }),
  revokeToken: (id: string) => modReq("DELETE", `/access/tokens/${encodeURIComponent(id)}`),
  revokeClient: (id: string) => modReq("DELETE", `/access/clients/${encodeURIComponent(id)}`),
  createPairing: () => modReq<ClientPairing>("POST", "/access/pairings", {}),
  pairingStatus: (id: string) => modReq<PairingStatus>("GET", `/access/pairings/${encodeURIComponent(id)}`),
  oidc: () => modReq<Oidc>("GET", "/access/oidc"),
  saveOidc: (b: { issuer: string; client_id: string; secret?: string }) => modReq<Oidc>("PUT", "/access/oidc", b),
  dropOidc: () => modReq<Oidc>("DELETE", "/access/oidc"),
}

function QrSvg({ text, size = 216 }: { text: string; size?: number }) {
  const rendered = useMemo(() => {
    try {
      return qrSvgPath(encodeQr(text), 2)
    } catch {
      return null
    }
  }, [text])
  if (!rendered) return null
  return (
    <div className="inline-flex rounded-xl bg-white p-3 shadow-[0_0_0_1px_rgb(16_24_40/0.06),0_2px_8px_-2px_rgb(16_24_40/0.1)]" data-pair-qr>
      <svg viewBox={rendered.viewBox} width={size} height={size} shapeRendering="crispEdges" role="img" aria-label="QR Code">
        <rect width="100%" height="100%" fill="#ffffff" />
        <path d={rendered.d} fill="#09090b" />
      </svg>
    </div>
  )
}

/** The pairing of another device: the QR code of a pairing link, and the link itself. A phone's camera opens the
 * link in a browser, the Steve app reads the same code. Used in 设置 · 访问与客户端 and in `PairDeviceDialog`. */
function PairContent({ onClaimed, ask = false }: { onClaimed?: () => void; ask?: boolean }) {
  const t = useT()
  const [pair, setPair] = useState<ClientPairing | null>(null)
  /* the address the code is drawn for: a device that cannot reach the first is given another */
  const [at, setAt] = useState(0)
  const [status, setStatus] = useState<PairingStatus | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")

  const generate = async () => {
    setBusy(true)
    setError("")
    try {
      const p = await access.createPairing()
      setPair(p)
      setAt(0)
      setStatus({ id: p.id, status: "pending", expires: p.expires, client: null })
    } catch (e) {
      setError((e as Error).message)
    }
    setBusy(false)
  }

  /* `ask`: the dialog was opened by an address, and an address alone does not make a code that signs a device in */
  const [asked, setAsked] = useState(ask)
  useEffect(() => {
    if (!ask) void generate()
  }, [])

  useEffect(() => {
    if (!pair || (status && status.status !== "pending")) return
    let live = true
    const poll = async () => {
      try {
        const s = await access.pairingStatus(pair.id)
        if (!live) return
        setStatus(s)
        if (s.status === "claimed" && s.client != null) onClaimed?.()
      } catch {
        /* transient poll error */
      }
    }
    const id = setInterval(() => void poll(), 2000)
    return () => {
      live = false
      clearInterval(id)
    }
  }, [pair?.id, status?.status])

  if (error) {
    return (
      <div className="flex flex-col items-start gap-3">
        <p className="type-callout text-destructive">{error}</p>
        <Button size="sm" variant="outline" onClick={() => void generate()}>{t("pages:access.pairRefresh")}</Button>
      </div>
    )
  }

  if (asked && !pair && !busy) {
    return <div className="flex justify-center py-6"><Button onClick={() => { setAsked(false); void generate() }} data-pair-show>{t("pages:access.pairShow")}</Button></div>
  }

  if (!pair || busy) {
    return <div className="flex items-center justify-center py-8"><Spinner className="size-5 text-muted-foreground" /></div>
  }

  const link = pair.links[Math.min(at, pair.links.length - 1)]
  const curStatus = status?.status === "claimed" && status.client != null ? "claimed" : status?.status === "expired" ? "expired" : "pending"

  return (
    <div className="flex flex-col items-center gap-4 text-center" data-pair-card>
      {link && curStatus === "pending" && <QrSvg text={link.link} />}

      {!link && (
        <p className="max-w-sm rounded-xl bg-attention/12 px-4 py-3 type-callout text-foreground/90" data-pair-loopback>
          {t("pages:access.pairRouteLoopback")}
        </p>
      )}

      {link && (
        <div className="w-full max-w-sm rounded-xl bg-muted/60 p-1.5 text-left" data-pair-addresses>
          <div className="mb-1 px-2 pt-1 type-label text-tertiary">{t(pair.links.length > 1 ? "pages:access.pairAddressPick" : "pages:access.pairAddress")}</div>
          <div className="flex flex-col gap-0.5">
            {pair.links.map((l, n) => (
              <button key={l.url} type="button" onClick={() => setAt(n)} aria-pressed={l === link} disabled={pair.links.length < 2}
                className={cn("flex items-center justify-between gap-2 rounded-lg px-2 py-1.5 text-left type-code", pair.links.length > 1 && "cursor-pointer hover:bg-background/70", l === link && pair.links.length > 1 && "bg-card shadow-[var(--lift)]")} data-pair-address={l.url}>
                <span className="truncate">{l.url}</span>
                <Pill className="font-sans">{t(`pages:access.route.${l.kind}`)}</Pill>
              </button>
            ))}
          </div>
        </div>
      )}

      <div className="flex items-center gap-2 type-footnote" data-pair-status={curStatus}>
        {curStatus === "claimed" ? (
          <span className="inline-flex items-center gap-1.5 font-medium text-success">
            <CheckCircle2Icon className="size-4" />
            {t("pages:access.pairClaimed", { name: status?.client?.name ?? "" })}
          </span>
        ) : curStatus === "expired" ? (
          <span className="text-destructive">{t("pages:access.pairExpired")}</span>
        ) : (
          <span className="inline-flex items-center gap-2 text-muted-foreground">
            <Spinner className="size-3.5" />
            {t("pages:access.pairWaiting")}
          </span>
        )}
      </div>

      <div className="flex flex-wrap items-center justify-center gap-2">
        <Button size="sm" variant="outline" onClick={() => void generate()} data-pair-refresh>
          <RefreshCwIcon className="size-3.5" />
          {t("pages:access.pairRefresh")}
        </Button>
        {link && curStatus === "pending" && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void navigator.clipboard?.writeText(link.link)
              toast(t("pages:access.copied"))
            }}
            data-pair-copy
          >
            {t("pages:access.pairCopyUri")}
          </Button>
        )}
      </div>
    </div>
  )
}

/** the pairing in a dialog: `openSheet("pairClient")`, from the sidebar's Hub switcher, ⌘K, 我的, and `#/hub/pair`
 * (`ask`: opened by an address, so the code is made on a click) */
export function PairDeviceDialog({ ask }: { ask?: boolean }) {
  const t = useT()
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="sm:max-w-md" data-pair-device-dialog>
        <DialogHeader>
          <DialogTitle>{t("pages:access.pairTitle")}</DialogTitle>
          <DialogDescription>{t("pages:access.pairSub")}</DialogDescription>
        </DialogHeader>
        <PairContent ask={ask} />
        <DialogFooter>
          <Button variant="outline" onClick={() => closeSheet()}>{t("pages:access.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** single sign-on (owner only): from the environment it is shown, from the settings it can be set and turned off */
function OidcSection() {
  const t = useT()
  const [o, setO] = useState<Oidc | null>(null)
  const [denied, setDenied] = useState(false)
  const [f, setF] = useState({ issuer: "", client_id: "", secret: "" })
  const [busy, setBusy] = useState(false)
  useEffect(() => { access.oidc().then((x) => { setO(x); setF({ issuer: x.issuer ?? "", client_id: x.client_id ?? "", secret: "" }) }, () => setDenied(true)) }, [])
  if (denied || !o) return null
  const save = async () => {
    setBusy(true)
    try { const x = await access.saveOidc({ issuer: f.issuer.trim(), client_id: f.client_id.trim(), ...(f.secret.trim() ? { secret: f.secret.trim() } : {}) }); setO(x); setF((v) => ({ ...v, secret: "" })); toast(t("pages:access.oidcSaved")) }
    catch (e) { toast((e as Error).message) }
    setBusy(false)
  }
  const off = async () => { try { const x = await access.dropOidc(); setO(x); setF({ issuer: "", client_id: "", secret: "" }) } catch (e) { toast((e as Error).message) } }
  /* a field with its label on the left (stacked on a phone), as a row of the panel */
  const field = (label: string, input: ReactNode) => <label className="grid gap-1.5 px-3 py-2.5 sm:grid-cols-[9rem_minmax(0,1fr)] sm:items-center sm:gap-3">
    <span className="type-callout text-muted-foreground">{label}</span>{input}</label>
  return <Section data-oidc label={t("pages:access.oidc")} note={t("pages:access.oidcSub")}>
    <Panel>
      <Row title={t("pages:access.oidcCallback")} sub={<span className="type-code break-all">{o.callback}</span>} />
      {o.source === "env" ? <Row title={o.issuer ?? ""} sub={t("pages:access.oidcFromEnv")} /> : <>
        {field("Issuer", <Input className="h-8" value={f.issuer} onChange={(e) => setF({ ...f, issuer: e.target.value })} placeholder="https://id.example.com/application/o/steve/" data-oidc-issuer />)}
        {field(t("pages:access.oidcClient"), <Input className="h-8" value={f.client_id} onChange={(e) => setF({ ...f, client_id: e.target.value })} placeholder={t("pages:access.oidcClient")} data-oidc-client />)}
        {field(t("pages:access.oidcSecret"), <Input className="h-8" type="password" value={f.secret} onChange={(e) => setF({ ...f, secret: e.target.value })} placeholder={o.has_secret ? t("pages:access.oidcSecretKept") : t("pages:access.oidcSecret")} autoComplete="new-password" data-oidc-secret />)}
        <div className="flex justify-end gap-2 px-3 py-2.5">{o.source === "settings" && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => void off()} data-oidc-off>{t("pages:access.oidcOff")}</Button>}
          <Button size="sm" variant="outline" onClick={() => void save()} disabled={busy || !f.issuer.trim() || !f.client_id.trim()} data-oidc-save>{busy ? <Spinner className="size-4" /> : t("pages:access.oidcSave")}</Button></div>
      </>}
    </Panel>
  </Section>
}

/** a route as words: “局域网 · TLS” */
const routeText = (t: ReturnType<typeof useT>, r: string | null) => {
  if (!r) return t("pages:access.route.unknown")
  const [base, tls] = r.split("+")
  const known = ["loopback", "lan", "tailnet", "proxy", "public"].includes(base)
  return (known ? t(`pages:access.route.${base as "lan"}`) : base) + (tls ? " · TLS" : "")
}

export function AccessSettingsPane() {
  const t = useT()
  const hub = useHubAddress()
  const [v, setV] = useState<View | null>(null)
  const [error, setError] = useState("")
  const [name, setName] = useState("")
  const [fresh, setFresh] = useState<string | null>(null)
  const [showPair, setShowPair] = useState(false)
  const [busy, setBusy] = useState(false)
  const load = () => access.get().then(setV, (e: Error) => setError(e.message))
  useEffect(() => { void load() }, [])
  const create = async () => {
    setBusy(true)
    try { const r = await access.create(name.trim()); setFresh(r.token); setName(""); await load() }
    catch (e) { toast((e as Error).message) }
    setBusy(false)
  }
  const revoke = async (f: () => Promise<unknown>) => { try { await f(); await load() } catch (e) { toast((e as Error).message) } }
  if (error) return <Notice tone="destructive">{error}</Notice>
  if (!v) return <Loading />
  const live = (x: { revoked: number | null }) => !x.revoked
  return <div className="flex flex-col gap-8" data-access-pane>
    <Section label={t("pages:access.here")}><Panel>
      <Row title={t("pages:access.hub")} sub={[hub, v.hub.version].filter(Boolean).join(" · ")} data-access-hub />
      <Row title={t("pages:access.thisClient")} sub={v.current ? routeText(t, v.route) : t("pages:access.browserSession")}>
        <Button size="sm" variant="outline" onClick={async () => { await api.logout().catch(() => {}); location.reload() }} data-sign-out>{t("pages:access.signOut")}</Button>
      </Row>
    </Panel></Section>

    <Section data-hub-switcher label={t("pages:access.hubs")} note={t("pages:access.hubsSub")}>
      {/* the shared chooser, laid into this page's panel: its list as rows with hairlines between them, and the
        * address, the name and 连接 on one line under them */}
      <div className={cn(LIFT, "p-1 [&_[data-hub-chooser]]:gap-0 [&_[data-hub-list]]:rounded-none [&_[data-hub-list]]:border-0 [&_[data-hub-row]]:mx-2 [&_[data-hub-row]]:border-border [&_[data-hub-row]]:px-0 [&_[data-hub-row]:last-child]:border-b [&_[data-add-hub]]:grid [&_[data-add-hub]]:gap-2 [&_[data-add-hub]]:px-3 [&_[data-add-hub]]:py-3 sm:[&_[data-add-hub]]:grid-cols-[minmax(0,3fr)_minmax(0,2fr)_auto] [&_[data-add-hub]>div:not(:has(button))]:col-span-full [&_[data-add-hub]_[data-slot=input]]:h-8 [&_[data-add-hub-submit]]:h-8 [&_[data-add-hub-submit]]:flex-none [&_[data-add-hub-submit]]:px-3")}>
        <HubChooser manage />
      </div>
    </Section>

    <Section data-pair-device-section label={t("pages:access.pairTitle")} note={showPair ? undefined : t("pages:access.pairSub")}>
      {showPair ? (
        <Card className="p-5 anim-in">
          <PairContent onClaimed={() => void load()} />
        </Card>
      ) : (
        <Panel><Row icon={<Tile I={QrCodeIcon} />} title={t("pages:access.pairButton")} onClick={() => setShowPair(true)} data-open-pair-qr><Chevron /></Row></Panel>
      )}
    </Section>

    <Section label={t("pages:access.tokens")} note={t("pages:access.tokensSub")}>
      {fresh && <div className="mb-3 rounded-xl bg-success/10 px-4 py-3 anim-in" data-fresh-token>
        <div className="type-footnote text-muted-foreground">{t("pages:access.shownOnce")}</div>
        <code className="mt-1 block break-all type-code text-foreground">{fresh}</code>
        <div className="mt-2 flex gap-2"><Button size="sm" variant="outline" onClick={() => { void navigator.clipboard?.writeText(fresh); toast(t("pages:access.copied")) }}>{t("pages:access.copy")}</Button><Button size="sm" variant="ghost" onClick={() => setFresh(null)}>{t("pages:access.done")}</Button></div>
      </div>}
      <Panel>
        {v.tokens.filter(live).map((x) => <Row key={x.id} title={x.name} sub={t("pages:access.tokenSub", { made: ago(x.created), used: x.last_used ? ago(x.last_used) : t("pages:access.never") })}>
          <Button size="sm" variant="ghost" onClick={() => void revoke(() => access.revokeToken(x.id))} data-revoke-token={x.id}>{t("pages:access.revoke")}</Button></Row>)}
        <FieldRow onSubmit={(e) => { e.preventDefault(); void create() }}><Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("pages:access.tokenName")} data-token-name />
          <Button size="sm" variant="outline" type="submit" disabled={busy} data-new-token>{busy ? <Spinner className="size-4" /> : t("pages:access.newToken")}</Button></FieldRow>
      </Panel>
    </Section>

    <OidcSection />

    <Section label={t("pages:access.clients")}><Panel>
      {v.clients.filter(live).map((c) => <Row key={c.id} className="group/client" title={<span className="flex items-center gap-2">{c.name}<span className="type-footnote text-muted-foreground">{c.kind}{c.platform ? ` · ${c.platform}` : ""}</span>{c.id === v.current && <Pill tone="info">{t("pages:access.current")}</Pill>}</span>}
        sub={`${routeText(t, c.last_route)}${c.last_ip ? ` · ${c.last_ip}` : ""} · ${c.last_seen ? ago(c.last_seen) : t("pages:access.never")}`}>
        {c.id !== v.current && <Button size="sm" variant="ghost" className="text-muted-foreground opacity-0 group-hover/client:opacity-100 group-focus-within/client:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100" onClick={() => void revoke(() => access.revokeClient(c.id))} data-revoke-client={c.id}>{t("pages:access.signOutClient")}</Button>}</Row>)}
      {!v.clients.some(live) && <Row title={t("pages:access.noClients")} />}
    </Panel></Section>
  </div>
}
