/* 设置 · 操作记录: what was changed in this workspace and by whom, newest first: the Hub's audit trail, for the
 * instance's owner, as the affectUI Activity list. An entry reads as a sentence (what · on what), with who did it (you,
 * a Bot on its own, YOLO for you, another member, the Hub) as a dot, grouped by day; the detail its writer kept (and,
 * for YOLO, why it could) opens under it. The filters (by who) and the search work on the pages read so far. An action this page has
 * no words for goes by its code. */
import { useEffect, useMemo, useState } from "react"
import { BotIcon, BrainIcon, CircleCheckIcon, ContactRoundIcon, FolderIcon, HistoryIcon, KeyRoundIcon, ListTodoIcon, LockIcon, SettingsIcon, SparklesIcon, WorkflowIcon, ZapIcon } from "lucide-react"
import { api, HubError, modReq } from "@/api"
import { Button } from "@/components/ui/button"
import { ActivityList, type ActivityItem, type ActivityWho } from "@/components/affect/agent-activity"
import { st, useStore } from "@/store"
import { Blank, Notice, Loading, go } from "@/ui"
import { t as tr, useT } from "@/i18n"
import { date, monthDayTime } from "@/i18n/format"
import { actionKeys, changed, detailLines, purposeOf, type AuditEntry, type AuditPage } from "@/audit"

export const AUDIT_CAT = Object.defineProperties(["audit", "", HistoryIcon, ""] as [string, string, any, string], {
  1: { get: () => tr("manage:audit.title"), enumerable: true },
  3: { get: () => tr("manage:audit.sub"), enumerable: true },
})

const PAGE = 50
type Words = { names: Record<string, string>; purposes: Record<string, string> }

/** "新建 Bot「代码评审」": the action in words, with what it was done to when that has a name. An action on
 *  something of a `type` (a party that is a contact or an org) is worded for that type where there are words for it */
function title(e: AuditEntry, w: Words): string {
  const action = tr(actionKeys(e).map((k) => `manage:audit.action.${k}`), { defaultValue: e.action })
  const target = e.target === st.workspace?.id ? "" : w.names[e.target] ?? ""
  return target ? tr("manage:audit.withTarget", { action, target }) : action
}
/** what an entry changed, in a few words: the profile's parts, the purposes, the Bots; "" when the title says it all */
function summary(e: AuditEntry, w: Words): string {
  const d = e.detail ?? {}, sep = tr("shell:list.sep")
  const list = (v: unknown) => (Array.isArray(v) ? v.map(String) : [])
  const purpose = (k: string) => w.purposes[k] ?? k
  const fields = list(d.fields).map((f) => tr(`manage:audit.field.${f}`, { defaultValue: f }))
  const parts: string[] = []
  if (fields.length) parts.push(fields.join(sep))
  if (typeof d.memories === "number" && d.memories > 0) parts.push(tr("manage:audit.memories", { count: d.memories }))
  if (typeof d.person === "string") parts.push(tr("manage:audit.person", { name: w.names[d.person] ?? d.person }))
  if (e.action.startsWith("onboard.")) {
    parts.push(...list(d.purposes).map(purpose), ...list(d.created))
    if (typeof d.how === "string") parts.push(tr(`manage:audit.how.${d.how}`, { defaultValue: d.how }))
  }
  parts.push(...changed(e).map(purposeOf).filter(Boolean).map(purpose))
  return parts.join(sep)
}

/** the day an entry is filed under: 今天, 昨天, or its date (with the year when it is not this one) */
function dayOf(ts: number) {
  const d = new Date(ts), today = new Date(); today.setHours(0, 0, 0, 0)
  const diff = Math.round((today.getTime() - new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()) / 86400000)
  if (diff <= 0) return tr("manage:audit.today")
  if (diff === 1) return tr("manage:audit.yesterday")
  return date(d, { ...(d.getFullYear() !== today.getFullYear() && { year: "numeric" }), month: "short", day: "numeric", weekday: "short" })
}

/** what kind of thing an action is about, as the row's glyph */
const MARKS: [RegExp, typeof HistoryIcon][] = [[/^decision\./, CircleCheckIcon], [/^workflow\.|^flow\.|^run\./, WorkflowIcon], [/^item\./, ListTodoIcon], [/^project\./, FolderIcon], [/^bot\./, BotIcon], [/^mm\./, BrainIcon], [/^(pair|client|token|passkey|oidc|user)\./, KeyRoundIcon], [/^(party|team|steward)\./, ContactRoundIcon], [/^vault\./, LockIcon], [/^yolo\./, ZapIcon], [/^onboard\./, SparklesIcon], [/^(settings|config|workspace)\./, SettingsIcon]]
const markOf = (action: string) => { const I = MARKS.find(([r]) => r.test(action))?.[1] ?? HistoryIcon; return <I /> }

/** who did it: you, another member, a Bot on its own, YOLO answering for you, or the Hub itself. The Hub writes its
 *  own upkeep (a migration, an expiry, a restart's cleanup, a send attempt, what it seeds) under the main Bot's id,
 *  so that goes by the action, not the actor */
type Who = "me" | "bot" | "yolo" | "member" | "hub"
const UPKEEP = /\.(migrate|expire)$|^vault\.use_(failed|expired)$|^mail\.send\.attempt$/
function whoOf(e: AuditEntry, me: string | null): Who {
  const actor = e.actor
  if (actor === "yolo") return "yolo"
  if (me && actor === me) return "me"
  if (actor.startsWith("usr_")) return "member"
  if (UPKEEP.test(e.action) || e.detail?.source === "seed") return "hub"
  if (st.bots[actor] || st.allBots.some((b) => b.id === actor)) return "bot"
  return "hub"
}
const WHOS = (): ActivityWho[] => [
  { id: "me", label: tr("manage:audit.by.me"), tone: "self" },
  { id: "bot", label: tr("manage:audit.by.bot"), tone: "auto" },
  { id: "yolo", label: tr("manage:audit.by.yolo"), tone: "approved" },
  { id: "member", label: tr("manage:audit.by.member"), tone: "other" },
  { id: "hub", label: tr("manage:audit.by.hub"), tone: "other" },
]

export function AuditSettingsPane() {
  useStore()
  const t = useT()
  const [page, setPage] = useState<AuditPage | null>(null)
  const [err, setErr] = useState<string | null>(null), [denied, setDenied] = useState(false), [busy, setBusy] = useState(false)
  const [purposes, setPurposes] = useState<Record<string, string>>({})
  const [me, setMe] = useState<string | null>(null)
  const load = (before?: number) => {
    setBusy(true); setErr(null)
    return modReq<AuditPage>("GET", `/audit?limit=${PAGE}${before ? `&before=${before}` : ""}`)
      .then((p) => setPage((old) => (before && old ? { entries: [...old.entries, ...p.entries], names: { ...old.names, ...p.names }, more: p.more } : p)))
      .catch((e) => { if (e instanceof HubError && e.status === 403) setDenied(true); else setErr(e instanceof Error ? e.message : String(e)) })
      .finally(() => setBusy(false))
  }
  useEffect(() => {
    void load()
    /* the purposes by the names the models page gives them; without them an entry says the purpose's key */
    modReq<{ purposes: { key: string; label: string }[] }>("GET", "/settings/models").then((v) => setPurposes(Object.fromEntries(v.purposes.map((p) => [p.key, p.label])))).catch(() => {})
    /* who "you" are, so your own steps say 你 */
    api.me().then((r) => setMe(r.user.id), () => {})
  }, [])
  const w: Words = { names: page?.names ?? {}, purposes }
  const items = useMemo(() => {
    if (!page) return []
    const one = (e: AuditEntry): ActivityItem => {
      const by = whoOf(e, me), name = by === "hub" ? tr("manage:audit.by.hub") : w.names[e.actor] ?? st.bots[e.actor]?.n ?? e.actor
      const item = st.items.find((i) => i.id === e.target)
      return {
        id: String(e.id), at: e.ts, mark: markOf(e.action), text: title(e, w), task: summary(e, w) || undefined, by,
        /* you and YOLO go by the filter's words; a Bot, a member or the Hub by its name when it has one */
        byName: by === "me" || by === "yolo" || by === "hub" ? undefined : name,
        /* why it could, only where the trail says so: YOLO answered for you. Who did it is in the details */
        why: by === "yolo" ? tr("manage:audit.why.yolo") : undefined,
        open: item ? { label: tr("manage:audit.openItem", { id: item.id }), run: () => go("#/items/" + item.id) } : undefined,
        details: [
          { label: tr("manage:audit.when"), value: monthDayTime(e.ts) },
          { label: tr("manage:audit.who"), value: name === e.actor || (by !== "hub" && !e.actor.includes("_")) ? name : `${name}  ${e.actor}` },
          { label: tr("manage:audit.what"), value: [e.action, e.target].filter(Boolean).join("  "), mono: true },
          ...detailLines(e, w.names).map(([k, v]) => { const purpose = purposes[purposeOf(k)]; return { label: purpose ?? k, value: v } }),
        ],
        data: { "data-audit-entry": e.action },
      }
    }
    /* within a day, a run of one person doing what reads as the same thing folds into one row */
    const out: ActivityItem[] = []
    let run: AuditEntry[] = []
    /* a step of a run says what tells it apart from the others: what it changed, or else what it was done to */
    const step = (e: AuditEntry): ActivityItem => { const x = one(e); return { ...x, task: x.task ?? (e.target && e.target !== st.workspace?.id ? w.names[e.target] ?? e.target : undefined) } }
    const flush = () => { if (!run.length) return; const head = one(run[0]); out.push(run.length > 1 ? { ...head, steps: run.map(step) } : head); run = [] }
    for (const e of page.entries) {
      const a = run[0]
      if (a && (new Date(a.ts).toDateString() !== new Date(e.ts).toDateString() || a.actor !== e.actor || a.action !== e.action || title(a, w) !== title(e, w))) flush()
      run.push(e)
    }
    flush()
    return out
  }, [page, purposes, me, st.items])
  if (denied) return <Blank data-audit-denied>{t("manage:audit.ownerOnly")}</Blank>
  if (!page) return err
    ? <Notice tone="destructive" action={<Button size="xs" variant="ghost" onClick={() => void load()}>{t("common:action.retry")}</Button>}>{t("manage:audit.failed", { error: err })}</Notice>
    : <Loading className="py-12" />
  const last = page.entries[page.entries.length - 1]
  if (page.entries.length === 0) return <Blank data-audit-empty>{t("manage:audit.empty")}</Blank>
  return <div className="flex flex-col gap-4" data-audit>
    <p className="type-footnote text-muted-foreground">{t("manage:audit.hint")}</p>
    <ActivityList items={items} whos={WHOS()} dayName={dayOf}
      labels={{ all: t("manage:audit.all"), search: t("manage:audit.search"), what: t("manage:audit.whatHappened"), why: t("manage:audit.whyCould"), noMatch: t("manage:audit.noMatch"), noMatchText: page.more ? t("manage:audit.noMatchMore") : t("manage:audit.noMatchText"), showAll: t("manage:audit.showAll"), steps: t("manage:audit.steps"), filters: t("manage:audit.filters") }}
 />
    {err && <Notice tone="destructive">{t("manage:audit.failed", { error: err })}</Notice>}
    {page.more && last && <Button size="sm" variant="outline" className="self-start" disabled={busy} onClick={() => void load(last.id)} data-audit-more>{busy ? t("manage:audit.loading") : t("manage:audit.more")}</Button>}
  </div>
}
