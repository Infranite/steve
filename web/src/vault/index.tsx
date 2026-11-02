/* 保险箱 (accounts-vault.md §4, §8; prototype people.tsx VaultView / VaultItemView): items with masked fields, the use log,
 * grants to Bots; enabling the vault and its one-time recovery code. Secret values appear only through useReveal. */
import { useCallback, useMemo, useState, type ReactNode } from "react"
import { ChevronRightIcon, CopyIcon, CreditCardIcon, EyeIcon, EyeOffIcon, FileTextIcon, IdCardIcon, KeyRoundIcon, LinkIcon, LockIcon, PencilIcon, PlusIcon, PrinterIcon, ShieldIcon, TerminalIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Checkbox } from "@/components/ui/checkbox"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { go, Page, Group, Row, SectionLabel, Segmented, Meta, Empty, Chips, BotAvatar } from "@/ui"
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { st, tm, botName, toast } from "@/store"
import { accountsApi, type RAccount } from "@/api/accounts"
import { NotWired } from "@/api"
import { vaultApi, keyStoreState, SENS_LABEL, MODE_LABEL, USE_STATUS, type RItem, type RField, type RUse, type RVaultStatus, type Sensitivity } from "@/api/vault"
import { useLoad, useBusy, report, Dots } from "@/accounts/load"
import { NewVaultDialog } from "@/vault/new-item"
import { useReveal, Countdown } from "@/vault/reveal"

const TPL_I: Record<string, any> = { login: KeyRoundIcon, bank_card: CreditCardIcon, api_credential: TerminalIcon, id_document: IdCardIcon, secure_note: FileTextIcon, ssh_key: LockIcon, recovery_codes: ShieldIcon }
const Ico = ({ I, className }: { I: any; className?: string }) => <span className={"flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground " + (className ?? "")} aria-hidden><I className="size-4" strokeWidth={1.5} /></span>
const Section = ({ label, right, children }: { label: string; right?: ReactNode; children: ReactNode }) => <section><SectionLabel right={right}>{label}</SectionLabel>{children}</section>
const Note = ({ children }: { children: ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>
const BACK: [string, string] = ["#/accounts", "账号"]
/** the vault's templates have Chinese names in the API; until they load, the ids map to the prototype's names */
const TPL_NAME: Record<string, string> = { login: "登录", bank_card: "银行卡", api_credential: "API 凭据", id_document: "证件", secure_note: "安全笔记", ssh_key: "SSH 密钥", recovery_codes: "恢复码" }
const tplName = (id: string) => TPL_NAME[id] ?? id
/** uses come from V4; a Hub without it shows the items without a use log */
const usesOr = (p: Promise<RUse[]>) => p.catch((e) => { if (e instanceof NotWired) return null; throw e })

export function VaultRoute({ id }: { id?: string }) { return id ? <VaultItemView key={id} id={id} /> : <VaultView /> }

function VaultRow({ v }: { v: RItem }) {
  return <Row icon={<Ico I={TPL_I[v.template] ?? KeyRoundIcon} />} onClick={() => go("#/vault/" + v.id)} data-vault-row={v.title}
    title={<span className="inline-flex items-center gap-1.5">{v.title}{v.sensitivity === "high" && <Badge variant="outline" className="font-normal text-muted-foreground">高密</Badge>}</span>}
    sub={<Dots><span>{tplName(v.template)}</span><span>{v.vault.name}</span>{v.account && <span>{v.account.display_name}</span>}{v.site && <span>{v.site}</span>}{v.grants.length > 0 && <span>{v.grants.length} 个授权</span>}<span>更新于 {tm(v.updated)}</span></Dots>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
}
function UseRow({ u, showItem }: { u: RUse; showItem?: boolean }) {
  const bot = u.actor.type === "bot" ? u.actor.id : u.actor.type === "steve" ? "steve" : null
  return <Row icon={bot ? <BotAvatar k={bot} size={24} /> : <Ico I={u.mode === "copy" ? CopyIcon : EyeIcon} className="size-6! rounded-full!" />} onClick={showItem ? () => go("#/vault/" + u.item_id) : undefined} data-use-row={u.id}
    title={u.actor.type === "me" ? `你${u.mode === "copy" ? "复制" : "查看"}了原值` : `${u.actor.name} · ${u.purpose}`}
    sub={<Dots>{showItem && <span>{u.item_title}</span>}<span>{MODE_LABEL[u.mode] ?? u.mode}</span>{u.status !== "ok" && <span>{USE_STATUS[u.status] ?? u.status}</span>}{u.work_item_id && <span className="font-mono">{u.work_item_id}</span>}</Dots>}>
    <span className="type-footnote text-muted-foreground">{tm(u.created)}</span></Row>
}

/* ───────── 启用保险箱 → 恢复码 ───────── */
export function RecoveryCode({ code, onDone, title = "抄下恢复码" }: { code: string; onDone: () => void; title?: string }) {
  const [saved, setSaved] = useState(false)
  const groups = code.split(/[-\s]+/).filter(Boolean)
  const print = () => { const w = window.open("", "_blank", "width=600,height=400"); if (!w) return; w.document.title = "Steve 保险箱恢复码"; const pre = w.document.createElement("pre"); pre.style.cssText = "font:20px/1.8 ui-monospace,monospace;padding:32px"; pre.textContent = "Steve 保险箱恢复码\n\n" + groups.join("  ") + "\n\n" + new Date().toLocaleString(); w.document.body.appendChild(pre); w.print(); w.close() }
  return (
    <div className="flex flex-col gap-4 rounded-xl border px-5 py-5" data-recovery>
      <div><div className="type-headline">{title}</div><div className="mt-1 type-callout text-muted-foreground">恢复码就是保险箱的主密钥，只显示这一次。换机器或钥匙串丢了，要靠它恢复；丢了恢复码又丢了钥匙串，保险箱里的东西就再也打不开。不要存进 Steve 自己。</div></div>
      <div className="grid grid-cols-2 gap-2 rounded-lg bg-muted px-4 py-3 font-mono text-[15px] tracking-wider sm:grid-cols-4" aria-label="恢复码" data-recovery-code>{groups.map((g, n) => <span key={n} className="select-all">{g}</span>)}</div>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" onClick={() => navigator.clipboard?.writeText(code).then(() => toast("已复制恢复码"), () => toast("浏览器不允许写剪贴板"))}><CopyIcon />复制</Button>
        <Button size="sm" variant="outline" onClick={print}><PrinterIcon />打印</Button>
      </div>
      <label className="flex items-center gap-2.5 type-callout"><Checkbox checked={saved} onCheckedChange={(v) => setSaved(!!v)} data-recovery-saved />我已经抄下或打印了恢复码，放在 Steve 以外的地方</label>
      <Button className="self-start" disabled={!saved} onClick={onDone} data-recovery-done>完成</Button>
    </div>
  )
}
function EnableVault({ onDone }: { onDone: () => void }) {
  const [code, setCode] = useState<string | null>(null), [busy, act] = useBusy()
  if (code) return <RecoveryCode code={code} onDone={() => { setCode(null); onDone() }} />
  return (
    <div className="flex flex-col gap-4 rounded-xl border px-5 py-5" data-enable-vault>
      <div className="flex items-start gap-3"><Ico I={LockIcon} className="size-10 rounded-xl" /><div><div className="type-headline">启用保险箱</div>
        <div className="mt-1 type-callout text-muted-foreground">密码、令牌、银行卡、证件放在这里。每个条目单独加密，主密钥放在这台 Hub 的系统钥匙串（或密钥环）里，数据库和备份里只有密文。Bot 拿不到原值，只能申请使用，由 Steve 在执行时注入；你查看原值要本机验证。</div></div></div>
      <div className="type-callout text-muted-foreground">启用时会生成一个恢复码，只显示一次，请准备好抄下或打印。</div>
      <Button className="self-start" disabled={busy} onClick={() => void act(() => vaultApi.init()).then((r) => r && setCode(r.recovery_code))} data-vault-init>{busy && <Spinner />}启用保险箱</Button>
    </div>
  )
}
const KeychainBanner = ({ s }: { s: RVaultStatus }) => s.initialized && s.keychain !== "ok" ? <div className="mb-6 rounded-xl border border-attention/40 bg-attention/5 px-4 py-3 type-callout" role="alert" data-keychain={s.keychain}>保险箱暂时打不开：{keyStoreState(s)}{s.keychain === "missing" && <> · <a className="text-blue hover:underline" href="#/settings/vault">用恢复码恢复</a></>}</div> : null

/* ═════════ 保险箱首页 ═════════ */
let vtab: "全部" | "个人" | "共享" = "全部", vtpl = "全部"
function VaultView() {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [newItem, setNewItem] = useState(false)
  const L = useLoad(async () => {
    const status = await vaultApi.status()
    if (!status.initialized) return { status, items: [] as RItem[], uses: null as RUse[] | null }
    const [items, uses] = await Promise.all([vaultApi.items(), usesOr(vaultApi.uses({ limit: 6 }))])
    return { status, items, uses }
  }, [], ["vault"])
  if (L.s === "unwired") return <Unwired id="vault" />
  if (L.s === "loading") return <Page title="保险箱" back={BACK}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="保险箱" back={BACK}><Empty title={L.err} /></Page>
  const { status, items, uses } = L.data
  const sub = "密码、令牌、银行卡、证件。值加密保存，Bot 拿不到原值，只能申请使用，由 Steve 在执行时注入"
  if (!status.initialized) return <Page title="保险箱" back={BACK} sub={sub} max={640}><EnableVault onDone={() => void L.reload()} /></Page>
  const scope = vtab === "个人" ? "personal" : vtab === "共享" ? "shared" : null
  const tpls = [...new Set(items.map((v) => v.template))]
  const xs = items.filter((v) => (!scope || v.vault.scope === scope) && (vtpl === "全部" || tplName(v.template) === vtpl))
  return (
    <Page title="保险箱" back={BACK} sub={sub} max={760}
      actions={<><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => go("#/settings/vault")} data-vault-settings>设置</Button><Button size="sm" onClick={() => setNewItem(true)} disabled={status.keychain !== "ok"} data-new-vault><PlusIcon />新建条目</Button></>}
      toolbar={<Segmented value={vtab} onChange={(v) => { vtab = v; r() }} options={["全部", "个人", "共享"] as const} />}>
      {newItem && <NewVaultDialog onClose={() => setNewItem(false)} />}
      <KeychainBanner s={status} />
      <div className="flex flex-col gap-8">
        <div>{tpls.length > 1 && <Chips className="mb-4" value={vtpl} onChange={(v) => { vtpl = v; r() }} options={["全部", ...tpls.map(tplName)]} />}
          {xs.length ? <Group>{xs.map((v) => <VaultRow key={v.id} v={v} />)}</Group> : <Empty title={items.length ? "没有符合的条目" : "还没有条目"} sub={items.length ? undefined : "用右上角的“新建条目”保存第一个密码或令牌。"} />}</div>
        {uses && <Section label="最近使用">{uses.length ? <Group>{uses.map((u) => <UseRow key={u.id} u={u} showItem />)}</Group> : <Empty title="还没有使用记录" />}</Section>}
        <Section label="规则"><Group>
          <Row title="Bot 使用条目" sub="默认每次都要你批准；普通和敏感的条目可以授权给某个 Bot 在某个项目目录或网站上使用，最长 24 小时" />
          <Row title="高密条目（银行卡、证件）" sub="不允许授权，每次都要批准；用银行卡付款属于花钱，任何授权都不能覆盖" />
          <Row title="查看原值" sub={`在 Steve 里查看机密字段需要本机验证（Touch ID 或通行密钥）；查看也会记录。${status.passkeys ? "" : "这个工作空间还没有登记通行密钥。"}`} onClick={() => go("#/settings/vault")}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
          <Row icon={<Ico I={LinkIcon} />} title={<span className="text-muted-foreground">外部保险库</span>} sub="以后可以通过插件接入 1Password、Bitwarden 等，作为引用型保险库；现在不支持"><Badge variant="outline" className="font-normal text-muted-foreground">预留</Badge></Row>
        </Group></Section>
      </div>
    </Page>
  )
}

/* ═════════ 条目 ═════════ */
function VaultItemView({ id }: { id: string }) {
  const L = useLoad(async () => {
    const [item, uses] = await Promise.all([vaultApi.item(id), usesOr(vaultApi.uses({ item: id, limit: 50 }))])
    return { item, uses }
  }, [id], ["vault"])
  const [del, setDel] = useState(false), [edit, setEdit] = useState<RField | null>(null), [, act] = useBusy()
  const reload = L.reload
  const used = useCallback(() => void reload(), [reload])
  const R = useReveal(id, used)
  if (L.s === "unwired") return <Unwired id="vault" />
  if (L.s === "loading") return <Page title="保险箱" back={["#/vault", "保险箱"]}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="保险箱" back={["#/vault", "保险箱"]}><Empty title={L.status === 404 ? "条目不存在" : L.err} /></Page>
  const { item: v, uses } = L.data
  const fields = v.fields ?? []
  const setSens = async (x: Sensitivity) => {
    const n = v.grants.length
    if (await act(() => vaultApi.patchItem(v.id, { sensitivity: x }), x === "high" && n ? `已改成高密，${n} 个授权已撤销` : "已保存")) void reload()
  }
  return (
    <Page title={v.title} back={["#/vault", "保险箱"]} max={680} sub={<Meta><span>{tplName(v.template)}</span><span>{v.vault.name}保险库</span>{v.site && <span>{v.site}</span>}<span>更新于 {tm(v.updated)}</span></Meta>}
      actions={<Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDel(true)} data-del-vault>删除</Button>}>
      {R.dialog}
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>删除“{v.title}”？</AlertDialogTitle><AlertDialogDescription>字段的值会被销毁，不能恢复；{v.grants.length} 个授权一起撤销。</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={async () => { if (await act(() => vaultApi.deleteItem(v.id), "已删除")) go("#/vault") }} data-del-confirm>删除</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditField item={v.id} f={edit} onClose={() => setEdit(null)} onDone={() => { R.hide(edit.id); void reload() }} />}
      <div className="flex flex-col gap-8">
        <div className="-mt-2 flex items-center gap-3"><Ico I={TPL_I[v.template] ?? KeyRoundIcon} className="size-12 rounded-xl" />
          <div className="flex items-center gap-1 type-callout text-muted-foreground">敏感度{v.template === "bank_card" ? <span className="px-2 text-foreground" title="银行卡的敏感度固定为高密">高密</span>
            : <Select quiet testId="vault-level" value={v.sensitivity} onChange={(x) => void setSens(x as Sensitivity)} options={(["normal", "sensitive", "high"] as const).map((x) => ({ v: x, l: SENS_LABEL[x] }))} />}</div></div>
        <Section label="字段">{fields.length ? <Group>{fields.map((fl) => {
          const s = R.shown[fl.id], masked = fl.secret && !s
          return (
            <Row key={fl.id} title={fl.label} data-field={fl.label}
              sub={s ? <Countdown until={s.until} /> : undefined}>
              <span className={"max-w-[16rem] truncate type-callout sm:max-w-[22rem] " + (fl.secret ? "font-mono" : "")} data-field-value={masked ? "masked" : "shown"}>{masked ? (fl.hint || "••••••••") : s ? s.value : (fl.value ?? "")}</span>
              {fl.secret && (s ? <Button variant="ghost" size="icon-xs" aria-label={"隐藏" + fl.label} className="text-muted-foreground" onClick={() => R.hide(fl.id)} data-hide><EyeOffIcon /></Button>
                : <Button variant="ghost" size="icon-xs" aria-label={"显示" + fl.label} className="text-muted-foreground" disabled={R.busy === fl.id} onClick={() => void R.reveal(fl.id, false)} data-reveal>{R.busy === fl.id ? <Spinner /> : <EyeIcon />}</Button>)}
              <Button variant="ghost" size="icon-xs" aria-label={"复制" + fl.label} className="text-muted-foreground" disabled={R.busy === fl.id} onClick={() => { if (fl.secret) void R.reveal(fl.id, true); else navigator.clipboard?.writeText(fl.value ?? "").then(() => toast("已复制"), () => toast("浏览器不允许写剪贴板")) }} data-copy><CopyIcon /></Button>
              <Button variant="ghost" size="icon-xs" aria-label={"修改" + fl.label} className="text-muted-foreground" onClick={() => setEdit(fl)} data-edit-field><PencilIcon /></Button>
            </Row>)
        })}</Group> : <Empty title="没有字段" />}
          <Note>机密字段只显示掩码；查看或复制原值要本机验证，30 秒后自动隐藏，并写进使用记录。</Note></Section>
        <LinkedAccount v={v} onChange={() => void reload()} />
        <Grants v={v} onChange={() => void reload()} />
        {uses && <Section label="使用记录">{uses.length ? <Group>{uses.map((u) => <UseRow key={u.id} u={u} />)}</Group> : <Empty title="还没有使用过" />}
          <Note>只记录谁在什么时候为什么用了它，不记录值；会话记录里出现的值会自动打码。</Note></Section>}
      </div>
    </Page>
  )
}

/** replace a field's whole value; a secret is never read back for editing */
function EditField({ item, f, onClose, onDone }: { item: string; f: RField; onClose: () => void; onDone: () => void }) {
  const [val, setVal] = useState(f.secret ? "" : f.value ?? ""), [busy, act] = useBusy()
  const save = async () => { const r = await act(() => vaultApi.putField(item, f.id, val), "已保存"); setVal(""); if (r) { onDone(); onClose() } }
  return (
    <AlertDialog open onOpenChange={(o) => !o && onClose()}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>修改“{f.label}”</AlertDialogTitle><AlertDialogDescription>{f.secret ? "填写新的值，整个替换旧值并重新加密；旧值不会显示出来。" : "这个字段不是机密，明文保存。"}</AlertDialogDescription></AlertDialogHeader>
      <form onSubmit={(e) => { e.preventDefault(); if (!busy) void save() }} autoComplete="off">
        <Input aria-label={f.label} autoFocus className="rounded-lg font-mono" type={f.secret ? "password" : "text"} autoComplete={f.secret ? "new-password" : "off"} spellCheck={false} value={val} onChange={(e) => setVal(e.target.value)} data-edit-value />
      </form>
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction disabled={busy || (f.secret && !val)} onClick={(e) => { e.preventDefault(); void save() }} data-edit-field-save>保存</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  )
}

function LinkedAccount({ v, onChange }: { v: RItem; onChange: () => void }) {
  const [opts, setOpts] = useState<RAccount[] | null>(null), [busy, act] = useBusy()
  const load = () => { if (!opts) accountsApi.accounts({ scope: "mine" }).then(setOpts, (e) => { if (!(e instanceof NotWired)) report(e); setOpts([]) }) }
  const set = async (acc: string) => { if (await act(() => vaultApi.patchItem(v.id, { account_id: acc || null }), acc ? "已关联" : "已取消关联")) onChange() }
  return (
    <Section label="关联账号" right={opts === null ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={load} data-change-account>{v.account ? "更换" : "关联"}</Button> : undefined}>
      {opts !== null && <div className="mb-2 flex items-center gap-2"><Select testId="vault-account" value={v.account?.id ?? ""} onChange={(x) => void set(x)} options={[{ v: "", l: "不关联" }, ...opts.map((a) => ({ v: a.id, l: `${a.display_name || a.handle}（${a.system.name}）` }))]} />{busy && <Spinner />}</div>}
      <Group>{v.account ? <Row icon={<Ico I={KeyRoundIcon} />} title={v.account.display_name} onClick={() => go("#/accounts/" + v.account!.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
        : <Row title="没有关联账号" sub="关联后，账号页能看到这个条目；Bot 申请使用时也会写明是哪个账号" />}</Group>
    </Section>
  )
}

function Grants({ v, onChange }: { v: RItem; onChange: () => void }) {
  const bots = Object.values(st.bots).filter((b) => !b.main)
  const [gBot, setGBot] = useState(bots[0]?.id ?? ""), [gScope, setGScope] = useState(""), [gFor, setGFor] = useState<"1" | "8" | "24">("1"), [busy, act] = useBusy()
  const scopes = useMemo(() => {
    const def = st.bots[gBot]?.project
    const ps = [...st.projects.filter((p) => !p.archived)].sort((a, b) => (a.id === def ? -1 : b.id === def ? 1 : 0))
    return [...ps.map((p) => ({ v: "p:" + p.id, l: "项目 " + p.name })), { v: "scratch", l: "没有项目的事项" }, ...(v.site ? [{ v: "s:" + v.site, l: "网站 " + v.site }] : [])]
  }, [gBot, v.site, st.ver])
  const scope = scopes.some((s) => s.v === gScope) ? gScope : scopes[0]?.v ?? ""
  const add = async () => {
    const sc = scope.startsWith("p:") ? { project_id: scope.slice(2) } : scope.startsWith("s:") ? { site: scope.slice(2) } : { scratch: true }
    if (await act(() => vaultApi.grant(v.id, { bot_id: gBot, scope: sc, hours: +gFor as 1 | 8 | 24 }), "已授权")) onChange()
  }
  const live = v.grants.filter((g) => g.until > Date.now())
  return (
    <Section label="使用授权">
      {live.length ? <Group>{live.map((g) => <Row key={g.id} icon={<BotAvatar k={g.bot_id} size={24} />} title={botName(g.bot_id)} sub={`${g.scope_label} · 到 ${tm(g.until)}`} data-grant={g.bot_id}>
        <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => vaultApi.revokeGrant(g.id), "已撤销授权").then((r) => r && onChange())} data-revoke-grant>撤销</Button></Row>)}</Group>
        : v.sensitivity === "high" || v.template === "bank_card" ? null : <div className="px-1 type-callout text-muted-foreground">没有授权，每次使用都要你批准。</div>}
      {v.sensitivity === "high" || v.template === "bank_card" ? <Note>高密条目不允许授权，每次都要你批准。</Note> :
        bots.length === 0 ? <Note>还没有 Bot 可以授权。</Note> :
        <form className="mt-3 flex flex-wrap items-center gap-2" data-new-grant onSubmit={(e) => { e.preventDefault(); void add() }}>
          <Select testId="grant-bot" value={gBot} onChange={setGBot} options={bots.map((b) => ({ v: b.id, l: botName(b.id) }))} />
          <Select testId="grant-scope" value={scope} onChange={setGScope} options={scopes} />
          <Select testId="grant-for" value={gFor} onChange={(x) => setGFor(x as "1" | "8" | "24")} options={[["1", "1 小时"], ["8", "8 小时"], ["24", "24 小时"]].map(([x, l]) => ({ v: x, l }))} />
          <Button size="sm" variant="outline" type="submit" disabled={busy || !gBot || !scope} data-add-grant>授权</Button></form>}
    </Section>
  )
}
