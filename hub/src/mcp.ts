/* Steve tools, served over MCP (streamable HTTP). Each session gets a bearer token scoped to its own item.
 * Asking, requesting approval and delivering are "send and end the turn": the answer arrives later as a new message,
 * so nothing waits in memory and a Hub restart loses nothing. */
import type { IncomingMessage, ServerResponse } from "node:http"
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js"
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js"
import { z } from "zod"
import { sessions, type Session } from "./repo.js"

export type ToolActions = {
  update(s: Session, now: string, plan: { step: string; done: boolean }[]): void
  ask(s: Session, question: string, options: string[]): void
  requestApproval(s: Session, action: string, detail: string): void
  /** a refusal comes back as text for the agent */
  deliver(s: Session, summary: string, outputs: { kind: string; title: string; location?: string }[], checks: { name: string; passed: boolean }[], unfinished?: string[]): string | void
  publish(s: Session, branch: string, summary: string): void
}
let actions: ToolActions
export const setToolActions = (a: ToolActions) => { actions = a }
/** for tests: call a tool's action without the MCP transport */
export const toolActions = () => actions

const END = "End your turn now. The user's answer will arrive as your next message."
function server(s: Session) {
  const m = new McpServer({ name: "steve", version: "1.0.0" })
  m.registerTool("steve_update", {
    description: "Tell Steve (and the user) what you are doing now and your current plan. Call it when you start, and whenever the plan or your current step changes.",
    inputSchema: { now: z.string().describe("one short sentence, in the user's language"), plan: z.array(z.object({ step: z.string(), done: z.boolean() })).describe("the whole plan, in order") },
  }, async ({ now, plan }) => { actions.update(s, now, plan); return { content: [{ type: "text", text: "Steve updated the item." }] } })
  m.registerTool("steve_ask", {
    description: "Ask the user a question you cannot answer yourself. Prefer concrete options. Do not ask in plain text; use this tool, then end your turn.",
    inputSchema: { question: z.string(), options: z.array(z.string()).describe("2–4 choices; the user may also answer freely") },
  }, async ({ question, options }) => { actions.ask(s, question, options); return { content: [{ type: "text", text: `Question sent to the user. ${END}` }] } })
  m.registerTool("steve_request_approval", {
    description: "Ask the user to approve an action with outside effects before doing it: deploying or releasing, sending a message or email, spending money, deleting data, pushing to a shared branch.",
    inputSchema: { action: z.string().describe("what you want to do"), detail: z.string().describe("exactly what will happen, so the user can decide") },
  }, async ({ action, detail }) => { actions.requestApproval(s, action, detail); return { content: [{ type: "text", text: `Approval requested. ${END}` }] } })
  m.registerTool("steve_deliver", {
    description: "Deliver the finished work for the user's acceptance. Include what you produced and the checks you ran. Then end your turn.",
    inputSchema: {
      summary: z.string().describe("what was done and how it was verified, in the user's language"),
      outputs: z.array(z.object({ kind: z.string().describe("e.g. 分支, 提交, PR, 文件, 文档"), title: z.string(), location: z.string().optional() })),
      checks: z.array(z.object({ name: z.string(), passed: z.boolean() })).describe("build, tests, manual checks you actually ran"),
      unfinished: z.array(z.string()).optional().describe("plan steps (exact text) you deliberately left open; every other step must already be done, or the delivery is refused"),
    },
  }, async ({ summary, outputs, checks, unfinished }) => {
    const refused = actions.deliver(s, summary, outputs, checks, unfinished)
    return refused ? { content: [{ type: "text", text: refused }], isError: true } : { content: [{ type: "text", text: `Delivered for acceptance. ${END}` }] }
  })
  m.registerTool("steve_publish", {
    description: "Ask the user to release a new version of Steve from your branch, after the item was accepted. Steve rebases the branch onto main (a conflict comes back to you), fast-forwards main to it, waits for other running work to finish, builds, restarts itself and rolls back if the health check fails. Then end your turn; the result arrives later.",
    inputSchema: { branch: z.string().describe("the git branch to release, e.g. steve/st-12"), summary: z.string().describe("what changes for the user, in Chinese") },
  }, async ({ branch, summary }) => { actions.publish(s, branch, summary); return { content: [{ type: "text", text: `Release requested. ${END}` }] } })
  return m
}

export async function handleMcp(req: IncomingMessage, res: ServerResponse) {
  const auth = req.headers.authorization ?? "", token = auth.startsWith("Bearer ") ? auth.slice(7) : ""
  const s = token ? sessions.byToken(token) : undefined
  if (!s) { res.writeHead(401).end(); return }
  const t = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined }), m = server(s)
  res.on("close", () => { void t.close(); void m.close() })
  await m.connect(t); await t.handleRequest(req, res)
}
