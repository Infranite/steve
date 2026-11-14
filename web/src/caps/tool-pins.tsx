/* Pins change what a Bot loads, never what it is allowed to call. Choices come from the Hub's
 * catalog, assigned providers and observed usage; no invented remote tool names or hand-entered IDs. */
import { useState } from "react"
import { PinIcon, PinOffIcon, SearchIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { SkillGrid, type Skill } from "@/components/affect/agent-skills"
import { Section, Loading, Blank, Notice, Disclosure } from "@/ui"
import { capsApi, type BotCaps, type ToolPin } from "@/api/caps"
import { errText, useList } from "./common"
import { toolDescKey } from "./steve"

type Choice = ToolPin & { name: string; description: string; category: string; enabled?: boolean }
const key = (p: ToolPin) => `${p.kind}:${p.target}`
const MAX_PINS = 32

export function BotToolPins({ bot, caps }: { bot: string; caps: BotCaps | null }) {
  const t = useT()
  const [data, load] = useList(() => Promise.all([capsApi.tools(bot), capsApi.steve()]), [bot])
  const [query, setQuery] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  if (data.s === "loading") return <Loading className="py-4" />
  if (data.s === "unwired") return <Blank>{t("caps:pins.unwired")}</Blank>
  if (data.s === "error") return <Notice tone="destructive">{data.e}<Button variant="ghost" size="xs" onClick={() => void load()}>{t("caps:pins.retry")}</Button></Notice>
  const [tools, server] = data.v
  const core = new Set(tools.core), pinned = new Set(tools.pins.map(key))
  const usage = new Map(tools.usage.map((u) => [u.tool, u]))
  const choices = new Map<string, Choice>()
  for (const x of server.tools) {
    const desc = toolDescKey(x.name)
    choices.set(`tool:${x.name}`, { kind: "tool", target: x.name, name: x.name, description: desc ? t(`caps:tool.${desc}`) : x.description, category: t("caps:pins.builtin") })
  }
  for (const name of tools.core) if (!choices.has(`tool:${name}`)) choices.set(`tool:${name}`, { kind: "tool", target: name, name, description: t("caps:pins.coreTool"), category: t("caps:pins.builtin") })
  for (const x of caps?.mcp ?? []) choices.set(`mcp:${x.id}`, { kind: "mcp", target: x.id, name: x.name, description: t("caps:pins.wholeServer"), category: "MCP", enabled: x.enabled })
  for (const x of caps?.a2a ?? []) choices.set(`a2a:${x.id}`, { kind: "a2a", target: x.id, name: x.title, description: x.description, category: "A2A", enabled: x.enabled })
  for (const u of tools.usage) if (!choices.has(`tool:${u.tool}`)) choices.set(`tool:${u.tool}`, { kind: "tool", target: u.tool, name: u.tool, description: t("caps:pins.observed"), category: t("caps:pins.observedSource") })
  for (const pin of tools.pins) if (!choices.has(key(pin))) choices.set(key(pin), { ...pin, name: pin.target, description: t("caps:pins.retained"), category: t("caps:pins.pinned") })
  const flip = async (p: ToolPin, on: boolean) => {
    setBusy(true); setError(null)
    try { await capsApi.pin(bot, p, on); await load() } catch (e) { setError(errText(e)) }
    finally { setBusy(false) }
  }
  const needle = query.trim().toLocaleLowerCase()
  const rows: Skill[] = [...choices.values()]
    .sort((a, b) => Number(core.has(b.target)) - Number(core.has(a.target)) || Number(pinned.has(key(b))) - Number(pinned.has(key(a))) || a.name.localeCompare(b.name))
    .filter((x) => !needle || `${x.name} ${x.description} ${x.category}`.toLocaleLowerCase().includes(needle))
    .map((x) => {
      const always = x.kind === "tool" && core.has(x.target), on = pinned.has(key(x)), u = usage.get(x.target)
      return { id: key(x), name: x.name, description: x.description, category: x.category, icon: <PinIcon />, source: always ? t("caps:pins.always") : x.enabled === false ? t("caps:pins.disabled") : on ? t("caps:pins.pinned") : x.category,
        uses: u ? t("caps:pins.usage", { days: tools.days, searched: u.searched, called: u.called }) : undefined,
        action: always ? undefined
          : <Button size="xs" variant={on ? "secondary" : "ghost"} disabled={busy || (!on && (tools.pins.length >= MAX_PINS || x.enabled === false))} onClick={() => void flip(x, !on)} aria-label={t(on ? "caps:pins.unpinName" : "caps:pins.pinName", { name: x.name })} data-tool-pin={key(x)} aria-pressed={on}>{on ? <PinOffIcon /> : <PinIcon />}{t(on ? "caps:pins.unpin" : "caps:pins.pin")}</Button> }
    })
  return <Section label={t("caps:pins.title")} note={t("caps:pins.note")} data-tool-pins>
    <div className="flex min-w-0 flex-col gap-3">
      <p className="type-footnote text-muted-foreground" data-tool-pin-count>{t("caps:pins.count", { core: tools.core.length, count: tools.pins.length, max: MAX_PINS })}</p>
      {tools.pins.length >= MAX_PINS && <Notice>{t("caps:pins.limit")}</Notice>}
      {error && <Notice tone="destructive" data-tool-pin-error>{error}</Notice>}
      <Disclosure label={t("caps:pins.browse")} data-tool-pin-browse>
        <div className="flex min-w-0 flex-col gap-3 pt-3">
          <Input value={query} onChange={(e) => setQuery(e.target.value)} prefix={<SearchIcon className="size-4" />} placeholder={t("caps:pins.search")} aria-label={t("caps:pins.search")} data-tool-pin-search />
          <SkillGrid skills={rows} labels={{ all: t("caps:pins.all"), categories: t("caps:pins.categories"), empty: t("caps:pins.empty") }} />
          <p className="type-footnote text-muted-foreground">{t("caps:pins.discovery")}</p>
        </div>
      </Disclosure>
    </div>
  </Section>
}
