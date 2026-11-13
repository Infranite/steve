/* 能力 → 技能：一个 Bot 的技能（GET/POST/PATCH/DELETE /bots/:id/skills）。技能是一份 SKILL.md：名称、什么时候用
 * （描述）和正文。会话打开时 Node 把启用的技能写到 harness 读技能的地方——哪种 harness 读、读哪里，是它的适配器声明的
 * （RRuntime.skills），页面只照着说「这个 harness 不读技能」。每个技能是一张和 MCP 服务器一样的 Integration Settings
 * 卡片：名称、描述、开关（马上保存）、编辑和删除。编辑在对话框里，正文可以在「编辑 / 预览」之间切换；也可以粘贴一整份
 * SKILL.md，开头的 name 和 description 会填进表单。 */
import { useEffect, useState } from "react"
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
import { isNotWired } from "@/api"
import { harnessSkills, runtimeName, toast } from "@/store"
import { botSkillsApi, type BotSkill } from "@/api/bot-skills"
import { parseSkillMd, skillName, skillNameOk, type SkillMd } from "@/skill-md"

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

type Load = { s: "loading" } | { s: "unwired" } | { s: "error"; e: string } | { s: "ok"; v: BotSkill[] }
/** what the dialog edits: a stored skill, or a new one (blank or from a pasted SKILL.md) */
type Editing = { x: BotSkill } | { draft: SkillMd }

export function BotSkillsSection({ bot, harness }: { bot: string; harness: string }) {
  const t = useT()
  const [d, setD] = useState<Load>({ s: "loading" })
  const [editing, setEditing] = useState<Editing | null>(null)
  const [importing, setImporting] = useState(false)
  const [removing, setRemoving] = useState<BotSkill | null>(null)
  const load = () => botSkillsApi.list(bot).then((v) => setD({ s: "ok", v }), (e) => setD(isNotWired(e) ? { s: "unwired" } : { s: "error", e: errText(e) }))
  useEffect(() => { setD({ s: "loading" }); void load() }, [bot])
  const toggle = async (x: BotSkill, on: boolean) => {
    if (d.s === "ok") setD({ s: "ok", v: d.v.map((y) => (y.id === x.id ? { ...y, enabled: on } : y)) })
    try { await botSkillsApi.update(bot, x.id, { enabled: on }); toast(t(on ? "manage:skills.on" : "manage:skills.off", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const remove = async (x: BotSkill) => {
    try { await botSkillsApi.remove(bot, x.id); toast(t("manage:skills.removed", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const blank = () => setEditing({ draft: { name: "", description: "", content: "" } })
  const actions = (size: "xs" | "sm") => (
    <span className="flex flex-wrap items-center justify-center gap-1">
      <Button size={size} variant={size === "sm" ? "default" : "ghost"} className={size === "xs" ? "text-muted-foreground" : undefined} onClick={blank} data-skill-add><PlusIcon />{t("manage:skills.add")}</Button>
      <Button size={size} variant="ghost" className="text-muted-foreground" onClick={() => setImporting(true)} data-skill-import><ClipboardPasteIcon />{t("manage:skills.import")}</Button>
    </span>
  )
  const reads = harnessSkills(harness)
  return (
    <Section label={t("manage:skills.section")} note={t("manage:skills.note")} right={d.s === "ok" && d.v.length > 0 ? actions("xs") : undefined} data-skills>
      {reads === false && d.s === "ok" && <Notice className="mb-3" data-skills-unread>{t("manage:skills.notRead", { harness: runtimeName(harness) ?? harness })}</Notice>}
      {d.s === "loading" ? <Loading className="py-8" />
        : d.s === "unwired" ? <Blank>{t("manage:skills.unwired")}</Blank>
        : d.s === "error" ? <Notice tone="destructive">{d.e}</Notice>
        : d.v.length === 0 ? <Empty compact className="bg-muted/50" icon={<SparklesIcon />} title={t("manage:skills.emptyTitle")} sub={t("manage:skills.emptySub")} data-skills-empty action={actions("sm")} />
        : <div className="flex flex-col gap-3" data-skills-list>{d.v.map((x) => <SkillCard key={x.id} x={x} onToggle={(on) => void toggle(x, on)} onEdit={() => setEditing({ x })} onRemove={() => setRemoving(x)} />)}</div>}
      {editing && <SkillDialog bot={bot} e={editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void load() }} />}
      {importing && <ImportDialog onClose={() => setImporting(false)} onRead={(draft) => { setImporting(false); setEditing({ draft }) }} />}
      <AlertDialog open={!!removing} onOpenChange={(o) => !o && setRemoving(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("manage:skills.removeTitle", { name: removing?.name ?? "" })}</AlertDialogTitle><AlertDialogDescription>{t("manage:skills.removeDesc")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const x = removing; setRemoving(null); if (x) void remove(x) }} data-skill-remove-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </Section>
  )
}

function SkillCard({ x, onToggle, onEdit, onRemove }: { x: BotSkill; onToggle: (on: boolean) => void; onEdit: () => void; onRemove: () => void }) {
  const t = useT()
  return (
    <IntegrationSettings name={x.name} logo={<SparklesIcon className="size-5" />} data-bot-skill={x.name} data-bot-skill-on={x.enabled}
      meta={<span className="line-clamp-2 whitespace-normal type-footnote" title={x.description} data-skill-description>{x.description}</span>}
      pausedMeta={<span className="type-footnote">{t("manage:skills.paused")}</span>}
      enabled={x.enabled} switchLabel={t("manage:skills.enable", { name: x.name })} onToggle={onToggle}
      aside={<span className="flex shrink-0 items-center">
        <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("manage:skills.edit", { name: x.name })} onClick={onEdit} data-skill-edit={x.name}><PencilIcon /></Button>
        <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("manage:skills.remove", { name: x.name })} onClick={onRemove} data-skill-remove={x.name}><TrashIcon /></Button>
      </span>} />
  )
}

function SkillDialog({ bot, e, onClose, onSaved }: { bot: string; e: Editing; onClose: () => void; onSaved: () => void }) {
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
  const save = async () => {
    setBusy(true); setErr(null)
    const b = { name: name.trim(), description: desc, content }
    try {
      const r = x ? await botSkillsApi.update(bot, x.id, b) : await botSkillsApi.create(bot, b)
      toast(t(x ? "manage:skills.saved" : "manage:skills.added", { name: r.name })); onSaved()
    } catch (e) { setErr(errText(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[720px]" data-skill-dialog>
        <DialogHeader><DialogTitle>{x ? t("manage:skills.dialogEdit", { name: x.name }) : t("manage:skills.dialogNew")}</DialogTitle><DialogDescription>{t("manage:skills.dialogDesc")}</DialogDescription></DialogHeader>
        <div className="flex min-w-0 flex-col gap-4">
          <TextField label={t("manage:skills.name")} hint={nameBad ? undefined : t("manage:skills.nameHint")} error={nameBad ? t("manage:skills.nameBad") : undefined} value={name} onChange={(ev) => setName(ev.target.value)} maxLength={64} inputClassName="font-mono" autoComplete="off" spellCheck={false} data-skill-name />
          <TextArea label={t("manage:skills.description")} hint={t("manage:skills.descriptionHint")} minRows={2} maxRows={4} maxLength={1024} value={description} onChange={(ev) => setDescription(ev.target.value)} data-skill-description-edit />
          <div className="flex min-w-0 flex-col gap-1.5">
            <div className="flex items-center justify-between gap-2">
              <span className="type-label text-muted-foreground">{t("manage:skills.content")}</span>
              <Segmented size="sm" value={view} onChange={setView} label={t("manage:skills.content")} options={[["write", t("manage:skills.write")], ["preview", t("manage:skills.preview")]] as const} />
            </div>
            {view === "write"
              ? <TextArea minRows={10} maxRows={22} inputClassName="type-code font-mono!" aria-label={t("manage:skills.content")} placeholder={t("manage:skills.contentPlaceholder")} value={content} onChange={(ev) => setContent(ev.target.value)} spellCheck={false} data-skill-content />
              : <div className="max-h-[50vh] min-h-40 overflow-auto rounded-xl bg-muted/50 px-4 py-3" data-skill-preview>
                {content.trim() ? <Markdown text={content} className="break-words type-callout" /> : <span className="type-callout text-muted-foreground">{t("manage:skills.previewEmpty")}</span>}
              </div>}
            <span className="type-footnote text-muted-foreground">{t("manage:skills.contentHint")}</span>
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
function ImportDialog({ onClose, onRead }: { onClose: () => void; onRead: (d: SkillMd) => void }) {
  const t = useT()
  const [text, setText] = useState("")
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[640px]" data-skill-import-dialog>
        <DialogHeader><DialogTitle>{t("manage:skills.importTitle")}</DialogTitle><DialogDescription>{t("manage:skills.importDesc")}</DialogDescription></DialogHeader>
        <Textarea rows={12} value={text} onChange={(e) => setText(e.target.value)} placeholder={"---\nname: code-review\ndescription: …\n---\n\n# …"} aria-label={t("manage:skills.importTitle")} className="max-h-[50vh] font-mono type-footnote" spellCheck={false} data-skill-paste />
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button disabled={!text.trim()} onClick={() => onRead(parseSkillMd(text))} data-skill-import-next>{t("manage:skills.importNext")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
