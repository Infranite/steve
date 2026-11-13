/* 一个 Bot 自己的 MCP 服务器（Rust Hub，steve-modules bot_mcp）：会话打开时随 ACP 的 mcpServers 交给 harness，
 * 和 Steve 自己的工具并列。机密（env 的值、header 的值）只存在保险箱里：读回来只有名字和 secret: true；
 * 保存时机密不带 value 就保留保险箱里原来的值。TS Hub 没有这些接口：一律是 NotWired。 */
import { modReq as req } from "@/api"

export type McpTransport = "stdio" | "http" | "sse"
/** an env variable (stdio) or a header (http, sse): a plain value, or a secret shown by name only */
export type McpVar = { name: string; value?: string; secret?: boolean }
export type McpServer = {
  id: string; bot_id: string; name: string; transport: McpTransport
  command: string | null; args: string[]; env: McpVar[]
  url: string | null; headers: McpVar[]
  enabled: boolean; created: number; updated: number
}
/** what POST / PATCH take: a secret without `value` keeps the stored one */
export type McpServerIn = Partial<{ name: string; transport: McpTransport; command: string; args: string[]; env: McpVar[]; url: string; headers: McpVar[]; enabled: boolean }>

const base = (bot: string) => `/bots/${encodeURIComponent(bot)}/mcp-servers`
export const botMcpApi = {
  list: (bot: string) => req<McpServer[]>("GET", base(bot)),
  create: (bot: string, b: McpServerIn) => req<McpServer>("POST", base(bot), b),
  update: (bot: string, id: string, b: McpServerIn) => req<McpServer>("PATCH", `${base(bot)}/${encodeURIComponent(id)}`, b),
  remove: (bot: string, id: string) => req<{ ok: true }>("DELETE", `${base(bot)}/${encodeURIComponent(id)}`),
}
