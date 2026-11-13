/* Steve itself as an MCP server. "Steve · 内置" is the server every Bot's session has, always on: its tools fold
 * inside the card, each with one line on what it is for (and, on a Bot's tab, how often that Bot called it). Below it
 * in 能力 → MCP: how a client outside Steve – Claude Desktop, Cursor, another agent – connects with one of your access
 * tokens: the address (as the Hub is configured to be reached), a token made here and shown once, a copyable
 * configuration for the client, and the tools such a client gets (the ones that need no Steve session). */
import { useState } from "react"
import { CheckIcon, CopyIcon, KeyRoundIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { TextField } from "@/components/ui/input"
import { CodeBlock } from "@/components/ui/code-block"
import { IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Robot } from "@/robot"
import { Section, Loading, Blank, Segmented, Notice, Pill, Panel, Disclosure, FieldRow } from "@/ui"
import { st, toast, useStore } from "@/store"
import { capsApi, type SteveServer, type SteveTool } from "@/api/caps"
import { errText, useList } from "./common"
import { CLIENTS, FILE, LANGUAGE, mcpUrl, snippet, type ClientKind } from "./snippets"

/* what each Steve tool does for the user, in a line; a tool added later shows the Hub's first sentence until it is
 * named here */
const DESC = {
  steve_update: "update", steve_ask: "ask", steve_request_approval: "approval", steve_deliver: "deliver", steve_publish: "publish", steve_lane_status: "laneStatus",
  steve_items_search: "itemsSearch", steve_item_read: "itemRead", steve_item_dependencies: "itemDependencies", steve_sessions_list: "sessionsList",
  steve_item_create: "itemCreate", steve_item_supplement: "itemSupplement",
  steve_workflow: "workflow", steve_debug: "debug", steve_upgrade: "upgrade", steve_devices: "devices", steve_reply_source: "replySource",
  steve_event_get: "eventGet", steve_im_read: "imRead", steve_knowledge_search: "knowledgeSearch", steve_knowledge_read: "knowledgeRead",
  steve_remember: "remember", steve_memory_list: "memoryList", steve_memory_tidy: "memoryTidy", steve_contacts_find: "contactsFind", steve_contacts_remember: "contactsRemember", steve_whois: "whois",
  steve_person_holdings: "personHoldings", steve_steward_propose: "stewardPropose", steve_secret_list: "secretList", steve_secret_use: "secretUse",
  steve_github_status: "githubStatus", steve_github_ci_log: "githubCiLog", steve_github_comment: "githubComment", steve_github_open_pr: "githubOpenPr",
  steve_mail_thread: "mailThread", steve_mail_attachment: "mailAttachment", steve_mail_draft: "mailDraft",
  steve_onboard_profile: "onboardProfile", steve_onboard_models: "onboardModels", steve_onboard_bots: "onboardBots", steve_onboard_done: "onboardDone",
} as const
type DescKey = (typeof DESC)[keyof typeof DESC]
export const toolDescKey = (name: string): DescKey | undefined => (DESC as Record<string, DescKey | undefined>)[name]

/** the tools in a fold: name, what it is for, and (on a Bot) how often it was called in 30 days */
function ToolList({ tools, counts }: { tools: SteveTool[]; counts?: Map<string, number> }) {
  const t = useT()
  return (
    <ul className="flex flex-col divide-y divide-border/60" data-steve-tools>
      {tools.map((x) => {
        const k = toolDescKey(x.name), n = counts?.get(x.name)
        return (
          <li key={x.name} className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-0.5 py-2 sm:flex-nowrap" data-steve-tool={x.name}>
            <span className="min-w-0 shrink-0 truncate font-mono type-footnote max-sm:basis-full sm:w-52" title={x.name}>{x.name}</span>
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
  const [d] = useList(() => capsApi.steve(), [])
  const tools = d.s === "ok" ? d.v.tools : []
  return (
    <IntegrationSettings name={t("caps:steve.name")} logo={<Robot k="steve" size={28} />} data-steve-server
      badge={<Pill tone="success" data-steve-always>{t("caps:steve.always")}</Pill>}
      meta={<span className="type-footnote">{d.s === "ok" ? t("caps:steve.meta", { count: tools.length }) : t("caps:steve.metaLoading")}</span>}>
      <div className="px-4 pb-3">
        {d.s === "loading" ? <Loading className="py-4" /> : d.s !== "ok" ? <Blank>{t("caps:steve.unwired")}</Blank>
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
  const [d] = useList(() => capsApi.steve(), [])
  const [at, setAt] = useState(0)
  const [client, setClient] = useState<ClientKind>("generic")
  const [name, setName] = useState(() => t("caps:outside.tokenNameDefault"))
  const [token, setToken] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const make = async () => {
    setBusy(true)
    try { const r = await capsApi.token(name.trim() || t("caps:outside.tokenNameDefault")); setToken(r.token); toast(t("caps:outside.made", { name: r.item.name })) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  if (d.s === "loading") return <Section label={t("caps:outside.section")}><Loading className="py-8" /></Section>
  if (d.s !== "ok") return <Section label={t("caps:outside.section")}><Blank>{t("caps:steve.unwired")}</Blank></Section>
  const v: SteveServer = d.v
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
          {!token && <span className="type-footnote text-muted-foreground">{t("caps:outside.noToken")} <a className="text-primary hover:underline" href="#/settings/access" data-outside-manage>{t("caps:outside.manage")}</a></span>}
        </div>
        <div className="flex min-w-0 flex-col gap-2">
          <div className="flex flex-wrap items-center justify-between gap-2"><span className="type-label text-muted-foreground">{t("caps:outside.client")}</span>
            <div className="-mx-1 max-w-full overflow-x-auto px-1 [scrollbar-width:none]"><Segmented size="sm" className="w-max" value={client} onChange={setClient} label={t("caps:outside.client")} options={CLIENTS.map((c) => [c, t(`caps:outside.c_${c}`)] as const)} /></div></div>
          <CodeBlock code={code} language={LANGUAGE[client]} filename={FILE[client]} wrap={client === "claudeCode"} onCopy={() => toast(t("caps:outside.copied"))} data-outside-snippet={client} />
          <span className="type-footnote text-muted-foreground" data-outside-where>{t(`caps:outside.where_${client}`)}</span>
        </div>
        <Disclosure label={t("caps:outside.scope", { count: v.outside.length })} data-outside-scope>
          <div className="pt-1"><span className="mb-1 block type-footnote text-muted-foreground">{t("caps:outside.scopeNote")}</span><ToolList tools={v.outside} /></div>
        </Disclosure>
      </div></Panel>
    </Section>
  )
}
