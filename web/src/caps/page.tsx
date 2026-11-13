/* 能力 (#/capabilities[/mcp|skills|a2a]): the workspace's library of what Bots can use, defined once and chosen per
 * Bot on its own 能力 tab. MCP 服务器 starts with Steve itself ("Steve · 内置", every session has it) and how a
 * client outside Steve connects to it; then the servers you add. 技能 is the skills; A2A the remote agents, with
 * Steve's own Agent Card below them. */
import { useT } from "@/i18n"
import { Page, Segmented, Section, go } from "@/ui"
import { McpLibrary } from "./mcp"
import { SkillsLibrary } from "./skills"
import { A2aLibrary, SteveAgent } from "./a2a"
import { OutsideConnect, SteveServerCard } from "./steve"

const TABS = ["mcp", "skills", "a2a"] as const
type Tab = (typeof TABS)[number]

export function CapabilitiesView({ tab }: { tab?: string }) {
  const t = useT()
  const cur: Tab = (TABS as readonly string[]).includes(tab ?? "") ? (tab as Tab) : "mcp"
  return (
    <Page title={t("caps:page.title")} sub={t("caps:page.sub")} max={760}>
      <div className="flex flex-col gap-8" data-caps-page={cur}>
        <Segmented className="self-start" value={cur} onChange={(v) => go(v === "mcp" ? "#/capabilities" : `#/capabilities/${v}`)} label={t("caps:page.title")}
          options={TABS.map((v) => [v, t(`caps:page.tab_${v}`)] as const)} />
        <div key={cur} className="flex flex-col gap-8 anim-in">
          {cur === "mcp" && <>
            <Section label={t("caps:steve.section")} note={t("caps:steve.note")}><SteveServerCard /></Section>
            <OutsideConnect />
            <McpLibrary />
          </>}
          {cur === "skills" && <SkillsLibrary />}
          {cur === "a2a" && <><A2aLibrary /><SteveAgent /></>}
        </div>
      </div>
    </Page>
  )
}
