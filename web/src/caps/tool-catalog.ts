import type { SteveTool } from "@/api/caps"

/* what each Steve tool does for the user, in a line; a tool added later shows the Hub's first sentence until it is
 * named here */
const DESC = {
  steve_update: "update", steve_ask: "ask", steve_request_approval: "approval", steve_deliver: "deliver", steve_deliver_file: "deliverFile", steve_publish: "publish", steve_lane_status: "laneStatus",
  steve_items_search: "itemsSearch", steve_item_read: "itemRead", steve_item_dependencies: "itemDependencies", steve_sessions_list: "sessionsList",
  steve_item_create: "itemCreate", steve_item_supplement: "itemSupplement",
  steve_workflow: "workflow", steve_debug: "debug", steve_upgrade: "upgrade", steve_devices: "devices", steve_reply_source: "replySource",
  steve_event_get: "eventGet", steve_im_read: "imRead", steve_knowledge_search: "knowledgeSearch", steve_knowledge_read: "knowledgeRead",
  steve_remember: "remember", steve_memory_list: "memoryList", steve_memory_tidy: "memoryTidy", steve_contacts_remember: "contactsRemember",
  steve_steward_propose: "stewardPropose", steve_secret_list: "secretList", steve_secret_use: "secretUse",
  steve_github_comment: "githubComment", steve_github_open_pr: "githubOpenPr",
  steve_mail_draft: "mailDraft",
  steve_contacts: "contacts", steve_github_read: "githubRead", steve_mail_read: "mailRead", steve_matters: "matters",
  steve_tools_search: "toolsSearch", steve_tool_call: "toolCall",
  steve_digest_batch: "digestBatch", steve_digest_read: "digestRead", steve_digest_chunk: "digestChunk", steve_digest_finish: "digestFinish", steve_digest_control: "digestControl", steve_digest_report: "digestReport",
  steve_todo_list: "todoList", steve_todo_add: "todoAdd", steve_todo_update: "todoUpdate", steve_todo_complete: "todoComplete",
  steve_onboard_profile: "onboardProfile", steve_onboard_models: "onboardModels", steve_onboard_bots: "onboardBots", steve_onboard_done: "onboardDone",
} as const
type DescKey = (typeof DESC)[keyof typeof DESC]
export const toolDescKey = (name: string): DescKey | undefined => (DESC as Record<string, DescKey | undefined>)[name]


export function toolCount(tool: SteveTool, counts: Map<string, number>): number {
  return counts.get(tool.name) ?? 0
}
