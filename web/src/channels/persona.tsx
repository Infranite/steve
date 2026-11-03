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
import { go, Page, Group, Row, SectionLabel, Empty, BotAvatar } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { st, botName } from "@/store"
import { useLoad, useBusy } from "@/accounts/load"
import { channelsApi, type Persona, type Reveal } from "@/api/channels"
import { PersonaAvatar } from "@/channels/basis"
import { OwnerChip } from "@/people/owner-chip"

const NOTHING = "（不透露身份，只回答问题本身）"
/** the answer to “你是谁？” — the Hub's `intro_of`, ported so the preview follows every keystroke */
export function introOf(p: { name: string; intro: string; reveal: Reveal }, owner: string) {
  const r = p.reveal, bits: string[] = []
  if (r.name) bits.push(`我是${p.name}`)
  if (r.role) bits.push(p.intro.replace(/^我是[^，,。]*[，,]?/, "").replace(/。$/, ""))
  if (r.ai) bits.push("我是 AI 助手")
  if (r.steve) bits.push("由 Steve 驱动")
  if (r.owner) bits.push(`归 ${owner} 管理`)
  const xs = bits.filter(Boolean)
  return bits.length ? xs.join("，") + "。" : NOTHING
}
/** how the Hub names the owner (`<主人称呼>`), read back from its own preview */
const ownerOf = (p: Persona) => /归 (.+?) 管理/.exec(p.intro_preview)?.[1] ?? "你"

const usedText = (p: Persona) => {
  const a = p.used_by.accounts.length, s = p.used_by.spaces.length
  return a + s ? `${a ? a + " 个账号默认用它" : ""}${a && s ? " · " : ""}${s ? "出现在 " + s + " 个群和私聊" : ""}` : "还没有账号或群在用"
}

/** the list, all of them or one Bot's (the Bot page's 对外身份 tab) */
export function PersonasSection({ bot }: { bot?: string }) {
  const L = useLoad(() => channelsApi.personas(), [], ["persona", "space", "channel"])
  const [busy, act] = useBusy()
  if (L.s === "unwired") return bot ? <Empty title="对外身份还没有接入" sub="接入能收发消息的账号后，这个 Bot 可以用一个或多个对外身份在群里出面。现在它只在 Steve 内部干活。" /> : <Unwired id="personas" />
  if (L.s === "loading") return <div className="flex justify-center py-10"><Spinner className="size-5 text-muted-foreground" /></div>
  if (L.s === "error") return <Empty title={L.err} />
  const xs = bot ? L.data.filter((p) => p.bot_id === bot) : L.data
  const create = async () => {
    const p = await act(() => channelsApi.newPersona({ name: "新身份", bot_id: bot ?? "steve", intro: "我是新身份。", reveal: { name: true, role: true, ai: true, steve: false, owner: false } }))
    if (p) go("#/channels/personas/" + p.id)
  }
  return (
    <section data-personas>
      <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void create()} data-new-persona><PlusIcon />新建</Button>}>对外身份 · {xs.length}</SectionLabel>
      {xs.length ? <Group>{xs.map((p) => (
        <Row key={p.id} icon={<PersonaAvatar bot={p.bot_id} size={32} />} onClick={() => go("#/channels/personas/" + p.id)} data-persona-row={p.name}
          title={<span className="inline-flex items-center gap-1.5">{p.name}{p.is_default && <span className="type-footnote text-muted-foreground">默认</span>}</span>}
          sub={`${botName(p.bot_id)} 在背后 · ${usedText(p)}`}><ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
        : <Empty title="这个 Bot 还没有对外身份" sub="新建一个，它就能在渠道的群里以这个名字出面。" />}
      <div className="mt-2 px-1 type-footnote text-muted-foreground">对外身份是 Bot 在渠道里的“脸”：名字、自我介绍、口吻和能透露什么。一个 Bot 可以有多个身份；身份背后的 Bot 可以随时换。</div>
    </section>
  )
}

/** #/channels/personas and #/channels/personas/:id */
export function PersonasRoute({ id }: { id?: string }) {
  if (id) return <PersonaView key={id} id={id} />
  return <Page title="对外身份" sub="Bot 在各个渠道里以什么身份出现" back={["#/channels", "渠道"]} max={700}><PersonasSection /></Page>
}

const RV: [keyof Reveal, string, string][] = [["name", "名字", "我是谁"], ["role", "职责", "负责什么"], ["ai", "是 AI 助手", "如实说明自己是 AI"], ["steve", "由 Steve 驱动", "背后的系统"], ["owner", "归谁管理", "你的名字"]]
type Form = { name: string; intro: string; soul: string; signature: string }
const formOf = (p: Persona): Form => ({ name: p.name, intro: p.intro, soul: p.soul, signature: p.signature })

function PersonaView({ id }: { id: string }) {
  const L = useLoad(() => channelsApi.persona(id), [id], ["persona"])
  const [form, setForm] = useState<Form | null>(null), [reveal, setReveal] = useState<Reveal | null>(null), [del, setDel] = useState(false), [busy, act] = useBusy()
  const p = L.s === "ok" ? L.data : null
  /* a change from elsewhere (the stream) replaces the form only when nothing is being edited */
  useEffect(() => { if (p) { setReveal(p.reveal); setForm((f) => (f && dirty(f, p) ? f : formOf(p))) } }, [p?.revision, p?.id])
  const preview = useMemo(() => (p && form && reveal ? introOf({ name: form.name, intro: form.intro, reveal }, ownerOf(p)) : ""), [p, form, reveal])
  if (L.s === "unwired") return <Unwired id="personas" />
  if (L.s === "loading") return <Page title="对外身份" back={["#/channels/personas", "对外身份"]}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="对外身份" back={["#/channels/personas", "对外身份"]}><Empty title={L.status === 404 ? "这个身份不存在" : L.err} sub={L.status === 404 ? "可能已经删除。" : undefined} /></Page>
  if (!p || !form || !reveal) return null
  const changed = dirty(form, p)
  const ok = form.name.trim().length >= 1 && form.name.trim().length <= 20 && form.intro.length <= 400 && form.soul.length <= 2000 && form.signature.length <= 60
  const save = async () => { const r = await act(() => channelsApi.patchPersona(p.id, { name: form.name.trim(), intro: form.intro.trim(), soul: form.soul.trim(), signature: form.signature.trim() }), "已保存"); if (r) { setForm(formOf(r)); void L.reload() } }
  const setRv = async (k: keyof Reveal, v: boolean) => { const was = reveal; setReveal({ ...reveal, [k]: v }); const r = await act(() => channelsApi.patchPersona(p.id, { reveal: { [k]: v } })); if (!r) setReveal(was); else void L.reload() }
  const setBot = async (b: string) => { if (await act(() => channelsApi.patchPersona(p.id, { bot_id: b }), `已换成 ${botName(b)}`)) void L.reload() }
  const accN = p.used_by.accounts.length, ownSpaces = p.used_by.spaces.filter((s) => s.own).length
  return (
    <Page title={p.name} back={["#/channels/personas", "对外身份"]} max={700} sub={`对外身份 · ${botName(p.bot_id)} 在背后${p.is_default ? " · 默认身份" : ""}`}
      actions={!p.is_default && <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDel(true)} data-del-persona>删除</Button>}
      footer={changed ? <div className="flex items-center justify-end gap-2 border-t bg-background/90 px-4 py-3 backdrop-blur sm:px-8" data-persona-savebar>
        <span className="mr-auto type-footnote text-muted-foreground">有改动还没保存</span>
        <Button size="sm" variant="ghost" onClick={() => setForm(formOf(p))}>还原</Button>
        <Button size="sm" disabled={busy || !ok} onClick={() => void save()} data-persona-save>{busy && <Spinner />}保存</Button></div> : undefined}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>删除“{p.name}”？</AlertDialogTitle>
          <AlertDialogDescription data-del-impact>{accN + ownSpaces ? `${accN ? `${accN} 个账号默认用它，删除后改用默认身份 Steve` : ""}${accN && ownSpaces ? "；" : ""}${ownSpaces ? `${ownSpaces} 个群单独用了它，删除后回到跟随账号` : ""}。已经发出去的消息不受影响。` : "没有账号或群在用它。已经发出去的消息不受影响。"}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={busy} onClick={async () => { const r = await act(() => channelsApi.deletePersona(p.id)); if (r) go("#/channels/personas") }} data-del-confirm>删除</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <div className="flex flex-col gap-8">
        <div className="-mt-2 flex items-center gap-4"><PersonaAvatar bot={p.bot_id} size={64} /><div className="min-w-0"><div className="type-body">{form.intro || "还没有自我介绍"}</div><div className="mt-0.5 type-footnote text-muted-foreground">平台上的名字和头像以接入的应用为准</div><OwnerChip objectType="persona" objectId={p.id} className="mt-1.5" /></div></div>
        <form className="contents" onSubmit={(e) => { e.preventDefault(); if (changed && ok && !busy) void save() }}>
          <FieldGroup className="gap-5">
            <F label="名字" n={form.name.length} max={20}><Input className="rounded-lg" value={form.name} maxLength={20} onChange={(e) => setForm({ ...form, name: e.target.value })} aria-invalid={!form.name.trim()} data-persona-name /></F>
            <F label="背后的 Bot" hint="能力、模型和记忆来自这个 Bot；换掉它，群里的人感觉不到"><div><Select testId="persona-bot" value={p.bot_id} onChange={(v) => void setBot(v)} options={Object.keys(st.bots).map((k) => ({ v: k, l: <span className="inline-flex items-center gap-1.5"><BotAvatar k={k} size={16} />{botName(k)}</span> }))} /></div></F>
            <F label="自我介绍" hint="被问“你是谁”时的说法" n={form.intro.length} max={400}><Textarea className="min-h-16 rounded-lg" value={form.intro} maxLength={400} onChange={(e) => setForm({ ...form, intro: e.target.value })} data-persona-intro /></F>
            <F label="口吻（soul）" hint="性格、语气、称呼方式；每个群的具体职责写在群里" n={form.soul.length} max={2000}><Textarea className="min-h-20 rounded-lg" value={form.soul} maxLength={2000} onChange={(e) => setForm({ ...form, soul: e.target.value })} data-soul /></F>
            <F label="签名" hint="可选，附在每条回复末尾" n={form.signature.length} max={60}><Input className="rounded-lg" value={form.signature} maxLength={60} onChange={(e) => setForm({ ...form, signature: e.target.value })} placeholder="例如：— 小运（AI 值班）" data-persona-sign /></F>
          </FieldGroup>
          <button type="submit" hidden />
        </form>
        <section data-reveal><SectionLabel>被问到身份时透露什么</SectionLabel>
          <Group>{RV.map(([k, l, d]) => <Row key={k} title={l} sub={d}><Switch aria-label={"透露" + l} checked={reveal[k]} disabled={busy} onCheckedChange={(v) => void setRv(k, v)} data-reveal-key={k} /></Row>)}</Group>
          <div className="mt-3 rounded-xl bg-muted/60 px-4 py-3" aria-live="polite"><div className="type-footnote text-muted-foreground">有人问“你是谁？”</div><div className="mt-1 type-body" data-intro-preview>{preview}</div></div>
          <div className="mt-2 px-1 type-footnote text-muted-foreground">默认透露名字、职责和“是 AI 助手”。关掉“是 AI 助手”后也不会主动声称自己是真人。</div></section>
        <section><SectionLabel>出现在</SectionLabel>
          {accN + p.used_by.spaces.length ? <Group>
            {p.used_by.accounts.map((a) => <Row key={a.account_id} title={a.name} sub="接入账号 · 默认身份" onClick={() => go("#/channels/acc/" + a.account_id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}
            {p.used_by.spaces.map((s) => <Row key={s.id} title={s.name} sub={s.own ? "本群单独设置" : "跟随账号"} onClick={() => go("#/channels/space/" + s.id)} data-used-space={s.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}
          </Group> : <Empty title="还没有群在用这个身份" sub="在接入账号或群页里选它。" />}</section>
      </div>
    </Page>
  )
}
/** a labelled field; the counter shows near the limit */
const F = ({ label, hint, children, n, max }: { label: string; hint?: string; children: React.ReactNode; n?: number; max?: number }) => (
    <Field className="gap-1.5"><FieldLabel className="type-label text-muted-foreground">{label}{max !== undefined && n !== undefined && n > max * 0.8 && <span className={n > max ? "ml-auto text-destructive" : "ml-auto"}>{n}/{max}</span>}</FieldLabel>{children}{hint && <FieldDescription className="type-footnote">{hint}</FieldDescription>}</Field>)
const dirty = (f: Form, p: Persona) => f.name !== p.name || f.intro !== p.intro || f.soul !== p.soul || f.signature !== p.signature
