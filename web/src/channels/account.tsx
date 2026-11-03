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
import { channelsApi, FEISHU_APP, FEISHU_USER, TRIGGER_ALL_DMS, TRIGGER_EVERYWHERE, attentionOf, isDm, spaceTitle, type ChannelAccount, type Rule, type Space, type Trigger } from "@/api/channels"
import { AttentionTag, Name, SpaceAvatar, kindLine } from "@/channels/space-bits"
import { Rules, ruleText, ruleOk } from "@/channels/rules"
import { PersonaAvatar } from "@/channels/basis"
import { PermissionList, SelfButtons } from "@/integrations/feishu"

const Note = ({ children }: { children: React.ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>
const TRIGGER_WORD = /^\/[A-Za-z0-9_]{1,20}$/

export function AccountView({ id }: { id: string }) {
  const L = useLoad(async () => {
    const a = await channelsApi.account(id)
    const feishuBot = a?.connection.adapter === FEISHU_APP
    const [personas, all] = await Promise.all([channelsApi.personas(), feishuBot ? accountsApi.connections() : Promise.resolve([])])
    /* Feishu only: “你本人” through the same app, a user connection on this app */
    const self = all.filter((c) => c.adapter === FEISHU_USER)
    return { a, personas, self, feishuBot }
  }, [id], ["connection", "account", "space", "persona", "channel"])
  const [busy, act] = useBusy(), [rules, setRules] = useState<Rule[] | null>(null)
  if (L.s === "unwired") return <Unwired id="accounts" />
  if (L.s === "loading") return <Page title="接入账号" back={["#/channels", "渠道"]}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="接入账号" back={["#/channels", "渠道"]}><Empty title={L.err} /></Page>
  const { a, personas, self, feishuBot } = L.data
  if (!a) return <Page title="接入账号" back={["#/channels", "渠道"]}><Empty title="这个接入账号不存在" sub="可能已经断开，或者不是收消息的账号。" /></Page>
  const me = a.mode === "self", c = a.connection
  const [st, dot] = CONN_STATUS[c.status] ?? [c.status, "idle"]
  const patch = async (b: Parameters<typeof channelsApi.patchAccount>[1], ok: string) => { const r = await act(() => channelsApi.patchAccount(c.id, b), ok); if (r) await L.reload(); return !!r }
  const rulesNow = rules ?? a.rules
  return (
    <Page title={a.name} back={["#/channels", "渠道"]} max={700} sub={<Meta><span>{a.provider.name}</span><span>{me ? "你本人" : "机器人"}</span><span className="font-mono">{a.account.handle}</span></Meta>}
      actions={<Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => go("#/accounts/" + a.account.id)} data-open-account>账号与授权</Button>}>
      <div className="flex flex-col gap-8" data-channel-account={c.id}>
        <section><SectionLabel>状态</SectionLabel>
          <Group><Row title={<span className="inline-flex items-center gap-2"><StatusDot s={dot} />{st}</span>} data-conn-state
            sub={[c.health.last_ok ? `上次正常 ${tm(c.health.last_ok)}` : "", c.status !== "ok" && c.health.last_error ? c.health.last_error : "", feishuBot ? "长连接接收事件" : c.adapter === FEISHU_USER ? "约 20 秒轮询一次你的会话" : ""].filter(Boolean).join(" · ")}>
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => accountsApi.test(c.id)).then((r) => { if (r) { toast(r.status === "ok" ? "连接正常" : "检查没有通过"); void L.reload() } })} data-conn-test><RefreshCwIcon />检查</Button></Row>
            {(c.status === "reauth" || c.status === "error") && <Row title="要重新授权" sub="在账号页重新授权或重新填写凭据" onClick={() => go(`#/accounts/${a.account.id}/reauth`)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>}
          </Group></section>
        <section><SectionLabel>以谁的身份</SectionLabel>
          <Group>{me
            ? <Row icon={<PersonaAvatar bot={null} size={32} />} title="你本人" sub="别人看到的是你在说话。Steve 默认只起草，经你批准才发出；可以用下面的触发语放开" />
            : <Row icon={<PersonaAvatar bot={a.personaBot} size={32} />} title={<span>默认对外身份 · {a.persona?.name ?? "—"}</span>} sub="没有单独设置的群都用这个身份；平台上显示的名字和头像以平台为准" onClick={!a.editable && a.persona ? () => go("#/channels/personas/" + a.persona!.id) : undefined}>
              {a.editable ? <Select align="end" testId="acc-default-persona" value={a.persona?.id ?? ""} onChange={(v) => void patch({ persona_id: v }, "已换默认身份")} options={personas.map((p) => ({ v: p.id, l: p.name }))} />
                : a.persona && <ChevronRightIcon className="size-4 text-tertiary" aria-label="编辑身份" />}</Row>}
          </Group>
          {!me && !a.editable && <Note>这个 Hub 还不能改账号的默认身份；每个群可以在群页单独换。</Note>}</section>
        <section><SectionLabel>默认响应规则</SectionLabel>
          {a.editable && rulesNow ? <>
            <Rules rules={rulesNow} self={me} onChange={setRules} note="每个群可以单独覆盖。更复杂的判断用“流程”编排。" />
            {rules && <div className="mt-2 flex gap-2"><Button size="sm" disabled={busy || !rules.every(ruleOk)} onClick={() => void patch({ rules }, "已保存规则").then((ok) => ok && setRules(null))} data-acc-rules-save>保存规则</Button><Button size="sm" variant="ghost" onClick={() => setRules(null)}>还原</Button></div>}
          </> : a.rules ? <><Group>{a.rules.map((r, n) => <Row key={n} title={ruleText(r, me)} />)}</Group><Note>{a.editable ? "" : "这个 Hub 还不能在这里改账号的默认规则；每个群可以在群页单独设置。"}</Note></>
            : <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground">还看不到默认规则：它加入第一个群后，这里显示群里跟随的默认规则。</div>}</section>
        {me && <TriggerSection a={a} onSave={(t) => patch({ trigger: t }, "已保存触发语")} busy={busy} />}
        {feishuBot && <section><SectionLabel>你本人</SectionLabel>
          {self.length ? <Group>{self.map((s) => <Row key={s.id} icon={<PersonaAvatar bot={null} size={28} />} title="已授权你本人" sub={(CONN_STATUS[s.status]?.[0] ?? s.status) + " · Steve 替你看私聊，默认只起草"} onClick={() => go("#/channels/acc/" + s.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <div className="flex flex-col gap-3 rounded-xl border px-4 py-3.5" data-self-auth>
              <div className="type-callout text-muted-foreground">通过这个应用授权你本人：Steve 约 20 秒读一次你的私聊和群消息，别人找你时起草回复等你批准。机器人看到你发的消息时，也会认出是你。</div>
              <div className="flex flex-wrap justify-end gap-2"><SelfButtons app={c.id} /></div></div>}</section>}
        {!me && <section><SectionLabel>权限</SectionLabel>{a.permissions ? <PermissionList checks={a.permissions} /> : <div className="rounded-lg bg-muted/60 px-3 py-2 type-callout text-muted-foreground" data-permissions="none">这个 Hub 没有返回权限检查的结果，以平台的开发者后台为准。开通新权限后要在后台发布新版本，再点上面的“检查”。</div>}</section>}
        <section><SectionLabel>所在的群和私聊 · {a.spaces.length}</SectionLabel>
          {a.spaces.length ? <Group>{a.spaces.map((s) => (
            <Row key={s.id} icon={<SpaceAvatar s={s} size={28} />} onClick={() => go("#/channels/space/" + s.id)} data-space-row={spaceTitle(s)}
              title={<span className="flex min-w-0 items-center gap-1.5"><Name>{spaceTitle(s)}</Name><span className="shrink-0 type-footnote text-muted-foreground">{kindLine(s)}</span><AttentionTag a={attentionOf(s)} />{s.has_external && <Badge variant="outline" className="shrink-0 font-normal text-muted-foreground">外部</Badge>}{s.muted && <span className="shrink-0 type-footnote text-muted-foreground">只记录</span>}{!s.configured && <Badge variant="outline" className="shrink-0 font-normal text-attention">新</Badge>}</span>}
              sub={<span className="block truncate">{`${me ? "你本人" : s.persona?.name ?? "—"}${s.persona && !s.persona.inherited ? "（本群单独设置）" : ""} · ${s.duty ? s.duty : "还没写职责"}`}</span>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
            : <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground">{me ? "还没有会话：授权后第一次轮询到的会话会出现在这里。" : "还没有加入任何群：把它拉进群，或者给它发一条私聊。"}</div>}
        </section>
      </div>
    </Page>
  )
}

const SCOPE_MAX = 6
type Scope = "all" | "dms" | "some"
const scopeOf = (where: string[]): Scope => !where.length || where.includes(TRIGGER_EVERYWHERE) ? "all" : where.length === 1 && where[0] === TRIGGER_ALL_DMS ? "dms" : "some"

function TriggerSection({ a, onSave, busy }: { a: ChannelAccount; onSave: (t: Partial<Trigger>) => Promise<boolean>; busy: boolean }) {
  const [word, setWord] = useState("")
  /* `/bot` is your command anywhere: every space you can see, not only this account's */
  const [all, setAll] = useState<Space[] | null>(null)
  useEffect(() => { let live = true; channelsApi.spaces().then((x) => live && setAll(x), () => live && setAll(a.spaces)); return () => { live = false } }, [a.connection.id])
  const [picking, setPicking] = useState(false), [some, setSome] = useState(false)
  const t = a.trigger
  if (!a.editable || !t) return <section><SectionLabel>触发语 · 让 Steve 直接以你的名义发出</SectionLabel>
    <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground">这个 Hub 还不能设置触发语。没有触发语时，以你本人发出的消息一律先起草，经你批准才发。</div></section>
  const add = () => { const w = word.trim().startsWith("/") ? word.trim() : "/" + word.trim(); if (!TRIGGER_WORD.test(w)) { toast("触发语以 / 开头，后面是 1–20 个字母、数字或下划线"); return } if (!t.words.includes(w) && t.words.length < 10) void onSave({ words: [...t.words, w] }); setWord("") }
  const spaces = all ?? a.spaces
  const scope: Scope = some ? "some" : scopeOf(t.where)
  const chosen = t.where.filter((x) => x !== TRIGGER_ALL_DMS && x !== TRIGGER_EVERYWHERE)
  const byId = new Map(spaces.map((x) => [x.id, x]))
  const blocked = spaces.filter((x) => attentionOf(x) === "blocked")
  const blockedHere = scope === "all" ? blocked : scope === "dms" ? blocked.filter(isDm) : blocked.filter((x) => chosen.includes(x.id))
  const setScope = (v: Scope) => {
    if (v === "some") { setSome(true); if (!chosen.length) setPicking(true); return }
    setSome(false); void onSave({ where: [v === "all" ? TRIGGER_EVERYWHERE : TRIGGER_ALL_DMS] })
  }
  const word0 = t.words[0] ?? "/bot"
  return (
    <section data-trigger><SectionLabel>触发语 · 让 Steve 直接以你的名义发出</SectionLabel>
      <Group>
        <Row title="启用触发语" sub="只有你本人发的触发语有效；别人发不会让 Steve 以你的名义说话"><Switch aria-label="启用触发语" checked={t.on} disabled={busy} onCheckedChange={(v) => void onSave({ on: v })} data-trigger-on /></Row>
        <Row title="触发语" sub="消息以它开头时（前面 @ 了谁也算），Steve 按后面的话完成并直接发出">
          <span className="flex flex-wrap items-center justify-end gap-1">{t.words.map((w) => <span key={w} className="inline-flex h-6 items-center gap-1 rounded-full bg-muted pl-2.5 pr-1 font-mono text-[12px]">{w}<button type="button" aria-label={"删除 " + w} disabled={busy} onClick={() => void onSave({ words: t.words.filter((x) => x !== w) })} className="flex size-4 items-center justify-center rounded-full hover:bg-background"><XIcon className="size-3" /></button></span>)}
            <Input aria-label="添加触发语，回车确认" className="h-7 w-24 rounded-full text-[12px]" placeholder="添加…" value={word} onChange={(e) => setWord(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && word.trim()) { e.preventDefault(); add() } }} data-trigger-word /></span></Row>
        <div className="flex min-w-0 flex-col gap-2 px-2 py-2.5" data-trigger-scope={scope}>
          <div className="type-body">在哪里生效</div>
          <Segmented className="w-full [&>div]:w-full" size="sm" value={scope} onChange={setScope} options={[["all", "所有群和私聊"], ["dms", "所有私聊"], ["some", "指定群"]] as const} />
          {scope === "some" && <div className="flex min-w-0 flex-wrap items-center gap-1" data-trigger-chosen>
            {chosen.slice(0, SCOPE_MAX).map((id) => { const x = byId.get(id); const name = x ? spaceTitle(x) : "已不在的空间"; return (
              <span key={id} className="inline-flex h-6 min-w-0 max-w-[12rem] items-center gap-1 rounded-full bg-foreground pl-2.5 pr-1 text-[12px] text-background" title={name} data-trigger-where={name}>
                <span className="truncate">{name}</span>
                <button type="button" aria-label={"移除 " + name} disabled={busy} onClick={() => { const rest = chosen.filter((y) => y !== id); void onSave({ where: rest.length ? rest : [TRIGGER_EVERYWHERE] }); if (!rest.length) setSome(false) }} className="flex size-4 shrink-0 items-center justify-center rounded-full hover:bg-background/20"><XIcon className="size-3" /></button></span>) })}
            {chosen.length > SCOPE_MAX && <button type="button" onClick={() => setPicking(true)} className="h-6 rounded-full bg-muted px-2.5 text-[12px] text-muted-foreground hover:text-foreground" data-trigger-more>+{chosen.length - SCOPE_MAX}</button>}
            <button type="button" onClick={() => setPicking(true)} className="h-6 rounded-full border border-dashed px-2.5 text-[12px] text-muted-foreground hover:text-foreground" data-trigger-pick>{chosen.length ? "修改…" : "选择群…"}</button>
            {!chosen.length && <span className="type-footnote text-muted-foreground">还没选：选好之前仍按“所有群和私聊”生效</span>}
          </div>}
          <div className="type-footnote text-muted-foreground">{scope === "all" ? `在你看得到的所有群和私聊里，${word0} 都能让 Steve 代你说话。其他消息仍然只起草。` : scope === "dms" ? `只在私聊里生效（包括和自己的对话）；群里仍然只起草。` : `只在选中的群里生效；其他地方仍然只起草。`}</div>
          {blockedHere.length > 0 && <div className="flex items-start gap-1.5 rounded-lg bg-attention/10 px-3 py-2 type-footnote" data-trigger-blocked>
            <BanIcon className="mt-px size-3.5 shrink-0 text-attention" aria-hidden /><span className="min-w-0">{blockedHere.length} 个已屏蔽的{blockedHere.length === 1 ? `空间（${spaceTitle(blockedHere[0])}）` : "空间"}里 {word0} 不生效：屏蔽优先于触发语。不提醒的群里照样生效。</span></div>}
        </div>
        <Row title="标明“由 Steve 代发”" sub="在代发的消息末尾加一个小标记"><Switch aria-label="标明由 Steve 代发" checked={t.mark} disabled={busy} onCheckedChange={(v) => void onSave({ mark: v })} /></Row>
      </Group>
      <Note>触发后可以回答、总结、查资料、建事项；对外承诺、花钱、删除仍然需要你批准。</Note>
      {picking && <ScopePicker spaces={spaces} chosen={chosen} onClose={() => setPicking(false)} onSave={async (ids) => { const ok = await onSave({ where: ids.length ? ids : [TRIGGER_EVERYWHERE] }); if (ok) { setPicking(false); setSome(ids.length > 0) } }} busy={busy} />}
    </section>
  )
}

/** the full list of spaces for 指定群, searchable; blocked ones cannot be chosen (屏蔽 wins over trigger words) */
function ScopePicker({ spaces, chosen, onClose, onSave, busy }: { spaces: Space[]; chosen: string[]; onClose: () => void; onSave: (ids: string[]) => void; busy: boolean }) {
  const [q, setQ] = useState(""), [sel, setSel] = useState<string[]>(chosen)
  const groups = spaces.filter((x) => !isDm(x) && x.status === "active")
  const shown = groups.filter((x) => !q.trim() || spaceTitle(x).toLowerCase().includes(q.trim().toLowerCase()))
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex max-h-[80vh] flex-col gap-3 rounded-2xl sm:max-w-[460px]" data-scope-picker>
        <DialogHeader><DialogTitle>{"选择触发语生效的群"}</DialogTitle><DialogDescription>只在选中的群里生效。已屏蔽的群不能选：屏蔽优先于触发语。</DialogDescription></DialogHeader>
        <Input autoFocus placeholder="搜索群名" value={q} onChange={(e) => setQ(e.target.value)} aria-label="搜索群名" data-scope-search />
        <div className="-mx-2 min-h-0 flex-1 overflow-y-auto" role="list">
          {shown.length ? shown.map((x) => { const off = attentionOf(x) === "blocked", on = sel.includes(x.id), name = spaceTitle(x); return (
            <label key={x.id} className={cn("flex min-w-0 cursor-pointer items-center gap-2.5 rounded-lg px-2 py-2 hover:bg-accent", off && "cursor-not-allowed opacity-50")} role="listitem" title={name} data-scope-option={name}>
              <Checkbox checked={on} disabled={off} onCheckedChange={(v) => setSel(v ? [...sel, x.id] : sel.filter((y) => y !== x.id))} aria-label={name} />
              <SpaceAvatar s={x} size={24} />
              <span className="min-w-0 flex-1"><span className="block truncate type-body">{name}</span><span className="block truncate type-footnote text-muted-foreground">{off ? "已屏蔽，触发语在这里不生效" : kindLine(x)}</span></span></label>) })
            : <div className="px-2 py-6 text-center type-callout text-muted-foreground">没有匹配的群</div>}
        </div>
        <DialogFooter><Button variant="ghost" onClick={onClose}>取消</Button><Button disabled={busy} onClick={() => onSave(sel)} data-scope-save>{sel.length ? `选中 ${sel.length} 个群` : "改回所有群和私聊"}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
