/* 能力 (Rust Hub, steve-modules caps): the workspace's library of MCP servers, skills and remote A2A agents, each
 * defined once and assigned per Bot (switched on or off for that Bot). A secret (an MCP env value or header, an
 * agent's key) lives only in the vault: answers carry its name or `has_secret`, never the value; saving it again
 * without one keeps the stored value. `/capabilities/steve` is Steve itself as the built-in MCP server: the tools
 * every session has, the ones an outside client (Claude Desktop, Cursor, another agent) gets with an access token,
 * and the addresses it connects to. The TS Hub has none of this: NotWired. */
import { modReq as req } from "@/api"

export type CapKind = "mcp" | "skill" | "a2a"
/** a Bot that uses an entry, and whether it is switched on for it */
export type CapUser = { id: string; enabled: boolean }

export type McpTransport = "stdio" | "http" | "sse"
/** an env variable (stdio) or a header (http, sse): a plain value, or a secret shown by name only */
export type McpVar = { name: string; value?: string; secret?: boolean }
export type McpServer = {
  id: string; name: string; transport: McpTransport
  command: string | null; args: string[]; env: McpVar[]
  url: string | null; headers: McpVar[]
  created: number; updated: number
  /** in the library: who uses it */ bots?: CapUser[]
  /** on a Bot: switched on for it */ enabled?: boolean
}
/** what POST / PATCH take: a secret without `value` keeps the stored one; `bot` on POST also assigns it */
export type McpServerIn = Partial<{ name: string; transport: McpTransport; command: string; args: string[]; env: McpVar[]; url: string; headers: McpVar[]; bot: string }>

export type Skill = { id: string; name: string; description: string; content: string; created: number; updated: number; bots?: CapUser[]; enabled?: boolean }
export type SkillIn = Partial<{ name: string; description: string; content: string; bot: string }>

export type A2aAuth = { scheme: "none" } | { scheme: "bearer" } | { scheme: "header"; header: string }
export type A2aSkill = { id: string; name: string; description: string; tags: string[] }
export type A2aAgent = {
  id: string; name: string; tool: string; card_url: string
  title: string; description: string; url: string | null; version: string | null; provider: string | null; protocol: string | null
  skills: A2aSkill[]; auth: A2aAuth; has_secret: boolean
  fetched: number; created: number; updated: number
  bots?: CapUser[]; enabled?: boolean
}
export type A2aAgentIn = Partial<{ card_url: string; name: string; auth: A2aAuth; secret: string; refresh: boolean; bot: string }>
/** an Agent Card as fetched, not kept */
export type AgentCard = { name: string; description?: string; url: string; version?: string; protocolVersion?: string; provider?: { organization?: string }; skills?: { id: string; name?: string; description?: string; tags?: string[] }[] }

export type ToolPin = { kind: "tool" | "mcp" | "a2a"; target: string }
export type BotTools = { core: string[]; pins: ToolPin[]; usage: { tool: string; searched: number; called: number }[]; days: number }
export type BotCaps = { mcp: McpServer[]; skills: Skill[]; a2a: A2aAgent[] }

export type SteveTool = { name: string; description: string }
export type SteveServer = {
  tools: SteveTool[]
  outside: SteveTool[]
  /** where an outside client connects, the public address first */
  endpoints: { url: string; kind: "public" | "lan" | "local" }[]
  agent_card: string
  a2a: string
}

const lib = (k: string) => `/capabilities/${k}`
const one = (k: string, id: string) => `${lib(k)}/${encodeURIComponent(id)}`

export const capsApi = {
  mcp: {
    list: () => req<McpServer[]>("GET", lib("mcp-servers")),
    create: (b: McpServerIn) => req<McpServer>("POST", lib("mcp-servers"), b),
    update: (id: string, b: McpServerIn) => req<McpServer>("PATCH", one("mcp-servers", id), b),
    remove: (id: string) => req<{ ok: true }>("DELETE", one("mcp-servers", id)),
  },
  skills: {
    list: () => req<Skill[]>("GET", lib("skills")),
    create: (b: SkillIn) => req<Skill>("POST", lib("skills"), b),
    update: (id: string, b: SkillIn) => req<Skill>("PATCH", one("skills", id), b),
    remove: (id: string) => req<{ ok: true }>("DELETE", one("skills", id)),
  },
  a2a: {
    list: () => req<A2aAgent[]>("GET", lib("a2a-agents")),
    preview: (card_url: string) => req<{ card_url: string; card: AgentCard }>("POST", lib("a2a-agents") + "/preview", { card_url }),
    create: (b: A2aAgentIn) => req<A2aAgent>("POST", lib("a2a-agents"), b),
    update: (id: string, b: A2aAgentIn) => req<A2aAgent>("PATCH", one("a2a-agents", id), b),
    remove: (id: string) => req<{ ok: true }>("DELETE", one("a2a-agents", id)),
  },
  tools: (bot: string) => req<BotTools>("GET", `/bots/${encodeURIComponent(bot)}/tools`),
  pin: (bot: string, pin: ToolPin, on: boolean) => req(on ? "PUT" : "DELETE", `/bots/${encodeURIComponent(bot)}/tool-pins/${pin.kind}/${encodeURIComponent(pin.target)}`),
  bot: (bot: string) => req<BotCaps>("GET", `/bots/${encodeURIComponent(bot)}/capabilities`),
  assign: (bot: string, kind: CapKind, id: string, enabled = true) => req("PUT", `/bots/${encodeURIComponent(bot)}/capabilities/${kind}/${encodeURIComponent(id)}`, { enabled }),
  unassign: (bot: string, kind: CapKind, id: string) => req("DELETE", `/bots/${encodeURIComponent(bot)}/capabilities/${kind}/${encodeURIComponent(id)}`),
  steve: () => req<SteveServer>("GET", "/capabilities/steve"),
  /** an access token for an outside client: its text comes back once */
  token: (name: string) => req<{ token: string; item: { id: string; name: string } }>("POST", "/access/tokens", { name }),
}
