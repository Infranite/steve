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
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { botName, st, tm } from "@/store"
import { useLoad, useBusy, report } from "@/accounts/load"
import { channelsApi, ATTENTION, LEVEL, MSG_STATUS, ROLE, attentionOf, spaceTitle, type Attention, type ChanMessage, type Level, type Rule, type SpacePatch } from "@/api/channels"
import { AttentionTag, Conflicts, SpaceAvatar, kindLine } from "@/channels/space-bits"
import { Rules, ruleText, ruleOk } from "@/channels/rules"
import { BasisView, PersonaAvatar } from "@/channels/basis"
import { useT } from "@/i18n"

const Note = ({ children }: { children: React.ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>

export function SpaceView({ id }: { id: string }) {
  const t = useT()
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
  const home: [string, string] = ["#/channels", t("channels:list.title")]
  if (L.s === "loading") return <Page title={t("channels:space.title")} back={home}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title={t("channels:space.title")} back={home}><Empty title={L.status === 404 ? t("channels:space.notFound") : L.err} sub={L.status === 404 ? t("channels:space.notFoundSub") : undefined} /></Page>
  const { s, acc, personas } = L.data
  const me = acc?.mode === "self", accName = acc?.name ?? t("channels:space.account")
  const persona = s.persona ? personas.find((p) => p.id === s.persona!.id) : undefined
  const save = async (b: SpacePatch, ok?: string) => { const r = await act(() => channelsApi.patchSpace(s.id, b), ok); if (r) await L.reload(); return !!r }
  const recent = [...s.recent, ...older]
  const loadOlder = async () => {
    const last = recent[recent.length - 1]; if (!last) return
    setMore("loading")
    try { const xs = await channelsApi.messages(s.id, last.created, 30); setOlder([...older, ...xs]); setMore(xs.length < 30 ? "end" : "idle") } catch (e) { report(e); setMore("idle") }
  }
  const toFlow = async () => { const r = await act(() => channelsApi.toFlow(s.id), t("channels:space.toFlow")); if (r?.flow_id) go("#/auto/rule/" + r.flow_id) }
  const rulesNow = rules ?? s.rules
  const att = attentionOf(s), title = spaceTitle(s)
  const rulesOff = (s.conflicts ?? []).some((c) => c.key === "rules" && c.level === "conflict")
  const ids = s.identities ?? []
  const setAttention = (a: Attention) => void save({ attention: a }, t(a === "blocked" ? "channels:space.blocked" : a === "quiet" ? "channels:space.quiet" : "channels:space.realtime"))
  return (
    <Page title={title} back={acc ? ["#/channels/acc/" + acc.connection.id, accName] : home} max={720}
      sub={<span className="flex min-w-0 items-center gap-2"><SpaceAvatar s={s} size={22} /><Meta>{acc && <span>{acc.provider.name}</span>}<span>{kindLine(s)}</span>{s.counterpart && s.counterpart.name !== title && <span>{s.counterpart.name}</span>}{s.has_external && <span>{t("channels:word.externalMembers")}</span>}<span>{t("channels:space.via", { name: accName })}</span>{s.status !== "active" && <span>{t(s.status === "left" ? "channels:space.left" : "channels:space.archived")}</span>}</Meta></span>}
      actions={<AttentionTag a={att} />}>
      <div className="flex flex-col gap-8" data-space={s.id}>
        {!s.configured && <div className="rounded-xl border px-4 py-3 type-callout" data-space-new>{t("channels:space.newNote", { kind: kindLine(s).split(" · ")[0] })}{s.suggest_quiet && <span className="mt-1 block text-muted-foreground">{t("channels:space.suggestQuiet", { reason: s.suggest_quiet })}</span>}</div>}
        {s.attention !== undefined && <section data-attention><SectionLabel>{t("channels:bits.disturb")}</SectionLabel>
          <Segmented className="w-full [&>div]:w-full" value={att} onChange={(v) => v !== att && setAttention(v)} options={(Object.keys(ATTENTION) as Attention[]).map((a) => [a, ATTENTION[a][0]] as const)} />
          <Note>{ATTENTION[att][1]}</Note>
          <Conflicts items={s.conflicts} keys={["trigger"]} /></section>}
        <section data-role><SectionLabel>{t("channels:space.steveHere")}</SectionLabel>
          <Group>
            {s.role && <Row title={t("channels:space.role")} sub={att !== "realtime" ? t("channels:space.roleOff", { attention: ATTENTION[att][0] }) : ROLE[s.role]?.[1]}><span className="type-callout text-muted-foreground" data-role-value={s.role}>{ROLE[s.role]?.[0] ?? s.role}</span></Row>}
            <Row title={t("channels:word.recordOnly")} sub={t("channels:space.mutedSub")}><Switch checked={s.muted} disabled={busy} onCheckedChange={(v) => void save({ muted: v }, t(v ? "channels:space.mutedOn" : "channels:space.mutedOff"))} aria-label={t("channels:word.recordOnly")} data-space-on /></Row>
            {ids.length > 0 && <Row title={t("channels:space.voice")} sub={t(ids.length > 1 ? "channels:space.voiceMany" : "channels:space.voiceOne")}>
              <Select align="end" testId="space-voice" value={s.voice?.chosen ? s.voice.connection_id : "__auto"} onChange={(v) => void save({ voice: v === "__auto" ? null : v }, t("channels:space.voiceChanged"))}
                options={[{ v: "__auto", l: s.voice && !s.voice.chosen ? t("channels:space.autoNamed", { name: s.voice.name }) : t("channels:space.auto") }, ...ids.map((i) => ({ v: i.connection_id, l: t(i.mode === "self" ? "channels:space.voiceSelf" : "channels:space.voiceBot", { name: i.name }) }))]} /></Row>}
          </Group>
          <Conflicts items={s.conflicts} keys={["voice"]} /></section>
        <section><SectionLabel>{t("channels:space.speaker")}</SectionLabel>
          <Group><Row icon={<PersonaAvatar bot={me ? null : persona?.bot_id} size={36} />} title={me ? t("channels:word.you") : s.persona?.name ?? "—"}
            sub={me ? t("channels:space.selfSub") : persona?.intro_preview} onClick={!me && persona ? () => go("#/channels/personas/" + persona.id) : undefined} data-space-persona-row>
            {!me && persona && <ChevronRightIcon className="size-4 text-tertiary" aria-label={t("channels:word.editPersona")} />}</Row>
            {!me && <Row title={t("channels:space.groupUses")} sub={t(s.persona?.inherited ? "channels:space.followDefault" : "channels:space.own")}>
              <Select align="end" testId="space-persona" value={s.persona && !s.persona.inherited ? s.persona.id : "__acc"} onChange={(v) => void save({ persona_id: v === "__acc" ? null : v }, t("channels:space.personaChanged"))}
                options={[{ v: "__acc", l: acc?.persona ? t("channels:space.followNamed", { name: acc.persona.name }) : t("channels:space.follow") }, ...personas.map((p) => ({ v: p.id, l: p.name }))]} /></Row>}</Group>
          {!me && s.has_external && persona?.reveal.steve && <Note>{t("channels:space.revealsSteve", { name: persona.name })}</Note>}
        </section>
        <section><SectionLabel right={duty !== null && duty !== s.duty && <span className="flex gap-1"><Button size="xs" variant="ghost" onClick={() => setDuty(null)}>{t("channels:word.revert")}</Button><Button size="xs" disabled={busy || duty.length > 4000} onClick={() => void save({ duty: duty.trim() }, t("channels:space.dutySaved")).then((ok) => ok && setDuty(null))} data-duty-save>{t("common:action.save")}</Button></span>}>{t("channels:space.duty")}</SectionLabel>
          <Textarea aria-label={t("channels:space.duty")} className="min-h-24 rounded-xl" value={duty ?? s.duty} maxLength={4000} onChange={(e) => setDuty(e.target.value)} placeholder={t("channels:space.dutyPlaceholder")} data-agents />
          <Note>{t("channels:space.dutyNote")}</Note></section>
        <section data-rules-section={rulesOff ? "off" : "on"}><SectionLabel>{t("channels:space.whenRespond")}</SectionLabel>
          <Conflicts items={s.conflicts} keys={["rules", "flow"]} className="mb-2 mt-0" />
          <div className={cn(rulesOff && "opacity-55")}>
          {s.flow_id ? <Group><Row icon={<WorkflowIcon className="size-4 text-muted-foreground" aria-hidden />} title={t("channels:space.toFlow")} sub={t("channels:space.flowSub")} onClick={() => go("#/auto/rule/" + s.flow_id)}>
              <Button size="xs" variant="outline" disabled={busy} onClick={(e) => { e.stopPropagation(); void act(() => channelsApi.backToRules(s.id), t("channels:space.backToRulesToast")).then((r) => r && L.reload()) }} data-back-to-rules>{t("channels:space.backToRules")}</Button></Row></Group>
            : rulesNow ? <>
              <Rules rules={rulesNow} self={me} onChange={setRules} note={t("channels:space.rulesNote")} />
              <div className="mt-2 flex flex-wrap gap-2">
                {rules && <Button size="sm" disabled={busy || !rules.every(ruleOk)} onClick={() => void save({ rules }, t("channels:word.rulesSaved")).then((ok) => ok && setRules(null))} data-rules-save>{t("channels:word.saveRules")}</Button>}
                {rules && <Button size="sm" variant="ghost" onClick={() => setRules(null)}>{t("channels:word.revert")}</Button>}
                <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void save({ rules: null }, t("channels:space.followedToast")).then((ok) => ok && setRules(null))} data-follow-account>{t("channels:space.followAccount")}</Button>
                {!rules && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void toFlow()} data-to-flow><WorkflowIcon />{t("channels:space.convertFlow")}</Button>}
              </div></>
            : <div className="flex flex-col gap-2"><div className="px-1 type-footnote text-muted-foreground">{t("channels:space.inherited")}</div>
              <Group>{s.effective_rules.map((r, n) => <Row key={n} title={ruleText(r, me)} />)}</Group>
              <div className="flex flex-wrap gap-2"><Button size="xs" variant="outline" onClick={() => setRules(s.effective_rules.map((r) => ({ ...r })))} data-override-rules>{t("channels:space.override")}</Button>
                <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void toFlow()} data-to-flow><WorkflowIcon />{t("channels:space.convertFlow")}</Button></div></div>}
          </div>
          <Note>{t("channels:space.flowNote")}</Note>
        </section>
        <section><SectionLabel>{t("channels:space.knowledge")}</SectionLabel>
          <Group>
            <Row title={t("channels:space.disclosure")} sub={LEVEL[s.disclosure]?.[1]}><Select align="end" testId="space-disclosure" value={s.disclosure} onChange={(v) => void save({ disclosure: v as Level }, t("common:state.saved"))} options={(Object.keys(LEVEL) as Level[]).map((v) => ({ v, l: LEVEL[v][0] }))} /></Row>
            <Row title={t("channels:space.memory")} sub={t("channels:space.memorySub")}><Segmented size="sm" value={s.memory_scope} onChange={(v) => void save({ memory_scope: v }, t("common:state.saved"))} options={[["space", t("channels:space.memorySpace")], ["persona", t("channels:space.memoryPersona")]] as const} /></Row>
          </Group>
          <Note>{t("channels:space.knowledgeNote")}</Note></section>
        <section><SectionLabel>{t("channels:space.other")}</SectionLabel>
          <Group>
            <Row title={t("channels:space.itemsTo")} sub={s.item_defaults.tags.length ? t("channels:space.tags", { count: s.item_defaults.tags.length }) : t("channels:space.noTags")}>
              <Select align="end" testId="space-item-bot" value={s.item_defaults.bot_id ?? "__persona"} onChange={(v) => void save({ item_defaults: { ...s.item_defaults, bot_id: v === "__persona" ? null : v } }, t("common:state.saved"))}
                options={[{ v: "__persona", l: t("channels:space.personaBot") }, ...Object.keys(st.bots).map((k) => ({ v: k, l: botName(k) }))]} /></Row>
            <Row title={t("channels:space.collab")} sub={t("channels:space.collabSub")}><Switch aria-label={t("channels:space.collab")} checked={s.collab} disabled={busy} onCheckedChange={(v) => void save({ collab: v }, t(v ? "channels:space.collabOn" : "channels:space.collabOff"))} data-collab /></Row>
          </Group></section>
        <section><SectionLabel>{t("channels:space.members")}</SectionLabel>
          <Group><Row title={s.member_count ? t("channels:bits.members", { count: s.member_count }) : t("channels:space.membersUnknown")} sub={[t(s.has_external ? "channels:space.externalSub" : "channels:space.internalSub"), s.teams.length ? t("channels:space.teams", { count: s.teams.length }) : ""].filter(Boolean).join(" · ")}>
            {s.has_external && <Badge variant="outline" className="font-normal text-muted-foreground">{t("channels:word.external")}</Badge>}</Row></Group>
          <Note>{t("channels:space.membersNote")}</Note></section>
        <section><SectionLabel>{t("channels:space.recent")}</SectionLabel>
          {recent.length ? <div className="flex flex-col gap-3" data-recent>{recent.map((m) => <Msg key={m.id} m={m} bot={persona?.bot_id} me={me} />)}
            {more !== "end" && recent.length >= 30 && <Button size="sm" variant="ghost" className="self-center text-muted-foreground" disabled={more === "loading"} onClick={() => void loadOlder()} data-older>{more === "loading" && <Spinner />}{t("channels:space.older")}</Button>}</div>
            : <Empty title={t("channels:space.noMessages")} sub={t("channels:space.noMessagesSub")} />}
        </section>
      </div>
    </Page>
  )
}

function Msg({ m, bot, me }: { m: ChanMessage; bot?: string; me: boolean }) {
  const t = useT()
  const out = m.direction === "out"
  return (
    <div className={cn("flex gap-2.5", out && "rounded-xl bg-muted/40 p-2.5")} data-msg={m.id}>
      {out ? <PersonaAvatar bot={me ? null : bot} size={24} /> : <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] text-muted-foreground" aria-hidden>{(m.sender.name || "?")[0]}</span>}
      <div className="min-w-0 flex-1">
        <div className="type-footnote text-muted-foreground">{m.sender.name || t("channels:space.unknownSender")} · {tm(m.created)}{MSG_STATUS[m.status] && <span className={m.status === "failed" || m.status === "blocked" ? "text-destructive" : undefined}> · {MSG_STATUS[m.status]}</span>}{m.on_behalf === "proxy" && ` · ${t("channels:space.proxy")}`}</div>
        <ChannelMessageBody message={m} />
        {m.work_item_id && <button type="button" className="mt-1 type-footnote text-blue hover:underline" onClick={() => go("#/items/" + m.work_item_id)}>{t("channels:space.madeItem", { id: m.work_item_id })}</button>}
        {m.decision_id && m.status === "draft" && <button type="button" className="mt-1 block type-footnote text-blue hover:underline" onClick={() => go("#/inbox")}>{t("channels:space.approve")}</button>}
        <BasisView b={m.basis} />
      </div>
    </div>
  )
}

