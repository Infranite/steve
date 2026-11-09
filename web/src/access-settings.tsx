/* 设置 · 访问与客户端 (A4): this page's Hub and client, the access tokens clients sign in with, and every signed-in
 * client of yours with the route it last came by. A token's text is shown once, when it is made. */
import { ago } from "@/i18n/format"
import { t as tr, useT } from "@/i18n"
import { useEffect, useState } from "react"
import { KeyRoundIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { api, modReq } from "@/api"
import { hubBase, setHub } from "@/hub"
import { toast } from "@/store"
import { Group, Row, SectionLabel } from "@/ui"

export const ACCESS_CAT = Object.defineProperties(["access", "", KeyRoundIcon, ""], {
  1: { get: () => tr("pages:access.cat.title"), enumerable: true },
  3: { get: () => tr("pages:access.cat.desc"), enumerable: true },
}) as [string, string, any, string]

type Token = { id: string; name: string; created: number; expires: number | null; last_used: number | null; revoked: number | null }
type Client = { id: string; kind: string; name: string; platform: string; version: string; origin: string | null; created: number; last_seen: number | null; last_route: string | null; last_ip: string | null; revoked: number | null }
type View = { tokens: Token[]; clients: Client[]; current: string | null; route: string | null; hub: { version: string } }
type Oidc = { source: "env" | "settings" | "none"; issuer: string | null; client_id: string | null; has_secret: boolean; callback: string; enabled: boolean }

const access = {
  get: () => modReq<View>("GET", "/access"),
  create: (name: string) => modReq<{ token: string; item: Token }>("POST", "/access/tokens", { name }),
  revokeToken: (id: string) => modReq("DELETE", `/access/tokens/${encodeURIComponent(id)}`),
  revokeClient: (id: string) => modReq("DELETE", `/access/clients/${encodeURIComponent(id)}`),
  oidc: () => modReq<Oidc>("GET", "/access/oidc"),
  saveOidc: (b: { issuer: string; client_id: string; secret?: string }) => modReq<Oidc>("PUT", "/access/oidc", b),
  dropOidc: () => modReq<Oidc>("DELETE", "/access/oidc"),
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
  const [v, setV] = useState<View | null>(null)
  const [error, setError] = useState("")
  const [name, setName] = useState("")
  const [fresh, setFresh] = useState<string | null>(null)
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
  const hub = hubBase() || location.origin
  return <div className="flex flex-col gap-8" data-access-pane>
    <section><SectionLabel>{t("pages:access.here")}</SectionLabel><Group>
      <Row title={t("pages:access.hub")} sub={`${hub} · ${v.hub.version}`}>{hubBase() && <Button size="sm" variant="ghost" onClick={() => { setHub(null); location.reload() }} data-change-hub>{t("pages:access.changeHub")}</Button>}</Row>
      <Row title={t("pages:access.thisClient")} sub={v.current ? routeText(t, v.route) : t("pages:access.browserSession")}>
        <Button size="sm" variant="outline" onClick={async () => { await api.logout().catch(() => {}); location.reload() }} data-sign-out>{t("pages:access.signOut")}</Button>
      </Row>
    </Group></section>

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
