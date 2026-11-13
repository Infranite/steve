/* 能力 → A2A: remote agents Steve's Bots can hand work to over the Agent2Agent protocol. One is added by its Agent Card
 * URL: the Hub reads the card (a host alone is looked up at /.well-known/agent-card.json) and the page shows what it
 * says of itself – name, description, skills – before it is kept. A key it wants goes to the vault. A Bot that uses
 * one gets one Steve tool, steve_a2a_<name>, to send it a task and get the result; every harness the same way.
 * Below the list: Steve's own card, for agents that want to hand work to Steve. */
import { useState, type ReactNode } from "react"
import { NetworkIcon, PencilIcon, PlusIcon, RefreshCwIcon, TrashIcon, LockIcon } from "lucide-react"
import { useT } from "@/i18n"
import { ago } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Input, TextField } from "@/components/ui/input"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { IntegrationRow, IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Section, Empty, Loading, Blank, Segmented, Notice, Panel, Row } from "@/ui"
import { toast } from "@/store"
import { capsApi, type A2aAgent, type A2aAgentIn, type A2aAuth, type AgentCard } from "@/api/caps"
import { AssignMenu, UsedBy, errText, useList } from "./common"
import { CopyLine } from "./steve"

export function A2aLibrary() {
  const t = useT()
  const [d, load] = useList(() => capsApi.a2a.list(), [])
  const [editing, setEditing] = useState<A2aAgent | "new" | null>(null)
  const [removing, setRemoving] = useState<A2aAgent | null>(null)
  const remove = async (x: A2aAgent) => {
    try { await capsApi.a2a.remove(x.id); toast(t("caps:a2a.removed", { name: x.title })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const refresh = async (x: A2aAgent) => {
    try { const r = await capsApi.a2a.update(x.id, { refresh: true }); toast(t("caps:a2a.refreshed", { name: r.title })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const add = <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEditing("new")} data-a2a-add><PlusIcon />{t("caps:a2a.add")}</Button>
  return (
    <Section label={t("caps:a2a.section")} note={t("caps:a2a.note")} right={d.s === "ok" && d.v.length > 0 ? add : undefined} data-a2a>
      {d.s === "loading" ? <Loading className="py-8" />
        : d.s === "unwired" ? <Blank>{t("caps:a2a.unwired")}</Blank>
        : d.s === "error" ? <Notice tone="destructive">{d.e}</Notice>
        : d.v.length === 0 ? <Empty compact className="bg-muted/50" icon={<NetworkIcon />} title={t("caps:a2a.emptyTitle")} sub={t("caps:a2a.emptySub")} data-a2a-empty
            action={<Button size="sm" onClick={() => setEditing("new")} data-a2a-add><PlusIcon />{t("caps:a2a.add")}</Button>} />
        : <div className="flex flex-col gap-3" data-a2a-list>{d.v.map((x) => (
            <AgentCardView key={x.id} x={x}
              aside={<>
                <AssignMenu kind="a2a" id={x.id} name={x.title} users={x.bots ?? []} onChange={() => void load()} />
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:a2a.refresh")} title={t("caps:a2a.refresh")} onClick={() => void refresh(x)} data-a2a-refresh={x.name}><RefreshCwIcon /></Button>
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:a2a.edit", { name: x.title })} onClick={() => setEditing(x)} data-a2a-edit={x.name}><PencilIcon /></Button>
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:a2a.remove", { name: x.title })} onClick={() => setRemoving(x)} data-a2a-remove={x.name}><TrashIcon /></Button>
              </>}
              footer={<span className="flex min-w-0 flex-wrap items-center gap-2"><span className="type-footnote text-muted-foreground">{t("caps:use.usedBy")}</span><UsedBy users={x.bots ?? []} /></span>} />))}</div>}
      {editing && <AgentDialog x={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void load() }} />}
      <AlertDialog open={!!removing} onOpenChange={(o) => !o && setRemoving(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("caps:a2a.removeTitle", { name: removing?.title ?? "" })}</AlertDialogTitle><AlertDialogDescription>{t("caps:a2a.removeDesc", { count: removing?.bots?.length ?? 0 })}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const x = removing; setRemoving(null); if (x) void remove(x) }} data-a2a-remove-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </Section>
  )
}

/** the skills a card lists, as quiet chips with their description on hover */
function CardSkills({ skills }: { skills: { id: string; name?: string; description?: string }[] }) {
  const t = useT()
  if (!skills.length) return <span className="type-footnote text-muted-foreground">{t("caps:a2a.noSkills")}</span>
  return <span className="flex flex-wrap gap-1.5" data-a2a-skills>{skills.map((s) => <span key={s.id} title={s.description} className="rounded-md bg-muted/70 px-1.5 py-0.5 type-footnote" data-a2a-skill={s.id}>{s.name || s.id}</span>)}</span>
}

export function AgentCardView({ x, aside, footer, enabled, onToggle, switchLabel }: { x: A2aAgent; aside?: ReactNode; footer?: ReactNode; enabled?: boolean; onToggle?: (on: boolean) => void; switchLabel?: string }) {
  const t = useT()
  const facts = [x.provider && t("caps:a2a.provider", { org: x.provider }), x.version && t("caps:a2a.version", { v: x.version }), t("caps:a2a.fetched", { time: ago(x.fetched) })].filter(Boolean).join(" · ")
  return (
    <IntegrationSettings name={x.title} logo={<NetworkIcon className="size-5" />} data-a2a-agent={x.name} data-a2a-on={enabled}
      badge={x.auth.scheme !== "none" ? <Badge variant="secondary" data-a2a-auth={x.auth.scheme}><LockIcon className="size-3" />{t(x.auth.scheme === "bearer" ? "caps:a2a.authBearer" : "caps:a2a.authHeader")}</Badge> : undefined}
      meta={<span className="line-clamp-2 whitespace-normal type-footnote" title={x.description}>{x.description || facts}</span>}
      pausedMeta={<span className="type-footnote">{t("caps:bot.paused")}</span>}
      enabled={enabled} switchLabel={switchLabel} onToggle={onToggle}
      aside={aside && <span className="flex shrink-0 items-center">{aside}</span>} footer={footer}>
      <IntegrationSection>
        <IntegrationRow title={t("caps:a2a.skills")} hint={<CardSkills skills={x.skills} />} />
        <IntegrationRow title={t("caps:a2a.toolLabel")} hint={<span className="font-mono type-footnote" data-a2a-tool>{x.tool}</span>} />
        <IntegrationRow title={t("caps:a2a.endpoint")} hint={<span className="block truncate font-mono type-footnote" title={x.url ?? ""}>{x.url}</span>} />
        {x.description && <IntegrationRow title={t("caps:a2a.about")} hint={<span className="type-footnote">{facts}</span>} />}
      </IntegrationSection>
    </IntegrationSettings>
  )
}

const AUTH = ["none", "bearer", "header"] as const

/** add one by its card (with `bot`: also given to that Bot), or edit one */
export function AgentDialog({ x, bot, onClose, onSaved }: { x: A2aAgent | null; bot?: string; onClose: () => void; onSaved: (a: A2aAgent) => void }) {
  const t = useT()
  const [url, setUrl] = useState(x?.card_url ?? "")
  const [card, setCard] = useState<AgentCard | null>(null)
  const [name, setName] = useState(x?.name ?? "")
  const [scheme, setScheme] = useState<A2aAuth["scheme"]>(x?.auth.scheme ?? "none")
  const [header, setHeader] = useState(x?.auth.scheme === "header" ? x.auth.header : "X-Api-Key")
  const [secret, setSecret] = useState("")
  const [busy, setBusy] = useState(false), [err, setErr] = useState<string | null>(null)
  const fetchCard = async () => {
    setBusy(true); setErr(null)
    try { const r = await capsApi.a2a.preview(url.trim()); setCard(r.card); setUrl(r.card_url) } catch (e) { setErr(errText(e)); setCard(null) } finally { setBusy(false) }
  }
  const auth: A2aAuth = scheme === "header" ? { scheme, header: header.trim() } : { scheme }
  const kept = !!x?.has_secret && x.auth.scheme === scheme && scheme !== "none"
  const save = async () => {
    setBusy(true); setErr(null)
    const b: A2aAgentIn = { auth, ...(secret ? { secret } : {}), ...(x && name.trim() ? { name: name.trim() } : {}) }
    try {
      const r = x ? await capsApi.a2a.update(x.id, { ...b, ...(url.trim() !== x.card_url ? { card_url: url.trim() } : {}) })
        : await capsApi.a2a.create({ ...b, card_url: url.trim(), ...(bot ? { bot } : {}) })
      toast(t(x ? "caps:a2a.saved" : "caps:a2a.added", { name: r.title })); onSaved(r)
    } catch (e) { setErr(errText(e)) } finally { setBusy(false) }
  }
  const ready = !!x || !!card
  const needSecret = scheme !== "none" && !kept && !secret
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[560px]" data-a2a-dialog>
        <DialogHeader><DialogTitle>{x ? t("caps:a2a.dialogEdit", { name: x.title }) : t("caps:a2a.dialogNew")}</DialogTitle><DialogDescription>{bot && !x ? t("caps:a2a.dialogDescBot") : t("caps:a2a.dialogDesc")}</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-4">
          <div className="flex items-end gap-2">
            <TextField className="min-w-0 flex-1" label={t("caps:a2a.cardUrl")} hint={t("caps:a2a.cardUrlHint")} value={url} onChange={(e) => { setUrl(e.target.value); if (!x) setCard(null) }} onKeyDown={(e) => { if (e.key === "Enter" && !x && url.trim()) void fetchCard() }}
              placeholder="https://agent.example.com" inputMode="url" inputClassName="font-mono" autoComplete="off" spellCheck={false} data-a2a-url />
            {!x && <Button variant="outline" className="mb-6" disabled={busy || !url.trim()} onClick={() => void fetchCard()} data-a2a-fetch>{t("caps:a2a.fetch")}</Button>}
          </div>
          {card && <Panel data-a2a-preview>
            <Row icon={<NetworkIcon className="size-4 text-muted-foreground" />} title={card.name} sub={card.description}>{card.version && <span className="type-footnote text-muted-foreground">{t("caps:a2a.version", { v: card.version })}</span>}</Row>
            <div className="px-4 pb-3"><CardSkills skills={card.skills ?? []} /></div>
          </Panel>}
          {ready && <>
            {x && <TextField label={t("caps:a2a.name")} hint={t("caps:a2a.nameHint")} value={name} onChange={(e) => setName(e.target.value)} maxLength={40} inputClassName="font-mono" autoComplete="off" spellCheck={false} data-a2a-name />}
            <div className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("caps:a2a.auth")}</span>
              <Segmented size="sm" value={scheme} onChange={setScheme} label={t("caps:a2a.auth")} options={AUTH.map((v) => [v, t(`caps:a2a.auth_${v}`)] as const)} /></div>
            {scheme === "header" && <TextField label={t("caps:a2a.header")} value={header} onChange={(e) => setHeader(e.target.value)} inputClassName="font-mono" autoComplete="off" spellCheck={false} data-a2a-header />}
            {scheme !== "none" && <label className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("caps:a2a.secret")}</span>
              <Input type="password" value={secret} onChange={(e) => setSecret(e.target.value)} placeholder={kept ? t("caps:a2a.secretKept") : ""} className="font-mono" autoComplete="new-password" spellCheck={false} data-a2a-secret /></label>}
          </>}
          {err && <Notice tone="destructive" data-a2a-error>{err}</Notice>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button disabled={busy || !ready || needSecret} onClick={() => void save()} data-a2a-save>{t("common:action.save")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** Steve's own card: where another agent finds it, and where it sends tasks */
export function SteveAgent() {
  const t = useT()
  const [d] = useList(() => capsApi.steve(), [])
  return (
    <Section label={t("caps:a2a.own")} note={t("caps:a2a.ownNote")} data-a2a-own>
      {d.s === "loading" ? <Loading className="py-8" /> : d.s !== "ok" ? <Blank>{t("caps:a2a.unwired")}</Blank> : (
        <Panel><div className="flex flex-col gap-4 p-3 sm:p-4">
          <CopyLine label={t("caps:a2a.ownCard")} value={d.v.agent_card} data-a2a-own-card />
          <CopyLine label={t("caps:a2a.ownEndpoint")} value={d.v.a2a} data-a2a-own-endpoint />
        </div></Panel>)}
    </Section>
  )
}
