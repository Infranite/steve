/* 账号 (accounts-vault.md §8, prototype people.tsx AccountsView / AccountDetail): your and your company's accounts, grouped
 * by system; connections to Steve with their health, capabilities, re-authorization, pause and disconnect. */
import { useEffect, useMemo, useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlugIcon, CloudIcon, CodeIcon, CalendarIcon, CreditCardIcon, LockIcon, MailIcon, MessageSquareIcon, PhoneIcon, PlusIcon, RefreshCwIcon, KeyRoundIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { go, Page, Group, Row, SectionLabel, Meta, Empty, StatusDot } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { tm, toast } from "@/store"
import { accountsApi, capLabel, kindLabel, KIND_LABEL, CONN_STATUS, type RAccount, type RAdapter, type RConnection, type RMatchRule, type RUsage } from "@/api/accounts"
import { useLoad, useBusy, report, Dots } from "@/accounts/load"
import { NewAccountDialog, ConnectDialog, CredentialDialog } from "@/accounts/dialogs"
import { NewVaultDialog } from "@/vault/new-item"
import { MailSettingsSection } from "@/mail/settings"; import { MAIL_ADAPTERS } from "@/api/mail"
import { useT } from "@/i18n"
import { num } from "@/i18n/format"

const SYS_I: Record<string, any> = { comm: MailIcon, code: CodeIcon, cloud: CloudIcon, finance: CreditCardIcon, calendar: CalendarIcon }
const iconOf = (a: { system: { id: string; kind: string } }) => a.system.id === "phone" ? PhoneIcon : /feishu|slack/.test(a.system.id) ? MessageSquareIcon : SYS_I[a.system.kind] ?? KeyRoundIcon
export const Ico = ({ I, className }: { I: any; className?: string }) => <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground", className)} aria-hidden><I className="size-4" strokeWidth={1.5} /></span>
const Section = ({ label, right, children }: { label: string; right?: ReactNode; children: ReactNode }) => <section><SectionLabel right={right}>{label}</SectionLabel>{children}</section>
const Note = ({ children }: { children: ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>
const Loading = ({ title, back }: { title: string; back?: [string, string] }) => <Page title={title} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
const Failed = ({ title, err, back }: { title: string; err: string; back?: [string, string] }) => <Page title={title} back={back}><Empty title={err} /></Page>

export function AcctRow({ a, showOwner }: { a: RAccount; showOwner?: boolean }) {
  const t = useT()
  const c = a.connection
  return (
    <Row icon={<Ico I={iconOf(a)} />} onClick={() => go("#/accounts/" + a.id)} data-acct-row={a.handle}
      title={<span className="inline-flex flex-wrap items-center gap-1.5">{a.display_name || a.handle}<span className="type-footnote text-muted-foreground">{a.system.name} · {kindLabel(a.kind)}</span>{!a.verified && <Badge variant="outline" className="font-normal text-attention">{t("accounts:row.unverified")}</Badge>}</span>}
      sub={<Dots>{a.handle !== a.display_name && <span className="font-mono">{a.handle}</span>}{showOwner && !a.party.is_me && <span>{a.party.name}</span>}
        {c ? <span>{c.caps.length ? t("accounts:row.connectedCaps", { caps: c.caps.map(capLabel).join(t("accounts:listSep")) }) : t("accounts:row.connected")}</span> : <span>{t("accounts:row.source", { source: a.source_label || a.source })}</span>}
        {a.vault_items.length > 0 && <span className="inline-flex items-center gap-1"><LockIcon className="size-3" aria-hidden />{t("accounts:row.vaultItems", { count: a.vault_items.length })}</span>}</Dots>}>
      {c && c.status !== "ok" && <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={CONN_STATUS[c.status]?.[1]} />{CONN_STATUS[c.status]?.[0]}</span>}
      <ChevronRightIcon className="size-4 text-tertiary" /></Row>
  )
}
/** rows grouped by system: one hairline group per system, its name above when there is more than one */
function BySystem({ xs, empty, showOwner }: { xs: RAccount[]; empty: string; showOwner?: boolean }) {
  const groups = useMemo(() => { const m = new Map<string, RAccount[]>(); for (const a of [...xs].sort((x, y) => x.system.name.localeCompare(y.system.name))) m.set(a.system.name, [...(m.get(a.system.name) ?? []), a]); return [...m] }, [xs])
  if (!xs.length) return <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground">{empty}</div>
  if (groups.length === 1) return <Group>{xs.map((a) => <AcctRow key={a.id} a={a} showOwner={showOwner} />)}</Group>
  return <div className="flex flex-col gap-3">{groups.map(([n, g]) => <div key={n} data-system-group={n}><div className="mb-0.5 px-1 type-footnote text-muted-foreground">{n}</div><Group>{g.map((a) => <AcctRow key={a.id} a={a} showOwner={showOwner} />)}</Group></div>)}</div>
}

/** `#/accounts`, `#/accounts/:id`, `#/accounts/:id/reauth`; `#/accounts?error=…` is the OAuth callback's failure */
export function AccountsRoute({ route, id, sub }: { route: string; id?: string; sub?: string }) {
  useEffect(() => {
    const m = /\?(.*)$/.exec(location.hash); if (!m || !route.includes("?")) return
    const err = new URLSearchParams(m[1]).get("error"); if (err) toast(err)
    history.replaceState(null, "", location.pathname + "#/accounts")
    window.dispatchEvent(new HashChangeEvent("hashchange"))
  }, [route])
  /* `/reauth` (from the 待处理 card) is consumed once: the page is re-entered without it, then the connection starts it */
  useEffect(() => { if (id && sub === "reauth") { reauthFor = id; history.replaceState(null, "", location.pathname + "#/accounts/" + id); window.dispatchEvent(new HashChangeEvent("hashchange")) } }, [id, sub])
  if (id && sub === "reauth") return null
  return id ? <AccountDetail key={id} id={id} /> : <AccountsView />
}
let reauthFor: string | null = null

function AccountsView() {
  const t = useT()
  const L = useLoad(async () => {
    const [mine, company, rules, systems] = await Promise.all([accountsApi.accounts({ scope: "mine" }), accountsApi.accounts({ scope: "company" }), accountsApi.rules(), accountsApi.systems()])
    const adapters = await accountsApi.adapters().catch(() => [] as RAdapter[]) // no adapters: only "record an account"
    return { mine, company, rules, systems, adapters }
  }, [], ["account", "connection"])
  const [dlg, setDlg] = useState<{ k: "new" } | { k: "connect"; a: RAdapter } | null>(null)
  if (L.s === "unwired") return <Unwired id="accounts" />
  if (L.s === "loading") return <Loading title={t("accounts:page.title")} />
  if (L.s === "error") return <Failed title={t("accounts:page.title")} err={L.err} />
  const { mine, company, rules, systems, adapters } = L.data
  const conn = mine.filter((a) => a.connection), rest = mine.filter((a) => !a.connection)
  const offer = adapters.filter((a) => a.auth === "oauth" || a.auth === "credential")
  return (
    <Page title={t("accounts:page.title")} sub={t("accounts:page.sub")} max={760}
      actions={<><Button size="sm" variant="outline" onClick={() => go("#/vault")} data-open-vault><LockIcon />{t("accounts:page.vault")}</Button>
        <DropdownMenu><DropdownMenuTrigger render={<Button size="sm" data-add-account-menu />}><PlusIcon />{t("accounts:page.addAccount")}</DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-56">
            {offer.map((a) => <DropdownMenuItem key={a.id} onClick={() => setDlg({ k: "connect", a })} data-add-adapter={a.id}>{a.name}</DropdownMenuItem>)}
            {offer.length > 0 && <DropdownMenuSeparator />}
            <DropdownMenuItem onClick={() => setDlg({ k: "new" })} data-add-plain>{t("accounts:page.addPlain")}</DropdownMenuItem>
          </DropdownMenuContent></DropdownMenu></>}>
      {dlg?.k === "new" && <NewAccountDialog systems={systems} onClose={() => setDlg(null)} onDone={(id) => { setDlg(null); go("#/accounts/" + id) }} />}
      {dlg?.k === "connect" && <ConnectDialog adapter={dlg.a} onClose={() => setDlg(null)} />}
      <div className="flex flex-col gap-8">
        <Section label={t("accounts:page.connectedSection", { n: num(conn.length) })}><BySystem xs={conn} empty={t("accounts:page.connectedEmpty")} /></Section>
        <Group><Row icon={<Ico I={PlugIcon} />} title={t("accounts:page.integrations")} sub={t("accounts:page.integrationsSub")} onClick={() => go("#/integrations")} data-goto-integrations><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>
        <Section label={t("accounts:page.companySection", { n: num(company.length) })}><BySystem xs={company} empty={t("accounts:page.companyEmpty")} />
          <Note>{t("accounts:page.companyNote")}</Note></Section>
        <Section label={t("accounts:page.otherSection", { n: num(rest.length) })}><BySystem xs={rest} empty={t("accounts:page.otherEmpty")} />
          <Note>{t("accounts:page.otherNote")}</Note></Section>
        <MatchRules rules={rules} onChange={L.reload} />
      </div>
    </Page>
  )
}

function MatchRules({ rules, onChange }: { rules: RMatchRule[]; onChange: () => void }) {
  const t = useT()
  const r = rules.find((x) => x.kind === "email_prefix")
  const [busy, act] = useBusy(), [edit, setEdit] = useState<string | null>(null)
  if (!r) return null
  const save = async (b: Parameters<typeof accountsApi.patchRule>[1]) => { if (await act(() => accountsApi.patchRule(r.id, b))) onChange() }
  return (
    <Section label={t("accounts:rules.title")}>
      <Group>
        <Row title={t("accounts:rules.prefix")} sub={t("accounts:rules.prefixSub")}><Switch aria-label={t("accounts:rules.prefix")} checked={r.enabled} disabled={busy} onCheckedChange={(v) => void save({ enabled: v })} data-prefix-rule /></Row>
        <Row title={t("accounts:rules.sameOrg")} sub={t("accounts:rules.sameOrgSub")}><Switch aria-label={t("accounts:rules.sameOrg")} checked={r.config.same_org_only} disabled={!r.enabled || busy} onCheckedChange={(v) => void save({ config: { same_org_only: v } })} /></Row>
        {edit === null
          ? <Row title={t("accounts:rules.exclude")} sub={r.config.exclude.join(t("accounts:listSep")) || t("accounts:rules.excludeNone")}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEdit(r.config.exclude.join(", "))} data-edit-exclude>{t("common:action.edit")}</Button></Row>
          : <form className="flex items-center gap-2 px-2 py-2" onSubmit={(e) => { e.preventDefault(); const xs = edit.split(/[,，、\s]+/).map((x) => x.trim()).filter(Boolean).slice(0, 50); void save({ config: { exclude: xs } }).then(() => setEdit(null)) }}>
            <Input aria-label={t("accounts:rules.excludeInput")} autoFocus className="h-8 flex-1 rounded-lg" value={edit} onChange={(e) => setEdit(e.target.value)} data-exclude-input />
            <Button size="sm" variant="ghost" type="button" onClick={() => setEdit(null)}>{t("common:action.cancel")}</Button><Button size="sm" type="submit" disabled={busy}>{t("common:action.save")}</Button></form>}
        <Row title={t("accounts:rules.minLen")} sub={t("accounts:rules.minLenSub")}><Select align="end" testId="min-len" value={String(r.config.min_len)} onChange={(v) => void save({ config: { min_len: +v } })} options={Array.from({ length: 10 }, (_, n) => ({ v: String(n + 1), l: t("accounts:rules.chars", { count: n + 1 }) }))} /></Row>
      </Group>
      <Note>{t("accounts:rules.note")}</Note>
    </Section>
  )
}

/* ═════════ 账号详情 ═════════ */
function AccountDetail({ id }: { id: string }) {
  const t = useT()
  const L = useLoad(async () => {
    const a = await accountsApi.account(id)
    const [c, adapters] = await Promise.all([a.connection ? accountsApi.connection(a.connection.id) : Promise.resolve(null), a.connection ? accountsApi.adapters().catch(() => [] as RAdapter[]) : Promise.resolve([] as RAdapter[])])
    return { a, c, adapter: c ? adapters.find((x) => x.id === c.adapter) ?? null : null }
  }, [id], ["account", "connection", "vault"])
  const [rm, setRm] = useState(false), [edit, setEdit] = useState(false), [newItem, setNewItem] = useState(false), [busy, act] = useBusy()
  if (L.s === "unwired") return <Unwired id="accounts" />
  if (L.s === "loading") return <Loading title={t("accounts:page.title")} back={["#/accounts", t("accounts:page.title")]} />
  if (L.s === "error") return <Failed title={t("accounts:page.title")} err={L.status === 404 ? t("accounts:detail.notFound") : L.err} back={["#/accounts", t("accounts:page.title")]} />
  const { a, c, adapter } = L.data
  const mine = a.party.is_me || a.party.type === "org" || a.party.type === "team"
  const back: [string, string] = mine ? ["#/accounts", t("accounts:page.title")] : ["#/contacts/" + a.party.id, a.party.name]
  return (
    <Page title={a.display_name || a.handle} back={back} max={680} sub={<Meta><span>{a.system.name}</span><span>{kindLabel(a.kind)}</span><span className="font-mono">{a.handle}</span></Meta>}
      actions={<><Button size="sm" variant="ghost" onClick={() => setEdit(true)} data-edit-account>{t("common:action.edit")}</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setRm(true)} data-rm-plain>{t("common:action.remove")}</Button></>}>
      <AlertDialog open={rm} onOpenChange={setRm}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("accounts:detail.rmTitle")}</AlertDialogTitle><AlertDialogDescription>{c ? t("accounts:detail.rmConnected") : a.vault_items.length ? t("accounts:detail.rmRecordVault", { count: a.vault_items.length }) : t("accounts:detail.rmRecord")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel>{!c && <AlertDialogAction variant="destructive" onClick={async () => { if (await act(() => accountsApi.deleteAccount(a.id), t("accounts:detail.removed"))) go(back[0]) }} data-rm-confirm>{t("common:action.remove")}</AlertDialogAction>}</AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditAccount a={a} onClose={() => setEdit(false)} onDone={L.reload} />}
      {newItem && <NewVaultDialog account={{ id: a.id, display_name: a.display_name || a.handle }} onClose={() => setNewItem(false)} />}
      <div className="flex flex-col gap-8">
        <Section label={t("accounts:detail.owner")}><Group><Row title={a.party.name} sub={t(a.verified ? "accounts:detail.verifiedSource" : "accounts:detail.unverifiedSource", { source: a.source_label || a.source })} onClick={() => go("#/contacts/" + a.party.id)}>
          {!a.verified && <Button size="xs" variant="outline" disabled={busy} onClick={(e) => { e.stopPropagation(); void act(() => accountsApi.patchAccount(a.id, { verified: true }), t("accounts:detail.verified")).then((r) => r && L.reload()) }} data-verify-account>{t("common:action.confirm")}</Button>}
          <ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></Section>
        {mine && c && MAIL_ADAPTERS.includes(c.adapter) && <MailSettingsSection connection={c.id} />}
        {mine && <Section label={t("accounts:detail.connection")}>{c ? <Connection c={c} adapter={adapter} label={a.display_name || a.handle} onChange={L.reload} />
          : <div className="rounded-xl border border-dashed px-4 py-4 type-callout text-muted-foreground">{a.system.kind === "finance" ? t("accounts:detail.notConnectedFinance") : t("accounts:detail.notConnected")}</div>}</Section>}
        {mine && <Section label={t("accounts:detail.vaultSection", { n: num(a.vault_items.length) })} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewItem(true)} data-new-vault-for-account><PlusIcon />{t("accounts:detail.newVaultItem")}</Button>}>
          {a.vault_items.length ? <Group>{a.vault_items.map((v) => <Row key={v.id} icon={<Ico I={KeyRoundIcon} />} title={v.title} onClick={() => go("#/vault/" + v.id)} data-vault-row={v.title}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <Empty title={t("accounts:detail.vaultEmpty")} sub={t("accounts:detail.vaultEmptySub")} />}</Section>}
      </div>
    </Page>
  )
}

function EditAccount({ a, onClose, onDone }: { a: RAccount; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [n, setN] = useState(a.display_name), [k, setK] = useState(a.kind), [busy, act] = useBusy()
  const save = async () => { if (await act(() => accountsApi.patchAccount(a.id, { display_name: n.trim(), kind: k }), t("common:state.saved"))) { onDone(); onClose() } }
  return (
    <AlertDialog open onOpenChange={(o) => !o && onClose()}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("accounts:edit.title")}</AlertDialogTitle><AlertDialogDescription>{a.system.name} · <span className="font-mono">{a.handle}</span></AlertDialogDescription></AlertDialogHeader>
      <form className="flex flex-col gap-3" onSubmit={(e) => { e.preventDefault(); void save() }}>
        <label className="flex flex-col gap-1.5 type-callout">{t("accounts:edit.displayName")}<Input className="rounded-lg" value={n} maxLength={80} onChange={(e) => setN(e.target.value)} data-edit-name /></label>
        <div className="flex items-center justify-between type-callout">{t("accounts:edit.kind")}<Select align="end" value={k} onChange={setK} options={Object.entries(KIND_LABEL).map(([v, l]) => ({ v, l }))} /></div>
      </form>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction disabled={busy} onClick={(e) => { e.preventDefault(); void save() }} data-edit-save>{t("common:action.save")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  )
}

function Connection({ c, adapter, label, onChange }: { c: RConnection; adapter: RAdapter | null; label: string; onChange: () => void }) {
  const t = useT()
  const [busy, act] = useBusy(), [cred, setCred] = useState<string[] | null>(null), [usage, setUsage] = useState<RUsage | null>(null), [asking, setAsking] = useState(false)
  const [s, dot] = CONN_STATUS[c.status] ?? [c.status, "idle"]
  const reauth = async () => {
    const r = await act(() => accountsApi.reauth(c.id))
    if (!r) return
    if ("url" in r) location.href = r.url
    else setCred(r.fields)
  }
  /* from the 待处理 card: “重新授权” lands here with /reauth and goes on directly */
  useEffect(() => { if (reauthFor === c.account_id) { reauthFor = null; void reauth() } }, [])
  const askDisconnect = async () => { setAsking(true); try { setUsage(await accountsApi.usage(c.id)) } catch (e) { report(e) } finally { setAsking(false) } }
  const used = usage ? [...usage.flows.map((f) => t("accounts:conn.usedFlow", { name: f.name })), ...usage.sources.map((x) => t("accounts:conn.usedSource", { name: x.name })), ...(usage.spaces ? [t("accounts:conn.usedSpaces", { count: usage.spaces })] : [])] : []
  const toggleCap = (cap: string, on: boolean) => void act(() => accountsApi.patchConnection(c.id, { caps: on ? [...c.caps, cap] : c.caps.filter((x) => x !== cap) })).then((r) => r && onChange())
  return (
    <div className="flex flex-col gap-4" data-connection={c.id} data-connection-status={c.status}>
      {cred && adapter && <CredentialDialog adapter={adapter} connection={c.id} only={cred} onClose={() => setCred(null)} onDone={onChange} />}
      <AlertDialog open={!!usage} onOpenChange={(o) => !o && setUsage(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("accounts:conn.disconnectTitle", { label })}</AlertDialogTitle><AlertDialogDescription>{used.length ? t("accounts:conn.disconnectDesc") : t("accounts:conn.disconnectDescUnused")}</AlertDialogDescription></AlertDialogHeader>
        {used.length > 0 && <div className="type-callout" data-usage><div className="mb-1.5 text-muted-foreground">{t("accounts:conn.willStop")}</div><ul className="list-disc pl-5">{used.map((u) => <li key={u}>{u}</li>)}</ul></div>}
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={async () => { if (await act(() => accountsApi.disconnect(c.id), t("accounts:conn.disconnected"))) onChange() }} data-disconnect-confirm>{t("accounts:conn.disconnect")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <Group>
        <Row title={<span className="inline-flex items-center gap-2"><StatusDot s={dot} />{s}</span>} data-conn-state
          sub={<Dots><span>{adapter?.name ?? c.adapter}</span>{c.last_sync ? <span>{t("accounts:conn.lastSync", { time: tm(c.last_sync) })}</span> : null}{c.health.last_ok ? <span>{t("accounts:conn.lastOk", { time: tm(c.health.last_ok) })}</span> : null}</Dots>}>
          <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => accountsApi.test(c.id)).then((r) => { if (r) { toast(r.status === "ok" ? t("accounts:conn.testOk") : t("accounts:conn.testFailed")); onChange() } })} data-conn-test><RefreshCwIcon />{t("accounts:conn.test")}</Button></Row>
        {c.health.last_error && c.status !== "ok" && <Row title={t("accounts:conn.lastError")} sub={<span className="font-mono">{c.health.last_error}</span>}>{c.health.error_at ? <span className="type-footnote text-muted-foreground">{tm(c.health.error_at)}</span> : null}</Row>}
        {(c.status === "reauth" || c.status === "error") && <Row title={t("accounts:conn.reauth")} sub={c.auth === "oauth" ? t("accounts:conn.reauthOAuth") : t("accounts:conn.reauthCred")}><Button size="sm" disabled={busy} onClick={() => void reauth()} data-reauth>{t("accounts:conn.reauth")}</Button></Row>}
        <Row title={t("accounts:conn.receiving")} sub={c.status === "paused" ? t("accounts:conn.pausedSub") : t("accounts:conn.receivingSub")}>
          <Switch aria-label={t("accounts:conn.receiving")} checked={c.status !== "paused"} disabled={busy || c.status === "revoked"} onCheckedChange={(v) => void act(() => accountsApi.patchConnection(c.id, { paused: !v }), v ? t("accounts:conn.resumed") : t("accounts:conn.paused")).then((r) => r && onChange())} data-conn-pause /></Row>
      </Group>
      {c.available_caps.length > 0 && <div><div className="mb-1 type-footnote text-muted-foreground">{t("accounts:conn.allow")}</div><Group>{c.available_caps.map((cap) => (
        <Row key={cap} title={capLabel(cap)}><Switch aria-label={capLabel(cap)} checked={c.caps.includes(cap)} disabled={busy} onCheckedChange={(v) => toggleCap(cap, v)} data-cap-toggle={cap} /></Row>))}</Group></div>}
      {c.caps.includes("msg.receive") && <Group><Row title={t("accounts:conn.identity")} sub={t("accounts:conn.identitySub")} onClick={() => go("#/channels/acc/" + c.account_id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>}
      {c.auth === "credential" && c.status === "ok" && adapter?.fields?.some((f) => f.secret) && <Button size="sm" variant="ghost" className="self-start text-muted-foreground" disabled={busy} onClick={() => setCred(adapter.fields!.filter((f) => f.secret).map((f) => f.key))} data-replace-cred>{t("accounts:conn.replaceCred")}</Button>}
      <Button size="sm" variant="ghost" className="self-start text-destructive" disabled={asking || busy} onClick={() => void askDisconnect()} data-disconnect>{asking && <Spinner />}{t("accounts:conn.disconnectAction")}</Button>
    </div>
  )
}
