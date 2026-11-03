/* 渠道 (channels-personas.md §8; prototype channels.tsx ChannelsTab): the conversation spaces — groups and DMs — of every
 * connection that carries messages, whatever the provider, with provider badges and filters by provider and account;
 * the channel accounts; and 对外身份. Each space opens its own settings (rules, persona, mute, disclosure).
 * Routes: #/channels, #/channels/acc/:id, #/channels/space/:id, #/channels/personas[/:id]. Connecting a provider is
 * on 接入 (#/integrations); nothing here is specific to one provider. */
import { useMemo, useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlugIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { go, Page, Group, Row, SectionLabel, Empty, StatusDot } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { tm } from "@/store"
import { useLoad } from "@/accounts/load"
import { CONN_STATUS } from "@/api/accounts"
import { channelsApi, attentionOf, spaceTitle, type Attention, type ChannelAccount, type Space } from "@/api/channels"
import { useBusy } from "@/accounts/load"
import { AttentionMenu, AttentionTag, Name, SpaceAvatar, kindLine } from "@/channels/space-bits"
import { PersonaAvatar } from "@/channels/basis"
import { SpaceView } from "@/channels/space"
import { AccountView } from "@/channels/account"
import { PersonasRoute, PersonasSection } from "@/channels/persona"

/** #/channels[/acc|space|personas/:id] (App.tsx) */
export function ChannelsRoute({ b, id }: { b?: string; id?: string }) {
  const x = (id ?? "").split("?")[0]
  if (b === "space" && x) return <SpaceView key={x} id={x} />
  if (b === "acc" && x) return <AccountView key={x} id={x} />
  if (b === "personas") return <PersonasRoute id={x || undefined} />
  return <ChannelsView />
}

const ALL = "__all"

function ChannelsView() {
  const L = useLoad(async () => {
    const [accounts, personas] = await Promise.all([channelsApi.accounts(), channelsApi.personas()])
    return { accounts, personas }
  }, [], ["connection", "account", "space", "persona", "channel"])
  const [prov, setProv] = useState(ALL), [acc, setAcc] = useState(ALL), [showBlocked, setShowBlocked] = useState(false)
  const [busy, act] = useBusy()
  const data = L.s === "ok" ? L.data : null
  const providers = useMemo(() => { const m = new Map<string, string>(); for (const a of data?.accounts ?? []) m.set(a.provider.id, a.provider.name); return [...m] }, [data])
  const accounts = (data?.accounts ?? []).filter((a) => prov === ALL || a.provider.id === prov)
  const shown = accounts.filter((a) => acc === ALL || a.connection.id === acc)
  const every = shown.flatMap((a) => a.spaces.map((s) => ({ s, a }))).sort((x, y) => (y.s.last_message ?? 0) - (x.s.last_message ?? 0))
  /* 屏蔽 spaces are hidden unless you ask for them */
  const blocked = every.filter(({ s }) => attentionOf(s) === "blocked")
  const spaces = showBlocked ? blocked : every.filter(({ s }) => attentionOf(s) !== "blocked")
  const pick = (s: Space, a: Attention) => void act(() => channelsApi.patchSpace(s.id, { attention: a }), a === "blocked" ? `已屏蔽「${spaceTitle(s)}」` : a === "quiet" ? `「${spaceTitle(s)}」不再提醒你` : `「${spaceTitle(s)}」恢复实时`).then((r) => r && L.reload())
  if (L.s === "unwired") return <Unwired id="channels" />
  if (L.s === "loading") return <Page title="渠道"><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="渠道"><Empty title={L.err} /></Page>
  const all = L.data.accounts
  const many = all.length > 1
  return (
    <Page title="渠道" sub="各个接入里的群和私聊。每个群单独设置在这里说话的身份、什么时候响应、内容能传到哪里" max={760}
      toolbar={all.length > 0 && <div className="flex flex-wrap items-center gap-2" data-channel-filters>
        {providers.length > 1 && <Select testId="ch-provider" value={prov} onChange={(v) => { setProv(v); setAcc(ALL) }} options={[{ v: ALL, l: "全部来源" }, ...providers.map(([v, l]) => ({ v, l }))]} />}
        {many && <Select testId="ch-account" value={acc} onChange={setAcc} options={[{ v: ALL, l: "全部账号" }, ...accounts.map((a) => ({ v: a.connection.id, l: `${a.name} · ${a.mode === "self" ? "你本人" : "机器人"}` }))]} />}
      </div>}>
      <div className="flex flex-col gap-8" data-channels-page>
        {!all.length ? <div className="flex flex-col items-start gap-3 rounded-xl border px-4 py-3.5" data-channels-empty>
          <div><div className="type-headline">还没有接入任何渠道</div>
            <div className="mt-0.5 type-callout text-muted-foreground">渠道来自“接入”里能收发消息的连接。接入之后，它所在的群和私聊会出现在这里，Steve 会问你要不要设置。</div></div>
          <Button size="sm" variant="outline" onClick={() => go("#/integrations")} data-goto-integrations><PlugIcon />去接入</Button></div>
          : <>
            <section><SectionLabel right={(blocked.length > 0 || showBlocked) && <button type="button" aria-pressed={showBlocked} onClick={() => setShowBlocked(!showBlocked)} className={cn("h-6 rounded-full px-2.5 text-[12px]", showBlocked ? "bg-foreground text-background" : "bg-muted text-muted-foreground hover:text-foreground")} data-blocked-filter>{showBlocked ? "返回全部" : `已屏蔽 ${blocked.length}`}</button>}>
              {showBlocked ? `已屏蔽 · ${spaces.length}` : `群和私聊 · ${spaces.length}`}</SectionLabel>
              {spaces.length ? <Group>{spaces.map(({ s, a }) => (
                <div key={s.id} className="group/space flex min-w-0 items-center gap-1" data-space-item={s.id}>
                  <Row className="min-w-0 flex-1" icon={<SpaceAvatar s={s} size={32} />} onClick={() => go("#/channels/space/" + s.id)} data-space-row={spaceTitle(s)} data-space-kind={s.kind}
                    title={<span className="flex min-w-0 items-center gap-1.5"><Name className={cn(attentionOf(s) !== "realtime" && "text-muted-foreground")}>{spaceTitle(s)}</Name>
                      <AttentionTag a={attentionOf(s)} />{s.muted && attentionOf(s) === "realtime" && <span className="shrink-0 type-footnote text-muted-foreground">只记录</span>}
                      {s.has_external && <Badge variant="outline" className="shrink-0 font-normal text-muted-foreground">外部</Badge>}{!s.configured && <Badge variant="outline" className="shrink-0 font-normal text-attention">新</Badge>}</span>}
                    sub={<span className="block truncate">{[kindLine(s), a.provider.name, a.name + (a.mode === "self" ? "（你本人）" : ""), s.last_message ? "最近 " + tm(s.last_message) : ""].filter(Boolean).join(" · ")}</span>}>
                    <ChevronRightIcon className="size-4 text-tertiary" /></Row>
                  <AttentionMenu s={s} disabled={busy} onPick={(x) => pick(s, x)} /></div>))}</Group>
                : <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground">{showBlocked ? "没有屏蔽的群。" : acc !== ALL || prov !== ALL ? "筛选条件下没有群或私聊。" : "还没有群或私聊：把机器人拉进群，或者给它发一条私聊；以你本人接入的账号，第一次读到的会话会出现在这里。"}</div>}</section>
            <section><SectionLabel>接入账号 · {accounts.length}</SectionLabel>
              <Group>{accounts.map((a) => <AccountRow key={a.connection.id} a={a} />)}</Group>
              <div className="mt-2 px-1 type-footnote text-muted-foreground">要接入新的账号，去 <button type="button" className="text-blue hover:underline" onClick={() => go("#/integrations")}>接入</button>。</div></section>
          </>}
        <PersonasSection />
      </div>
    </Page>
  )
}

function AccountRow({ a }: { a: ChannelAccount }) {
  const [s, dot] = CONN_STATUS[a.connection.status] ?? [a.connection.status, "idle"]
  return (
    <Row icon={<PersonaAvatar bot={a.mode === "self" ? null : a.personaBot} size={32} />} onClick={() => go("#/channels/acc/" + a.connection.id)} data-channel-row={a.connection.id}
      title={<span className="flex min-w-0 items-center gap-1.5"><Name>{a.name}</Name><Badge variant="outline" className="shrink-0 font-normal text-muted-foreground">{a.mode === "self" ? "你本人" : "机器人"}</Badge><span className="shrink-0 type-footnote text-muted-foreground">{a.provider.name}</span></span>}
      sub={`${a.mode === "self" ? "Steve 替你看消息，默认只起草" : "对外身份：" + (a.persona?.name ?? "默认身份")} · ${a.spaces.length} 个群和私聊`}>
      {a.connection.status === "ok" ? <span className="type-footnote text-muted-foreground">正常</span> : <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={dot} />{s}</span>}
      <ChevronRightIcon className="size-4 text-tertiary" /></Row>
  )
}
