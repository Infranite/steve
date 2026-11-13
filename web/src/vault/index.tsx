/* 保险箱 (accounts-vault.md §4, §8; prototype people.tsx VaultView / VaultItemView): items with masked fields, the use log,
 * grants to Bots; enabling the vault and its one-time recovery code. Secret values appear only through useReveal. */
import { useCallback, useMemo, useState } from "react"
import { CopyIcon, CreditCardIcon, EyeIcon, EyeOffIcon, FileTextIcon, IdCardIcon, KeyRoundIcon, LinkIcon, LockIcon, PencilIcon, PlusIcon, PrinterIcon, ShieldIcon, TerminalIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Checkbox } from "@/components/ui/checkbox"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { go, Page, Row, Segmented, Meta, Empty, Chips, BotAvatar } from "@/ui"
import { Panel, Section, Note, Blank, Notice, Pill, Tile, Chevron, Loading, LIFT } from "@/settings-kit"
import { cn } from "cn"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { st, tm, botName, toast } from "@/store"
import { accountsApi, type RAccount } from "@/api/accounts"
import { NotWired } from "@/api"
import { vaultApi, keyStoreState, SENS_LABEL, MODE_LABEL, USE_STATUS, type RItem, type RField, type RUse, type RVaultStatus, type Sensitivity } from "@/api/vault"
import { useLoad, useBusy, report, Dots } from "@/accounts/load"
import { NewVaultDialog } from "@/vault/new-item"
import { useReveal, Countdown } from "@/vault/reveal"
import { t as tr, useT } from "@/i18n"
import { date } from "@/i18n/format"

const TPL_I: Record<string, any> = { login: KeyRoundIcon, bank_card: CreditCardIcon, api_credential: TerminalIcon, id_document: IdCardIcon, secure_note: FileTextIcon, ssh_key: LockIcon, recovery_codes: ShieldIcon }
const Ico = Tile
/** a section with nothing in it yet: one quiet line, not a page-sized empty state */
const Quiet = Blank
const back = (): [string, string] => ["#/accounts", tr("vault:backAccounts")]
const backVault = (): [string, string] => ["#/vault", tr("vault:title")]
/** the vault's templates have Chinese names in the API; here the ids map to the prototype's names, in the UI language */
const TPL_IDS = ["login", "bank_card", "api_credential", "id_document", "secure_note", "ssh_key", "recovery_codes"] as const
const tplName = (id: string) => ((TPL_IDS as readonly string[]).includes(id) ? tr(`vault:tpl.${id as (typeof TPL_IDS)[number]}`) : id)
/** uses come from V4; a Hub without it shows the items without a use log */
const usesOr = (p: Promise<RUse[]>) => p.catch((e) => { if (e instanceof NotWired) return null; throw e })

export function VaultRoute({ id }: { id?: string }) { return id ? <VaultItemView key={id} id={id} /> : <VaultView /> }

function VaultRow({ v }: { v: RItem }) {
  const t = useT()
  return <Row icon={<Ico I={TPL_I[v.template] ?? KeyRoundIcon} />} onClick={() => go("#/vault/" + v.id)} data-vault-row={v.title}
    title={<span className="inline-flex items-center gap-1.5">{v.title}{v.sensitivity === "high" && <Pill tone="attention">{SENS_LABEL.high}</Pill>}</span>}
    sub={<Dots><span>{tplName(v.template)}</span><span>{v.vault.name}</span>{v.account && <span>{v.account.display_name}</span>}{v.site && <span>{v.site}</span>}{v.grants.length > 0 && <span>{t("vault:row.grants", { count: v.grants.length })}</span>}<span>{t("vault:row.updated", { time: tm(v.updated) })}</span></Dots>}><Chevron /></Row>
}
function UseRow({ u, showItem }: { u: RUse; showItem?: boolean }) {
  const t = useT()
  const bot = u.actor.type === "bot" ? u.actor.id : u.actor.type === "steve" ? "steve" : null
  return <Row icon={bot ? <BotAvatar k={bot} size={24} /> : <Ico I={u.mode === "copy" ? CopyIcon : EyeIcon} className="size-6! rounded-full!" />} onClick={showItem ? () => go("#/vault/" + u.item_id) : undefined} data-use-row={u.id}
    title={u.actor.type === "me" ? (u.mode === "copy" ? t("vault:use.meCopied") : t("vault:use.meViewed")) : `${u.actor.name} · ${u.purpose}`}
    sub={<Dots>{showItem && <span>{u.item_title}</span>}<span>{MODE_LABEL[u.mode] ?? u.mode}</span>{u.status !== "ok" && <span>{USE_STATUS[u.status] ?? u.status}</span>}{u.work_item_id && <span className="font-mono">{u.work_item_id}</span>}</Dots>}>
    <span className="type-footnote text-muted-foreground">{tm(u.created)}</span></Row>
}

/* ───────── 启用保险箱 → 恢复码 ───────── */
export function RecoveryCode({ code, onDone, title }: { code: string; onDone: () => void; title?: string }) {
  const t = useT()
  const [saved, setSaved] = useState(false)
  const groups = code.split(/[-\s]+/).filter(Boolean)
  const print = () => { const w = window.open("", "_blank", "width=600,height=400"); if (!w) return; w.document.title = t("vault:recovery.printTitle"); const pre = w.document.createElement("pre"); pre.style.cssText = "font:20px/1.8 ui-monospace,monospace;padding:32px"; pre.textContent = t("vault:recovery.printTitle") + "\n\n" + groups.join("  ") + "\n\n" + date(new Date(), { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" }); w.document.body.appendChild(pre); w.print(); w.close() }
  return (
    <div className={cn(LIFT, "flex flex-col gap-4 p-5 anim-in")} data-recovery>
      <div><div className="type-headline">{title ?? t("vault:recovery.title")}</div><div className="mt-1 type-callout text-muted-foreground">{t("vault:recovery.body")}</div></div>
      <div className="grid grid-cols-2 gap-2 rounded-lg bg-muted/60 px-4 py-3 font-mono type-headline font-medium tracking-wider sm:grid-cols-4" aria-label={t("vault:recovery.label")} data-recovery-code>{groups.map((g, n) => <span key={n} className="select-all">{g}</span>)}</div>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" onClick={() => navigator.clipboard?.writeText(code).then(() => toast(t("vault:recovery.copied")), () => toast(t("vault:clipboardDenied")))}><CopyIcon />{t("common:action.copy")}</Button>
        <Button size="sm" variant="outline" onClick={print}><PrinterIcon />{t("vault:recovery.print")}</Button>
      </div>
      <label className="flex items-center gap-2.5 type-callout"><Checkbox checked={saved} onCheckedChange={(v) => setSaved(!!v)} data-recovery-saved />{t("vault:recovery.saved")}</label>
      <Button className="self-start" disabled={!saved} onClick={onDone} data-recovery-done>{t("vault:recovery.done")}</Button>
    </div>
  )
}
function EnableVault({ onDone }: { onDone: () => void }) {
  const t = useT()
  const [code, setCode] = useState<string | null>(null), [busy, act] = useBusy()
  if (code) return <RecoveryCode code={code} onDone={() => { setCode(null); onDone() }} />
  return (
    <div className={cn(LIFT, "flex flex-col gap-4 p-5")} data-enable-vault>
      <div className="flex items-start gap-3"><Ico I={LockIcon} className="size-10 rounded-xl bg-blue/10 text-blue" /><div><div className="type-headline">{t("vault:enable.title")}</div>
        <div className="mt-1 type-callout text-muted-foreground">{t("vault:enable.body")}</div></div></div>
      <div className="type-callout text-muted-foreground">{t("vault:enable.note")}</div>
      <Button className="self-start" disabled={busy} onClick={() => void act(() => vaultApi.init()).then((r) => r && setCode(r.recovery_code))} data-vault-init>{busy && <Spinner />}{t("vault:enable.title")}</Button>
    </div>
  )
}
function KeychainBanner({ s }: { s: RVaultStatus }) {
  const t = useT()
  return s.initialized && s.keychain !== "ok" ? <Notice tone="attention" className="mb-8" role="alert" data-keychain={s.keychain}>{t("vault:keychainBanner", { state: keyStoreState(s) })}{s.keychain === "missing" && <> · <a className="font-medium text-blue hover:underline" href="#/settings/vault">{t("vault:recoverWithCode")}</a></>}</Notice> : null
}

/* ═════════ 保险箱首页 ═════════ */
/* the filters survive leaving the page; vtpl is a template name as shown, "" for all */
let vtab: "all" | "personal" | "shared" = "all", vtpl = ""
function VaultView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [newItem, setNewItem] = useState(false)
  const L = useLoad(async () => {
    const status = await vaultApi.status()
    if (!status.initialized) return { status, items: [] as RItem[], uses: null as RUse[] | null }
    const [items, uses] = await Promise.all([vaultApi.items(), usesOr(vaultApi.uses({ limit: 6 }))])
    return { status, items, uses }
  }, [], ["vault"])
  if (L.s === "unwired") return <Unwired id="vault" />
  if (L.s === "loading") return <Page title={t("vault:title")} back={back()}><Loading /></Page>
  if (L.s === "error") return <Page title={t("vault:title")} back={back()}><Empty title={L.err} /></Page>
  const { status, items, uses } = L.data
  const sub = t("vault:home.sub")
  if (!status.initialized) return <Page title={t("vault:title")} back={back()} sub={sub} max={640}><EnableVault onDone={() => void L.reload()} /></Page>
  const scope = vtab === "all" ? null : vtab
  const tpls = [...new Set(items.map((v) => v.template))]
  const allTpl = t("vault:scope.all"), tplOpts = [allTpl, ...tpls.map(tplName)]
  const curTpl = vtpl && tplOpts.includes(vtpl) ? vtpl : allTpl
  const xs = items.filter((v) => (!scope || v.vault.scope === scope) && (curTpl === allTpl || tplName(v.template) === curTpl))
  return (
    <Page title={t("vault:title")} back={back()} sub={sub} max={760}
      actions={<><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => go("#/settings/vault")} data-vault-settings>{t("vault:home.settings")}</Button><Button size="sm" onClick={() => setNewItem(true)} disabled={status.keychain !== "ok"} data-new-vault><PlusIcon />{t("vault:home.newItem")}</Button></>}
      toolbar={<Segmented value={vtab} onChange={(v) => { vtab = v; r() }} options={[["all", t("vault:scope.all")], ["personal", t("vault:scope.personal")], ["shared", t("vault:scope.shared")]] as const} />}>
      {newItem && <NewVaultDialog onClose={() => setNewItem(false)} />}
      <KeychainBanner s={status} />
      <div className="flex flex-col gap-8">
        <div>{tpls.length > 1 && <Chips className="mb-4" value={curTpl} onChange={(v) => { vtpl = v === allTpl ? "" : v; r() }} options={tplOpts} />}
          {xs.length ? <Panel>{xs.map((v) => <VaultRow key={v.id} v={v} />)}</Panel> : <Empty title={items.length ? t("vault:home.noMatch") : t("vault:home.noItems")} sub={items.length ? undefined : status.keychain === "ok" ? t("vault:home.noItemsSub") : t("vault:home.noItemsLockedSub")} />}</div>
        {uses && <Section label={t("vault:home.recentUses")}>{uses.length ? <Panel>{uses.map((u) => <UseRow key={u.id} u={u} showItem />)}</Panel> : <Quiet>{t("vault:home.noUses")}</Quiet>}</Section>}
        <Section label={t("vault:home.rules")}><Panel>
          <Row title={t("vault:home.ruleBotTitle")} sub={t("vault:home.ruleBotSub")} />
          <Row title={t("vault:home.ruleHighTitle")} sub={t("vault:home.ruleHighSub")} />
          <Row title={t("vault:home.ruleViewTitle")} sub={status.passkeys ? t("vault:home.ruleViewSub") : t("vault:home.ruleViewSubNoPasskey")} onClick={() => go("#/settings/vault")}><Chevron /></Row>
          <Row icon={<Ico I={LinkIcon} />} title={<span className="text-muted-foreground">{t("vault:home.externalTitle")}</span>} sub={t("vault:home.externalSub")}><Pill>{t("vault:home.reserved")}</Pill></Row>
        </Panel></Section>
      </div>
    </Page>
  )
}

/* ═════════ 条目 ═════════ */
function VaultItemView({ id }: { id: string }) {
  const t = useT()
  const L = useLoad(async () => {
    const [item, uses] = await Promise.all([vaultApi.item(id), usesOr(vaultApi.uses({ item: id, limit: 50 }))])
    return { item, uses }
  }, [id], ["vault"])
  const [del, setDel] = useState(false), [edit, setEdit] = useState<RField | null>(null), [, act] = useBusy()
  const reload = L.reload
  const used = useCallback(() => void reload(), [reload])
  const R = useReveal(id, used)
  if (L.s === "unwired") return <Unwired id="vault" />
  if (L.s === "loading") return <Page title={t("vault:title")} back={backVault()}><Loading /></Page>
  if (L.s === "error") return <Page title={t("vault:title")} back={backVault()}><Empty title={L.status === 404 ? t("vault:item.notFound") : L.err} /></Page>
  const { item: v, uses } = L.data
  const fields = v.fields ?? []
  const setSens = async (x: Sensitivity) => {
    const n = v.grants.length
    if (await act(() => vaultApi.patchItem(v.id, { sensitivity: x }), x === "high" && n ? t("vault:item.madeHigh", { count: n }) : t("common:state.saved"))) void reload()
  }
  return (
    <Page title={v.title} back={backVault()} max={680} sub={<Meta><span>{tplName(v.template)}</span><span>{t("vault:item.vaultName", { name: v.vault.name })}</span>{v.site && <span>{v.site}</span>}<span>{t("vault:row.updated", { time: tm(v.updated) })}</span></Meta>}
      actions={<Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDel(true)} data-del-vault>{t("common:action.delete")}</Button>}>
      {R.dialog}
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("vault:item.deleteTitle", { title: v.title })}</AlertDialogTitle><AlertDialogDescription>{t("vault:item.deleteBody", { count: v.grants.length })}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={async () => { if (await act(() => vaultApi.deleteItem(v.id), t("vault:item.deleted"))) go("#/vault") }} data-del-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditField item={v.id} f={edit} onClose={() => setEdit(null)} onDone={() => { R.hide(edit.id); void reload() }} />}
      <div className="flex flex-col gap-8">
        <div className="-mt-2 flex items-center gap-3"><Ico I={TPL_I[v.template] ?? KeyRoundIcon} className="size-12 rounded-xl bg-card shadow-[var(--lift)] [&>svg]:size-5" />
          <div className="flex items-center gap-1 type-callout text-muted-foreground">{t("vault:item.sensitivity")}{v.template === "bank_card" ? <span className="px-2 text-foreground" title={t("vault:item.bankCardFixed")}>{SENS_LABEL.high}</span>
            : <Select quiet testId="vault-level" value={v.sensitivity} onChange={(x) => void setSens(x as Sensitivity)} options={(["normal", "sensitive", "high"] as const).map((x) => ({ v: x, l: SENS_LABEL[x] }))} />}</div></div>
        <Section label={t("vault:item.fields")}>{fields.length ? <Panel>{fields.map((fl) => {
          const s = R.shown[fl.id], masked = fl.secret && !s
          return (
            <Row key={fl.id} title={fl.label} data-field={fl.label}
              sub={s ? <Countdown until={s.until} /> : undefined}>
              <span className={"max-w-[16rem] truncate type-callout sm:max-w-[22rem] " + (fl.secret ? "font-mono" : "")} data-field-value={masked ? "masked" : "shown"}>{masked ? (fl.hint || "••••••••") : s ? s.value : (fl.value ?? "")}</span>
              {fl.secret && (s ? <Button variant="ghost" size="icon-xs" aria-label={t("vault:item.hideField", { label: fl.label })} className="text-muted-foreground" onClick={() => R.hide(fl.id)} data-hide><EyeOffIcon /></Button>
                : <Button variant="ghost" size="icon-xs" aria-label={t("vault:item.showField", { label: fl.label })} className="text-muted-foreground" disabled={R.busy === fl.id} onClick={() => void R.reveal(fl.id, false)} data-reveal>{R.busy === fl.id ? <Spinner /> : <EyeIcon />}</Button>)}
              <Button variant="ghost" size="icon-xs" aria-label={t("vault:item.copyField", { label: fl.label })} className="text-muted-foreground" disabled={R.busy === fl.id} onClick={() => { if (fl.secret) void R.reveal(fl.id, true); else navigator.clipboard?.writeText(fl.value ?? "").then(() => toast(t("common:action.copied")), () => toast(t("vault:clipboardDenied"))) }} data-copy><CopyIcon /></Button>
              <Button variant="ghost" size="icon-xs" aria-label={t("vault:item.editField", { label: fl.label })} className="text-muted-foreground" onClick={() => setEdit(fl)} data-edit-field><PencilIcon /></Button>
            </Row>)
        })}</Panel> : <Quiet>{t("vault:item.noFields")}</Quiet>}
          <Note>{t("vault:item.fieldsNote")}</Note></Section>
        <LinkedAccount v={v} onChange={() => void reload()} />
        <Grants v={v} onChange={() => void reload()} />
        {uses && <Section label={t("vault:item.useLog")}>{uses.length ? <Panel>{uses.map((u) => <UseRow key={u.id} u={u} />)}</Panel> : <Quiet>{t("vault:item.neverUsed")}</Quiet>}
          <Note>{t("vault:item.useLogNote")}</Note></Section>}
      </div>
    </Page>
  )
}

/** replace a field's whole value; a secret is never read back for editing */
function EditField({ item, f, onClose, onDone }: { item: string; f: RField; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [val, setVal] = useState(f.secret ? "" : f.value ?? ""), [busy, act] = useBusy()
  const save = async () => { const r = await act(() => vaultApi.putField(item, f.id, val), t("common:state.saved")); setVal(""); if (r) { onDone(); onClose() } }
  return (
    <AlertDialog open onOpenChange={(o) => !o && onClose()}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("vault:edit.title", { label: f.label })}</AlertDialogTitle><AlertDialogDescription>{f.secret ? t("vault:edit.secretBody") : t("vault:edit.plainBody")}</AlertDialogDescription></AlertDialogHeader>
      <form onSubmit={(e) => { e.preventDefault(); if (!busy) void save() }} autoComplete="off">
        <Input aria-label={f.label} autoFocus className="font-mono" type={f.secret ? "password" : "text"} autoComplete={f.secret ? "new-password" : "off"} spellCheck={false} value={val} onChange={(e) => setVal(e.target.value)} data-edit-value />
      </form>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction disabled={busy || (f.secret && !val)} onClick={(e) => { e.preventDefault(); void save() }} data-edit-field-save>{t("common:action.save")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  )
}

function LinkedAccount({ v, onChange }: { v: RItem; onChange: () => void }) {
  const t = useT()
  const [opts, setOpts] = useState<RAccount[] | null>(null), [busy, act] = useBusy()
  const load = () => { if (!opts) accountsApi.accounts({ scope: "mine" }).then(setOpts, (e) => { if (!(e instanceof NotWired)) report(e); setOpts([]) }) }
  const set = async (acc: string) => { if (await act(() => vaultApi.patchItem(v.id, { account_id: acc || null }), acc ? t("vault:account.linked") : t("vault:account.unlinked"))) onChange() }
  return (
    <Section label={t("vault:account.section")} right={opts === null ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={load} data-change-account>{v.account ? t("vault:account.change") : t("vault:account.link")}</Button> : undefined}>
      {opts !== null && <div className="mb-2 flex items-center gap-2"><Select testId="vault-account" value={v.account?.id ?? ""} onChange={(x) => void set(x)} options={[{ v: "", l: t("vault:account.none") }, ...opts.map((a) => ({ v: a.id, l: t("vault:account.option", { name: a.display_name || a.handle, system: a.system.name }) }))]} />{busy && <Spinner />}</div>}
      <Panel>{v.account ? <Row icon={<Ico I={KeyRoundIcon} />} title={v.account.display_name} onClick={() => go("#/accounts/" + v.account!.id)}><Chevron /></Row>
        : <Row title={t("vault:account.noneTitle")} sub={t("vault:account.noneSub")} />}</Panel>
    </Section>
  )
}

function Grants({ v, onChange }: { v: RItem; onChange: () => void }) {
  const t = useT()
  const bots = Object.values(st.bots).filter((b) => !b.main)
  const [gBot, setGBot] = useState(bots[0]?.id ?? ""), [gScope, setGScope] = useState(""), [gFor, setGFor] = useState<"1" | "8" | "24">("1"), [busy, act] = useBusy()
  const scopes = useMemo(() => {
    const def = st.bots[gBot]?.project
    const ps = [...st.projects.filter((p) => !p.archived)].sort((a, b) => (a.id === def ? -1 : b.id === def ? 1 : 0))
    return [...ps.map((p) => ({ v: "p:" + p.id, l: t("vault:grants.project", { name: p.name }) })), { v: "scratch", l: t("vault:grants.scratch") }, ...(v.site ? [{ v: "s:" + v.site, l: t("vault:grants.site", { site: v.site }) }] : [])]
  }, [gBot, v.site, st.ver, t])
  const scope = scopes.some((s) => s.v === gScope) ? gScope : scopes[0]?.v ?? ""
  const add = async () => {
    const sc = scope.startsWith("p:") ? { project_id: scope.slice(2) } : scope.startsWith("s:") ? { site: scope.slice(2) } : { scratch: true }
    if (await act(() => vaultApi.grant(v.id, { bot_id: gBot, scope: sc, hours: +gFor as 1 | 8 | 24 }), t("vault:grants.granted"))) onChange()
  }
  const live = v.grants.filter((g) => g.until > Date.now())
  return (
    <Section label={t("vault:grants.section")}>
      {live.length ? <Panel>{live.map((g) => <Row key={g.id} icon={<BotAvatar k={g.bot_id} size={24} />} title={botName(g.bot_id)} sub={t("vault:grants.rowSub", { scope: g.scope_label, time: tm(g.until) })} data-grant={g.bot_id}>
        <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => vaultApi.revokeGrant(g.id), t("vault:grants.revoked")).then((r) => r && onChange())} data-revoke-grant>{t("vault:grants.revoke")}</Button></Row>)}</Panel>
        : v.sensitivity === "high" || v.template === "bank_card" ? null : <Quiet>{t("vault:grants.none")}</Quiet>}
      {v.sensitivity === "high" || v.template === "bank_card" ? <Note>{t("vault:grants.highNote")}</Note> :
        bots.length === 0 ? <Note>{t("vault:grants.noBots")}</Note> :
        <form className="mt-3 flex flex-wrap items-center gap-2" data-new-grant onSubmit={(e) => { e.preventDefault(); void add() }}>
          <Select testId="grant-bot" value={gBot} onChange={setGBot} options={bots.map((b) => ({ v: b.id, l: botName(b.id) }))} />
          <Select testId="grant-scope" value={scope} onChange={setGScope} options={scopes} />
          <Select testId="grant-for" value={gFor} onChange={(x) => setGFor(x as "1" | "8" | "24")} options={(["1", "8", "24"] as const).map((x) => ({ v: x, l: t("vault:grants.hours", { count: +x }) }))} />
          <Button size="sm" variant="outline" type="submit" disabled={busy || !gBot || !scope} data-add-grant>{t("vault:grants.grant")}</Button></form>}
    </Section>
  )
}
