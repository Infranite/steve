/* 能力 → MCP 服务器: the workspace's MCP servers, each an affectUI Integration Settings card (name, transport, what it
 * runs or where it is, its env variables or headers, a secret by name only). In the library a card says which Bots
 * use it and lets you choose them; on a Bot's 能力 tab the same card carries that Bot's switch. Editing is a dialog:
 * stdio takes a command, arguments and env variables, http / sse an address and headers; any value can be marked
 * secret – its value goes to the Hub's vault and the page never gets it back (left empty when editing, it is kept). */
import { useState, type ReactNode } from "react"
import { GlobeIcon, LockIcon, PencilIcon, PlusIcon, PlugIcon, RadioTowerIcon, TerminalIcon, TrashIcon, XIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Input, TextField } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { IntegrationRow, IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Section, Empty, Loading, Blank, Segmented, Notice } from "@/ui"
import { toast } from "@/store"
import { capsApi, type McpServer, type McpServerIn, type McpTransport, type McpVar } from "@/api/caps"
import { AssignMenu, UsedBy, errText, useList } from "./common"

const ICON = { stdio: TerminalIcon, http: GlobeIcon, sse: RadioTowerIcon } as const

/** the library's servers */
export function McpLibrary() {
  const t = useT()
  const [d, load] = useList(() => capsApi.mcp.list(), [])
  const [editing, setEditing] = useState<McpServer | "new" | null>(null)
  const [removing, setRemoving] = useState<McpServer | null>(null)
  const remove = async (x: McpServer) => {
    try { await capsApi.mcp.remove(x.id); toast(t("caps:mcp.removed", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const add = <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEditing("new")} data-mcp-add><PlusIcon />{t("caps:mcp.add")}</Button>
  return (
    <Section label={t("caps:mcp.section")} note={t("caps:mcp.note")} right={d.s === "ok" && d.v.length > 0 ? add : undefined} data-mcp>
      {d.s === "loading" ? <Loading className="py-8" />
        : d.s === "unwired" ? <Blank>{t("caps:mcp.unwired")}</Blank>
        : d.s === "error" ? <Notice tone="destructive">{d.e}</Notice>
        : d.v.length === 0 ? <Empty compact className="bg-muted/50" icon={<PlugIcon />} title={t("caps:mcp.emptyTitle")} sub={t("caps:mcp.emptySub")} data-mcp-empty
            action={<Button size="sm" onClick={() => setEditing("new")} data-mcp-add><PlusIcon />{t("caps:mcp.add")}</Button>} />
        : <div className="flex flex-col gap-3" data-mcp-list>{d.v.map((x) => (
            <McpServerCard key={x.id} x={x}
              aside={<>
                <AssignMenu kind="mcp" id={x.id} name={x.name} users={x.bots ?? []} onChange={() => void load()} />
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:mcp.edit", { name: x.name })} onClick={() => setEditing(x)} data-mcp-edit={x.name}><PencilIcon /></Button>
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:mcp.remove", { name: x.name })} onClick={() => setRemoving(x)} data-mcp-remove={x.name}><TrashIcon /></Button>
              </>}
              footer={<span className="flex min-w-0 flex-wrap items-center gap-2"><span className="type-footnote text-muted-foreground">{t("caps:use.usedBy")}</span><UsedBy users={x.bots ?? []} /></span>} />))}</div>}
      {editing && <McpServerDialog x={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void load() }} />}
      <AlertDialog open={!!removing} onOpenChange={(o) => !o && setRemoving(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("caps:mcp.removeTitle", { name: removing?.name ?? "" })}</AlertDialogTitle><AlertDialogDescription>{t("caps:mcp.removeDesc", { count: removing?.bots?.length ?? 0 })}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const x = removing; setRemoving(null); if (x) void remove(x) }} data-mcp-remove-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </Section>
  )
}

/** one server: in the library (no switch) or on a Bot (`enabled` and `onToggle`: its switch for that Bot) */
export function McpServerCard({ x, aside, footer, enabled, onToggle, switchLabel }: { x: McpServer; aside?: ReactNode; footer?: ReactNode; enabled?: boolean; onToggle?: (on: boolean) => void; switchLabel?: string }) {
  const t = useT()
  const I = ICON[x.transport] ?? PlugIcon
  const vars = x.transport === "stdio" ? x.env : x.headers
  const where = x.transport === "stdio" ? [x.command, ...x.args].filter(Boolean).join(" ") : x.url ?? ""
  return (
    <IntegrationSettings name={x.name} logo={<I className="size-5" />} data-mcp-server={x.name} data-mcp-on={enabled}
      badge={<Badge variant="secondary" data-mcp-transport={x.transport}>{t(`caps:mcp.${x.transport}`)}</Badge>}
      meta={<span className="block truncate font-mono type-footnote" title={where}>{where}</span>}
      pausedMeta={<span className="type-footnote">{t("caps:bot.paused")}</span>}
      enabled={enabled} switchLabel={switchLabel} onToggle={onToggle}
      aside={aside && <span className="flex shrink-0 items-center">{aside}</span>} footer={footer}>
      <IntegrationSection>
        <IntegrationRow title={t(x.transport === "stdio" ? "caps:mcp.env" : "caps:mcp.headers")}
          hint={vars.length ? <span className="flex flex-wrap gap-1.5" data-mcp-vars>{vars.map((v) => (
            <span key={v.name} className="inline-flex items-center gap-1 rounded-md bg-muted/70 px-1.5 py-0.5 font-mono type-footnote" data-mcp-var={v.name} data-secret={v.secret || undefined}>
              {v.secret && <LockIcon className="size-3" aria-label={t("caps:mcp.secretMark")} />}<span>{v.name}{!v.secret && v.value ? <span className="text-muted-foreground">={v.value}</span> : null}</span>
            </span>))}</span> : t(x.transport === "stdio" ? "caps:mcp.noVars" : "caps:mcp.noHeaders")} />
      </IntegrationSection>
    </IntegrationSettings>
  )
}

/** one variable in the form: `kept` is a secret the vault already has (empty value keeps it) */
type FormVar = { key: number; name: string; value: string; secret: boolean; kept: boolean }
let nextKey = 1
const formVars = (vs: McpVar[]): FormVar[] => vs.map((v) => ({ key: nextKey++, name: v.name, value: v.secret ? "" : v.value ?? "", secret: !!v.secret, kept: !!v.secret }))
const outVars = (vs: FormVar[]): McpVar[] => vs.filter((v) => v.name.trim()).map((v) => (v.secret ? (v.value === "" && v.kept ? { name: v.name.trim(), secret: true } : { name: v.name.trim(), value: v.value, secret: true }) : { name: v.name.trim(), value: v.value }))

/** a new server (with `bot`: also given to that Bot) or an edit of one */
export function McpServerDialog({ x, bot, onClose, onSaved }: { x: McpServer | null; bot?: string; onClose: () => void; onSaved: (s: McpServer) => void }) {
  const t = useT()
  const [name, setName] = useState(x?.name ?? "")
  const [transport, setTransport] = useState<McpTransport>(x?.transport ?? "stdio")
  const [command, setCommand] = useState(x?.command ?? "")
  const [args, setArgs] = useState((x?.args ?? []).join("\n"))
  const [url, setUrl] = useState(x?.url ?? "")
  const [env, setEnv] = useState<FormVar[]>(formVars(x?.env ?? []))
  const [headers, setHeaders] = useState<FormVar[]>(formVars(x?.headers ?? []))
  const [busy, setBusy] = useState(false), [err, setErr] = useState<string | null>(null)
  const stdio = transport === "stdio"
  const vars = stdio ? env : headers, setVars = stdio ? setEnv : setHeaders
  const shared = (x?.bots?.length ?? 0) > 1
  const save = async () => {
    setBusy(true); setErr(null)
    const b: McpServerIn = stdio
      ? { name: name.trim(), transport, command: command.trim(), args: args.split("\n").map((a) => a.trim()).filter(Boolean), env: outVars(env) }
      : { name: name.trim(), transport, url: url.trim(), headers: outVars(headers) }
    try {
      const r = x ? await capsApi.mcp.update(x.id, b) : await capsApi.mcp.create(bot ? { ...b, bot } : b)
      toast(t(x ? "caps:mcp.saved" : "caps:mcp.added", { name: r.name })); onSaved(r)
    } catch (e) { setErr(errText(e)) } finally { setBusy(false) }
  }
  const setVar = (k: number, p: Partial<FormVar>) => setVars(vars.map((v) => (v.key === k ? { ...v, ...p } : v)))
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[560px]" data-mcp-dialog>
        <DialogHeader><DialogTitle>{x ? t("caps:mcp.dialogEdit", { name: x.name }) : t("caps:mcp.dialogNew")}</DialogTitle><DialogDescription>{bot && !x ? t("caps:mcp.dialogDescBot") : t("caps:mcp.dialogDesc")}</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-4">
          {shared && <Notice data-mcp-shared>{t("caps:mcp.shared", { count: x?.bots?.length ?? 0 })}</Notice>}
          <TextField label={t("caps:mcp.name")} hint={t("caps:mcp.nameHint")} value={name} onChange={(e) => setName(e.target.value)} maxLength={64} autoComplete="off" spellCheck={false} data-mcp-name />
          <div className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("caps:mcp.transport")}</span>
            <Segmented size="sm" value={transport} onChange={setTransport} label={t("caps:mcp.transport")} options={(["stdio", "http", "sse"] as const).map((v) => [v, t(`caps:mcp.${v}`)] as const)} /></div>
          {stdio ? <>
            <TextField label={t("caps:mcp.command")} hint={t("caps:mcp.commandHint")} value={command} onChange={(e) => setCommand(e.target.value)} inputClassName="font-mono" autoComplete="off" spellCheck={false} data-mcp-command />
            <label className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("caps:mcp.args")}</span>
              <Textarea rows={3} value={args} onChange={(e) => setArgs(e.target.value)} className="font-mono type-footnote" spellCheck={false} data-mcp-args />
              <span className="type-footnote text-muted-foreground">{t("caps:mcp.argsHint")}</span></label>
          </> : <TextField label={t("caps:mcp.url")} hint={t("caps:mcp.urlHint")} value={url} onChange={(e) => setUrl(e.target.value)} inputMode="url" inputClassName="font-mono" autoComplete="off" spellCheck={false} data-mcp-url />}
          <div className="flex flex-col gap-2" data-mcp-vars-edit>
            <span className="type-label text-muted-foreground">{t(stdio ? "caps:mcp.env" : "caps:mcp.headers")}</span>
            {vars.map((v, i) => (
              <div key={v.key} className="flex flex-wrap items-center gap-2 sm:flex-nowrap" data-mcp-var-row={i}>
                <Input aria-label={t("caps:mcp.varName")} placeholder={t("caps:mcp.varName")} value={v.name} onChange={(e) => setVar(v.key, { name: e.target.value })} className="font-mono sm:w-40 sm:flex-none" autoComplete="off" spellCheck={false} data-mcp-var-name />
                <Input aria-label={t("caps:mcp.varValue")} placeholder={v.secret && v.kept ? t("caps:mcp.secretKept") : t("caps:mcp.varValue")} type={v.secret ? "password" : "text"} value={v.value} onChange={(e) => setVar(v.key, { value: e.target.value })} className="min-w-0 flex-1 font-mono" autoComplete="new-password" spellCheck={false} data-mcp-var-value />
                <Switch size="sm" label={t("caps:mcp.secret")} checked={v.secret} onCheckedChange={(on) => setVar(v.key, { secret: on, kept: on && v.kept })} data-mcp-var-secret />
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:mcp.removeVar", { name: v.name || String(i + 1) })} onClick={() => setVars(vars.filter((y) => y.key !== v.key))}><XIcon /></Button>
              </div>))}
            <div><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setVars([...vars, { key: nextKey++, name: "", value: "", secret: false, kept: false }])} data-mcp-var-add><PlusIcon />{t("caps:mcp.addVar")}</Button></div>
          </div>
          {err && <Notice tone="destructive" data-mcp-error>{err}</Notice>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button disabled={busy || !name.trim()} onClick={() => void save()} data-mcp-save>{t("common:action.save")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
