/* 能力 → MCP 服务器：一个 Bot 自己接的 MCP 服务器（GET/POST/PATCH/DELETE /bots/:id/mcp-servers）。每个服务器是一张
 * affectUI 的 Integration Settings 卡片：名字、连接方式、开关（马上保存），环境变量或请求头的名字（机密只显示名字），编辑和删除。
 * 编辑在对话框里：stdio 填命令、参数和环境变量，http / sse 填地址和请求头，每一项可以标成机密——机密的值交给 Hub 存进保险箱，
 * 页面再也拿不到它；编辑时留空就保留原来的值。下面是还没有做的「技能」和「A2A」，只标“规划中”，没有假功能。 */
import { useEffect, useState } from "react"
import { GlobeIcon, LockIcon, PencilIcon, PlusIcon, PlugIcon, RadioTowerIcon, TerminalIcon, TrashIcon, XIcon, SparklesIcon, NetworkIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Input, TextField } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { IntegrationRow, IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Section, Panel, Row, Pill, Empty, Loading, Blank, Segmented, Tile, Notice } from "@/ui"
import { isNotWired } from "@/api"
import { toast } from "@/store"
import { botMcpApi, type McpServer, type McpServerIn, type McpTransport, type McpVar } from "@/api/bot-mcp"

const ICON = { stdio: TerminalIcon, http: GlobeIcon, sse: RadioTowerIcon } as const
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

type Load = { s: "loading" } | { s: "unwired" } | { s: "error"; e: string } | { s: "ok"; v: McpServer[] }

export function BotMcpSection({ bot }: { bot: string }) {
  const t = useT()
  const [d, setD] = useState<Load>({ s: "loading" })
  const [editing, setEditing] = useState<McpServer | "new" | null>(null)
  const [removing, setRemoving] = useState<McpServer | null>(null)
  const load = () => botMcpApi.list(bot).then((v) => setD({ s: "ok", v }), (e) => setD(isNotWired(e) ? { s: "unwired" } : { s: "error", e: errText(e) }))
  useEffect(() => { setD({ s: "loading" }); void load() }, [bot])
  const toggle = async (x: McpServer, on: boolean) => {
    if (d.s === "ok") setD({ s: "ok", v: d.v.map((y) => (y.id === x.id ? { ...y, enabled: on } : y)) })
    try { await botMcpApi.update(bot, x.id, { enabled: on }); toast(t(on ? "manage:mcp.on" : "manage:mcp.off", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const remove = async (x: McpServer) => {
    try { await botMcpApi.remove(bot, x.id); toast(t("manage:mcp.removed", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const add = <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEditing("new")} data-mcp-add><PlusIcon />{t("manage:mcp.add")}</Button>
  return (
    <Section label={t("manage:mcp.section")} note={t("manage:mcp.note")} right={d.s === "ok" && d.v.length > 0 ? add : undefined} data-mcp>
      {d.s === "loading" ? <Loading className="py-8" />
        : d.s === "unwired" ? <Blank>{t("manage:mcp.unwired")}</Blank>
        : d.s === "error" ? <Notice tone="destructive">{d.e}</Notice>
        : d.v.length === 0 ? <Empty compact className="bg-muted/50" icon={<PlugIcon />} title={t("manage:mcp.emptyTitle")} sub={t("manage:mcp.emptySub")} data-mcp-empty
            action={<Button size="sm" onClick={() => setEditing("new")} data-mcp-add><PlusIcon />{t("manage:mcp.add")}</Button>} />
        : <div className="flex flex-col gap-3" data-mcp-list>{d.v.map((x) => <ServerCard key={x.id} x={x} onToggle={(on) => void toggle(x, on)} onEdit={() => setEditing(x)} onRemove={() => setRemoving(x)} />)}</div>}
      {editing && <ServerDialog bot={bot} x={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void load() }} />}
      <AlertDialog open={!!removing} onOpenChange={(o) => !o && setRemoving(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("manage:mcp.removeTitle", { name: removing?.name ?? "" })}</AlertDialogTitle><AlertDialogDescription>{t("manage:mcp.removeDesc")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const x = removing; setRemoving(null); if (x) void remove(x) }} data-mcp-remove-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
    </Section>
  )
}

function ServerCard({ x, onToggle, onEdit, onRemove }: { x: McpServer; onToggle: (on: boolean) => void; onEdit: () => void; onRemove: () => void }) {
  const t = useT()
  const I = ICON[x.transport] ?? PlugIcon
  const vars = x.transport === "stdio" ? x.env : x.headers
  const where = x.transport === "stdio" ? [x.command, ...x.args].filter(Boolean).join(" ") : x.url ?? ""
  return (
    <IntegrationSettings name={x.name} logo={<I className="size-5" />} data-mcp-server={x.name} data-mcp-on={x.enabled}
      badge={<Badge variant="secondary" data-mcp-transport={x.transport}>{t(`manage:mcp.${x.transport}`)}</Badge>}
      meta={<span className="block truncate font-mono type-footnote" title={where}>{where}</span>}
      pausedMeta={<span className="type-footnote">{t("manage:mcp.paused")}</span>}
      enabled={x.enabled} switchLabel={t("manage:mcp.enable", { name: x.name })} onToggle={onToggle}
      aside={<span className="flex shrink-0 items-center">
        <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("manage:mcp.edit", { name: x.name })} onClick={onEdit} data-mcp-edit={x.name}><PencilIcon /></Button>
        <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("manage:mcp.remove", { name: x.name })} onClick={onRemove} data-mcp-remove={x.name}><TrashIcon /></Button>
      </span>}>
      <IntegrationSection>
        <IntegrationRow title={t(x.transport === "stdio" ? "manage:mcp.env" : "manage:mcp.headers")}
          hint={vars.length ? <span className="flex flex-wrap gap-1.5" data-mcp-vars>{vars.map((v) => (
            <span key={v.name} className="inline-flex items-center gap-1 rounded-md bg-muted/70 px-1.5 py-0.5 font-mono type-footnote" data-mcp-var={v.name} data-secret={v.secret || undefined}>
              {v.secret && <LockIcon className="size-3" aria-label={t("manage:mcp.secretMark")} />}<span>{v.name}{!v.secret && v.value ? <span className="text-muted-foreground">={v.value}</span> : null}</span>
            </span>))}</span> : t(x.transport === "stdio" ? "manage:mcp.noVars" : "manage:mcp.noHeaders")} />
      </IntegrationSection>
    </IntegrationSettings>
  )
}

/** one variable in the form: `kept` is a secret the vault already has (empty value keeps it) */
type FormVar = { key: number; name: string; value: string; secret: boolean; kept: boolean }
let nextKey = 1
const formVars = (vs: McpVar[]): FormVar[] => vs.map((v) => ({ key: nextKey++, name: v.name, value: v.secret ? "" : v.value ?? "", secret: !!v.secret, kept: !!v.secret }))
const outVars = (vs: FormVar[]): McpVar[] => vs.filter((v) => v.name.trim()).map((v) => (v.secret ? (v.value === "" && v.kept ? { name: v.name.trim(), secret: true } : { name: v.name.trim(), value: v.value, secret: true }) : { name: v.name.trim(), value: v.value }))

function ServerDialog({ bot, x, onClose, onSaved }: { bot: string; x: McpServer | null; onClose: () => void; onSaved: () => void }) {
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
  const save = async () => {
    setBusy(true); setErr(null)
    const b: McpServerIn = stdio
      ? { name: name.trim(), transport, command: command.trim(), args: args.split("\n").map((a) => a.trim()).filter(Boolean), env: outVars(env) }
      : { name: name.trim(), transport, url: url.trim(), headers: outVars(headers) }
    try {
      const r = x ? await botMcpApi.update(bot, x.id, b) : await botMcpApi.create(bot, b)
      toast(t(x ? "manage:mcp.saved" : "manage:mcp.added", { name: r.name })); onSaved()
    } catch (e) { setErr(errText(e)) } finally { setBusy(false) }
  }
  const setVar = (k: number, p: Partial<FormVar>) => setVars(vars.map((v) => (v.key === k ? { ...v, ...p } : v)))
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-[560px]" data-mcp-dialog>
        <DialogHeader><DialogTitle>{x ? t("manage:mcp.dialogEdit", { name: x.name }) : t("manage:mcp.dialogNew")}</DialogTitle><DialogDescription>{t("manage:mcp.dialogDesc")}</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-4">
          <TextField label={t("manage:mcp.name")} hint={t("manage:mcp.nameHint")} value={name} onChange={(e) => setName(e.target.value)} maxLength={64} autoComplete="off" spellCheck={false} data-mcp-name />
          <div className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("manage:mcp.transport")}</span>
            <Segmented size="sm" value={transport} onChange={setTransport} label={t("manage:mcp.transport")} options={(["stdio", "http", "sse"] as const).map((v) => [v, t(`manage:mcp.${v}`)] as const)} /></div>
          {stdio ? <>
            <TextField label={t("manage:mcp.command")} hint={t("manage:mcp.commandHint")} value={command} onChange={(e) => setCommand(e.target.value)} inputClassName="font-mono" autoComplete="off" spellCheck={false} data-mcp-command />
            <label className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("manage:mcp.args")}</span>
              <Textarea rows={3} value={args} onChange={(e) => setArgs(e.target.value)} className="font-mono type-footnote" spellCheck={false} data-mcp-args />
              <span className="type-footnote text-muted-foreground">{t("manage:mcp.argsHint")}</span></label>
          </> : <TextField label={t("manage:mcp.url")} hint={t("manage:mcp.urlHint")} value={url} onChange={(e) => setUrl(e.target.value)} inputMode="url" inputClassName="font-mono" autoComplete="off" spellCheck={false} data-mcp-url />}
          <div className="flex flex-col gap-2" data-mcp-vars-edit>
            <span className="type-label text-muted-foreground">{t(stdio ? "manage:mcp.env" : "manage:mcp.headers")}</span>
            {vars.map((v, i) => (
              <div key={v.key} className="flex flex-wrap items-center gap-2 sm:flex-nowrap" data-mcp-var-row={i}>
                <Input aria-label={t("manage:mcp.varName")} placeholder={t("manage:mcp.varName")} value={v.name} onChange={(e) => setVar(v.key, { name: e.target.value })} className="font-mono sm:w-40 sm:flex-none" autoComplete="off" spellCheck={false} data-mcp-var-name />
                <Input aria-label={t("manage:mcp.varValue")} placeholder={v.secret && v.kept ? t("manage:mcp.secretKept") : t("manage:mcp.varValue")} type={v.secret ? "password" : "text"} value={v.value} onChange={(e) => setVar(v.key, { value: e.target.value })} className="min-w-0 flex-1 font-mono" autoComplete="new-password" spellCheck={false} data-mcp-var-value />
                <Switch size="sm" label={t("manage:mcp.secret")} checked={v.secret} onCheckedChange={(on) => setVar(v.key, { secret: on, kept: on && v.kept })} data-mcp-var-secret />
                <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("manage:mcp.removeVar", { name: v.name || String(i + 1) })} onClick={() => setVars(vars.filter((y) => y.key !== v.key))}><XIcon /></Button>
              </div>))}
            <div><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setVars([...vars, { key: nextKey++, name: "", value: "", secret: false, kept: false }])} data-mcp-var-add><PlusIcon />{t("manage:mcp.addVar")}</Button></div>
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

/** what a Bot will be able to have besides tools: named here, marked planned, nothing to press */
export function PlannedCaps() {
  const t = useT()
  return (
    <Panel data-caps-planned>
      <Row icon={<Tile I={SparklesIcon} />} title={t("manage:mcp.skills")} sub={t("manage:mcp.skillsSub")} data-planned="skills"><Pill>{t("manage:mcp.planned")}</Pill></Row>
      <Row icon={<Tile I={NetworkIcon} />} title={t("manage:mcp.a2a")} sub={t("manage:mcp.a2aSub")} data-planned="a2a"><Pill>{t("manage:mcp.planned")}</Pill></Row>
    </Panel>
  )
}
