/* 对外身份 (channels-personas.md §3, §8; prototype channels.tsx PersonasSection / PersonaView / introOf): a Bot's “face” in
 * the channels — name, self-introduction, voice, what it reveals, signature. Routes: #/channels/personas, #/channels/personas/:id, and the
 * Bot page's 对外身份 tab. Nothing here sends a message. */
import { useEffect, useMemo, useState } from "react"
import { ChevronRightIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { go, Page, Group, Row, SectionLabel, Empty, BotAvatar, Panel, Chevron } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { st, botName } from "@/store"
import { useLoad, useBusy } from "@/accounts/load"
import { channelsApi, type Persona, type Reveal } from "@/api/channels"
import { PersonaAvatar } from "@/channels/basis"
import { OwnerChip } from "@/people/owner-chip"
import { t, useT } from "@/i18n"
import { introOf, newPersonaDefaults, ownerOf } from "@/channels/persona-intro"

const usedText = (p: Persona) => {
  const a = p.used_by.accounts.length, s = p.used_by.spaces.length
  return a + s ? [a ? t("channels:persona.usedAccounts", { count: a }) : "", s ? t("channels:persona.usedSpaces", { count: s }) : ""].filter(Boolean).join(" · ") : t("channels:persona.usedNone")
}

/** the list, all of them or one Bot's (the Bot page's 对外身份 tab). `panel`: the rows in a lifted Panel, as the
 *  configuration pages lay them out */
export function PersonasSection({ bot, panel = false }: { bot?: string; panel?: boolean }) {
  const t = useT()
  const L = useLoad(() => channelsApi.personas(), [], ["persona", "space", "channel"])
  const [busy, act] = useBusy()
  if (L.s === "unwired") return bot ? <Empty title={t("channels:persona.unwiredTitle")} sub={t("channels:persona.unwiredSub")} /> : <Unwired id="personas" />
  if (L.s === "loading") return <div className="flex justify-center py-10"><Spinner className="size-5 text-muted-foreground" /></div>
  if (L.s === "error") return <Empty title={L.err} />
  const xs = bot ? L.data.filter((p) => p.bot_id === bot) : L.data
  const create = async () => {
    const p = await act(() => channelsApi.newPersona({ ...newPersonaDefaults(), bot_id: bot ?? "steve" }))
    if (p) go("#/channels/personas/" + p.id)
  }
  const List = panel ? Panel : Group
  return (
    <section data-personas>
      <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void create()} data-new-persona><PlusIcon />{t("channels:persona.create")}</Button>}>{t("channels:persona.listLabel", { n: xs.length })}</SectionLabel>
      {xs.length ? <List>{xs.map((p) => (
        <Row key={p.id} icon={<PersonaAvatar bot={p.bot_id} size={32} />} onClick={() => go("#/channels/personas/" + p.id)} data-persona-row={p.name}
          title={<span className="inline-flex items-center gap-1.5">{p.name}{p.is_default && <span className="type-footnote text-muted-foreground">{t("channels:persona.isDefault")}</span>}</span>}
          sub={t("channels:persona.rowSub", { bot: botName(p.bot_id), used: usedText(p) })}><Chevron /></Row>))}</List>
        : <Empty title={t("channels:persona.emptyTitle")} sub={t("channels:persona.emptySub")} />}
      <div className="mt-3 type-footnote text-muted-foreground">{t("channels:persona.explain")}</div>
    </section>
  )
}

/** #/channels/personas and #/channels/personas/:id */
export function PersonasRoute({ id }: { id?: string }) {
  const t = useT()
  if (id) return <PersonaView key={id} id={id} />
  return <Page title={t("channels:persona.title")} sub={t("channels:persona.routeSub")} back={["#/channels", t("channels:list.title")]} max={700}><PersonasSection /></Page>
}

const RV: (keyof Reveal)[] = ["name", "role", "ai", "steve", "owner"]
type Form = { name: string; intro: string; soul: string; signature: string }
const formOf = (p: Persona): Form => ({ name: p.name, intro: p.intro, soul: p.soul, signature: p.signature })

function PersonaView({ id }: { id: string }) {
  const t = useT()
  const L = useLoad(() => channelsApi.persona(id), [id], ["persona"])
  const [form, setForm] = useState<Form | null>(null), [reveal, setReveal] = useState<Reveal | null>(null), [del, setDel] = useState(false), [busy, act] = useBusy()
  const p = L.s === "ok" ? L.data : null
  /* a change from elsewhere (the stream) replaces the form only when nothing is being edited */
  useEffect(() => { if (p) { setReveal(p.reveal); setForm((f) => (f && dirty(f, p) ? f : formOf(p))) } }, [p?.revision, p?.id])
  const preview = useMemo(() => (p && form && reveal ? introOf({ name: form.name, intro: form.intro, reveal }, ownerOf(p)) : ""), [p, form, reveal])
  if (L.s === "unwired") return <Unwired id="personas" />
  const back: [string, string] = ["#/channels/personas", t("channels:persona.title")]
  if (L.s === "loading") return <Page title={t("channels:persona.title")} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title={t("channels:persona.title")} back={back}><Empty title={L.status === 404 ? t("channels:persona.notFound") : L.err} sub={L.status === 404 ? t("channels:persona.notFoundSub") : undefined} /></Page>
  if (!p || !form || !reveal) return null
  const changed = dirty(form, p)
  const ok = form.name.trim().length >= 1 && form.name.trim().length <= 20 && form.intro.length <= 400 && form.soul.length <= 2000 && form.signature.length <= 60
  const save = async () => { const r = await act(() => channelsApi.patchPersona(p.id, { name: form.name.trim(), intro: form.intro.trim(), soul: form.soul.trim(), signature: form.signature.trim() }), t("common:state.saved")); if (r) { setForm(formOf(r)); void L.reload() } }
  const setRv = async (k: keyof Reveal, v: boolean) => { const was = reveal; setReveal({ ...reveal, [k]: v }); const r = await act(() => channelsApi.patchPersona(p.id, { reveal: { [k]: v } })); if (!r) setReveal(was); else void L.reload() }
  const setBot = async (b: string) => { if (await act(() => channelsApi.patchPersona(p.id, { bot_id: b }), t("channels:persona.botChanged", { bot: botName(b) }))) void L.reload() }
  const accN = p.used_by.accounts.length, ownSpaces = p.used_by.spaces.filter((s) => s.own).length
  return (
    <Page title={p.name} back={back} max={700} sub={t(p.is_default ? "channels:persona.pageSubDefault" : "channels:persona.pageSub", { bot: botName(p.bot_id) })}
      actions={!p.is_default && <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDel(true)} data-del-persona>{t("common:action.delete")}</Button>}
      footer={changed ? <div className="flex items-center justify-end gap-2 border-t bg-background/90 px-4 py-3 backdrop-blur sm:px-8" data-persona-savebar>
        <span className="mr-auto type-footnote text-muted-foreground">{t("channels:persona.unsaved")}</span>
        <Button size="sm" variant="ghost" onClick={() => setForm(formOf(p))}>{t("channels:word.revert")}</Button>
        <Button size="sm" disabled={busy || !ok} onClick={() => void save()} data-persona-save>{busy && <Spinner />}{t("common:action.save")}</Button></div> : undefined}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("channels:persona.delTitle", { name: p.name })}</AlertDialogTitle>
          <AlertDialogDescription data-del-impact>{delImpact(accN, ownSpaces)}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={busy} onClick={async () => { const r = await act(() => channelsApi.deletePersona(p.id)); if (r) go("#/channels/personas") }} data-del-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <div className="flex flex-col gap-8">
        <div className="-mt-2 flex items-center gap-4"><PersonaAvatar bot={p.bot_id} size={64} /><div className="min-w-0"><div className="type-body">{form.intro || t("channels:persona.noIntro")}</div><div className="mt-0.5 type-footnote text-muted-foreground">{t("channels:persona.platformNote")}</div><OwnerChip objectType="persona" objectId={p.id} className="mt-1.5" /></div></div>
        <form className="contents" onSubmit={(e) => { e.preventDefault(); if (changed && ok && !busy) void save() }}>
          <FieldGroup className="gap-5">
            <F label={t("channels:persona.name")} n={form.name.length} max={20}><Input className="rounded-lg" value={form.name} maxLength={20} onChange={(e) => setForm({ ...form, name: e.target.value })} aria-invalid={!form.name.trim()} data-persona-name /></F>
            <F label={t("channels:persona.bot")} hint={t("channels:persona.botHint")}><div><Select testId="persona-bot" value={p.bot_id} onChange={(v) => void setBot(v)} options={Object.keys(st.bots).map((k) => ({ v: k, l: <span className="inline-flex items-center gap-1.5"><BotAvatar k={k} size={16} />{botName(k)}</span> }))} /></div></F>
            <F label={t("channels:persona.intro")} hint={t("channels:persona.introHint")} n={form.intro.length} max={400}><Textarea className="min-h-16 rounded-lg" value={form.intro} maxLength={400} onChange={(e) => setForm({ ...form, intro: e.target.value })} data-persona-intro /></F>
            <F label={t("channels:persona.soul")} hint={t("channels:persona.soulHint")} n={form.soul.length} max={2000}><Textarea className="min-h-20 rounded-lg" value={form.soul} maxLength={2000} onChange={(e) => setForm({ ...form, soul: e.target.value })} data-soul /></F>
            <F label={t("channels:persona.signature")} hint={t("channels:persona.signatureHint")} n={form.signature.length} max={60}><Input className="rounded-lg" value={form.signature} maxLength={60} onChange={(e) => setForm({ ...form, signature: e.target.value })} placeholder={t("channels:persona.signaturePlaceholder")} data-persona-sign /></F>
          </FieldGroup>
          <button type="submit" hidden />
        </form>
        <section data-reveal><SectionLabel>{t("channels:persona.revealLabel")}</SectionLabel>
          <Group>{RV.map((k) => { const l = t(`channels:persona.rv.${k}`); return <Row key={k} title={l} sub={t(`channels:persona.rv.${k}Sub`)}><Switch aria-label={t("channels:persona.revealAria", { what: l })} checked={reveal[k]} disabled={busy} onCheckedChange={(v) => void setRv(k, v)} data-reveal-key={k} /></Row> })}</Group>
          <div className="mt-3 rounded-xl bg-muted/60 px-4 py-3" aria-live="polite"><div className="type-footnote text-muted-foreground">{t("channels:persona.askWho")}</div><div className="mt-1 type-body" data-intro-preview>{preview}</div></div>
          <div className="mt-3 type-footnote text-muted-foreground">{t("channels:persona.revealNote")}</div></section>
        <section><SectionLabel>{t("channels:persona.usedIn")}</SectionLabel>
          {accN + p.used_by.spaces.length ? <Group>
            {p.used_by.accounts.map((a) => <Row key={a.account_id} title={a.name} sub={t("channels:persona.usedAccount")} onClick={() => go("#/channels/acc/" + a.account_id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}
            {p.used_by.spaces.map((s) => <Row key={s.id} title={s.name} sub={t(s.own ? "channels:persona.usedOwn" : "channels:persona.usedFollow")} onClick={() => go("#/channels/space/" + s.id)} data-used-space={s.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}
          </Group> : <Empty title={t("channels:persona.noUseTitle")} sub={t("channels:persona.noUseSub")} />}</section>
      </div>
    </Page>
  )
}
/** a labelled field; the counter shows near the limit */
const F = ({ label, hint, children, n, max }: { label: string; hint?: string; children: React.ReactNode; n?: number; max?: number }) => (
    <Field className="gap-1.5"><FieldLabel className="type-label text-muted-foreground">{label}{max !== undefined && n !== undefined && n > max * 0.8 && <span className={n > max ? "ml-auto tabular-nums text-destructive" : "ml-auto tabular-nums"}>{n}/{max}</span>}</FieldLabel>{children}{hint && <FieldDescription className="type-footnote">{hint}</FieldDescription>}</Field>)
/** what deleting a persona changes: the accounts that default to it, the spaces that chose it */
function delImpact(accN: number, ownSpaces: number) {
  if (!(accN + ownSpaces)) return t("channels:persona.delNone")
  const a = accN ? t("channels:persona.delAccounts", { count: accN }) : "", b = ownSpaces ? t("channels:persona.delSpaces", { count: ownSpaces }) : ""
  return t("channels:persona.delImpact", { impact: a && b ? t("channels:persona.delBoth", { a, b }) : a || b })
}
const dirty = (f: Form, p: Persona) => f.name !== p.name || f.intro !== p.intro || f.soul !== p.soul || f.signature !== p.signature
