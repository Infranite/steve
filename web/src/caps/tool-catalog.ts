import type { SteveTool, ToolPin, BotTools } from "@/api/caps"

/* what each Steve tool does for the user, in a line; a tool added later shows the Hub's first sentence until it is
 * named here */
const DESC = {
  steve_update: "update", steve_ask: "ask", steve_request_approval: "approval", steve_deliver: "deliver", steve_deliver_file: "deliverFile", steve_publish: "publish", steve_lane_status: "laneStatus",
  steve_items_search: "itemsSearch", steve_item_read: "itemRead", steve_item_dependencies: "itemDependencies", steve_sessions_list: "sessionsList",
  steve_item_create: "itemCreate", steve_item_supplement: "itemSupplement",
  steve_workflow: "workflow", steve_debug: "debug", steve_upgrade: "upgrade", steve_devices: "devices", steve_reply_source: "replySource",
  steve_event_get: "eventGet", steve_im_read: "imRead", steve_knowledge_search: "knowledgeSearch", steve_knowledge_read: "knowledgeRead",
  steve_remember: "remember", steve_memory_list: "memoryList", steve_memory_tidy: "memoryTidy", steve_contacts_find: "contactsFind", steve_contacts_remember: "contactsRemember", steve_whois: "whois",
  steve_person_holdings: "personHoldings", steve_steward_propose: "stewardPropose", steve_secret_list: "secretList", steve_secret_use: "secretUse",
  steve_github_status: "githubStatus", steve_github_ci_log: "githubCiLog", steve_github_comment: "githubComment", steve_github_open_pr: "githubOpenPr",
  steve_mail_thread: "mailThread", steve_mail_attachment: "mailAttachment", steve_mail_draft: "mailDraft",
  steve_contacts: "contacts", steve_github_read: "githubRead", steve_mail_read: "mailRead", steve_matters: "matters",
  steve_tools_search: "toolsSearch", steve_tool_call: "toolCall",
  steve_digest_batch: "digestBatch", steve_digest_read: "digestRead", steve_digest_chunk: "digestChunk", steve_digest_finish: "digestFinish", steve_digest_control: "digestControl", steve_digest_report: "digestReport", steve_matters_search: "mattersSearch", steve_matter_read: "matterRead",
  steve_todo_list: "todoList", steve_todo_add: "todoAdd", steve_todo_update: "todoUpdate", steve_todo_complete: "todoComplete",
  steve_onboard_profile: "onboardProfile", steve_onboard_models: "onboardModels", steve_onboard_bots: "onboardBots", steve_onboard_done: "onboardDone",
} as const
type DescKey = (typeof DESC)[keyof typeof DESC]
export const toolDescKey = (name: string): DescKey | undefined => (DESC as Record<string, DescKey | undefined>)[name]


/** Alias metadata comes from the Hub, so remote tools with similar names stay independent. */
export function canonicalTool(name: string, catalog: SteveTool[]): string {
  return catalog.find((t) => t.aliases?.includes(name))?.name ?? name
}
export function catalogPin(pin: ToolPin, catalog: SteveTool[]): ToolPin {
  return pin.kind === "tool" ? { ...pin, target: canonicalTool(pin.target, catalog) } : pin
}
export function catalogUsage(rows: BotTools["usage"], catalog: SteveTool[]): BotTools["usage"] {
  const sums = new Map<string, BotTools["usage"][number]>()
  for (const row of rows) {
    const tool = canonicalTool(row.tool, catalog), was = sums.get(tool)
    sums.set(tool, { tool, searched: (was?.searched ?? 0) + row.searched, called: (was?.called ?? 0) + row.called })
  }
  return [...sums.values()]
}
export function toolCount(tool: SteveTool, counts: Map<string, number>): number {
  return [...new Set([tool.name, ...(tool.aliases ?? [])])].reduce((sum, name) => sum + (counts.get(name) ?? 0), 0)
}
