/* 接入账号 (channels-personas.md §2.2, §8; prototype channels.tsx AccountView): health, who it speaks as, the default
 * response rules, your trigger words, the permission check, “授权你本人”, and its groups and DMs. Route #/channels/acc/:id
 * (connection or account id). Re-authorizing and removing the account live on the accounts page; connecting on 接入. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { BanIcon, ChevronRightIcon, RefreshCwIcon, XIcon } from "lucide-react"
import { Checkbox } from "@/components/ui/checkbox"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { go, Page, Group, Row, SectionLabel, Segmented, Meta, Empty, StatusDot } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { tm, toast } from "@/store"
import { useLoad, useBusy } from "@/accounts/load"
import { accountsApi, CONN_STATUS } from "@/api/accounts"
import { channelsApi, TRIGGER_ALL_DMS, TRIGGER_EVERYWHERE, attentionOf, isDm, spaceTitle, type ChannelAccount, type Rule, type Space, type Trigger } from "@/api/channels"
import { AttentionTag, Name, SpaceAvatar, kindLine } from "@/channels/space-bits"
import { Rules, ruleText, ruleOk } from "@/channels/rules"
import { PersonaAvatar } from "@/channels/basis"
import { PermissionList, SelfButtons } from "@/integrations/feishu"
import { integrationsApi, type IAdapter } from "@/api/integrations"
import { receiveNote, selfSidesFor } from "@/integrations/sides"
import { useT } from "@/i18n"

const Note = ({ children }: { children: React.ReactNode }) => <div className="mt-3 type-footnote text-muted-foreground">{children}</div>
const TRIGGER_WORD = /^\/[A-Za-z0-9_]{1,20}$/
/** the undo windows an account can choose (seconds; the Hub takes 0–60) */
const SEND_DELAYS = [0, 3, 5, 10, 20, 30, 60]

export function AccountView({ id }: { id: string }) {
  const t = useT()
  const L = useLoad(async () => {
    const a = await channelsApi.account(id)
    const [personas, ig] = await Promise.all([channelsApi.personas(), integrationsApi.all().catch(() => null)])
    /* “你本人” through this bot: a side acting as you that pairs with this bot's adapter, and its connections */
    const adapters: IAdapter[] = ig?.providers.flatMap((p) => p.adapters) ?? []
    const sides = a && a.mode === "bot" ? selfSidesFor(adapters, a.connection.adapter) : []
    const all = sides.length ? await accountsApi.connections() : []
    const self = all.filter((c) => sides.some((x) => x.id === c.adapter))
    const side = (sides[0] as IAdapter | undefined) ?? null
    return { a, personas, self, side }
  }, [id], ["connection", "account", "space", "persona", "channel"])
  const [busy, act] = useBusy(), [rules, setRules] = useState<Rule[] | null>(null)
  if (L.s === "unwired") return <Unwired id="accounts" />
  const back: [string, string] = ["#/channels", t("channels:list.title")]
  if (L.s === "loading") return <Page title={t("channels:account.title")} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title={t("channels:account.title")} back={back}><Empty title={L.err} /></Page>
  const { a, personas, self, side } = L.data
  if (!a) return <Page title={t("channels:account.title")} back={back}><Empty title={t("channels:account.notFound")} sub={t("channels:account.notFoundSub")} /></Page>
  const me = a.mode === "self", c = a.connection
  const [st, dot] = CONN_STATUS[c.status] ?? [c.status, "idle"]
  const patch = async (b: Parameters<typeof channelsApi.patchAccount>[1], ok: string) => { const r = await act(() => channelsApi.patchAccount(c.id, b), ok); if (r) await L.reload(); return !!r }
  const rulesNow = rules ?? a.rules
  return (
    <Page title={a.name} back={back} max={700} sub={<Meta><span>{a.provider.name}</span><span>{t(me ? "channels:word.you" : "channels:word.bot")}</span><span className="font-mono">{a.account.handle}</span></Meta>}
      actions={<Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => go("#/accounts/" + a.account.id)} data-open-account>{t("channels:account.accountAuth")}</Button>}>
      <div className="flex flex-col gap-8" data-channel-account={c.id}>
        <section><SectionLabel>{t("channels:account.status")}</SectionLabel>
          <Group><Row title={<span className="inline-flex items-center gap-2"><StatusDot s={dot} />{st}</span>} data-conn-state
            sub={[c.health.last_ok ? t("channels:account.lastOk", { time: tm(c.health.last_ok) }) : "", c.status !== "ok" && c.health.last_error ? c.health.last_error : "", (() => { const n = receiveNote(a.receive); return n ? t(`channels:account.${n}`) : "" })()].filter(Boolean).join(" · ")}>
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => accountsApi.test(c.id)).then((r) => { if (r) { toast(t(r.status === "ok" ? "channels:account.testOk" : "channels:account.testFailed")); void L.reload() } })} data-conn-test><RefreshCwIcon />{t("channels:account.test")}</Button></Row>
            {(c.status === "reauth" || c.status === "error") && <Row title={t("channels:account.reauth")} sub={t("channels:account.reauthSub")} onClick={() => go(`#/accounts/${a.account.id}/reauth`)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>}
          </Group></section>
        <section><SectionLabel>{t("channels:account.asWhom")}</SectionLabel>
          <Group>{me
            ? <Row icon={<PersonaAvatar bot={null} size={32} />} title={t("channels:word.you")} sub={t("channels:account.selfSub")} />
            : <Row icon={<PersonaAvatar bot={a.personaBot} size={32} />} title={<span>{t("channels:account.defaultPersona", { name: a.persona?.name ?? "—" })}</span>} sub={t("channels:account.defaultPersonaSub")} onClick={!a.editable && a.persona ? () => go("#/channels/personas/" + a.persona!.id) : undefined}>
              {a.editable ? <Select align="end" testId="acc-default-persona" value={a.persona?.id ?? ""} onChange={(v) => void patch({ persona_id: v }, t("channels:account.personaChanged"))} options={personas.map((p) => ({ v: p.id, l: p.name }))} />
                : a.persona && <ChevronRightIcon className="size-4 text-tertiary" aria-label={t("channels:word.editPersona")} />}</Row>}
          </Group>
          {!me && !a.editable && <Note>{t("channels:account.personaReadonly")}</Note>}</section>
        <section><SectionLabel>{t("channels:account.defaultRules")}</SectionLabel>
          {a.editable && rulesNow ? <>
            <Rules rules={rulesNow} self={me} onChange={setRules} note={t("channels:account.rulesNote")} />
            {rules && <div className="mt-2 flex gap-2"><Button size="sm" disabled={busy || !rules.every(ruleOk)} onClick={() => void patch({ rules }, t("channels:word.rulesSaved")).then((ok) => ok && setRules(null))} data-acc-rules-save>{t("channels:word.saveRules")}</Button><Button size="sm" variant="ghost" onClick={() => setRules(null)}>{t("channels:word.revert")}</Button></div>}
          </> : a.rules ? <><Group>{a.rules.map((r, n) => <Row key={n} title={ruleText(r, me)} />)}</Group><Note>{a.editable ? "" : t("channels:account.rulesReadonly")}</Note></>
            : <div className="rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground">{t("channels:account.rulesUnknown")}</div>}</section>
        {a.editable && a.sendDelay && <section><SectionLabel>{t("channels:account.sendDelay")}</SectionLabel>
          <Group><Row title={t("channels:account.sendDelayTitle")} sub={t("channels:account.sendDelaySub")}>
            <Select align="end" testId="acc-send-delay" value={a.sendDelay.set === null ? "default" : String(a.sendDelay.set)}
              onChange={(v) => void patch({ send_delay_s: v === "default" ? null : Number(v) }, t("channels:account.sendDelaySaved"))}
              options={[{ v: "default", l: t("channels:account.sendDelayDefault", { n: a.sendDelay.def }) }, ...SEND_DELAYS.map((n) => ({ v: String(n), l: t("channels:account.sendDelaySeconds", { n }) }))]} /></Row></Group></section>}
        {me && <TriggerSection a={a} onSave={(x) => patch({ trigger: x }, t("channels:account.triggerSaved"))} busy={busy} />}
        {side && <section><SectionLabel>{t("channels:word.you")}</SectionLabel>
          {self.length ? <Group>{self.map((s) => <Row key={s.id} icon={<PersonaAvatar bot={null} size={28} />} title={t("channels:account.selfAuthorized")} sub={t("channels:account.selfAuthorizedSub", { status: CONN_STATUS[s.status]?.[0] ?? s.status })} onClick={() => go("#/channels/acc/" + s.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <div className="flex flex-col gap-3 rounded-xl bg-card p-5 shadow-[var(--lift)]" data-self-auth>
              <div className="type-callout text-muted-foreground">{t("channels:account.selfAuthHint")}</div>
              <div className="flex flex-wrap justify-end gap-2"><SelfButtons app={c.id} side={side} /></div></div>}</section>}
        {!me && <section><SectionLabel>{t("channels:account.permissions")}</SectionLabel>{a.permissions ? <PermissionList checks={a.permissions} /> : <div className="rounded-xl bg-muted/60 px-4 py-3 type-callout text-muted-foreground" data-permissions="none">{t("channels:account.permissionsNone")}</div>}</section>}
        <section><SectionLabel>{t("channels:account.spacesLabel", { n: a.spaces.length })}</SectionLabel>
          {a.spaces.length ? <Group>{a.spaces.map((s) => (
            <Row key={s.id} icon={<SpaceAvatar s={s} size={28} />} onClick={() => go("#/channels/space/" + s.id)} data-space-row={spaceTitle(s)}
              title={<span className="flex min-w-0 items-center gap-1.5"><Name>{spaceTitle(s)}</Name><span className="shrink-0 type-footnote text-muted-foreground">{kindLine(s)}</span><AttentionTag a={attentionOf(s)} />{s.has_external && <Badge variant="secondary" className="shrink-0 text-muted-foreground">{t("channels:word.external")}</Badge>}{s.muted && <span className="shrink-0 type-footnote text-muted-foreground">{t("channels:word.recordOnly")}</span>}{!s.configured && <Badge variant="secondary" className="shrink-0 bg-attention/12 text-attention">{t("channels:word.new")}</Badge>}</span>}
              sub={<span className="block truncate">{t(s.persona && !s.persona.inherited ? "channels:account.spaceSubOwn" : "channels:account.spaceSub", { who: me ? t("channels:word.you") : s.persona?.name ?? "—", duty: s.duty ? s.duty : t("channels:account.noDuty") })}</span>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
            : <div className="rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground">{t(me ? "channels:account.noSessions" : "channels:account.noGroups")}</div>}
        </section>
      </div>
    </Page>
  )
}

const SCOPE_MAX = 6
type Scope = "all" | "dms" | "some"
const scopeOf = (where: string[]): Scope => !where.length || where.includes(TRIGGER_EVERYWHERE) ? "all" : where.length === 1 && where[0] === TRIGGER_ALL_DMS ? "dms" : "some"

function TriggerSection({ a, onSave, busy }: { a: ChannelAccount; onSave: (t: Partial<Trigger>) => Promise<boolean>; busy: boolean }) {
  const t = useT()
  const [word, setWord] = useState("")
  /* `/bot` is your command anywhere: every space you can see, not only this account's */
  const [all, setAll] = useState<Space[] | null>(null)
  useEffect(() => { let live = true; channelsApi.spaces().then((x) => live && setAll(x), () => live && setAll(a.spaces)); return () => { live = false } }, [a.connection.id])
  const [picking, setPicking] = useState(false), [some, setSome] = useState(false)
  const tr = a.trigger
  if (!a.editable || !tr) return <section><SectionLabel>{t("channels:trigger.label")}</SectionLabel>
    <div className="rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground">{t("channels:trigger.unsupported")}</div></section>
  const add = () => { const w = word.trim().startsWith("/") ? word.trim() : "/" + word.trim(); if (!TRIGGER_WORD.test(w)) { toast(t("channels:trigger.invalid")); return } if (!tr.words.includes(w) && tr.words.length < 10) void onSave({ words: [...tr.words, w] }); setWord("") }
  const spaces = all ?? a.spaces
  const scope: Scope = some ? "some" : scopeOf(tr.where)
  const chosen = tr.where.filter((x) => x !== TRIGGER_ALL_DMS && x !== TRIGGER_EVERYWHERE)
  const byId = new Map(spaces.map((x) => [x.id, x]))
  const blocked = spaces.filter((x) => attentionOf(x) === "blocked")
  const blockedHere = scope === "all" ? blocked : scope === "dms" ? blocked.filter(isDm) : blocked.filter((x) => chosen.includes(x.id))
  const setScope = (v: Scope) => {
    if (v === "some") { setSome(true); if (!chosen.length) setPicking(true); return }
    setSome(false); void onSave({ where: [v === "all" ? TRIGGER_EVERYWHERE : TRIGGER_ALL_DMS] })
  }
  const word0 = tr.words[0] ?? "/bot"
  return (
    <section data-trigger><SectionLabel>{t("channels:trigger.label")}</SectionLabel>
      <Group>
        <Row title={t("channels:trigger.enable")} sub={t("channels:trigger.enableSub")}><Switch aria-label={t("channels:trigger.enable")} checked={tr.on} disabled={busy} onCheckedChange={(v) => void onSave({ on: v })} data-trigger-on /></Row>
        <Row title={t("channels:trigger.words")} sub={t("channels:trigger.wordsSub")}>
          <span className="flex flex-wrap items-center justify-end gap-1">{tr.words.map((w) => <span key={w} className="inline-flex h-6 items-center gap-1 rounded-full bg-muted pl-2.5 pr-1 type-footnote font-mono">{w}<button type="button" aria-label={t("channels:trigger.removeWord", { word: w })} disabled={busy} onClick={() => void onSave({ words: tr.words.filter((x) => x !== w) })} className="flex size-4 items-center justify-center rounded-full hover:bg-background"><XIcon className="size-3" /></button></span>)}
            <Input aria-label={t("channels:trigger.addAria")} className="h-7 w-24 rounded-full md:type-footnote" placeholder={t("channels:trigger.addPlaceholder")} value={word} onChange={(e) => setWord(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && word.trim()) { e.preventDefault(); add() } }} data-trigger-word /></span></Row>
        <div className="flex min-w-0 flex-col gap-2 px-2 py-2.5" data-trigger-scope={scope}>
          <div className="type-body">{t("channels:trigger.where")}</div>
          <Segmented className="w-full [&>div]:w-full" size="sm" value={scope} onChange={setScope} options={[["all", t("channels:trigger.scopeAll")], ["dms", t("channels:trigger.scopeDms")], ["some", t("channels:trigger.scopeSome")]] as const} />
          {scope === "some" && <div className="flex min-w-0 flex-wrap items-center gap-1" data-trigger-chosen>
            {chosen.slice(0, SCOPE_MAX).map((id) => { const x = byId.get(id); const name = x ? spaceTitle(x) : t("channels:trigger.goneSpace"); return (
              <span key={id} className="inline-flex h-6 min-w-0 max-w-[12rem] items-center gap-1 rounded-full bg-primary/10 pl-2.5 pr-1 type-footnote font-medium text-primary" title={name} data-trigger-where={name}>
                <span className="truncate">{name}</span>
                <button type="button" aria-label={t("channels:trigger.removeWhere", { name })} disabled={busy} onClick={() => { const rest = chosen.filter((y) => y !== id); void onSave({ where: rest.length ? rest : [TRIGGER_EVERYWHERE] }); if (!rest.length) setSome(false) }} className="flex size-4 shrink-0 items-center justify-center rounded-full hover:bg-primary/15"><XIcon className="size-3" /></button></span>) })}
            {chosen.length > SCOPE_MAX && <button type="button" onClick={() => setPicking(true)} className="h-6 rounded-full bg-muted px-2.5 type-footnote tabular-nums text-muted-foreground hover:text-foreground" data-trigger-more>+{chosen.length - SCOPE_MAX}</button>}
            <button type="button" onClick={() => setPicking(true)} className="h-6 rounded-full border border-dashed px-2.5 type-footnote text-muted-foreground hover:border-solid hover:text-foreground" data-trigger-pick>{t(chosen.length ? "channels:trigger.change" : "channels:trigger.pick")}</button>
            {!chosen.length && <span className="type-footnote text-muted-foreground">{t("channels:trigger.notPicked")}</span>}
          </div>}
          <div className="type-footnote text-muted-foreground">{scope === "all" ? t("channels:trigger.hintAll", { word: word0 }) : t(scope === "dms" ? "channels:trigger.hintDms" : "channels:trigger.hintSome")}</div>
          {blockedHere.length > 0 && <div className="flex items-start gap-1.5 rounded-lg bg-attention/10 px-3 py-2 type-footnote anim-in" data-trigger-blocked>
            <BanIcon className="mt-px size-3.5 shrink-0 text-attention" aria-hidden /><span className="min-w-0">{blockedHere.length === 1 ? t("channels:trigger.blockedOne", { n: 1, name: spaceTitle(blockedHere[0]), word: word0 }) : t("channels:trigger.blockedMany", { count: blockedHere.length, word: word0 })}</span></div>}
        </div>
        <Row title={t("channels:trigger.mark")} sub={t("channels:trigger.markSub")}><Switch aria-label={t("channels:trigger.markAria")} checked={tr.mark} disabled={busy} onCheckedChange={(v) => void onSave({ mark: v })} /></Row>
      </Group>
      <Note>{t("channels:trigger.note")}</Note>
      {picking && <ScopePicker spaces={spaces} chosen={chosen} onClose={() => setPicking(false)} onSave={async (ids) => { const ok = await onSave({ where: ids.length ? ids : [TRIGGER_EVERYWHERE] }); if (ok) { setPicking(false); setSome(ids.length > 0) } }} busy={busy} />}
    </section>
  )
}

/** the full list of spaces for 指定群, searchable; blocked ones cannot be chosen (屏蔽 wins over trigger words) */
function ScopePicker({ spaces, chosen, onClose, onSave, busy }: { spaces: Space[]; chosen: string[]; onClose: () => void; onSave: (ids: string[]) => void; busy: boolean }) {
  const t = useT()
  const [q, setQ] = useState(""), [sel, setSel] = useState<string[]>(chosen)
  const groups = spaces.filter((x) => !isDm(x) && x.status === "active")
  const shown = groups.filter((x) => !q.trim() || spaceTitle(x).toLowerCase().includes(q.trim().toLowerCase()))
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex max-h-[80vh] flex-col gap-3 rounded-2xl sm:max-w-[460px]" data-scope-picker>
        <DialogHeader><DialogTitle>{t("channels:scope.title")}</DialogTitle><DialogDescription>{t("channels:scope.desc")}</DialogDescription></DialogHeader>
        <Input autoFocus placeholder={t("channels:scope.search")} value={q} onChange={(e) => setQ(e.target.value)} aria-label={t("channels:scope.search")} data-scope-search />
        <div className="-mx-2 min-h-0 flex-1 overflow-y-auto" role="list">
          {shown.length ? shown.map((x) => { const off = attentionOf(x) === "blocked", on = sel.includes(x.id), name = spaceTitle(x); return (
            <label key={x.id} className={cn("flex min-w-0 cursor-pointer items-center gap-2.5 rounded-lg px-2 py-2 hover:bg-accent", off && "cursor-not-allowed opacity-50")} role="listitem" title={name} data-scope-option={name}>
              <Checkbox checked={on} disabled={off} onCheckedChange={(v) => setSel(v ? [...sel, x.id] : sel.filter((y) => y !== x.id))} aria-label={name} />
              <SpaceAvatar s={x} size={24} />
              <span className="min-w-0 flex-1"><span className="block truncate type-body">{name}</span><span className="block truncate type-footnote text-muted-foreground">{off ? t("channels:scope.blocked") : kindLine(x)}</span></span></label>) })
            : <div className="px-2 py-6 text-center type-callout text-muted-foreground">{t("channels:scope.noMatch")}</div>}
        </div>
        <DialogFooter><Button variant="ghost" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={busy} onClick={() => onSave(sel)} data-scope-save>{sel.length ? t("channels:scope.choose", { count: sel.length }) : t("channels:scope.reset")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
