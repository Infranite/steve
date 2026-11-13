/* 能力 → 技能: the workspace's skills. A skill is a SKILL.md – a name, when to use it (the description) and the body;
 * when a session opens, the Node writes the ones its Bot has switched on where the harness reads skills (the harness
 * adapter says whether and where, RRuntime.skills). Each is a card like an MCP server's: in the library with the
 * Bots that use it, on a Bot's tab with that Bot's switch. Editing is a dialog whose body switches between writing
 * and a preview; a whole pasted SKILL.md fills the form from its front matter. */
import { useState, type ReactNode } from "react"
import { ClipboardPasteIcon, PencilIcon, PlusIcon, SparklesIcon, TrashIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { TextField } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { TextArea } from "@/components/affect/text-area-clean"
import { Markdown } from "@/components/markdown"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Section, Empty, Loading, Blank, Segmented, Notice } from "@/ui"
import { toast } from "@/store"
import { capsApi, type Skill } from "@/api/caps"
import { parseSkillMd, skillName, skillNameOk, type SkillMd } from "@/skill-md"
import { AssignMenu, UsedBy, errText, useList } from "./common"

/** what the dialog edits: a stored skill, or a new one (blank or from a pasted SKILL.md) */
export type Editing = { x: Skill } | { draft: SkillMd }

/** the two ways to add one: write it, or paste a SKILL.md */
export function SkillAddActions({ size, onBlank, onImport }: { size: "xs" | "sm"; onBlank: () => void; onImport: () => void }) {
  const t = useT()
  return (
    <span className="flex flex-wrap items-center justify-center gap-1">
      <Button size={size} variant={size === "sm" ? "default" : "ghost"} className={size === "xs" ? "text-muted-foreground" : undefined} onClick={onBlank} data-skill-add><PlusIcon />{t("caps:skills.add")}</Button>
      <Button size={size} variant="ghost" className="text-muted-foreground" onClick={onImport} data-skill-import><ClipboardPasteIcon />{t("caps:skills.import")}</Button>
    </span>
  )
}

export function SkillsLibrary() {
  const t = useT()
  const [d, load] = useList(() => capsApi.skills.list(), [])
  const [editing, setEditing] = useState<Editing | null>(null)
  const [importing, setImporting] = useState(false)
  const [removing, setRemoving] = useState<Skill | null>(null)
  const remove = async (x: Skill) => {
    try { await capsApi.skills.remove(x.id); toast(t("caps:skills.removed", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const blank = () => setEditing({ draft: { name: "", description: "", content: "" } })
  const actions = (size: "xs" | "sm") => <SkillAddActions size={size} onBlank={blank} onImport={() => setImporting(true)} />
  return (
    <Section label={t("caps:skills.section")} note={t("caps:skills.note")} right={d.s === "ok" && d.v.length > 0 ? actions("xs") : undefined} data-skills>
      {d.s === "loading" ? <Loading className="py-8" />
        : d.s === "unwired" ? <Blank>{t("caps:skills.unwired")}</Blank>
        : d.s === "error" ? <Notice tone="destructive">{d.e}</Notice>
        : d.v.length === 0 ? <Empty compact className="bg-muted/50" icon={<SparklesIcon />} title={t("caps:skills.emptyTitle")} sub={t("caps:skills.emptySub")} data-skills-empty action={actions("sm")} />
        : <div className="flex flex-col gap-3" data-skills-list>{d.v.map((x) => (
            <SkillCard key={x.id} x={x}
              aside={<>
                <AssignMenu kind="skill" id={x.id} name={x.name} users={x.bots ?? []} onChange={() => void load()} />
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:skills.edit", { name: x.name })} onClick={() => setEditing({ x })} data-skill-edit={x.name}><PencilIcon /></Button>
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:skills.remove", { name: x.name })} onClick={() => setRemoving(x)} data-skill-remove={x.name}><TrashIcon /></Button>
              </>}
              footer={<span className="flex min-w-0 flex-wrap items-center gap-2"><span className="type-footnote text-muted-foreground">{t("caps:use.usedBy")}</span><UsedBy users={x.bots ?? []} /></span>} />))}</div>}
      {editing && <SkillDialog e={editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void load() }} />}
      {importing && <ImportDialog onClose={() => setImporting(false)} onRead={(draft) => { setImporting(false); setEditing({ draft }) }} />}
      <AlertDialog open={!!removing} onOpenChange={(o) => !o && setRemoving(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("caps:skills.removeTitle", { name: removing?.name ?? "" })}</AlertDialogTitle><AlertDialogDescription>{t("caps:skills.removeDesc", { count: removing?.bots?.length ?? 0 })}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const x = removing; setRemoving(null); if (x) void remove(x) }} data-skill-remove-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </Section>
  )
}

export function SkillCard({ x, aside, footer, enabled, onToggle, switchLabel }: { x: Skill; aside?: ReactNode; footer?: ReactNode; enabled?: boolean; onToggle?: (on: boolean) => void; switchLabel?: string }) {
  const t = useT()
  return (
    <IntegrationSettings name={x.name} logo={<SparklesIcon className="size-5" />} data-bot-skill={x.name} data-bot-skill-on={enabled}
      meta={<span className="line-clamp-2 whitespace-normal type-footnote" title={x.description} data-skill-description>{x.description}</span>}
      pausedMeta={<span className="type-footnote">{t("caps:bot.paused")}</span>}
      enabled={enabled} switchLabel={switchLabel} onToggle={onToggle}
      aside={aside && <span className="flex shrink-0 items-center">{aside}</span>} footer={footer} />
  )
}

/** a new skill (with `bot`: also given to that Bot) or an edit of one */
export function SkillDialog({ e, bot, onClose, onSaved }: { e: Editing; bot?: string; onClose: () => void; onSaved: (s: Skill) => void }) {
  const t = useT()
  const x = "x" in e ? e.x : null
  const from = "x" in e ? e.x : e.draft
  const [name, setName] = useState(x ? from.name : skillName(from.name))
  const [description, setDescription] = useState(from.description)
  const [content, setContent] = useState(from.content)
  const [view, setView] = useState<"write" | "preview">("write")
  const [busy, setBusy] = useState(false), [err, setErr] = useState<string | null>(null)
  const nameBad = name.trim() !== "" && !skillNameOk(name.trim())
  const desc = description.replace(/\s+/g, " ").trim()
  const shared = (x?.bots?.length ?? 0) > 1
  const save = async () => {
    setBusy(true); setErr(null)
    const b = { name: name.trim(), description: desc, content }
    try {
      const r = x ? await capsApi.skills.update(x.id, b) : await capsApi.skills.create(bot ? { ...b, bot } : b)
      toast(t(x ? "caps:skills.saved" : "caps:skills.added", { name: r.name })); onSaved(r)
    } catch (e) { setErr(errText(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[720px]" data-skill-dialog>
        <DialogHeader><DialogTitle>{x ? t("caps:skills.dialogEdit", { name: x.name }) : t("caps:skills.dialogNew")}</DialogTitle><DialogDescription>{bot && !x ? t("caps:skills.dialogDescBot") : t("caps:skills.dialogDesc")}</DialogDescription></DialogHeader>
        <div className="flex min-w-0 flex-col gap-4">
          {shared && <Notice data-skill-shared>{t("caps:skills.shared", { count: x?.bots?.length ?? 0 })}</Notice>}
          <TextField label={t("caps:skills.name")} hint={nameBad ? undefined : t("caps:skills.nameHint")} error={nameBad ? t("caps:skills.nameBad") : undefined} value={name} onChange={(ev) => setName(ev.target.value)} maxLength={64} inputClassName="font-mono" autoComplete="off" spellCheck={false} data-skill-name />
          <TextArea label={t("caps:skills.description")} hint={t("caps:skills.descriptionHint")} minRows={2} maxRows={4} maxLength={1024} value={description} onChange={(ev) => setDescription(ev.target.value)} data-skill-description-edit />
          <div className="flex min-w-0 flex-col gap-1.5">
            <div className="flex items-center justify-between gap-2">
              <span className="type-label text-muted-foreground">{t("caps:skills.content")}</span>
              <Segmented size="sm" value={view} onChange={setView} label={t("caps:skills.content")} options={[["write", t("caps:skills.write")], ["preview", t("caps:skills.preview")]] as const} />
            </div>
            {view === "write"
              ? <TextArea minRows={10} maxRows={22} inputClassName="type-code font-mono!" aria-label={t("caps:skills.content")} placeholder={t("caps:skills.contentPlaceholder")} value={content} onChange={(ev) => setContent(ev.target.value)} spellCheck={false} data-skill-content />
              : <div className="max-h-[50vh] min-h-40 overflow-auto rounded-xl bg-muted/50 px-4 py-3" data-skill-preview>
                {content.trim() ? <Markdown text={content} className="break-words type-callout" /> : <span className="type-callout text-muted-foreground">{t("caps:skills.previewEmpty")}</span>}
              </div>}
            <span className="type-footnote text-muted-foreground">{t("caps:skills.contentHint")}</span>
          </div>
          {err && <Notice tone="destructive" data-skill-error>{err}</Notice>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button disabled={busy || !skillNameOk(name.trim()) || !desc} onClick={() => void save()} data-skill-save>{t("common:action.save")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** paste a whole SKILL.md: its name and description fill the form, the rest is the content */
export function ImportDialog({ onClose, onRead }: { onClose: () => void; onRead: (d: SkillMd) => void }) {
  const t = useT()
  const [text, setText] = useState("")
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[640px]" data-skill-import-dialog>
        <DialogHeader><DialogTitle>{t("caps:skills.importTitle")}</DialogTitle><DialogDescription>{t("caps:skills.importDesc")}</DialogDescription></DialogHeader>
        <Textarea rows={12} value={text} onChange={(e) => setText(e.target.value)} placeholder={"---\nname: code-review\ndescription: …\n---\n\n# …"} aria-label={t("caps:skills.importTitle")} className="max-h-[50vh] font-mono type-footnote" spellCheck={false} data-skill-paste />
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button disabled={!text.trim()} onClick={() => onRead(parseSkillMd(text))} data-skill-import-next>{t("caps:skills.importNext")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
