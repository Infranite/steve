/* 群聊与私聊 (channels-personas.md §8; prototype channels.tsx ChannelsTab): the conversation spaces — groups and DMs — of every
 * connection that carries messages, whatever the provider, with provider badges and filters by provider and account;
 * the channel accounts; and 对外身份. Each space opens its own settings (rules, persona, mute, disclosure).
 * Routes: #/channels, #/channels/acc/:id, #/channels/space/:id, #/channels/personas[/:id]. Connecting a provider is
 * on 连接 (#/integrations); nothing here is specific to one provider. */
import { useMemo, useState } from "react"
import { cn } from "cn"
import { BanIcon, BellOffIcon, ChevronRightIcon, MessagesSquareIcon, PlugIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { go, Page, Group, Row, SectionLabel, Section, Empty, StatusDot, ConnectPrompt, Notice, Pill, Blank } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { tm } from "@/store"
import { useLoad } from "@/accounts/load"
import { CONN_STATUS } from "@/api/accounts"
import { channelsApi, attentionOf, spaceTitle, type Attention, type ChannelAccount, type Space } from "@/api/channels"
import { useBusy } from "@/accounts/load"
import { AttentionMenu, Name, SpaceAvatar, doingLine } from "@/channels/space-bits"
import { PersonaAvatar } from "@/channels/basis"
import { SpaceView } from "@/channels/space"
import { AccountView } from "@/channels/account"
import { PersonasRoute, PersonasSection } from "@/channels/persona"
import { Trans } from "react-i18next"
import { useT } from "@/i18n"

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
  const t = useT()
  const L = useLoad(async () => {
    const [accounts, personas] = await Promise.all([channelsApi.accounts(), channelsApi.personas()])
    return { accounts, personas }
  }, [], ["connection", "account", "space", "persona", "channel"])
  const [acc, setAcc] = useState(ALL), [showBlocked, setShowBlocked] = useState(false)
  const [busy, act] = useBusy()
  const data = L.s === "ok" ? L.data : null
  const accounts = data?.accounts ?? []
  const shown = accounts.filter((a) => acc === ALL || a.connection.id === acc)
  const every = useMemo(() => shown.flatMap((a) => a.spaces.map((s) => ({ s, a }))).sort((x, y) => (y.s.last_message ?? 0) - (x.s.last_message ?? 0)), [data, acc])
  /* 屏蔽 spaces are hidden unless you ask for them */
  const blocked = every.filter(({ s }) => attentionOf(s) === "blocked")
  const spaces = showBlocked ? blocked : every.filter(({ s }) => attentionOf(s) !== "blocked")
  const pick = (s: Space, a: Attention) => void act(() => channelsApi.patchSpace(s.id, { attention: a }), t(a === "blocked" ? "channels:list.blockedToast" : a === "quiet" ? "channels:list.quietToast" : "channels:list.realtimeToast", { name: spaceTitle(s) })).then((r) => r && L.reload())
  if (L.s === "unwired") return <Unwired id="channels" />
  if (L.s === "loading") return <Page title={t("channels:list.title")}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title={t("channels:list.title")}><Empty title={L.err} /></Page>
  const all = L.data.accounts
  const many = all.length > 1
  const total = all.reduce((n, a) => n + a.spaces.length, 0)
  /* what needs you: an account that stopped working, groups nobody has set up yet */
  const broken = all.filter((a) => a.connection.status !== "ok")
  const fresh = all.flatMap((a) => a.spaces).filter((s) => !s.configured && attentionOf(s) !== "blocked" && s.status === "active")
  return (
    <Page title={t("channels:list.title")} sub={all.length ? t("channels:list.subCount", { spaces: total, accounts: all.length }) : t("channels:list.sub")} max={760}
      toolbar={many && <div className="flex flex-wrap items-center gap-2" data-channel-filters>
        <Select testId="ch-account" value={acc} onChange={setAcc} options={[{ v: ALL, l: t("channels:list.allAccounts") }, ...all.map((a) => ({ v: a.connection.id, l: t("channels:list.accountOption", { name: a.name, mode: a.provider.name }) }))]} />
      </div>}>
      <div className="flex flex-col gap-8" data-channels-page>
        {!all.length ? <ConnectPrompt icon={<MessagesSquareIcon />} title={t("channels:list.emptyTitle")} sub={t("channels:list.emptySub")} data-channels-empty
          action={<Button onClick={() => go("#/integrations")} data-goto-integrations><PlugIcon />{t("channels:list.goConnect")}</Button>} />
          : <>
            {(broken.length > 0 || fresh.length > 0) && <Notice tone="attention" data-channels-attention>
              <ul className="flex flex-col gap-1">
                {broken.map((a) => <li key={a.connection.id} className="min-w-0"><a href={"#/channels/acc/" + a.connection.id} className="hover:underline" data-attention-account={a.connection.id}>
                  {t("channels:list.attnAccount", { name: a.name, status: (CONN_STATUS[a.connection.status] ?? [a.connection.status])[0] })}</a>
                  {a.connection.health.last_error && <span className="block truncate type-footnote text-muted-foreground" title={a.connection.health.last_error}>{a.connection.health.last_error}</span>}</li>)}
                {fresh.length > 0 && <li><a href={"#/channels/space/" + fresh[0].id} className="hover:underline" data-attention-new>{fresh.length === 1 ? t("channels:list.attnNewOne", { name: spaceTitle(fresh[0]) }) : t("channels:list.attnNew", { count: fresh.length, name: spaceTitle(fresh[0]) })}</a></li>}
              </ul></Notice>}
            <section><SectionLabel right={(blocked.length > 0 || showBlocked) && <button type="button" aria-pressed={showBlocked} onClick={() => setShowBlocked(!showBlocked)} className={cn("h-6 rounded-full px-2.5 type-footnote", showBlocked ? "bg-primary/10 font-medium text-primary" : "bg-muted text-muted-foreground hover:text-foreground")} data-blocked-filter>{showBlocked ? t("channels:list.showAll") : t("channels:list.blockedFilter", { n: blocked.length })}</button>}>
              {t(showBlocked ? "channels:list.blockedLabel" : "channels:list.spacesLabel", { n: spaces.length })}</SectionLabel>
              {spaces.length ? <Group>{spaces.map(({ s, a }) => (
                <div key={s.id} className="group/space flex min-w-0 items-center gap-1" data-space-item={s.id}>
                  <Row className="min-w-0 flex-1" icon={<SpaceAvatar s={s} size={32} />} onClick={() => go("#/channels/space/" + s.id)} data-space-row={spaceTitle(s)} data-space-kind={s.kind}
                    title={<span className="flex min-w-0 items-center gap-1.5"><Name className={cn(attentionOf(s) !== "realtime" && "text-muted-foreground")}>{spaceTitle(s)}</Name>
                      {s.has_external && <Pill className="font-normal">{t("channels:word.external")}</Pill>}{!s.configured && <Pill tone="attention">{t("channels:word.new")}</Pill>}</span>}
                    sub={<span className="flex min-w-0 items-center gap-1"><AttentionIcon a={attentionOf(s)} /><span className="truncate" data-space-doing>{[doingLine(s, a), many && acc === ALL ? a.name : ""].filter(Boolean).join(" · ")}</span></span>}>
                    {s.last_message ? <span className="type-footnote tabular-nums text-muted-foreground" title={t("channels:list.recent", { time: tm(s.last_message) })}>{tm(s.last_message)}</span> : null}</Row>
                  <AttentionMenu s={s} disabled={busy} onPick={(x) => pick(s, x)} /></div>))}</Group>
                : <Blank>{t(showBlocked ? "channels:list.noBlocked" : acc !== ALL ? "channels:list.noMatch" : "channels:list.noSpaces")}</Blank>}</section>
            <Section label={t("channels:list.accountsLabel", { n: all.length })} note={<Trans t={t} i18nKey="channels:list.connectMore" components={{ link: <button type="button" className="text-blue hover:underline" onClick={() => go("#/integrations")} /> }} />}>
              <Group>{all.map((a) => <AccountRow key={a.connection.id} a={a} />)}</Group></Section>
          </>}
        <PersonasSection />
      </div>
    </Page>
  )
}

const AttentionIcon = ({ a }: { a: Attention }) => a === "quiet" ? <BellOffIcon className="size-3.5 shrink-0" aria-hidden /> : a === "blocked" ? <BanIcon className="size-3.5 shrink-0" aria-hidden /> : null

function AccountRow({ a }: { a: ChannelAccount }) {
  const t = useT()
  const [s, dot] = CONN_STATUS[a.connection.status] ?? [a.connection.status, "idle"]
  const as = a.mode === "self" ? t("channels:list.rowAsYou") : t("channels:list.rowAsPersona", { persona: a.persona?.name ?? t("channels:word.defaultPersona") })
  return (
    <Row icon={<PersonaAvatar bot={a.mode === "self" ? null : a.personaBot} size={32} />} onClick={() => go("#/channels/acc/" + a.connection.id)} data-channel-row={a.connection.id}
      title={<span className="flex min-w-0 items-center gap-1.5"><Name>{a.name}</Name><Pill className="font-normal">{t(a.mode === "self" ? "channels:word.you" : "channels:word.bot")}</Pill></span>}
      sub={<span className="block truncate">{[a.provider.name, as, t("channels:list.rowCount", { count: a.spaces.length })].join(" · ")}</span>}>
      {a.connection.status !== "ok" && <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={dot} />{s}</span>}
      <ChevronRightIcon className="size-4 text-tertiary" /></Row>
  )
}
