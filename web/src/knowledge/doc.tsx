/* A document (`#/knowledge/doc/:id?seq=`) and a note (`#/knowledge/note/:id`): properties, the chunks the
 * index holds, and for notes the editor. */
import { useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Markdown } from "@/components/markdown"
import { Page, SectionLabel, Segmented, Empty, Meta, go } from "@/ui"
import { toast, tm } from "@/store"
import { kb, SOURCE_KIND, visLabel, levelLabel, type Doc, type Level } from "@/api/knowledge"
import { LevelSel, VisSel, errMsg } from "@/knowledge/fields"
import { t as tt, useT } from "@/i18n"

/** the back link, resolved at render so it follows the UI language */
const back = (): [string, string] => ["#/knowledge", tt("knowledge:page.title")]
const seqOf = () => { const v = new URLSearchParams(location.hash.split("?")[1] ?? "").get("seq"); return v === null ? null : Number(v) }

function useDoc(id: string) {
  const [d, setD] = useState<Doc | null>(null), [err, setErr] = useState("")
  useEffect(() => { let on = true; kb.doc(id).then((x) => on && setD(x), (e) => on && setErr(errMsg(e))); return () => { on = false } }, [id])
  return { d, setD, err }
}

function Prop({ label, children }: { label: string; children: ReactNode }) {
  return <div className="flex min-h-9 items-center gap-3 border-t py-1 first:border-t-0"><span className="w-24 shrink-0 type-footnote text-muted-foreground">{label}</span><div className="flex min-w-0 flex-1 items-center justify-end gap-2 text-right type-callout sm:justify-start sm:text-left">{children}</div></div>
}

/** source, path, level, visibility, about, uses; level and visibility can go back to following the source */
function Props({ d, setD }: { d: Doc; setD: (d: Doc) => void }) {
  const t = useT()
  const [src, setSrc] = useState<{ level: Level; visibility: string } | null>(null)
  useEffect(() => { kb.sources().then((xs) => { const s = xs.find((x) => x.id === d.source.id); if (s) setSrc({ level: s.level, visibility: s.visibility }) }, () => {}) }, [d.source.id])
  const patch = async (p: Parameters<typeof kb.patchDoc>[1], msg: string) => { try { setD(await kb.patchDoc(d.id, p)); toast(msg) } catch (e) { toast(errMsg(e)) } }
  return (
    <section aria-label={t("knowledge:doc.props")} className="rounded-xl bg-card px-4 py-1.5 shadow-[var(--lift)]" data-doc-props>
      <Prop label={t("knowledge:doc.source")}><span>{d.source.name}<span className="text-muted-foreground"> · {SOURCE_KIND[d.source.kind] ?? d.source.kind}</span></span></Prop>
      {d.source.kind !== "notes" && d.ref && <Prop label={t("knowledge:doc.path")}><span className="truncate font-mono type-footnote">{d.ref}</span></Prop>}
      <Prop label={t("knowledge:doc.level")}><LevelSel v={d.inherited.level ? "" : d.level} inherit={src?.level ?? d.level} on={(l) => void patch({ level: l || null }, l ? t("knowledge:doc.changedTo", { label: levelLabel(l) }) : t("knowledge:doc.backToInherit"))} testId="doc-level" /></Prop>
      <Prop label={t("knowledge:doc.visibility")}><VisSel v={d.inherited.visibility ? "" : d.visibility} inherit={src?.visibility ?? d.visibility} on={(v) => void patch({ visibility: v || null }, v ? t("knowledge:doc.changedTo", { label: visLabel(v) }) : t("knowledge:doc.backToInherit"))} testId="doc-vis" /></Prop>
      {d.about.length > 0 && <Prop label={t("knowledge:doc.about")}><span className="flex flex-wrap gap-1">{d.about.map((a) => <span key={a.type + a.id} className="inline-flex h-6 items-center rounded-full bg-muted px-2 type-footnote">{a.name ?? a.id}</span>)}</span></Prop>}
      <Prop label={t("knowledge:doc.use")}><span className="text-muted-foreground" data-doc-uses>{[t("knowledge:doc.uses", { count: d.uses }), t("knowledge:doc.chunks", { count: d.chunks }), t("knowledge:doc.updated", { time: tm(d.updated) || "—" })].join(" · ")}</span></Prop>
      {d.error && <Prop label={t("knowledge:doc.error")}><span className="text-destructive">{d.error}</span></Prop>}
    </section>
  )
}

function DeleteButton({ d, note }: { d: Doc; note?: boolean }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const del = async () => { try { await kb.deleteDoc(d.id); toast(note ? t("knowledge:doc.noteDeleted") : t("knowledge:doc.fileRemoved")); go("#/knowledge") } catch (e) { toast(errMsg(e)) } }
  return <>
    <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setOpen(true)} data-doc-delete><TrashIcon />{note ? t("common:action.delete") : t("knowledge:doc.remove")}</Button>
    <AlertDialog open={open} onOpenChange={setOpen}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{note ? t("knowledge:doc.deleteNoteTitle", { title: d.title }) : t("knowledge:doc.removeFileTitle", { title: d.title })}</AlertDialogTitle>
        <AlertDialogDescription>{note ? t("knowledge:doc.deleteNoteDesc") : t("knowledge:doc.removeFileDesc")}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => void del()} data-doc-delete-ok>{note ? t("common:action.delete") : t("knowledge:doc.remove")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </>
}

/** the indexed chunks; `?seq=` scrolls to that one and marks it */
function Chunks({ d }: { d: Doc }) {
  const t = useT()
  const seq = seqOf(), box = useRef<HTMLDivElement>(null)
  useEffect(() => { if (seq !== null) box.current?.querySelector(`[data-seq="${seq}"]`)?.scrollIntoView({ block: "center" }) }, [seq, d.id])
  if (!d.preview.length) return <Empty title={t("knowledge:doc.noContent")} sub={d.status === "pending" ? t("knowledge:doc.pendingSub") : t("knowledge:doc.noTextSub")} />
  return (
    <section ref={box} aria-label={t("knowledge:doc.chunksAria")}><SectionLabel>{d.preview.length < d.chunks ? t("knowledge:doc.chunksTitlePartial", { count: d.chunks, shown: d.preview.length }) : t("knowledge:doc.chunksTitle", { count: d.chunks })}</SectionLabel>
      <div className="flex flex-col gap-2">{d.preview.map((c) => (
        <div key={c.seq} data-seq={c.seq} aria-current={c.seq === seq ? "true" : undefined} className={cn("scroll-mt-24 rounded-xl bg-card px-4 py-3 shadow-[var(--lift)]", c.seq === seq && "bg-primary/5 ring-2 ring-primary/30")}>
          {c.heading && <SectionLabel className="mb-1 min-h-0">{c.heading}</SectionLabel>}
          <div className="min-w-0 break-words" data-chunk-text><Markdown text={c.text} className="type-callout" /></div>
        </div>))}</div>
    </section>
  )
}

export function DocView({ id }: { id: string }) {
  const t = useT()
  const { d, setD, err } = useDoc(id)
  /* notes have their own editor page */
  const isNote = d?.source.kind === "notes"
  useEffect(() => { if (isNote) location.replace("#/knowledge/note/" + id + (location.hash.includes("?") ? "?" + location.hash.split("?")[1] : "")) }, [isNote, id])
  if (err) return <Page title={t("knowledge:doc.title")} back={back()}><Empty title={t("knowledge:doc.openFailed")} sub={err} /></Page>
  if (!d) return <Page title={t("knowledge:doc.title")} back={back()}><Spin /></Page>
  if (isNote) return null
  const removable = d.source.kind === "files"
  return (
    <Page title={d.title} back={back()} max={760} sub={<Meta><span>{d.source.name}</span>{d.ref && <span className="font-mono">{d.ref}</span>}</Meta>} actions={removable ? <DeleteButton d={d} /> : undefined}>
      <div className="flex flex-col gap-8"><Props d={d} setD={setD} /><Chunks d={d} /></div>
    </Page>
  )
}
const Spin = () => { const t = useT(); return <div className="flex justify-center py-16" role="status" aria-label={t("knowledge:page.loading")}><Spinner className="size-5 text-muted-foreground" /></div> }

type Mode = "edit" | "preview" | "chunks"
export function NoteView({ id }: { id: string }) {
  const t = useT()
  const { d, setD, err } = useDoc(id)
  const [title, setTitle] = useState(""), [body, setBody] = useState(""), [mode, setMode] = useState<Mode>(seqOf() !== null ? "chunks" : "edit"), [busy, setBusy] = useState(false)
  useEffect(() => { if (d) { setTitle(d.title); setBody(d.body ?? "") } }, [d?.id]) // eslint-disable-line react-hooks/exhaustive-deps
  const dirty = !!d && (title.trim() !== d.title || body !== (d.body ?? ""))
  const save = async () => {
    if (!d || !dirty || busy || !title.trim()) return
    setBusy(true)
    try { setD(await kb.saveNote(d.id, { title: title.trim(), body })); toast(t("common:state.saved")) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  const saveRef = useRef(save); saveRef.current = save
  useEffect(() => {
    const h = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") { e.preventDefault(); void saveRef.current() } }
    window.addEventListener("keydown", h); return () => window.removeEventListener("keydown", h)
  }, [])
  if (err) return <Page title={t("knowledge:doc.noteTitle")} back={back()}><Empty title={t("knowledge:doc.noteOpenFailed")} sub={err} /></Page>
  if (!d) return <Page title={t("knowledge:doc.noteTitle")} back={back()}><Spin /></Page>
  return (
    <Page title={d.title} back={back()} max={760} sub={t("knowledge:doc.noteTitle")}
      actions={<><DeleteButton d={d} note /><Button size="sm" disabled={!dirty || busy || !title.trim()} onClick={() => void save()} title="⌘S" data-note-save>{busy && <Spinner className="size-3.5" />}{dirty ? t("common:action.save") : t("common:state.saved")}</Button></>}>
      <div className="flex flex-col gap-8">
        <Props d={d} setD={setD} />
        <section className="flex flex-col gap-3" aria-label={t("knowledge:doc.body")}>
          <div className="flex flex-wrap items-center justify-between gap-2"><Segmented size="sm" value={mode} onChange={setMode} options={[["edit", t("knowledge:doc.modeEdit")], ["preview", t("knowledge:doc.modePreview")], ["chunks", t("knowledge:doc.modeChunks")]] as const} /><span className="type-footnote text-muted-foreground">{t("knowledge:doc.saveHint")}</span></div>
          {mode === "edit" && <>
            <Input aria-label={t("knowledge:newNote.titleLabel")} maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} className="h-10 md:type-headline" data-note-title />
            <Textarea aria-label={t("knowledge:doc.body")} value={body} onChange={(e) => setBody(e.target.value)} className="min-h-[360px] font-mono md:type-code" placeholder={t("knowledge:doc.bodyPlaceholder")} data-note-body />
          </>}
          {mode === "preview" && <div className="min-h-40 rounded-xl bg-card px-5 py-4 type-body shadow-[var(--lift)] anim-in">{body.trim() ? <Markdown text={body} /> : <span className="text-muted-foreground">{t("knowledge:doc.noBody")}</span>}</div>}
          {mode === "chunks" && <Chunks d={d} />}
        </section>
      </div>
    </Page>
  )
}
