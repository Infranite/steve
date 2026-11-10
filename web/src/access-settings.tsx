/* 设置 · 访问与客户端 (A4): this page's Hub and client, multi-Hub switching, mobile QR pairing, access tokens, and
 * every signed-in client of yours with the route it last came by. A token's text is shown once, when it is made. */
import { ago } from "@/i18n/format"
import { t as tr, useT } from "@/i18n"
import { useEffect, useMemo, useState } from "react"
import { CheckCircle2Icon, GlobeIcon, KeyRoundIcon, QrCodeIcon, RefreshCwIcon, WifiIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { api, modReq } from "@/api"
import { API, forgetHub, hello, hubBase, nativeBridge, needsHttpsForCrossHost, normalize, rememberHub, savedHubs, setHub, type SavedHub } from "@/hub"
import { encodeQr, qrSvgPath } from "@/qr"
import { toast } from "@/store"
import { closeSheet, Group, Row, SectionLabel } from "@/ui"

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
  uri: string
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
    <div className="inline-flex rounded-2xl border border-border bg-white p-3 shadow-xs" data-pair-qr>
      <svg viewBox={rendered.viewBox} width={size} height={size} shapeRendering="crispEdges" role="img" aria-label="QR Code">
        <rect width="100%" height="100%" fill="#ffffff" />
        <path d={rendered.d} fill="#09090b" />
      </svg>
    </div>
  )
}

/** Shared QR pairing body used both inline in AccessSettingsPane and inside MobilePairDialog (⌘K / menu bar). */
function PairContent({ onClaimed }: { onClaimed?: () => void }) {
  const t = useT()
  const [pair, setPair] = useState<ClientPairing | null>(null)
  const [status, setStatus] = useState<PairingStatus | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")

  const generate = async () => {
    setBusy(true)
    setError("")
    try {
      const p = await access.createPairing()
      setPair(p)
      setStatus({ id: p.id, status: "pending", expires: p.expires, client: null })
    } catch (e) {
      setError((e as Error).message)
    }
    setBusy(false)
  }

  useEffect(() => {
    void generate()
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

  if (!pair || busy) {
    return <div className="flex items-center justify-center py-8"><Spinner className="size-5 text-muted-foreground" /></div>
  }

  const endpoints = [
    ...(pair.public_url ? [{ url: pair.public_url, kind: "public" as const }] : []),
    ...pair.lan_urls.map((url) => ({ url, kind: "lan" as const })),
  ]
  const curStatus = status?.status === "claimed" && status.client != null ? "claimed" : status?.status === "expired" ? "expired" : "pending"

  return (
    <div className="flex flex-col items-center gap-4 text-center" data-pair-card>
      {!pair.loopback_only && <QrSvg text={pair.uri} />}

      <div className="flex flex-wrap items-center justify-center gap-2">
        {pair.public_url ? (
          <Badge variant="outline" className="gap-1.5 font-normal" data-pair-route="public">
            <GlobeIcon className="size-3.5 text-primary" />
            {t("pages:access.pairRoutePublic")}
          </Badge>
        ) : !pair.loopback_only ? (
          <Badge variant="outline" className="gap-1.5 font-normal" data-pair-route="lan">
            <WifiIcon className="size-3.5 text-primary" />
            {t("pages:access.pairRouteLan")}
          </Badge>
        ) : null}
      </div>

      {pair.loopback_only && (
        <p className="max-w-sm rounded-lg bg-attention/10 px-3 py-2 type-footnote text-attention" data-pair-loopback>
          {t("pages:access.pairRouteLoopback")}
        </p>
      )}

      {endpoints.length > 0 && (
        <div className="w-full max-w-sm rounded-xl border border-border bg-muted/30 px-3 py-2 text-left">
          <div className="mb-1 type-footnote text-muted-foreground">{t("pages:access.pairEndpoints")}</div>
          <div className="flex flex-col gap-1">
            {endpoints.map((ep) => (
              <div key={ep.url} className="flex items-center justify-between gap-2 font-mono text-[12px]">
                <span className="truncate">{ep.url}</span>
                <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-sans text-[11px] text-muted-foreground">
                  {t(`pages:access.route.${ep.kind}`)}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="flex items-center gap-2 type-footnote" data-pair-status={curStatus}>
        {curStatus === "claimed" ? (
          <span className="inline-flex items-center gap-1.5 font-medium text-success">
            <CheckCircle2Icon className="size-4" />
            {t("pages:access.pairClaimed", { name: status?.client?.name ?? "iOS" })}
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
        {!pair.loopback_only && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void navigator.clipboard?.writeText(pair.uri)
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

/** Modal pairing dialog opened from ⌘K (`openSheet("pairClient")`). */
export function MobilePairDialog() {
  const t = useT()
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="sm:max-w-md" data-mobile-pair-dialog>
        <DialogHeader>
          <DialogTitle>{t("pages:access.pairTitle")}</DialogTitle>
          <DialogDescription>{t("pages:access.pairSub")}</DialogDescription>
        </DialogHeader>
        <PairContent />
        <DialogFooter>
          <Button variant="outline" onClick={() => closeSheet()}>{t("pages:access.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function HubSwitcherSection({ currentVersion }: { currentVersion: string }) {
  const t = useT()
  const bridge = nativeBridge()
  const activeBase = hubBase()
  const activeOrigin = activeBase || location.origin
  const [hubs, setHubs] = useState<SavedHub[]>(() => savedHubs())
  const [newUrl, setNewUrl] = useState("")
  const [newName, setNewName] = useState("")
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    setHubs(rememberHub(activeOrigin, { version: currentVersion }))
  }, [activeOrigin, currentVersion])

  const connectTo = async (rawUrl: string, label?: string) => {
    const base = normalize(rawUrl)
    if (!base) {
      toast(t("shell:app.badAddress"))
      return
    }
    if (!bridge && needsHttpsForCrossHost(base)) {
      toast(t("shell:app.needsHttps"))
      return
    }
    setBusy(true)
    try {
      const h = await hello(base)
      if (h.api !== API) {
        toast(t("shell:app.apiMismatch", { version: h.version }))
        setBusy(false)
        return
      }
      rememberHub(base, { name: label, version: h.version })
      setHub(base === location.origin ? null : base, { name: label, version: h.version })
      location.reload()
    } catch {
      toast(t("shell:app.notSteve"))
      setBusy(false)
    }
  }

  return (
    <section data-hub-switcher>
      <SectionLabel
        right={
          bridge?.openHubSwitcher ? (
            <Button size="xs" variant="outline" onClick={() => bridge.openHubSwitcher?.()} data-native-hub-switcher>
              {t("shell:cmd.switchHub")}
            </Button>
          ) : undefined
        }
      >
        {t("pages:access.hubs")}
      </SectionLabel>
      <p className="mb-3 type-footnote text-muted-foreground">{t("pages:access.hubsSub")}</p>
      <Group>
        {activeBase && (
          <Row title={t("pages:access.defaultHub")} sub={location.origin}>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setHub(null)
                location.reload()
              }}
              data-reset-default-hub
            >
              {t("pages:access.useDefaultHub")}
            </Button>
          </Row>
        )}
        {hubs.map((h) => {
          const isCurrent = h.url === activeOrigin
          return (
            <Row
              key={h.url}
              title={
                <span className="flex items-center gap-2">
                  <span>{h.name || h.url}</span>
                  {isCurrent && (
                    <span className="rounded bg-muted px-1.5 py-px text-[11px] text-muted-foreground">
                      {t("pages:access.current")}
                    </span>
                  )}
                </span>
              }
              sub={`${h.name ? `${h.url} · ` : ""}${h.version ?? ""}${h.lastUsed ? ` · ${ago(h.lastUsed)}` : ""}`}
            >
              {!isCurrent && (
                <div className="flex items-center gap-1.5">
                  <Button size="sm" variant="outline" disabled={busy} onClick={() => void connectTo(h.url, h.name)} data-switch-hub={h.url}>
                    {t("pages:access.switchTo")}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setHubs(forgetHub(h.url))} data-forget-hub={h.url}>
                    {t("pages:access.forgetHub")}
                  </Button>
                </div>
              )}
            </Row>
          )
        })}
        <Row
          title={
            <div className="flex w-full flex-col gap-2 sm:flex-row">
              <Input
                value={newUrl}
                onChange={(e) => setNewUrl(e.target.value)}
                placeholder={t("pages:access.addHubPlaceholder")}
                inputMode="url"
                className="flex-1"
                data-add-hub-url
              />
              <Input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder={t("pages:access.addHubName")}
                className="sm:max-w-44"
                data-add-hub-name
              />
            </div>
          }
        >
          <Button
            size="sm"
            disabled={busy || !newUrl.trim()}
            onClick={() => void connectTo(newUrl, newName)}
            data-add-hub-submit
          >
            {busy ? <Spinner className="size-4" /> : t("pages:access.addAndSwitch")}
          </Button>
        </Row>
      </Group>
    </section>
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
  return <section data-oidc><SectionLabel>{t("pages:access.oidc")}</SectionLabel>
    <p className="mb-3 type-footnote text-muted-foreground">{t("pages:access.oidcSub")}</p>
    <Group>
      <Row title={t("pages:access.oidcCallback")} sub={o.callback} />
      {o.source === "env" ? <Row title={o.issuer ?? ""} sub={t("pages:access.oidcFromEnv")} /> : <>
        <Row title={<Input value={f.issuer} onChange={(e) => setF({ ...f, issuer: e.target.value })} placeholder="https://id.example.com/application/o/steve/" data-oidc-issuer />} />
        <Row title={<Input value={f.client_id} onChange={(e) => setF({ ...f, client_id: e.target.value })} placeholder={t("pages:access.oidcClient")} data-oidc-client />} />
        <Row title={<Input type="password" value={f.secret} onChange={(e) => setF({ ...f, secret: e.target.value })} placeholder={o.has_secret ? t("pages:access.oidcSecretKept") : t("pages:access.oidcSecret")} autoComplete="new-password" data-oidc-secret />}>
          <div className="flex gap-2">{o.source === "settings" && <Button size="sm" variant="ghost" onClick={() => void off()} data-oidc-off>{t("pages:access.oidcOff")}</Button>}
            <Button size="sm" onClick={() => void save()} disabled={busy || !f.issuer.trim() || !f.client_id.trim()} data-oidc-save>{busy ? <Spinner className="size-4" /> : t("pages:access.oidcSave")}</Button></div>
        </Row>
      </>}
    </Group>
  </section>
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
  const bridge = nativeBridge()
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
  if (error) return <p className="type-callout text-destructive">{error}</p>
  if (!v) return <Spinner className="size-5 text-muted-foreground" />
  const live = (x: { revoked: number | null }) => !x.revoked
  const hub = bridge?.activeHub?.endpoint ?? (hubBase() || location.origin)
  return <div className="flex flex-col gap-8" data-access-pane>
    <section><SectionLabel>{t("pages:access.here")}</SectionLabel><Group>
      <Row title={t("pages:access.hub")} sub={`${hub} · ${v.hub.version}`}>
        <div className="flex items-center gap-2">
          {bridge?.openHubSwitcher ? (
            <Button size="sm" variant="outline" onClick={() => bridge.openHubSwitcher?.()} data-change-hub>{t("pages:access.changeHub")}</Button>
          ) : hubBase() ? (
            <Button size="sm" variant="ghost" onClick={() => { setHub(null); location.reload() }} data-change-hub>{t("pages:access.changeHub")}</Button>
          ) : null}
        </div>
      </Row>
      <Row title={t("pages:access.thisClient")} sub={v.current ? routeText(t, v.route) : t("pages:access.browserSession")}>
        <Button size="sm" variant="outline" onClick={async () => { await api.logout().catch(() => {}); location.reload() }} data-sign-out>{t("pages:access.signOut")}</Button>
      </Row>
    </Group></section>

    <section data-mobile-pair-section>
      <SectionLabel>{t("pages:access.pairTitle")}</SectionLabel>
      <p className="mb-3 type-footnote text-muted-foreground">{t("pages:access.pairSub")}</p>
      {showPair ? (
        <div className="rounded-2xl border border-border bg-card p-5">
          <PairContent onClaimed={() => void load()} />
        </div>
      ) : (
        <Group>
          <Row title={t("pages:access.pairTitle")}>
            <Button
              size="sm"
              onClick={() => {
                if (bridge?.openPairSheet) bridge.openPairSheet()
                else setShowPair(true)
              }}
              data-open-pair-qr
            >
              <QrCodeIcon className="size-4" />
              {t("pages:access.pairButton")}
            </Button>
          </Row>
        </Group>
      )}
    </section>

    {!bridge && <HubSwitcherSection currentVersion={v.hub.version} />}

    <section><SectionLabel>{t("pages:access.tokens")}</SectionLabel>
      <p className="mb-3 type-footnote text-muted-foreground">{t("pages:access.tokensSub")}</p>
      {fresh && <div className="mb-3 rounded-xl border border-border p-3" data-fresh-token>
        <div className="type-footnote text-muted-foreground">{t("pages:access.shownOnce")}</div>
        <code className="mt-1 block break-all font-mono text-[13px]">{fresh}</code>
        <div className="mt-2 flex gap-2"><Button size="sm" variant="outline" onClick={() => { void navigator.clipboard?.writeText(fresh); toast(t("pages:access.copied")) }}>{t("pages:access.copy")}</Button><Button size="sm" variant="ghost" onClick={() => setFresh(null)}>{t("pages:access.done")}</Button></div>
      </div>}
      <Group>
        {v.tokens.filter(live).map((x) => <Row key={x.id} title={x.name} sub={t("pages:access.tokenSub", { made: ago(x.created), used: x.last_used ? ago(x.last_used) : t("pages:access.never") })}>
          <Button size="sm" variant="ghost" onClick={() => void revoke(() => access.revokeToken(x.id))} data-revoke-token={x.id}>{t("pages:access.revoke")}</Button></Row>)}
        <Row title={<Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("pages:access.tokenName")} className="max-w-56" data-token-name />}>
          <Button size="sm" onClick={() => void create()} disabled={busy} data-new-token>{busy ? <Spinner className="size-4" /> : t("pages:access.newToken")}</Button></Row>
      </Group>
    </section>

    <OidcSection />

    <section><SectionLabel>{t("pages:access.clients")}</SectionLabel><Group>
      {v.clients.filter(live).map((c) => <Row key={c.id} title={<span className="flex items-center gap-2">{c.name}<span className="type-footnote text-muted-foreground">{c.kind}{c.platform ? ` · ${c.platform}` : ""}</span>{c.id === v.current && <span className="rounded bg-muted px-1.5 py-px text-[11px] text-muted-foreground">{t("pages:access.current")}</span>}</span>}
        sub={`${routeText(t, c.last_route)}${c.last_ip ? ` · ${c.last_ip}` : ""} · ${c.last_seen ? ago(c.last_seen) : t("pages:access.never")}`}>
        {c.id !== v.current && <Button size="sm" variant="ghost" onClick={() => void revoke(() => access.revokeClient(c.id))} data-revoke-client={c.id}>{t("pages:access.signOutClient")}</Button>}</Row>)}
      {!v.clients.some(live) && <Row title={t("pages:access.noClients")} />}
    </Group></section>
  </div>
}
