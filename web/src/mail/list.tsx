/* 邮件 (email.md §11): `#/mail`. Chips for 全部 / each mailbox, the five views (需要处理 · 收件箱 · 等对方 · 已发送 · 草稿)
 * and a search box over `GET /mail/threads`. No mailbox yet → the onboarding card; a Hub without the mail module (or
 * without the thread list) → the Unwired placeholder. */
import { useEffect, useMemo, useState } from "react"
import { MailIcon, PaperclipIcon, SearchIcon, PlusIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Page, Group, Segmented, Chips, Empty, ItemNo, StatusDot, go } from "@/ui"
import { Unwired } from "@/unwired"
import { tm } from "@/store"
import { ContactAvatar } from "@/people/avatar"
import { useLoad } from "@/accounts/load"
import { mailApi, VIEWS, DRAFT_STATUS, type MailView, type ThreadBrief, type MailAccount } from "@/api/mail"
import { useT } from "@/i18n"

const query = () => new URLSearchParams(/\?(.*)$/.exec(location.hash)?.[1] ?? "")
const isView = (v: string | null): v is MailView => !!v && VIEWS.some(([x]) => x === v)
/** keep the view, mailbox and search in the address, so back from a thread lands on the same list */
const remember = (p: { view: MailView; account: string; q: string }) => {
  const s = new URLSearchParams(); if (p.view !== "needs_reply") s.set("view", p.view); if (p.account) s.set("account", p.account); if (p.q) s.set("q", p.q)
  const h = "#/mail" + (s.toString() ? "?" + s : ""); if (location.hash !== h) history.replaceState(null, "", location.pathname + h)
}

export function MailView() {
  const t = useT()
  const A = useLoad(() => mailApi.accounts(), [], ["mail", "connection"])
  if (A.s === "unwired") return <Unwired id="mail" />
  if (A.s === "loading") return <Page title={t("mail:shared.title")}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (A.s === "error") return <Page title={t("mail:shared.title")}><Empty title={A.err} /></Page>
  if (!A.data.length) return <Onboarding />
  return <Threads accounts={A.data} />
}

/** no mailbox yet: the prototype's first-run card */
function Onboarding() {
  const t = useT()
  return (
    <Page title={t("mail:shared.title")} sub={t("mail:list.onboardingSub")} max={640}>
      <div className="flex items-start gap-3 rounded-xl border px-4 py-4" data-mail-onboarding>
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><MailIcon className="size-[18px]" strokeWidth={1.5} /></span>
        <div className="min-w-0 flex-1">
          <div className="type-headline">{t("mail:list.onboardingTitle")}</div>
          <div className="mt-0.5 type-callout text-muted-foreground">{t("mail:list.onboardingBody")}</div>
          <Button size="sm" className="mt-3" onClick={() => go("#/integrations/mail")} data-mail-connect><PlusIcon />{t("mail:list.connect")}</Button>
        </div>
      </div>
    </Page>
  )
}

function Threads({ accounts }: { accounts: MailAccount[] }) {
  const tr = useT(), ALL = tr("mail:list.all")
  const q0 = query()
  const [view, setView] = useState<MailView>(isView(q0.get("view")) ? (q0.get("view") as MailView) : "needs_reply")
  const [account, setAccount] = useState(accounts.some((a) => a.connection_id === q0.get("account")) ? q0.get("account")! : "")
  const [q, setQ] = useState(q0.get("q") ?? ""), [qq, setQq] = useState(q)
  useEffect(() => { const h = setTimeout(() => setQq(q.trim()), 250); return () => clearTimeout(h) }, [q])
  useEffect(() => remember({ view, account, q: qq }), [view, account, qq])
  const T = useLoad(() => mailApi.threads({ view, account: account || undefined, q: qq || undefined, limit: 100 }), [view, account, qq], ["mail"])
  const labels = useMemo(() => accounts.map((a) => a.address || a.connection_id), [accounts])
  if (T.s === "unwired") return <Unwired id="mail" />
  const unread = accounts.reduce((n, a) => n + a.unread, 0)
  return (
    <Page title={tr("mail:shared.title")} sub={unread ? tr("mail:shared.unread", { count: unread }) : undefined} max={820}
      toolbar={<div className="relative"><SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
        <Input type="search" aria-label={tr("mail:list.searchAria")} placeholder={tr("mail:list.searchPlaceholder")} className="h-8 w-56 rounded-lg pl-8" value={q} onChange={(e) => setQ(e.target.value)} data-mail-search /></div>}>
      <div className="mb-4 flex flex-col gap-3">
        {accounts.length > 1 && <Chips value={account ? labels[accounts.findIndex((a) => a.connection_id === account)] : ALL} options={[ALL, ...labels]}
          onChange={(l) => setAccount(l === ALL ? "" : accounts[labels.indexOf(l)]?.connection_id ?? "")} className="flex-wrap" />}
        <div className="-mx-1 overflow-x-auto px-1"><Segmented value={view} onChange={setView} options={VIEWS} /></div>
      </div>
      {T.s === "loading" ? <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
        : T.s === "error" ? <Empty title={T.err} />
        : !T.data.length ? <Empty title={qq ? tr("mail:list.noMatch") : tr(`mail:list.empty.${view}`)} />
        : <div data-mail-list={view}><Group>{T.data.map((t) => <ThreadRow key={t.id} t={t} showAccount={accounts.length > 1 && !account} />)}</Group></div>}
    </Page>
  )
}

function ThreadRow({ t, showAccount }: { t: ThreadBrief; showAccount: boolean }) {
  const tr = useT()
  const name = t.from?.name || t.from?.addr || tr("mail:list.noSender")
  return (
    <button type="button" onClick={() => go("#/mail/t/" + t.id)} role="listitem" className="flex w-full items-start gap-3 rounded-lg px-2 py-2.5 text-left transition-colors hover:bg-accent" data-thread-row={t.id} data-unread={t.unread > 0 || undefined}>
      <ContactAvatar name={name} size={32} className="mt-0.5" />
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-baseline gap-1.5">
          <span className={cn("min-w-0 truncate type-body", t.unread > 0 && "font-semibold")}>{name}</span>
          {t.from?.org && <span className="shrink-0 truncate type-footnote text-muted-foreground">{t.from.org}</span>}
          {t.count > 1 && <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{t.count}</span>}
          <span className="flex-1" />
          {t.has_attachments && <PaperclipIcon className="size-3.5 shrink-0 text-muted-foreground" aria-label={tr("mail:shared.hasAttachments")} />}
          <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{tm(t.last_at)}</span>
        </span>
        <span className={cn("block truncate type-callout", t.unread > 0 ? "text-foreground" : "text-foreground/85")}>{t.unread > 0 && <span className="mr-1.5 inline-block size-2 rounded-full bg-blue align-middle" aria-label={tr("mail:list.unread")} />}{t.subject || tr("mail:shared.noSubject")}</span>
        <span className="block truncate type-footnote text-muted-foreground">{t.snippet}</span>
        {(t.signal || t.item_id || t.draft || showAccount) && <span className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 type-footnote text-muted-foreground">
          {t.signal && <span className="inline-flex items-center gap-1.5" data-thread-signal><StatusDot s={/等你|需要/.test(t.signal.outcome_label) ? "wait" : "idle"} />{t.signal.outcome_label}</span>}
          {t.draft && <span className="inline-flex items-center gap-1.5" data-thread-draft-status={t.draft.status}><StatusDot s={DRAFT_STATUS[t.draft.status]?.[1] ?? "idle"} />{tr("mail:list.draftStatus", { status: DRAFT_STATUS[t.draft.status]?.[0] ?? t.draft.status })}</span>}
          {t.item_id && <ItemNo id={t.item_id} />}
          {showAccount && <span className="truncate">{t.connection.address}</span>}
        </span>}
      </span>
    </button>
  )
}
