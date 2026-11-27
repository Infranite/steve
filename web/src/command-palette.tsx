import { readViews } from "@/data/sync"
import { getItems, getSessions, getSession } from "@/store"
/* ⌘K: search and run anything from one field — the affectUI Command Menu over Steve's own data. With nothing typed it
 * lists the actions, the pages, then the items, sessions, projects and Bots; with text typed the things it found come
 * first: items and sessions by their words here plus the sessions the Hub's full-text search finds, the messages of
 * the conversation the Hub finds, and contacts / teams (where the contacts module is). Keyboard first: ↑ ↓, Tab
 * between groups, ↵ runs, Esc clears and then closes. */
import { useEffect, useState, type ReactNode } from "react"
import { ActivityIcon, BookOpenIcon, BotIcon, Building2Icon, CalendarIcon, ListChecksIcon, TargetIcon, ChartNoAxesColumnIcon, ContactRoundIcon, FolderIcon, GitPullRequestIcon, HistoryIcon, InboxIcon, KeyRoundIcon, ListTodoIcon, LockIcon, MailIcon, MessageCircleIcon, MessagesSquareIcon, MoonIcon, NewspaperIcon, PlugIcon, PlusIcon, QrCodeIcon, RadioIcon, ServerIcon, SettingsIcon, SquareTerminalIcon, SunIcon, UsersRoundIcon, WorkflowIcon, BlocksIcon, UploadIcon } from "lucide-react"
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { CommandMenu as Menu, type CommandGroup } from "@/components/affect/command-menu-clean"
import { api } from "@/api"
import { people, type PartyOption } from "@/api/people"
import { st, useStore, type Item, type Msg } from "@/store"
import { useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { closeSheet, go, openSheet, resolvedTheme, setTheme, StatusDot } from "@/ui"
import { jumpToMsg, searchThread } from "@/thread/history"
import { homeTilde } from "@/lib/path"
import { openQuickAdd } from "@/todos/quick"

const PARTY_HREF: Record<PartyOption["type"], string> = { contact: "#/contacts/", org: "#/orgs/", team: "#/teams/" }
const PARTY_ICON: Record<PartyOption["type"], typeof ContactRoundIcon> = { contact: ContactRoundIcon, org: Building2Icon, team: UsersRoundIcon }
/** a message as one line: its text without the line breaks, cut short */
const line = (s: string) => { const x = s.replace(/\s+/g, " ").trim(); return x.length > 90 ? x.slice(0, 90) + "…" : x }

/** the items in the order they are looked for: with text typed, the one whose number it is, then those whose number
 *  starts with it (by number), then the rest by when they last changed */
function ranked(items: Item[], q: string): Item[] {
  const x = q.trim().toUpperCase()
  if (!x) return items
  const num = (id: string) => parseInt(id.replace(/^\D+/, ""), 10) || 0
  const rank = (i: Item) => (i.id.toUpperCase() === x ? 0 : i.id.toUpperCase().startsWith(x) ? 1 : 2)
  return [...items].sort((a, b) => rank(a) - rank(b) || (rank(a) === 1 ? num(a.id) - num(b.id) : (b.updated ?? 0) - (a.updated ?? 0)))
}
/** whether a session matches the typed text by what is shown of it here (its title, its item), not only by its transcript */
const local = (s: { title: string; item?: string | null }, q: string) => { const x = q.trim().toLowerCase(); return !x || `${s.title} ${s.item ?? ""}`.toLowerCase().includes(x) }

type Found = { sessions: Set<string>; msgs: Msg[]; parties: PartyOption[] }
const NONE: Found = { sessions: new Set(), msgs: [], parties: [] }

/** what the Hub finds for the typed text: sessions by their whole transcript, messages of the conversation, parties */
function useFound(q: string): [Found, boolean] {
  const [found, setFound] = useState<Found>(NONE), [busy, setBusy] = useState(false)
  useEffect(() => {
    const x = q.trim(); if (!x) { setFound(NONE); setBusy(false); return }
    const ctl = new AbortController(); let live = true
    const h = setTimeout(() => {
      setBusy(true)
      void Promise.all([
        api.searchSessions(x).then(async (ids) => { await readViews("session", ids.filter((id) => !getSession(id))); return new Set(ids) }).catch(() => new Set<string>()),
        searchThread(x, ctl.signal).then((r) => r.msgs.slice(0, 8), () => [] as Msg[]),
        people.search(x).then((r) => [...r.contacts, ...r.teams, ...r.orgs], () => [] as PartyOption[]),
      ]).then(([sessions, msgs, parties]) => { if (live) setFound({ sessions, msgs, parties }) }).finally(() => { if (live) setBusy(false) })
    }, 150)
    return () => { live = false; ctl.abort(); clearTimeout(h) }
  }, [q])
  return [found, busy]
}

export function CommandPalette() {
  useStore()
  const t = useT()
  const [q, setQ] = useState("")
  const [found, busy] = useFound(q)

  /* what each row does, by its id; the rows are built with it. Built on every paint, not memoised: the store changes
   * sessions and items in place while they stream, so their references would not tell a memo anything changed */
  const runs = new Map<string, () => void>()
  const row = (id: string, label: string, icon: ReactNode, run: () => void, more: Partial<CommandGroup["commands"][number]> = {}) => { runs.set(id, run); return { id, label, icon, ...more } }
  const dark = resolvedTheme() === "dark"
  const actions: CommandGroup = { id: "actions", label: t("shell:cmd.actions"), commands: [
    row("a:newItem", t("shell:cmd.newItem"), <PlusIcon />, () => { go("#/items"); openSheet("newItem") }),
    row("a:newTodo", t("shell:cmd.newTodo"), <ListChecksIcon />, openQuickAdd, { data: { "data-cmd-new-todo": "" } }),
    row("a:newProject", t("shell:cmd.newProject"), <FolderIcon />, () => { go("#/projects"); openSheet("newProject") }),
    row("a:newBot", t("shell:cmd.newBot"), <BotIcon />, () => { go("#/bots"); openSheet("newBot") }, { data: { "data-cmd-newbot": "" } }),
    row("a:pair", t("devices:switcher.add"), <QrCodeIcon />, () => openSheet("addDevice"), { data: { "data-cmd-pair-device": "" } }),
    row("a:hubs", t("shell:cmd.switchHub"), <ServerIcon />, () => openSheet("hubs"), { data: { "data-cmd-switch-hub": "" } }),
    row("a:theme", dark ? t("shell:cmd.toLight") : t("shell:cmd.toDark"), dark ? <SunIcon /> : <MoonIcon />, () => setTheme(dark ? "light" : "dark"), { keywords: t("shell:cmd.themeWords") }),
  ] }
  const pageList: [string, string, typeof SettingsIcon][] = [
    ["", t("shell:cmd.page.chat"), MessageCircleIcon],
    ["inbox", t("shell:cmd.page.inbox"), InboxIcon],
    ["items", t("shell:cmd.page.items"), ListTodoIcon],
    ["projects", t("shell:cmd.page.projects"), FolderIcon],
    ["calendar", t("shell:cmd.page.calendar"), CalendarIcon],
    ["todos", t("shell:cmd.page.todos"), ListChecksIcon],
    ["todos?view=goals", t("shell:cmd.page.goals"), TargetIcon],
    ["feed", t("core:nav.feed"), NewspaperIcon],
    ["channels", t("shell:cmd.page.channels"), MessagesSquareIcon],
    ["mail", t("core:nav.mail"), MailIcon],
    ["contacts", t("shell:cmd.page.contacts"), ContactRoundIcon],
    ["knowledge", t("shell:cmd.page.knowledge"), BookOpenIcon],
    ["integrations", t("shell:cmd.page.integrations"), PlugIcon],
    ["github", "GitHub", GitPullRequestIcon],
    ["bots", "Bot", BotIcon],
    ["sessions", t("shell:cmd.page.sessions"), SquareTerminalIcon],
    ["auto", t("shell:cmd.page.auto"), WorkflowIcon],
    ["capabilities", t("shell:cmd.page.capabilities"), BlocksIcon],
    ["signals", t("shell:cmd.page.signals"), RadioIcon],
    ["stats", t("shell:cmd.page.stats"), ChartNoAxesColumnIcon],
    ["publications", t("pages:delivery.mine"), UploadIcon],
    ["accounts", t("shell:cmd.page.accounts"), KeyRoundIcon],
    ["vault", t("shell:cmd.page.vault"), LockIcon],
    ["devices", t("shell:cmd.page.devices"), ServerIcon],
    ["settings/general", t("shell:cmd.page.settings"), SettingsIcon],
    ["devices/hub", t("devices:cmd.hub"), ServerIcon],
    ["devices/clients", t("devices:cmd.clients"), KeyRoundIcon],
    ["settings/tags", t("shell:cmd.page.settingsTags"), SettingsIcon],
    ["settings/workspace", t("shell:cmd.page.settingsSpace"), SettingsIcon],
    ...(st.owner ? [
      ["devices/monitor", t("devices:cmd.monitor"), ActivityIcon] as [string, string, typeof SettingsIcon],
      ["settings/audit", t("shell:cmd.page.settingsAudit"), HistoryIcon] as [string, string, typeof SettingsIcon],
    ] : []),
  ]
  const pages: CommandGroup = { id: "pages", label: t("shell:cmd.pages"), commands: pageList.map(([h, l, I]) => row("p:" + h, l, <I />, () => go("#/" + h), { data: { "data-cmd-page": h } })) }
  const items: CommandGroup = { id: "items", label: t("shell:cmd.page.items"), commands: ranked(getItems(), q).map((i) => row("i:" + i.id, i.t, <StatusDot s={i.st} />, () => go("#/items/" + i.id), { hint: i.id, mono: true, data: { "data-cmd-item": i.id } })) }
  const sessions: CommandGroup = { id: "sessions", label: t("shell:cmd.page.sessions"), commands: [...new Map([...getSessions(), ...[...found.sessions].flatMap((id) => getSession(id) ?? [])].map((s) => [s.id, s])).values()].sort((a, b) => (b.at ?? 0) - (a.at ?? 0)).map((s) => row("s:" + s.id, s.title, <SquareTerminalIcon />, () => go("#/sessions/" + s.id), { hint: s.item ? `${s.item} · #${s.n}` : undefined, mono: true, keywords: s.item ?? "", always: found.sessions.has(s.id), ...(found.sessions.has(s.id) && !local(s, q) && { hint: [s.item, t("shell:cmd.inTranscript")].filter(Boolean).join(" · "), mono: false }), data: { "data-cmd-session": s.id } })) }
  const msgs: CommandGroup = { id: "messages", label: t("shell:cmd.messages"), commands: found.msgs.filter((m) => m.id).map((m) => row("m:" + m.id, line(m.t ?? ""), <MessageCircleIcon />, () => {
    /* the conversation takes the jump when it shows, if it is not showing yet */
    if (location.hash !== "#/" && location.hash !== "") go("#/")
    jumpToMsg(m.id!, m.created)
  }, { hint: m.created ? monthDayTime(m.created) : undefined, always: true, data: { "data-cmd-msg": m.id! } })) }
  const parties: CommandGroup = { id: "parties", label: t("shell:cmd.parties"), commands: found.parties.map((p) => { const I = PARTY_ICON[p.type] ?? ContactRoundIcon; return row("c:" + p.type + p.id, p.name, <I />, () => go(PARTY_HREF[p.type] + p.id), { hint: p.sub, always: true, data: { "data-cmd-party": p.id } }) }) }
  const projects: CommandGroup = { id: "projects", label: t("shell:cmd.page.projects"), commands: st.projects.map((p) => row("j:" + p.id, p.name, <FolderIcon />, () => go("#/projects/" + p.id), { hint: homeTilde(p.path), mono: true })) }
  const bots: CommandGroup = { id: "bots", label: "Bot", commands: Object.keys(st.bots).map((k) => row("b:" + k, st.bots[k].main ? st.bots[k].n : st.bots[k].n + " Bot", <BotIcon />, () => go("#/bots/" + k), { hint: st.bots[k].role, keywords: "Bot" })) }
  /* with text typed what it found leads; without, what one does and where one goes */
  const groups = q.trim() ? [items, sessions, msgs, parties, projects, bots, actions, pages] : [actions, pages, items, sessions, projects, bots]

  return (
    <Dialog open onOpenChange={(o, d) => {
      /* Esc with text typed clears it (the menu does that); only Esc on an empty field closes */
      if (!o && d.reason === "escape-key" && q.trim()) return
      if (!o) closeSheet()
    }}>
      <DialogContent showCloseButton={false} className="top-[16%] w-[calc(100%-1.5rem)] max-w-[600px]! translate-y-0 gap-0 overflow-visible bg-transparent p-0 shadow-none ring-0 sm:top-[20%]" data-command-palette>
        <DialogTitle className="sr-only">{t("common:action.search")}</DialogTitle>
        <DialogDescription className="sr-only">{t("shell:cmd.description")}</DialogDescription>
        <Menu autoFocus groups={groups} busy={busy} onQueryChange={setQ} onClose={closeSheet}
          onRun={(id) => { const f = runs.get(id); closeSheet(); f?.() }}
          labels={{ placeholder: t("shell:cmd.placeholder"), field: t("common:action.search"), list: t("shell:cmd.results"), empty: busy ? t("shell:cmd.searching") : t("shell:cmd.empty"), tabTip: t("shell:cmd.tabTip"), openTip: t("shell:cmd.openTip") }} />
      </DialogContent>
    </Dialog>
  )
}
