/* Steve itself as an MCP server. "Steve · 内置" is the server every Bot's session has, always on: its tools fold
 * inside the card, each with one line on what it is for (and, on a Bot's tab, how often that Bot called it). Below it
 * in 能力 → MCP: how a client outside Steve – Claude Desktop, Cursor, another agent – connects with one of your access
 * tokens: the address (as the Hub is configured to be reached), a token made here and shown once, a copyable
 * configuration for the client, and the tools such a client gets (the ones that need no Steve session). */
import { useState } from "react"
import { CheckIcon, CopyIcon, KeyRoundIcon, WrenchIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { TextField } from "@/components/ui/input"
import { CodeBlock } from "@/components/ui/code-block"
import { IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Robot } from "@/robot"
import { Section, Loading, Segmented, Notice, Pill, Panel, Disclosure, FieldRow } from "@/ui"
import { st, toast, useStore } from "@/store"
import { capsApi, type SteveServer, type SteveTool } from "@/api/caps"
import { useHubQuery } from "@/data/use-hub-query"
import { errText } from "./common"
import { CLIENTS, FILE, LANGUAGE, WRAP, mcpUrl, snippet, type ClientKind } from "./snippets"

import { toolDescKey, toolCount } from "./tool-catalog"
import { steveToolIcon } from "@/tool-icons"
export { toolDescKey } from "./tool-catalog"

/** the tools in a fold: name, what it is for, and (on a Bot) how often it was called in 30 days */
function ToolList({ tools, counts }: { tools: SteveTool[]; counts?: Map<string, number> }) {
  const t = useT()
  return (
    <ul className="flex flex-col divide-y divide-border/60" data-steve-tools>
      {tools.map((x) => {
        const k = toolDescKey(x.name), n = counts && toolCount(x, counts), Icon = steveToolIcon(x.name) ?? WrenchIcon
        return (
          <li key={x.name} className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-0.5 py-2 sm:flex-nowrap" data-steve-tool={x.name}>
            <span className="flex min-w-0 shrink-0 items-center gap-2 font-mono type-footnote max-sm:basis-full sm:w-52" title={x.name}><Icon className="size-3.5 shrink-0 text-muted-foreground" strokeWidth={1.5} aria-hidden /><span className="truncate">{x.name}</span></span>
            <span className="min-w-0 flex-1 type-footnote text-muted-foreground">{k ? t(`caps:tool.${k}`) : x.description}</span>
            {counts && <span className="shrink-0 type-footnote tabular-nums text-tertiary">{n ? t("caps:steve.uses", { count: n }) : t("caps:steve.unused")}</span>}
          </li>
        )
      })}
    </ul>
  )
}

/** "Steve · 内置": always on, its tools folded inside */
export function SteveServerCard({ counts, open }: { counts?: Map<string, number>; open?: boolean }) {
  const t = useT()
  const d = useHubQuery(["caps", "steve"], () => capsApi.steve(), { events: ["bot"], debounceMs: 300 })
  const tools = d.data?.tools ?? []
  return (
    <IntegrationSettings name={t("caps:steve.name")} logo={<Robot k="steve" size={28} />} data-steve-server
      badge={<Pill tone="success" data-steve-always>{t("caps:steve.always")}</Pill>}
      meta={<span className="type-footnote">{d.data ? t("caps:steve.meta", { count: tools.length }) : t("caps:steve.metaLoading")}</span>}>
      <div className="px-4 pb-3">
        {d.loading ? <Loading className="py-4" /> : d.error ? <Notice tone="destructive" role="alert">{d.error.message}</Notice>
          : <Disclosure defaultOpen={open} label={t("caps:steve.show", { count: tools.length })} data-steve-fold><div className="pt-1"><ToolList tools={tools} counts={counts} /></div></Disclosure>}
      </div>
    </IntegrationSettings>
  )
}

/** a value to copy, on one line */
export function CopyLine({ label, value, ...rest }: { label: string; value: string } & Record<`data-${string}`, unknown>) {
  const t = useT()
  const [done, setDone] = useState(false)
  const copy = () => { navigator.clipboard?.writeText(value).then(() => { setDone(true); setTimeout(() => setDone(false), 1400) }, () => toast(t("caps:outside.copyFailed"))) }
  return (
    <div className="flex min-w-0 flex-col gap-1" {...rest}>
      <span className="type-label text-muted-foreground">{label}</span>
      <div className="flex min-w-0 items-center gap-1 rounded-lg bg-muted/60 py-1 pr-1 pl-3">
        <span className="min-w-0 flex-1 truncate font-mono type-footnote" title={value} data-copy-value>{value}</span>
        <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:outside.copy", { what: label })} onClick={copy}>{done ? <CheckIcon /> : <CopyIcon />}</Button>
      </div>
    </div>
  )
}

/** Steve from outside: where to connect, a token, and the configuration for the client */
export function OutsideConnect() {
  const t = useT()
  useStore()
  const d = useHubQuery(["caps", "steve"], () => capsApi.steve(), { events: ["bot"], debounceMs: 300 })
  const [at, setAt] = useState(0)
  const [client, setClient] = useState<ClientKind>(() => CLIENTS[0] ?? "generic")
  const [name, setName] = useState(() => t("caps:outside.tokenNameDefault"))
  const [token, setToken] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const make = async () => {
    setBusy(true)
    try { const r = await capsApi.token(name.trim() || t("caps:outside.tokenNameDefault")); setToken(r.token); toast(t("caps:outside.made", { name: r.item.name })) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  if (d.loading) return <Section label={t("caps:outside.section")}><Loading className="py-8" /></Section>
  if (d.error || !d.data) return <Section label={t("caps:outside.section")}><Notice tone="destructive" role="alert">{d.error?.message ?? ""}</Notice></Section>
  const v: SteveServer = d.data
  const ep = v.endpoints[Math.min(at, v.endpoints.length - 1)]
  const url = mcpUrl(ep.url, st.workspace?.id ?? null, st.workspaces.length > 1)
  const code = snippet(client, url, token, t("caps:outside.tokenPlaceholder"))
  return (
    <Section label={t("caps:outside.section")} note={t("caps:outside.note")} data-outside>
      <Panel><div className="flex flex-col gap-6 p-3 sm:p-4">
        <div className="flex flex-col gap-2">
          <CopyLine label={t("caps:outside.endpoint")} value={url} data-outside-url />
          {v.endpoints.length > 1 && <Segmented size="sm" value={String(at)} onChange={(x) => setAt(Number(x))} label={t("caps:outside.endpoint")}
            options={v.endpoints.map((e, i) => [String(i), t(`caps:outside.kind_${e.kind}`)] as const)} />}
          {ep.kind === "local" && <span className="type-footnote text-muted-foreground" data-outside-local>{t("caps:outside.localOnly")}</span>}
        </div>
        <div className="flex flex-col gap-2" data-outside-token>
          <span className="type-label text-muted-foreground">{t("caps:outside.token")}</span>
          {token ? <Notice tone="success" icon={false} data-outside-token-made><span className="flex items-start gap-2"><KeyRoundIcon className="mt-0.5 size-4 shrink-0" />{t("caps:outside.tokenOnce")}</span></Notice>
            : <FieldRow className="px-0 py-0" onSubmit={(e) => { e.preventDefault(); void make() }}>
              <TextField className="min-w-0 flex-1" aria-label={t("caps:outside.tokenName")} value={name} onChange={(e) => setName(e.target.value)} maxLength={60} autoComplete="off" data-outside-token-name />
              <Button type="submit" variant="outline" disabled={busy} data-outside-token-create><KeyRoundIcon />{t("caps:outside.create")}</Button>
            </FieldRow>}
          {!token && <span className="type-footnote text-muted-foreground">{t("caps:outside.noToken")} <a className="text-primary hover:underline" href="#/devices/clients" data-outside-manage>{t("caps:outside.manage")}</a></span>}
        </div>
        <div className="flex min-w-0 flex-col gap-2">
          <div className="flex flex-wrap items-center justify-between gap-2"><span className="type-label text-muted-foreground">{t("caps:outside.client")}</span>
            <div className="-mx-1 max-w-full overflow-x-auto px-1 [scrollbar-width:none]"><Segmented size="sm" className="w-max" value={client} onChange={setClient} label={t("caps:outside.client")} options={CLIENTS.map((c) => [c, t(`caps:outside.c_${c}`, { defaultValue: c })] as const)} /></div></div>
          <CodeBlock code={code} language={LANGUAGE[client]} filename={FILE[client]} wrap={WRAP[client]} onCopy={() => toast(t("caps:outside.copied"))} data-outside-snippet={client} />
          <span className="type-footnote text-muted-foreground" data-outside-where>{t(`caps:outside.where_${client}`, { defaultValue: "" })}</span>
        </div>
        <Disclosure label={t("caps:outside.scope", { count: v.outside.length })} data-outside-scope>
          <div className="pt-1"><span className="mb-1 block type-footnote text-muted-foreground">{t("caps:outside.scopeNote")}</span><ToolList tools={v.outside} /></div>
        </Disclosure>
      </div></Panel>
    </Section>
  )
}
