/* A Bot's 能力 tab: what this Bot uses. Steve · 内置 first (always on, its tools folded inside with how often this
 * Bot called each in the last 30 days); then the MCP servers, skills and remote agents it is given from the library,
 * each with this Bot's own switch. 添加 picks one from the library or makes a new one, which goes into the library
 * and to this Bot at once. Editing one edits the library's entry (every Bot that uses it changes); taking it off
 * this Bot leaves it in the library. */
import { useState, type ReactNode } from "react"
import { MinusCircleIcon, NetworkIcon, PencilIcon, PlugIcon, PlusIcon, SparklesIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Section, Empty, Loading, Blank, Notice, Panel, Row } from "@/ui"
import { harnessSkills, runtimeName, st, toast } from "@/store"
import { stats } from "@/api/stats"
import { useStats } from "@/stats/format"
import { capsApi, type A2aAgent, type BotCaps as Caps, type CapKind, type McpServer, type Skill } from "@/api/caps"
import { errText, useList } from "./common"
import { McpServerCard, McpServerDialog } from "./mcp"
import { ImportDialog, SkillCard, SkillDialog, type Editing } from "./skills"
import { AgentCardView, AgentDialog } from "./a2a"
import { SteveServerCard } from "./steve"

type Entry = { id: string; name: string; enabled?: boolean }

export function BotCaps({ k, permName, permSub, onPerm }: { k: string; permName: string; permSub: string; onPerm: () => void }) {
  const t = useT()
  const b = st.bots[k]
  const [mine, load, setMine] = useList(() => capsApi.bot(k), [k])
  const [use] = useStats(() => stats.counts({ range: "30d", bot: k }, "tool"), [k])
  const counts = use.s === "ok" ? new Map(use.v.rows.filter((x) => x.dim === "mcp").map((x) => [x.label, x.count])) : undefined
  const [mcpEdit, setMcpEdit] = useState<McpServer | "new" | null>(null)
  const [skillEdit, setSkillEdit] = useState<Editing | null>(null)
  const [skillImport, setSkillImport] = useState(false)
  const [a2aEdit, setA2aEdit] = useState<A2aAgent | "new" | null>(null)
  if (!b) return null
  const toggle = async (kind: CapKind, x: Entry, on: boolean) => {
    if (mine.s === "ok") {
      const flip = <T extends Entry>(xs: T[]) => xs.map((y) => (y.id === x.id ? { ...y, enabled: on } : y))
      setMine({ mcp: flip(mine.v.mcp), skills: flip(mine.v.skills), a2a: flip(mine.v.a2a) })
    }
    try { await capsApi.assign(k, kind, x.id, on); toast(t(on ? "caps:bot.on" : "caps:bot.off", { name: x.name })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const drop = async (kind: CapKind, x: Entry) => {
    try { await capsApi.unassign(k, kind, x.id); toast(t("caps:bot.dropped", { name: x.name, bot: b.n })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const take = async (kind: CapKind, x: Entry) => {
    try { await capsApi.assign(k, kind, x.id, true); toast(t("caps:bot.taken", { name: x.name, bot: b.n })) } catch (e) { toast(errText(e)) }
    void load()
  }
  const aside = (kind: CapKind, x: Entry, onEdit: () => void) => <>
    <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:bot.edit", { name: x.name })} onClick={onEdit} data-cap-edit={x.name}><PencilIcon /></Button>
    <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label={t("caps:bot.drop", { name: x.name })} title={t("caps:bot.drop", { name: x.name })} onClick={() => void drop(kind, x)} data-cap-drop={x.name}><MinusCircleIcon /></Button>
  </>
  const v: Caps | null = mine.s === "ok" ? mine.v : null
  const reads = harnessSkills(b.harness)
  const body = (kind: CapKind, xs: Entry[] | undefined, empty: ReactNode, list: ReactNode) => mine.s === "loading" ? <Loading className="py-8" />
    : mine.s === "unwired" ? <Blank>{t("caps:bot.unwired")}</Blank>
    : mine.s === "error" ? <Notice tone="destructive">{mine.e}</Notice>
    : !xs?.length ? empty : <div className="flex flex-col gap-3" data-cap-list={kind}>{list}</div>
  return (
    <div className="flex flex-col gap-8 anim-in" data-caps>
      <Section label={t("caps:steve.section")} note={t("caps:bot.steveNote")}><SteveServerCard counts={counts} /></Section>

      <Section label={t("caps:mcp.section")} note={t("caps:bot.mcpNote")} data-mcp
        right={v && v.mcp.length > 0 ? <AddMenu kind="mcp" mine={v.mcp} onTake={(x) => void take("mcp", x)} onNew={() => setMcpEdit("new")} /> : undefined}>
        {body("mcp", v?.mcp, <Empty compact className="bg-muted/50" icon={<PlugIcon />} title={t("caps:mcp.emptyTitleBot")} sub={t("caps:mcp.emptySub")} data-mcp-empty
            action={<AddMenu kind="mcp" mine={[]} big onTake={(x) => void take("mcp", x)} onNew={() => setMcpEdit("new")} />} />,
          v?.mcp.map((x) => <McpServerCard key={x.id} x={x} enabled={x.enabled} switchLabel={t("caps:bot.enable", { name: x.name })} onToggle={(on) => void toggle("mcp", x, on)} aside={aside("mcp", x, () => setMcpEdit(x))} />))}
      </Section>

      <Section label={t("caps:skills.section")} note={t("caps:bot.skillsNote")} data-skills
        right={v && v.skills.length > 0 ? <AddMenu kind="skill" mine={v.skills} onTake={(x) => void take("skill", x)} onNew={() => setSkillEdit({ draft: { name: "", description: "", content: "" } })} onImport={() => setSkillImport(true)} /> : undefined}>
        {reads === false && v && <Notice className="mb-3" data-skills-unread>{t("caps:skills.notRead", { harness: runtimeName(b.harness) ?? b.harness })}</Notice>}
        {body("skill", v?.skills, <Empty compact className="bg-muted/50" icon={<SparklesIcon />} title={t("caps:skills.emptyTitleBot")} sub={t("caps:skills.emptySub")} data-skills-empty
            action={<AddMenu kind="skill" mine={[]} big onTake={(x) => void take("skill", x)} onNew={() => setSkillEdit({ draft: { name: "", description: "", content: "" } })} onImport={() => setSkillImport(true)} />} />,
          v?.skills.map((x) => <SkillCard key={x.id} x={x} enabled={x.enabled} switchLabel={t("caps:bot.enable", { name: x.name })} onToggle={(on) => void toggle("skill", x, on)} aside={aside("skill", x, () => setSkillEdit({ x }))} />))}
      </Section>

      <Section label={t("caps:a2a.section")} note={t("caps:bot.a2aNote")} data-a2a
        right={v && v.a2a.length > 0 ? <AddMenu kind="a2a" mine={v.a2a} onTake={(x) => void take("a2a", x)} onNew={() => setA2aEdit("new")} /> : undefined}>
        {body("a2a", v?.a2a.map((x) => ({ ...x, name: x.title })), <Empty compact className="bg-muted/50" icon={<NetworkIcon />} title={t("caps:a2a.emptyTitleBot")} sub={t("caps:a2a.emptySub")} data-a2a-empty
            action={<AddMenu kind="a2a" mine={[]} big onTake={(x) => void take("a2a", x)} onNew={() => setA2aEdit("new")} />} />,
          v?.a2a.map((x) => <AgentCardView key={x.id} x={x} enabled={x.enabled} switchLabel={t("caps:bot.enable", { name: x.title })} onToggle={(on) => void toggle("a2a", { ...x, name: x.title }, on)} aside={aside("a2a", { ...x, name: x.title }, () => setA2aEdit(x))} />))}
      </Section>

      <Section label={t("caps:bot.perm")} note={t("caps:bot.permNote")}>
        <Panel><Row title={permName} sub={permSub} data-caps-perm={b.permMode}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={onPerm}>{t("caps:bot.change")}</Button></Row></Panel>
      </Section>
      <p className="type-footnote text-muted-foreground" data-caps-library-link>{t("caps:bot.libraryLead")} <a className="text-primary hover:underline" href="#/capabilities">{t("caps:bot.library")}</a></p>

      {mcpEdit && <McpServerDialog x={mcpEdit === "new" ? null : mcpEdit} bot={k} onClose={() => setMcpEdit(null)} onSaved={() => { setMcpEdit(null); void load() }} />}
      {skillEdit && <SkillDialog e={skillEdit} bot={k} onClose={() => setSkillEdit(null)} onSaved={() => { setSkillEdit(null); void load() }} />}
      {skillImport && <ImportDialog onClose={() => setSkillImport(false)} onRead={(draft) => { setSkillImport(false); setSkillEdit({ draft }) }} />}
      {a2aEdit && <AgentDialog x={a2aEdit === "new" ? null : a2aEdit} bot={k} onClose={() => setA2aEdit(null)} onSaved={() => { setA2aEdit(null); void load() }} />}
    </div>
  )
}

/** 添加: the library's entries this Bot does not use yet, then making a new one (and, for skills, pasting a SKILL.md) */
function AddMenu({ kind, mine, big, onTake, onNew, onImport }: { kind: CapKind; mine: Entry[]; big?: boolean; onTake: (x: Entry) => void; onNew: () => void; onImport?: () => void }) {
  const t = useT()
  const [lib, setLib] = useState<Entry[] | null>(null)
  const open = (o: boolean) => {
    if (!o) return
    const f: Promise<Entry[]> = kind === "mcp" ? capsApi.mcp.list() : kind === "skill" ? capsApi.skills.list() : capsApi.a2a.list().then((xs) => xs.map((x) => ({ ...x, name: x.title })))
    f.then((xs) => setLib(xs.filter((x) => !mine.some((m) => m.id === x.id))), () => setLib([]))
  }
  const desc = (x: Entry) => (kind === "mcp" ? (x as McpServer).transport : kind === "skill" ? (x as Skill).description : (x as A2aAgent).description)
  return (
    <DropdownMenu onOpenChange={open}>
      <DropdownMenuTrigger render={<Button size={big ? "sm" : "xs"} variant={big ? "default" : "ghost"} className={big ? undefined : "text-muted-foreground"} data-cap-add={kind} />}><PlusIcon />{t("caps:bot.add")}</DropdownMenuTrigger>
      <DropdownMenuContent align={big ? "center" : "end"} className="max-h-96 w-72 overflow-y-auto">
        <DropdownMenuGroup>
        <DropdownMenuLabel>{t("caps:bot.fromLibrary")}</DropdownMenuLabel>
        {lib === null ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">{t("caps:bot.loading")}</div>
          : !lib.length ? <div className="px-2 py-1.5 type-footnote text-muted-foreground" data-cap-library-empty>{t("caps:bot.libraryEmpty")}</div>
          : lib.map((x) => (
            <DropdownMenuItem key={x.id} onClick={() => onTake(x)} data-cap-take={x.name}>
              <span className="flex min-w-0 flex-col"><span className="truncate">{x.name}</span><span className="truncate type-footnote text-muted-foreground">{desc(x)}</span></span>
            </DropdownMenuItem>))}
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={onNew} data-cap-new={kind}><PlusIcon />{t(`caps:bot.new_${kind}`)}</DropdownMenuItem>
        {onImport && <DropdownMenuItem onClick={onImport} data-cap-import={kind}>{t("caps:skills.import")}</DropdownMenuItem>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
