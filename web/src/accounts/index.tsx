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
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { tm, toast } from "@/store"
import { accountsApi, capLabel, kindLabel, KIND_LABEL, CONN_STATUS, type RAccount, type RAdapter, type RConnection, type RMatchRule, type RUsage } from "@/api/accounts"
import { useLoad, useBusy, report, Dots } from "@/accounts/load"
import { NewAccountDialog, ConnectDialog, CredentialDialog } from "@/accounts/dialogs"
import { NewVaultDialog } from "@/vault/new-item"
import { MailSettingsSection } from "@/mail/settings"; import { MAIL_ADAPTERS } from "@/api/mail"

const SYS_I: Record<string, any> = { comm: MailIcon, code: CodeIcon, cloud: CloudIcon, finance: CreditCardIcon, calendar: CalendarIcon }
const iconOf = (a: { system: { id: string; kind: string } }) => a.system.id === "phone" ? PhoneIcon : /feishu|slack/.test(a.system.id) ? MessageSquareIcon : SYS_I[a.system.kind] ?? KeyRoundIcon
export const Ico = ({ I, className }: { I: any; className?: string }) => <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground", className)} aria-hidden><I className="size-4" strokeWidth={1.5} /></span>
const Section = ({ label, right, children }: { label: string; right?: ReactNode; children: ReactNode }) => <section><SectionLabel right={right}>{label}</SectionLabel>{children}</section>
const Note = ({ children }: { children: ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>
const Loading = ({ title, back }: { title: string; back?: [string, string] }) => <Page title={title} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
const Failed = ({ title, err, back }: { title: string; err: string; back?: [string, string] }) => <Page title={title} back={back}><Empty title={err} /></Page>

export function AcctRow({ a, showOwner }: { a: RAccount; showOwner?: boolean }) {
  const c = a.connection
  return (
    <Row icon={<Ico I={iconOf(a)} />} onClick={() => go("#/accounts/" + a.id)} data-acct-row={a.handle}
      title={<span className="inline-flex flex-wrap items-center gap-1.5">{a.display_name || a.handle}<span className="type-footnote text-muted-foreground">{a.system.name} · {kindLabel(a.kind)}</span>{!a.verified && <Badge variant="outline" className="font-normal text-attention">未确认</Badge>}</span>}
      sub={<Dots>{a.handle !== a.display_name && <span className="font-mono">{a.handle}</span>}{showOwner && !a.party.is_me && <span>{a.party.name}</span>}
        {c ? <span>已接入 Steve{c.caps.length ? "：" + c.caps.map(capLabel).join("、") : ""}</span> : <span>来源：{a.source_label || a.source}</span>}
        {a.vault_items.length > 0 && <span className="inline-flex items-center gap-1"><LockIcon className="size-3" aria-hidden />{a.vault_items.length} 个保险箱条目</span>}</Dots>}>
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
  const L = useLoad(async () => {
    const [mine, company, rules, systems] = await Promise.all([accountsApi.accounts({ scope: "mine" }), accountsApi.accounts({ scope: "company" }), accountsApi.rules(), accountsApi.systems()])
    const adapters = await accountsApi.adapters().catch(() => [] as RAdapter[]) // no adapters: only "record an account"
    return { mine, company, rules, systems, adapters }
  }, [], ["account", "connection"])
  const [dlg, setDlg] = useState<{ k: "new" } | { k: "connect"; a: RAdapter } | null>(null)
  if (L.s === "unwired") return <Unwired id="accounts" />
  if (L.s === "loading") return <Loading title="账号" />
  if (L.s === "error") return <Failed title="账号" err={L.err} />
  const { mine, company, rules, systems, adapters } = L.data
  const conn = mine.filter((a) => a.connection), rest = mine.filter((a) => !a.connection)
  const offer = adapters.filter((a) => a.auth === "oauth" || a.auth === "credential")
  return (
    <Page title="账号" sub="你和本公司在各个系统里的账号。接入 Steve 的账号可以收发消息、产生事件；登录要用的密码和令牌放在保险箱" max={760}
      actions={<><Button size="sm" variant="outline" onClick={() => go("#/vault")} data-open-vault><LockIcon />保险箱</Button>
        <DropdownMenu><DropdownMenuTrigger render={<Button size="sm" data-add-account-menu />}><PlusIcon />添加账号</DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-56">
            {offer.map((a) => <DropdownMenuItem key={a.id} onClick={() => setDlg({ k: "connect", a })} data-add-adapter={a.id}>{a.name}</DropdownMenuItem>)}
            {offer.length > 0 && <DropdownMenuSeparator />}
            <DropdownMenuItem onClick={() => setDlg({ k: "new" })} data-add-plain>其他账号（只记录，不接入）</DropdownMenuItem>
          </DropdownMenuContent></DropdownMenu></>}>
      {dlg?.k === "new" && <NewAccountDialog systems={systems} onClose={() => setDlg(null)} onDone={(id) => { setDlg(null); go("#/accounts/" + id) }} />}
      {dlg?.k === "connect" && <ConnectDialog adapter={dlg.a} onClose={() => setDlg(null)} />}
      <div className="flex flex-col gap-8">
        <Section label={`已接入 Steve · ${conn.length}`}><BySystem xs={conn} empty="还没有接入的账号。到“接入”页按步骤接入飞书、GitHub 或邮箱。" /></Section>
        <Group><Row icon={<Ico I={PlugIcon} />} title="接入" sub="飞书、GitHub、邮箱等服务的连接，以及每个连接能做什么，在接入页统一管理" onClick={() => go("#/integrations")} data-goto-integrations><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>
        <Section label={`本公司的机器人 · ${company.length}`}><BySystem xs={company} empty="没有本公司的应用机器人账号。" />
          <Note>应用机器人以对外身份在群里说话，身份在 Bot 页管理。</Note></Section>
        <Section label={`其他账号 · ${rest.length}`}><BySystem xs={rest} empty="没有只记录的账号。" />
          <Note>没有接入 Steve 的账号，比如银行卡、云服务控制台。记下来是为了关联保险箱条目，以及让 Steve 知道你有哪些账号。</Note></Section>
        <MatchRules rules={rules} onChange={L.reload} />
      </div>
    </Page>
  )
}

function MatchRules({ rules, onChange }: { rules: RMatchRule[]; onChange: () => void }) {
  const r = rules.find((x) => x.kind === "email_prefix")
  const [busy, act] = useBusy(), [edit, setEdit] = useState<string | null>(null)
  if (!r) return null
  const save = async (b: Parameters<typeof accountsApi.patchRule>[1]) => { if (await act(() => accountsApi.patchRule(r.id, b))) onChange() }
  return (
    <Section label="识别规则">
      <Group>
        <Row title="邮箱前缀匹配" sub="同一个前缀的邮箱认作同一个人，例如 alice@example.org 和 hanmeimei@example.test"><Switch aria-label="邮箱前缀匹配" checked={r.enabled} disabled={busy} onCheckedChange={(v) => void save({ enabled: v })} data-prefix-rule /></Row>
        <Row title="只在同一组织的域名之间生效" sub="组织的域名在组织页设置"><Switch aria-label="只在同一组织的域名之间生效" checked={r.config.same_org_only} disabled={!r.enabled || busy} onCheckedChange={(v) => void save({ config: { same_org_only: v } })} /></Row>
        {edit === null
          ? <Row title="排除的公共前缀" sub={r.config.exclude.join("、") || "没有"}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEdit(r.config.exclude.join(", "))} data-edit-exclude>编辑</Button></Row>
          : <form className="flex items-center gap-2 px-2 py-2" onSubmit={(e) => { e.preventDefault(); const xs = edit.split(/[,，、\s]+/).map((x) => x.trim()).filter(Boolean).slice(0, 50); void save({ config: { exclude: xs } }).then(() => setEdit(null)) }}>
            <Input aria-label="排除的公共前缀，用逗号分开" autoFocus className="h-8 flex-1 rounded-lg" value={edit} onChange={(e) => setEdit(e.target.value)} data-exclude-input />
            <Button size="sm" variant="ghost" type="button" onClick={() => setEdit(null)}>取消</Button><Button size="sm" type="submit" disabled={busy}>保存</Button></form>}
        <Row title="最短前缀" sub="比这更短的前缀不参与匹配"><Select align="end" testId="min-len" value={String(r.config.min_len)} onChange={(v) => void save({ config: { min_len: +v } })} options={Array.from({ length: 10 }, (_, n) => ({ v: String(n + 1), l: `${n + 1} 个字符` }))} /></Row>
      </Group>
      <Note>只有精确的账号和这些规则会自动识别联系人；同名同组织这类模糊线索只做提议，放在 联系人 · 待确认。</Note>
    </Section>
  )
}

/* ═════════ 账号详情 ═════════ */
function AccountDetail({ id }: { id: string }) {
  const L = useLoad(async () => {
    const a = await accountsApi.account(id)
    const [c, adapters] = await Promise.all([a.connection ? accountsApi.connection(a.connection.id) : Promise.resolve(null), a.connection ? accountsApi.adapters().catch(() => [] as RAdapter[]) : Promise.resolve([] as RAdapter[])])
    return { a, c, adapter: c ? adapters.find((x) => x.id === c.adapter) ?? null : null }
  }, [id], ["account", "connection", "vault"])
  const [rm, setRm] = useState(false), [edit, setEdit] = useState(false), [newItem, setNewItem] = useState(false), [busy, act] = useBusy()
  if (L.s === "unwired") return <Unwired id="accounts" />
  if (L.s === "loading") return <Loading title="账号" back={["#/accounts", "账号"]} />
  if (L.s === "error") return <Failed title="账号" err={L.status === 404 ? "账号不存在" : L.err} back={["#/accounts", "账号"]} />
  const { a, c, adapter } = L.data
  const mine = a.party.is_me || a.party.type === "org" || a.party.type === "team"
  const back: [string, string] = mine ? ["#/accounts", "账号"] : ["#/contacts/" + a.party.id, a.party.name]
  return (
    <Page title={a.display_name || a.handle} back={back} max={680} sub={<Meta><span>{a.system.name}</span><span>{kindLabel(a.kind)}</span><span className="font-mono">{a.handle}</span></Meta>}
      actions={<><Button size="sm" variant="ghost" onClick={() => setEdit(true)} data-edit-account>编辑</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setRm(true)} data-rm-plain>移除</Button></>}>
      <AlertDialog open={rm} onOpenChange={setRm}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>移除这个账号？</AlertDialogTitle><AlertDialogDescription>{c ? "这个账号接入了 Steve，要先断开接入才能移除。" : `只移除 Steve 里的记录。${a.vault_items.length ? `关联的 ${a.vault_items.length} 个保险箱条目保留，只是不再关联。` : ""}`}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel>{!c && <AlertDialogAction variant="destructive" onClick={async () => { if (await act(() => accountsApi.deleteAccount(a.id), "已移除")) go(back[0]) }} data-rm-confirm>移除</AlertDialogAction>}</AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditAccount a={a} onClose={() => setEdit(false)} onDone={L.reload} />}
      {newItem && <NewVaultDialog account={{ id: a.id, display_name: a.display_name || a.handle }} onClose={() => setNewItem(false)} />}
      <div className="flex flex-col gap-8">
        <Section label="属于"><Group><Row title={a.party.name} sub={`${a.verified ? "已确认" : "未确认"} · 来源：${a.source_label || a.source}`} onClick={() => go("#/contacts/" + a.party.id)}>
          {!a.verified && <Button size="xs" variant="outline" disabled={busy} onClick={(e) => { e.stopPropagation(); void act(() => accountsApi.patchAccount(a.id, { verified: true }), "已确认").then((r) => r && L.reload()) }} data-verify-account>确认</Button>}
          <ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></Section>
        {mine && c && MAIL_ADAPTERS.includes(c.adapter) && <MailSettingsSection connection={c.id} />}
        {mine && <Section label="接入 Steve">{c ? <Connection c={c} adapter={adapter} label={a.display_name || a.handle} onChange={L.reload} />
          : <div className="rounded-xl border border-dashed px-4 py-4 type-callout text-muted-foreground">没有接入。{a.system.kind === "finance" ? "银行账号不接入 Steve；需要付款时由你批准后用保险箱里的卡信息代填。" : "需要的话可以在账号页用“添加账号”接入，或者通过插件接入。"}</div>}</Section>}
        {mine && <Section label={`保险箱条目 · ${a.vault_items.length}`} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewItem(true)} data-new-vault-for-account><PlusIcon />新建</Button>}>
          {a.vault_items.length ? <Group>{a.vault_items.map((v) => <Row key={v.id} icon={<Ico I={KeyRoundIcon} />} title={v.title} onClick={() => go("#/vault/" + v.id)} data-vault-row={v.title}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <Empty title="没有关联的条目" sub="登录这个账号要用的密码、令牌可以放进保险箱。" />}</Section>}
      </div>
    </Page>
  )
}

function EditAccount({ a, onClose, onDone }: { a: RAccount; onClose: () => void; onDone: () => void }) {
  const [n, setN] = useState(a.display_name), [k, setK] = useState(a.kind), [busy, act] = useBusy()
  const save = async () => { if (await act(() => accountsApi.patchAccount(a.id, { display_name: n.trim(), kind: k }), "已保存")) { onDone(); onClose() } }
  return (
    <AlertDialog open onOpenChange={(o) => !o && onClose()}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>编辑账号</AlertDialogTitle><AlertDialogDescription>{a.system.name} · <span className="font-mono">{a.handle}</span></AlertDialogDescription></AlertDialogHeader>
      <form className="flex flex-col gap-3" onSubmit={(e) => { e.preventDefault(); void save() }}>
        <label className="flex flex-col gap-1.5 type-callout">显示名<Input className="rounded-lg" value={n} maxLength={80} onChange={(e) => setN(e.target.value)} data-edit-name /></label>
        <div className="flex items-center justify-between type-callout">种类<Select align="end" value={k} onChange={setK} options={Object.entries(KIND_LABEL).map(([v, l]) => ({ v, l }))} /></div>
      </form>
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction disabled={busy} onClick={(e) => { e.preventDefault(); void save() }} data-edit-save>保存</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  )
}

function Connection({ c, adapter, label, onChange }: { c: RConnection; adapter: RAdapter | null; label: string; onChange: () => void }) {
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
  const used = usage ? [...usage.flows.map((f) => "流程「" + f.name + "」"), ...usage.sources.map((x) => "事件源「" + x.name + "」"), ...(usage.spaces ? [`${usage.spaces} 个空间`] : [])] : []
  const toggleCap = (cap: string, on: boolean) => void act(() => accountsApi.patchConnection(c.id, { caps: on ? [...c.caps, cap] : c.caps.filter((x) => x !== cap) })).then((r) => r && onChange())
  return (
    <div className="flex flex-col gap-4" data-connection={c.id} data-connection-status={c.status}>
      {cred && adapter && <CredentialDialog adapter={adapter} connection={c.id} only={cred} onClose={() => setCred(null)} onDone={onChange} />}
      <AlertDialog open={!!usage} onOpenChange={(o) => !o && setUsage(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>断开“{label}”？</AlertDialogTitle><AlertDialogDescription>Steve 会撤销授权并销毁保存的凭据；账号保留为只记录。{used.length ? "" : "现在没有流程、空间或事件源在用它。"}</AlertDialogDescription></AlertDialogHeader>
        {used.length > 0 && <div className="type-callout" data-usage><div className="mb-1.5 text-muted-foreground">这些会停止工作：</div><ul className="list-disc pl-5">{used.map((u) => <li key={u}>{u}</li>)}</ul></div>}
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={async () => { if (await act(() => accountsApi.disconnect(c.id), "已断开")) onChange() }} data-disconnect-confirm>断开</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <Group>
        <Row title={<span className="inline-flex items-center gap-2"><StatusDot s={dot} />{s}</span>} data-conn-state
          sub={<Dots><span>{adapter?.name ?? c.adapter}</span>{c.last_sync ? <span>上次同步 {tm(c.last_sync)}</span> : null}{c.health.last_ok ? <span>上次正常 {tm(c.health.last_ok)}</span> : null}</Dots>}>
          <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => accountsApi.test(c.id)).then((r) => { if (r) { toast(r.status === "ok" ? "连接正常" : "检查没有通过"); onChange() } })} data-conn-test><RefreshCwIcon />检查</Button></Row>
        {c.health.last_error && c.status !== "ok" && <Row title="最近的错误" sub={<span className="font-mono">{c.health.last_error}</span>}>{c.health.error_at ? <span className="type-footnote text-muted-foreground">{tm(c.health.error_at)}</span> : null}</Row>}
        {(c.status === "reauth" || c.status === "error") && <Row title="重新授权" sub={c.auth === "oauth" ? "在浏览器里重新同意授权" : "重新填写凭据"}><Button size="sm" disabled={busy} onClick={() => void reauth()} data-reauth>重新授权</Button></Row>}
        <Row title="接收中" sub={c.status === "paused" ? "已暂停：期间不接收，恢复后不补收" : "暂停后不再接收，恢复后不补收"}>
          <Switch aria-label="接收中" checked={c.status !== "paused"} disabled={busy || c.status === "revoked"} onCheckedChange={(v) => void act(() => accountsApi.patchConnection(c.id, { paused: !v }), v ? "已恢复" : "已暂停").then((r) => r && onChange())} data-conn-pause /></Row>
      </Group>
      {c.available_caps.length > 0 && <div><div className="mb-1 type-footnote text-muted-foreground">允许 Steve</div><Group>{c.available_caps.map((cap) => (
        <Row key={cap} title={capLabel(cap)}><Switch aria-label={capLabel(cap)} checked={c.caps.includes(cap)} disabled={busy} onCheckedChange={(v) => toggleCap(cap, v)} data-cap-toggle={cap} /></Row>))}</Group></div>}
      {c.caps.includes("msg.receive") && <Group><Row title="身份与响应规则" sub="在群里以什么身份出现、什么时候回复" onClick={() => go("#/channels/acc/" + c.account_id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>}
      {c.auth === "credential" && c.status === "ok" && adapter?.fields?.some((f) => f.secret) && <Button size="sm" variant="ghost" className="self-start text-muted-foreground" disabled={busy} onClick={() => setCred(adapter.fields!.filter((f) => f.secret).map((f) => f.key))} data-replace-cred>更换凭据</Button>}
      <Button size="sm" variant="ghost" className="self-start text-destructive" disabled={asking || busy} onClick={() => void askDisconnect()} data-disconnect>{asking && <Spinner />}断开接入</Button>
    </div>
  )
}
