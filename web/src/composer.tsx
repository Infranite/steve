/* The composer every page writes with: the main conversation, an item's page and a session's.
 * Steve's own behaviour – the marked text (items, mentions, commands), the #/@// menus, drafts that survive a reload,
 * uploads, idempotent sends – inside affectUI's Chat Composer card, with Run Controls' buttons at its end: Send when
 * the other side is idle; while it works Stop (or Esc), and once something is typed 排队 (and, where a running turn
 * takes a note, 插话: Enter steers, ⌥ Enter queues). What waits for the turn sits in the card above the field. */
import { Fragment, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { AtSignIcon, BotIcon, ClipboardListIcon, FileTextIcon, FolderPlusIcon, HashIcon, PaperclipIcon, SlashIcon, UsersRoundIcon } from "lucide-react"
import { cn } from "cn"
import { cue } from "@/bus"
import { useT } from "@/i18n"
import { st } from "@/store"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { useMentions } from "@/people/mention"
import { readDraft, saveDraft, draftToSend, clearSentDraft } from "@/composer-draft"
import { useUploads, PendingList, ACCEPT } from "@/attach"
import type { RFile } from "@/api"
import { Command, CommandGroup, CommandItem, CommandList } from "@/components/ui/command"
import { Kbd } from "@/components/ui/kbd"
import { ChatComposer, type ComposerAddRow } from "@/components/affect/chat-composer-clean"
import { RunActions, RunContext, runKey, type RunActionsLabels } from "@/components/affect/run-controls-clean"
import { BotAvatar, ItemNo, Status, useLinger } from "@/ui"

/** the composer's slash commands: [the token the Hub reads after "/", the command id]; label and description by id */
export const COMMANDS: [string, "newItem" | "newProject" | "digest" | "newBot"][] = [["新事项", "newItem"], ["新项目", "newProject"], ["汇报", "digest"], ["新 Bot", "newBot"]]
const CMD_ICON = { newItem: FileTextIcon, newProject: FolderPlusIcon, digest: ClipboardListIcon, newBot: BotIcon }
type MenuKind = "#" | "@" | "/"
/* the key that queues while a turn runs (runKey takes altKey): Option (⌥) on a Mac, Alt elsewhere */
const mac = typeof navigator !== "undefined" && /mac|iphone|ipad|ipod/i.test((navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform || navigator.platform || navigator.userAgent)
const ALT = mac ? { glyph: "⌥", name: "Option" } : { glyph: "Alt ", name: "Alt" }
/* what the composer marks in its text: the same tokens the Hub reads (an item id, “@名字”, a leading “/命令”), plus the one being typed */
type Mark = "item" | "bot" | "party" | "cmd" | "typing"
const MARK: Record<Mark, string> = {
  item: "bg-blue/12 text-blue ring-blue/12", bot: "bg-blue/12 text-blue ring-blue/12",
  party: "bg-foreground/10 text-foreground ring-foreground/10", cmd: "bg-foreground/10 text-foreground ring-foreground/10",
  typing: "bg-foreground/[0.05] text-muted-foreground ring-foreground/[0.05]",
}
const TOKEN = /(?<![\w@#-])#?[A-Za-z]{1,6}-\d+(?![\w-])|(?<!\S)@[^\s#@]+|^\/\S+/g
export function markText(v: string, at: (ins: string) => "bot" | "party" | undefined, typing?: [number, number] | null): [string, Mark | null][] {
  const rs: [number, number, Mark][] = []
  for (const m of v.matchAll(TOKEN)) {
    const t = m[0], k: Mark | undefined = t[0] === "@" ? at(t) : t[0] === "/" ? (COMMANDS.some(([n]) => "/" + n.replace(/\s+/g, "") === t.replace(/\s+/g, "")) ? "cmd" : undefined)
      : st.items.some((i) => i.id === t.replace(/^#/, "").toUpperCase()) ? "item" : undefined
    if (k) rs.push([m.index, m.index + t.length, k])
  }
  if (typing && typing[1] > typing[0] && !rs.some(([a, b]) => a < typing[1] && typing[0] < b)) rs.push([typing[0], typing[1], "typing"])
  rs.sort((a, b) => a[0] - b[0])
  const out: [string, Mark | null][] = []
  let at0 = 0
  for (const [a, b, k] of rs) { if (a > at0) out.push([v.slice(at0, a), null]); out.push([v.slice(a, b), k]); at0 = b }
  if (at0 < v.length) out.push([v.slice(at0), null])
  return out
}
/** what a page may do with its composer: a withdrawn queued message goes back into it when it is empty */
export type ComposerHandle = { empty: () => boolean; fill: (text: string, files: RFile[]) => void; focus: () => void }
/** how a message sent while the other side works goes: 插话 (into the running turn) or 排队 (after it) */
export type SendHow = "steer" | "queue"
export type ComposerProps = {
  handle?: { current: ComposerHandle | null }
  draftKey?: string
  /** `clientId`: the same for every retry of one unchanged draft (send idempotency); `how`: set while `busy` */
  placeholder: string; onSend: (t: string, files: string[], refs: RFile[], mentions: { kind: "party" | "bot"; id: string }[], clientId: string, how?: SendHow) => void | boolean | Promise<void | boolean>
  /** where the message goes (harness · model), a quiet label */
  model?: string
  /** the page's own toggles, after the plus */
  left?: ReactNode; autoFocus?: boolean; className?: string; testId?: string; disabled?: boolean
  /** `busy`: the other side is working. With `onStop` there is a stop button (and Esc), and sending still works: the
   * page queues what is sent meanwhile – or, with `steers`, Enter puts it into the running turn and ⌥ Enter queues it */
  busy?: boolean; onStop?: () => void | Promise<void>; steers?: boolean; onCommand?: (c: string) => void
  /** in the card above the field: what waits for the turn */
  queue?: ReactNode
  /** the share of the context window in use (0–1) and what it means, as a ring */
  context?: { value: number; title: string }
}
/** the run buttons' words (also the showroom's) */
export const runLabels = (tr: ReturnType<typeof useT>, steers?: boolean): RunActionsLabels => ({
  send: tr("core:composer.send"), sending: tr("core:composer.submitting"), stop: tr("core:composer.stop"), stopping: tr("core:composer.stopping"), stopTip: tr("core:composer.stopTip"),
  queue: tr("core:composer.queue"), queueTip: steers ? tr("core:composer.queueTipSteer", { key: ALT.glyph + "⏎" }) : tr("core:composer.queueSendTip"), steer: tr("core:composer.steer"), steerTip: tr("core:composer.steerTip"),
})
export function Composer(props: ComposerProps) {
  return <ComposerInput key={props.draftKey} {...props} />
}
function ComposerInput({ placeholder, onSend, model, left, autoFocus, className, testId, disabled, busy, onStop, steers, onCommand, handle, draftKey, queue, context }: ComposerProps) {
  const tr = useT()
  const [initial] = useState(() => readDraft(draftKey))
  const [v, setV] = useState(initial.text)
  const [sending, setSending] = useState(false)
  /* stop was pressed and the turn has not ended yet; a stop that never lands frees the button again */
  const [stopping, setStopping] = useState(false)
  useEffect(() => { if (!busy) setStopping(false) }, [busy])
  useEffect(() => { if (!stopping) return; const t = setTimeout(() => setStopping(false), 10_000); return () => clearTimeout(t) }, [stopping])
  const stop = () => { if (!onStop || stopping) return; setStopping(true); void Promise.resolve(onStop()).catch(() => setStopping(false)) }
  const queues = !!busy && !!onStop
  const inFlight = useRef(false)
  const up = useUploads(initial.files, draftKey, (files) => {
    const draft = readDraft(draftKey)
    saveDraft(draftKey, { ...draft, files })
  })
  const [drag, setDrag] = useState(0)
  const [menu, setMenu] = useState<{ kind: MenuKind; q: string; start: number; end: number } | null>(null)
  const [sel, setSel] = useState(0)
  const ta = useRef<HTMLTextAreaElement>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const chat = testId === "chat"
  const men = useMentions(chat && menu?.kind === "@" ? menu.q : null, initial.mentions)
  useEffect(() => { saveDraft(draftKey, { text: v, files: up.ready, mentions: men.snapshot() }) }, [draftKey, v, up.list])
  /* attachments go by id once uploaded; sending waits until every upload has finished (failed ones are just left out) */
  const live = useRef({ v, n: up.list.length }); live.current = { v, n: up.list.length }
  useEffect(() => {
    if (!handle) return
    handle.current = { empty: () => !live.current.v.trim() && !live.current.n, focus: () => ta.current?.focus(), fill: (t, fs) => { setV(t); up.put(fs); requestAnimationFrame(() => { ta.current?.focus(); ta.current?.setSelectionRange(t.length, t.length) }) } }
    return () => { handle.current = null }
  }, [handle])
  const has = !!v.trim() || up.ready.length > 0
  const canSend = !disabled && !up.uploading && has
  const send = async (how?: SendHow) => {
    const t = v.trim()
    if (disabled || (busy && !onStop) || inFlight.current || up.uploading || (!t && !up.ready.length)) return
    const sentDraft = draftToSend(draftKey, { text: v, files: up.ready, mentions: men.snapshot() })
    inFlight.current = true; setSending(true)
    try {
      const sent = await onSend(t || "（附件）", up.ready.map((f) => f.id), up.ready, men.take(t), sentDraft.clientId, queues ? how ?? (steers ? "steer" : "queue") : undefined)
      if (sent === false) return
      if (clearSentDraft(draftKey, sentDraft)) { setV(""); up.clear(); men.clear(); setMenu(null) }
    } catch { /* the action reports its error; keep the draft for retry */ }
    finally { inFlight.current = false; setSending(false); requestAnimationFrame(() => ta.current?.focus()) }
  }
  const takeFiles = (fs: FileList | File[] | null | undefined) => { if (!disabled && !inFlight.current && fs && fs.length) { up.add(fs); return true } return false }
  const isFileDrag = (e: React.DragEvent) => [...(e.dataTransfer?.types ?? [])].includes("Files")
  type Opt = { id: string; label: ReactNode; ins: string; cmd?: string }
  const opts: Opt[] = !menu ? [] : menu.kind === "#"
    ? st.items.filter((i) => !i.arch && (!menu.q || i.id.toLowerCase().includes(menu.q.toLowerCase()) || i.t.includes(menu.q))).slice(0, 6).map((i) => ({ id: i.id, ins: i.id, label: <><ItemNo id={i.id} className="w-14" /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><Status s={itemDisplayStatus(i)} /></> }))
    : menu.kind === "@" && men.opts ? men.opts
    : menu.kind === "@"
    ? [...Object.keys(st.bots).filter((k) => st.bots[k].on && !st.bots[k].main && (!menu.q || st.bots[k].n.includes(menu.q))).map((k) => ({ id: k, ins: "@" + st.bots[k].n.replace(/\s+/g, ""), label: <><BotAvatar k={k} size={18} /><span className="type-callout">{tr("core:composer.botName", { name: st.bots[k].n })}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.handTo", { role: st.bots[k].role })}</span></> })),
      ...st.pp.contacts.filter((c) => !c.me && c.kind !== "未确认" && (!menu.q || [c.n, ...c.aliases].some((x) => x.toLowerCase().includes(menu.q.toLowerCase())))).slice(0, 5).map((c) => ({ id: c.id, ins: "@" + c.n.split(" ")[0], label: <><span className="flex size-4.5 items-center justify-center rounded-full bg-muted type-label leading-none text-muted-foreground">{c.n[0]}</span><span className="type-callout">{c.n}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.relate", { info: [st.pp.orgs.find((o) => o.id === c.org)?.n, c.title].filter(Boolean).join(" · ") })}</span></> })),
      ...st.pp.teams.filter((g) => !menu.q || g.n.includes(menu.q)).slice(0, 3).map((g) => ({ id: g.id, ins: "@" + g.n.replace(/\s+/g, ""), label: <><UsersRoundIcon className="size-4.5 text-muted-foreground" /><span className="type-callout">{g.n}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.team", { count: g.members.length })}</span></> }))]
    : COMMANDS.filter(([n, c]) => !menu.q || n.includes(menu.q) || tr(`core:cmd.${c}`).toLowerCase().includes(menu.q.toLowerCase())).map(([, c]) => ({ id: c, ins: "", cmd: c, label: <><SlashIcon className="size-3.5 text-muted-foreground" /><span className="type-callout">{tr(`core:cmd.${c}`)}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr(`core:cmd.${c}Desc`)}</span></> }))
  const track = (val: string, caret: number) => {
    const before = val.slice(0, caret)
    const m = before.match(/#([^\s#@]{0,24})$/) || (chat ? before.match(/(?:^|\s)@([^\s#@]{0,12})$/) : null) || (chat ? before.match(/^\/(\S{0,12})$/) : null)
    const kind: MenuKind | null = !m ? null : m[0].trim().startsWith("#") ? "#" : m[0].trim().startsWith("@") ? "@" : "/"
    if (kind === "#" && !menu && chat) cue("inspect")
    setMenu(m && kind ? { kind, q: m[1], start: caret - m[0].trimStart().length, end: caret } : null); setSel(0)
  }
  const insert = (o: Opt) => {
    if (!menu) return
    men.pick(o)
    if (o.cmd) { setV(""); setMenu(null); onCommand?.(o.cmd); return }
    const nv = v.slice(0, menu.start) + o.ins + " " + v.slice(menu.end), pos = menu.start + o.ins.length + 1
    setV(nv); setMenu(null); requestAnimationFrame(() => { ta.current?.focus(); ta.current?.setSelectionRange(pos, pos) })
  }
  /* the plus menu's “#” and “@”: the trigger goes in at the caret (after a space), and its menu opens as if typed */
  const typeTrigger = (c: "#" | "@") => {
    const el = ta.current, at = el?.selectionStart ?? v.length, end = el?.selectionEnd ?? at
    const pre = v.slice(0, at), ins = (pre && !/\s$/.test(pre) ? " " : "") + c, nv = pre + ins + v.slice(end), pos = at + ins.length
    setV(nv); track(nv, pos)
    requestAnimationFrame(() => { ta.current?.focus(); ta.current?.setSelectionRange(pos, pos) })
  }
  const add: { title?: string; rows: ComposerAddRow[] }[] = [
    { rows: [
      { id: "files", label: tr("core:composer.addFiles"), description: tr("core:composer.addFilesDesc"), icon: <PaperclipIcon strokeWidth={1.75} /> },
      { id: "#", label: tr("core:composer.addItem"), description: "#", icon: <HashIcon strokeWidth={1.75} /> },
      ...(chat ? [{ id: "@", label: tr("core:composer.addMention"), description: "@", icon: <AtSignIcon strokeWidth={1.75} /> }] : []),
    ] },
    ...(chat && onCommand ? [{ title: tr("core:composer.headCommand"), rows: COMMANDS.map(([, c]) => { const I = CMD_ICON[c]; return { id: "cmd:" + c, label: tr(`core:cmd.${c}`), description: tr(`core:cmd.${c}Desc`), icon: <I strokeWidth={1.75} /> } }) }] : []),
  ]
  const onAdd = (id: string) => {
    if (id === "files") { fileInput.current?.click(); return }
    if (id === "#" || id === "@") { typeTrigger(id); return }
    if (id.startsWith("cmd:")) onCommand?.(id.slice(4))
  }
  /* the textarea's own text is transparent; this copy behind it, laid out the same way, shows the marks */
  const mirror = useRef<HTMLDivElement>(null)
  const atKind = (ins: string): "bot" | "party" | undefined => {
    const m = men.known(ins); if (m) return m.kind
    const n = ins.slice(1)
    if (Object.values(st.bots).some((b) => b.on && [b.n, b.n + "Bot"].map((x) => x.replace(/\s+/g, "")).includes(n))) return "bot"
    if (st.pp.contacts.some((c) => !c.me && c.n.split(" ")[0] === n) || st.pp.teams.some((g) => g.n.replace(/\s+/g, "") === n)) return "party"
  }
  const marked = markText(v, atKind, menu && [menu.start, menu.end])
  useLayoutEffect(() => { if (mirror.current && ta.current) mirror.current.scrollTop = ta.current.scrollTop })
  const head = menu?.kind === "#" ? <><HashIcon className="size-3.5" />{tr("core:composer.headItem")}</> : menu?.kind === "@" ? <><AtSignIcon className="size-3.5" />{tr("core:composer.headMention")}</> : <><SlashIcon className="size-3.5" />{tr("core:composer.headCommand")}</>
  const [floating, leaving] = useLinger(menu && opts.length > 0 && { kind: menu.kind, opts, groups: menu.kind === "@" && men.opts ? men.groups(opts) : [{ head, xs: opts }] })
  /* the keys, said once in the row: while a turn runs what Enter does, else how to send (the chat says its menus instead) */
  const hint = queues ? (steers ? <><Kbd>⏎</Kbd>{tr("core:composer.steer")}<Kbd>{ALT.glyph}⏎</Kbd>{tr("core:composer.queue")}</> : <><Kbd>Esc</Kbd>{tr("core:composer.stop")}</>)
    : chat ? <><Kbd>#</Kbd>{tr("core:composer.hintItem")}<Kbd>@</Kbd>Bot<Kbd>/</Kbd>{tr("core:composer.hintCommand")}</>
    : <><Kbd>⏎</Kbd>{tr("core:composer.hintSend")}<Kbd>⇧⏎</Kbd>{tr("core:composer.hintNewline")}</>
  const keysHelp = queues ? (steers ? tr("core:composer.keysSteer", { alt: ALT.name }) : tr("core:composer.keysQueue")) : tr("core:composer.keysSend")
  return (
    <div className="relative" data-dropzone
      onDragEnter={(e) => { if (isFileDrag(e)) { e.preventDefault(); setDrag((n) => n + 1) } }}
      onDragOver={(e) => { if (isFileDrag(e)) { e.preventDefault(); e.dataTransfer.dropEffect = "copy" } }}
      onDragLeave={(e) => { if (isFileDrag(e)) setDrag((n) => Math.max(0, n - 1)) }}
      onDrop={(e) => { if (isFileDrag(e)) { e.preventDefault(); setDrag(0); takeFiles(e.dataTransfer.files) } }}
      onKeyDown={(e) => { if (e.key === "Escape" && queues && !menu && !e.nativeEvent.isComposing && !e.defaultPrevented && (e.target as HTMLElement).closest("[data-composer]")) { e.preventDefault(); stop() } }}>
      {floating && (
        <Command shouldFilter={false} value={floating.opts[sel]?.id} className="absolute inset-x-2 bottom-full z-30 mb-2 h-auto max-w-md origin-bottom-left rounded-2xl bg-popover shadow-[var(--lift-hover)] anim-float" data-ref-menu={leaving ? undefined : floating.kind} data-leaving={leaving || undefined} aria-hidden={leaving || undefined}>
          <CommandList>
            {floating.groups.filter((g) => g.xs.length).map((g, k) => (
            <CommandGroup key={k} heading={<span className="inline-flex items-center gap-1.5">{g.head}</span>}>
              {g.xs.map((o) => { const n = floating.opts.indexOf(o); return (
                <CommandItem key={o.id} value={o.id} onMouseDown={(e) => { e.preventDefault(); insert(o) }} onSelect={() => insert(o)} onMouseEnter={() => setSel(n)} data-ref-opt={o.id} className="gap-2.5 rounded-lg">{o.label}</CommandItem>
              ) })}
            </CommandGroup>))}
          </CommandList>
        </Command>
      )}
      <input ref={fileInput} type="file" multiple accept={ACCEPT} className="hidden" data-file-input onChange={(e) => { if (e.target.files?.length) takeFiles(e.target.files); e.target.value = "" }} />
      <ChatComposer className={className} testId={testId} drop={drag > 0} disabled={disabled} onFocusField={() => ta.current?.focus()}
        head={(queue || up.list.length > 0) ? <>{queue}{up.list.length > 0 && <PendingList list={up.list} onRemove={(key) => { if (!inFlight.current) up.remove(key) }} />}</> : undefined}
        add={add} addLabel={tr("core:composer.add")} onAdd={onAdd} tools={left && <span className="contents max-sm:[&_[data-slot=tabs-list]]:h-7! max-sm:[&_[data-slot=tabs-trigger]]:px-2 max-sm:[&_[data-slot=tabs-trigger]]:type-footnote">{left}</span>}
        model={model}
        end={<>
          {/* once something is typed during a turn, the buttons themselves say what it can do */}
          {!(queues && has) && <span className={cn("mr-1 hidden items-center gap-1 whitespace-nowrap type-footnote text-tertiary", chat || queues ? "lg:inline-flex" : "xl:inline-flex")} aria-hidden data-composer-hint>{hint}</span>}
          {context && <RunContext value={context.value} title={context.title} />}
          <RunActions running={!!busy} has={has} canSend={canSend} sending={sending} stopping={stopping} steers={steers} onStop={onStop ? stop : undefined}
            onSend={() => void send()} onQueue={() => void send("queue")} onSteer={() => void send("steer")}
            labels={runLabels(tr, steers)} />
        </>}>
        <div ref={mirror} aria-hidden data-composer-marks className="ui-ccc__under [scrollbar-gutter:stable]">
          {marked.map(([t, k], n) => k ? <span key={n} data-mark={k} className={cn("rounded-md ring-2", MARK[k])}>{t}</span> : <Fragment key={n}>{t}</Fragment>)}{v.endsWith("\n") && "\u200b"}
        </div>
        <textarea ref={ta} autoFocus={autoFocus} disabled={disabled || sending} value={v} placeholder={drag > 0 ? tr("core:composer.dropFiles") : placeholder} rows={1}
          data-under aria-label={placeholder} aria-description={keysHelp} className="ui-ccc__message relative [scrollbar-gutter:stable]"
          onPaste={(e) => { if (e.clipboardData?.files?.length && takeFiles(e.clipboardData.files)) e.preventDefault() }}
          onChange={(e) => { if (chat && !v && e.target.value) cue("input"); else if (chat && v && !e.target.value && !st.thinking) cue("idle"); setV(e.target.value); track(e.target.value, e.target.selectionStart ?? e.target.value.length) }}
          onScroll={(e) => { if (mirror.current) mirror.current.scrollTop = e.currentTarget.scrollTop }}
          onKeyDown={(e) => {
            if (menu && opts.length) {
              if (e.key === "ArrowDown") { e.preventDefault(); setSel((x) => (x + 1) % opts.length); return }
              if (e.key === "ArrowUp") { e.preventDefault(); setSel((x) => (x - 1 + opts.length) % opts.length); return }
              if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); insert(opts[sel]); return }
              if (e.key === "Escape") { e.preventDefault(); setMenu(null); return }
            }
            const k = runKey(e)
            if (!k) return
            e.preventDefault()
            if (busy && !onStop) return
            void send(queues ? (k === "queue" || !steers ? "queue" : "steer") : undefined)
          }} />
      </ChatComposer>
    </div>
  )
}
