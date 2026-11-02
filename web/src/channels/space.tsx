import { ChannelMessageBody } from "@/channels/message-body"
/* 群和私聊 (channels-personas.md §2.2–§2.6, §8; prototype channels.tsx SpaceView): who speaks here, the duty, when to
 * respond, how far its content may travel, collaboration, members and the recent messages with their basis.
 * Route #/channels/space/:id. Changing settings never sends anything to the chat. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon, WorkflowIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { go, Page, Group, Row, SectionLabel, Segmented, Meta, Empty } from "@/ui"
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { botName, st, tm } from "@/store"
import { useLoad, useBusy, report } from "@/accounts/load"
import { channelsApi, ATTENTION, LEVEL, MSG_STATUS, ROLE, attentionOf, spaceTitle, type Attention, type ChanMessage, type Level, type Rule, type SpacePatch } from "@/api/channels"
import { AttentionTag, Conflicts, SpaceAvatar, kindLine } from "@/channels/space-bits"
import { Rules, ruleText, ruleOk } from "@/channels/rules"
import { BasisView, PersonaAvatar } from "@/channels/basis"

const Note = ({ children }: { children: React.ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>

export function SpaceView({ id }: { id: string }) {
  const L = useLoad(async () => {
    const s = await channelsApi.space(id)
    const [acc, personas] = await Promise.all([channelsApi.account(s.connection_id), channelsApi.personas()])
    return { s, acc, personas }
  }, [id], ["space", "chan_message", "persona", "channel"])
  const [busy, act] = useBusy()
  const [duty, setDuty] = useState<string | null>(null), [rules, setRules] = useState<Rule[] | null>(null)
  const [older, setOlder] = useState<ChanMessage[]>([]), [more, setMore] = useState<"idle" | "loading" | "end">("idle")
  useEffect(() => { setOlder([]); setMore("idle") }, [id])
  if (L.s === "unwired") return <Unwired id="accounts" />
  if (L.s === "loading") return <Page title="群" back={["#/channels", "渠道"]}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="群" back={["#/channels", "渠道"]}><Empty title={L.status === 404 ? "这个群不存在" : L.err} sub={L.status === 404 ? "可能不在当前工作空间，或者账号已经移除。" : undefined} /></Page>
  const { s, acc, personas } = L.data
  const me = acc?.mode === "self", accName = acc?.name ?? "接入账号"
  const persona = s.persona ? personas.find((p) => p.id === s.persona!.id) : undefined
  const save = async (b: SpacePatch, ok?: string) => { const r = await act(() => channelsApi.patchSpace(s.id, b), ok); if (r) await L.reload(); return !!r }
  const recent = [...s.recent, ...older]
  const loadOlder = async () => {
    const last = recent[recent.length - 1]; if (!last) return
    setMore("loading")
    try { const xs = await channelsApi.messages(s.id, last.created, 30); setOlder([...older, ...xs]); setMore(xs.length < 30 ? "end" : "idle") } catch (e) { report(e); setMore("idle") }
  }
  const toFlow = async () => { const r = await act(() => channelsApi.toFlow(s.id), "已转成流程"); if (r?.flow_id) go("#/auto/rule/" + r.flow_id) }
  const rulesNow = rules ?? s.rules
  const att = attentionOf(s), title = spaceTitle(s)
  const rulesOff = (s.conflicts ?? []).some((c) => c.key === "rules" && c.level === "conflict")
  const ids = s.identities ?? []
  const setAttention = (a: Attention) => void save({ attention: a }, a === "blocked" ? "已屏蔽：不再处理这里的新消息" : a === "quiet" ? "不提醒：只记录，不打扰你" : "已恢复实时")
  return (
    <Page title={title} back={acc ? ["#/channels/acc/" + acc.connection.id, accName] : ["#/channels", "渠道"]} max={720}
      sub={<span className="flex min-w-0 items-center gap-2"><SpaceAvatar s={s} size={22} /><Meta>{acc && <span>{acc.provider.name}</span>}<span>{kindLine(s)}</span>{s.counterpart && s.counterpart.name !== title && <span>{s.counterpart.name}</span>}{s.has_external && <span>有外部成员</span>}<span>接入：{accName}</span>{s.status !== "active" && <span>{s.status === "left" ? "已退出" : "已归档"}</span>}</Meta></span>}
      actions={<AttentionTag a={att} />}>
      <div className="flex flex-col gap-8" data-space={s.id}>
        {!s.configured && <div className="rounded-xl border px-4 py-3 type-callout" data-space-new>这是新加入的{kindLine(s).split(" · ")[0]}，还没有设置过。现在按账号的默认规则处理；改动任何一项就算设置过了。{s.suggest_quiet && <span className="mt-1 block text-muted-foreground">{s.suggest_quiet}，可以设为“不提醒”。</span>}</div>}
        {s.attention !== undefined && <section data-attention><SectionLabel>要不要打扰你</SectionLabel>
          <Segmented className="w-full [&>div]:w-full" value={att} onChange={(v) => v !== att && setAttention(v)} options={(Object.keys(ATTENTION) as Attention[]).map((a) => [a, ATTENTION[a][0]] as const)} />
          <Note>{ATTENTION[att][1]}</Note>
          <Conflicts items={s.conflicts} keys={["trigger"]} /></section>}
        <section data-role><SectionLabel>Steve 在这里</SectionLabel>
          <Group>
            {s.role && <Row title="角色" sub={att !== "realtime" ? `你设了“${ATTENTION[att][0]}”：这里的响应规则暂不生效` : ROLE[s.role]?.[1]}><span className="type-callout text-muted-foreground" data-role-value={s.role}>{ROLE[s.role]?.[0] ?? s.role}</span></Row>}
            <Row title="只记录" sub="不发言，也不起草；转给你的仍进待处理（静默）"><Switch checked={s.muted} disabled={busy} onCheckedChange={(v) => void save({ muted: v }, v ? "只记录，不发言" : "已恢复按规则处理")} aria-label="只记录" data-space-on /></Row>
            {ids.length > 0 && <Row title="替 Steve 发言的身份" sub={ids.length > 1 ? "一个空间最多一个身份发言，其他身份只观察" : "只有这个身份在这里"}>
              <Select align="end" testId="space-voice" value={s.voice?.chosen ? s.voice.connection_id : "__auto"} onChange={(v) => void save({ voice: v === "__auto" ? null : v }, "已换发言身份")}
                options={[{ v: "__auto", l: `自动${s.voice && !s.voice.chosen ? `（${s.voice.name}）` : ""}` }, ...ids.map((i) => ({ v: i.connection_id, l: `${i.name}${i.mode === "self" ? "（代你）" : "（机器人）"}` }))]} /></Row>}
          </Group>
          <Conflicts items={s.conflicts} keys={["voice"]} /></section>
        <section><SectionLabel>在这里说话的是</SectionLabel>
          <Group><Row icon={<PersonaAvatar bot={me ? null : persona?.bot_id} size={36} />} title={me ? "你本人" : s.persona?.name ?? "—"}
            sub={me ? "以你本人接入，回复以你的名义发出；默认只起草，经你批准才发" : persona?.intro_preview} onClick={!me && persona ? () => go("#/channels/personas/" + persona.id) : undefined} data-space-persona-row>
            {!me && persona && <ChevronRightIcon className="size-4 text-tertiary" aria-label="编辑身份" />}</Row>
            {!me && <Row title="这个群用" sub={s.persona?.inherited ? "跟随账号的默认身份" : "本群单独设置"}>
              <Select align="end" testId="space-persona" value={s.persona && !s.persona.inherited ? s.persona.id : "__acc"} onChange={(v) => void save({ persona_id: v === "__acc" ? null : v }, "已换身份")}
                options={[{ v: "__acc", l: `跟随账号${acc?.persona ? `（${acc.persona.name}）` : ""}` }, ...personas.map((p) => ({ v: p.id, l: p.name }))]} /></Row>}</Group>
          {!me && s.has_external && persona?.reveal.steve && <Note>这个群有外部成员，而“{persona.name}”会透露自己由 Steve 驱动。需要的话在身份里关掉。</Note>}
        </section>
        <section><SectionLabel right={duty !== null && duty !== s.duty && <span className="flex gap-1"><Button size="xs" variant="ghost" onClick={() => setDuty(null)}>还原</Button><Button size="xs" disabled={busy || duty.length > 4000} onClick={() => void save({ duty: duty.trim() }, "已保存职责").then((ok) => ok && setDuty(null))} data-duty-save>保存</Button></span>}>在这个群里的职责</SectionLabel>
          <Textarea aria-label="在这个群里的职责" className="min-h-24 rounded-xl" value={duty ?? s.duty} maxLength={4000} onChange={(e) => setDuty(e.target.value)} placeholder="在这里负责什么、遵守什么规则。例如：只回答部署和告警相关的问题；不确定时 @你" data-agents />
          <Note>相当于这个群的 agents.md。说话方式和性格写在对外身份里（soul），不用每个群重复。</Note></section>
        <section data-rules-section={rulesOff ? "off" : "on"}><SectionLabel>什么时候响应</SectionLabel>
          <Conflicts items={s.conflicts} keys={["rules", "flow"]} className="mb-2 mt-0" />
          <div className={cn(rulesOff && "opacity-55")}>
          {s.flow_id ? <Group><Row icon={<WorkflowIcon className="size-4 text-muted-foreground" aria-hidden />} title="已转成流程" sub="这个群的消息由流程处理；规则表单停用" onClick={() => go("#/auto/rule/" + s.flow_id)}>
              <Button size="xs" variant="outline" disabled={busy} onClick={(e) => { e.stopPropagation(); void act(() => channelsApi.backToRules(s.id), "已改回规则，流程停用但保留").then((r) => r && L.reload()) }} data-back-to-rules>改回规则</Button></Row></Group>
            : rulesNow ? <>
              <Rules rules={rulesNow} self={me} onChange={setRules} note="这个群单独设置的规则，覆盖账号的默认规则。" />
              <div className="mt-2 flex flex-wrap gap-2">
                {rules && <Button size="sm" disabled={busy || !rules.every(ruleOk)} onClick={() => void save({ rules }, "已保存规则").then((ok) => ok && setRules(null))} data-rules-save>保存规则</Button>}
                {rules && <Button size="sm" variant="ghost" onClick={() => setRules(null)}>还原</Button>}
                <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void save({ rules: null }, "已改回跟随账号").then((ok) => ok && setRules(null))} data-follow-account>改回跟随账号</Button>
                {!rules && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void toFlow()} data-to-flow><WorkflowIcon />转成流程</Button>}
              </div></>
            : <div className="flex flex-col gap-2"><div className="px-1 type-footnote text-muted-foreground">跟随账号的默认规则：</div>
              <Group>{s.effective_rules.map((r, n) => <Row key={n} title={ruleText(r, me)} />)}</Group>
              <div className="flex flex-wrap gap-2"><Button size="xs" variant="outline" onClick={() => setRules(s.effective_rules.map((r) => ({ ...r })))} data-override-rules>为这个群单独设置</Button>
                <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void toFlow()} data-to-flow><WorkflowIcon />转成流程</Button></div></div>}
          </div>
          <Note>更复杂的判断用“流程”编排：转成流程后由流程处理这个群的消息。</Note>
        </section>
        <section><SectionLabel>知识与记忆</SectionLabel>
          <Group>
            <Row title="这里的内容在别处能说到什么程度" sub={LEVEL[s.disclosure]?.[1]}><Select align="end" testId="space-disclosure" value={s.disclosure} onChange={(v) => void save({ disclosure: v as Level }, "已保存")} options={(Object.keys(LEVEL) as Level[]).map((v) => ({ v, l: LEVEL[v][0] }))} /></Row>
            <Row title="记忆" sub="这个群里记住的事，是否和同一身份在其他群共享"><Segmented size="sm" value={s.memory_scope} onChange={(v) => void save({ memory_scope: v }, "已保存")} options={[["space", "仅本群"], ["persona", "与身份共享"]] as const} /></Row>
          </Group>
          <Note>回答问题时可以用到其他群、事项和邮件里的知识来推理；回复里能说到什么程度，按每条知识来源的披露级别和这个群的听众决定。</Note></section>
        <section><SectionLabel>其他</SectionLabel>
          <Group>
            <Row title="从这里建的事项交给" sub={s.item_defaults.tags.length ? `默认带 ${s.item_defaults.tags.length} 个标签` : "不加默认标签"}>
              <Select align="end" testId="space-item-bot" value={s.item_defaults.bot_id ?? "__persona"} onChange={(v) => void save({ item_defaults: { ...s.item_defaults, bot_id: v === "__persona" ? null : v } }, "已保存")}
                options={[{ v: "__persona", l: "身份背后的 Bot" }, ...Object.keys(st.bots).map((k) => ({ v: k, l: botName(k) }))]} /></Row>
            <Row title="和群里其他机器人协作" sub="允许它响应其他机器人的消息。关闭可以避免互相触发"><Switch aria-label="和群里其他机器人协作" checked={s.collab} disabled={busy} onCheckedChange={(v) => void save({ collab: v }, v ? "已允许协作" : "已关闭协作")} data-collab /></Row>
          </Group></section>
        <section><SectionLabel>成员</SectionLabel>
          <Group><Row title={s.member_count ? `${s.member_count} 人` : "人数未知"} sub={[s.has_external ? "有外部成员，默认按客户群对待" : "都是本公司的人", s.teams.length ? `对应 ${s.teams.length} 个团队` : ""].filter(Boolean).join(" · ")}>
            {s.has_external && <Badge variant="outline" className="font-normal text-muted-foreground">外部</Badge>}</Row></Group>
          <Note>成员每 6 小时和进群、退群时刷新，每个人按账号认到联系人。</Note></section>
        <section><SectionLabel>最近</SectionLabel>
          {recent.length ? <div className="flex flex-col gap-3" data-recent>{recent.map((m) => <Msg key={m.id} m={m} bot={persona?.bot_id} me={me} />)}
            {more !== "end" && recent.length >= 30 && <Button size="sm" variant="ghost" className="self-center text-muted-foreground" disabled={more === "loading"} onClick={() => void loadOlder()} data-older>{more === "loading" && <Spinner />}更早的消息</Button>}</div>
            : <Empty title="还没有消息" sub="群里有人说话后会出现在这里。" />}
        </section>
      </div>
    </Page>
  )
}

function Msg({ m, bot, me }: { m: ChanMessage; bot?: string; me: boolean }) {
  const out = m.direction === "out"
  return (
    <div className={cn("flex gap-2.5", out && "rounded-xl bg-muted/40 p-2.5")} data-msg={m.id}>
      {out ? <PersonaAvatar bot={me ? null : bot} size={24} /> : <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] text-muted-foreground" aria-hidden>{(m.sender.name || "?")[0]}</span>}
      <div className="min-w-0 flex-1">
        <div className="type-footnote text-muted-foreground">{m.sender.name || "未知"} · {tm(m.created)}{MSG_STATUS[m.status] && <span className={m.status === "failed" || m.status === "blocked" ? "text-destructive" : undefined}> · {MSG_STATUS[m.status]}</span>}{m.on_behalf === "proxy" && " · 由 Steve 代发"}</div>
        <ChannelMessageBody message={m} />
        {m.work_item_id && <button type="button" className="mt-1 type-footnote text-blue hover:underline" onClick={() => go("#/items/" + m.work_item_id)}>建成了事项 {m.work_item_id}</button>}
        {m.decision_id && m.status === "draft" && <button type="button" className="mt-1 block type-footnote text-blue hover:underline" onClick={() => go("#/inbox")}>在待处理里批准</button>}
        <BasisView b={m.basis} />
      </div>
    </div>
  )
}

