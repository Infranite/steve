/* The configuration an outside MCP client needs to reach Steve: the Hub's /mcp address and a user's access token in
 * the Authorization header, in the shape each client reads. Built from what the Hub says its addresses are, never a
 * host written here. Without a token the snippet carries a placeholder to replace. */

export type ClientKind = "generic" | "claudeDesktop" | "cursor" | "claudeCode"
export const CLIENTS: readonly ClientKind[] = ["generic", "claudeDesktop", "cursor", "claudeCode"]
/** what a snippet is: JSON for a config file, or a shell command */
export const LANGUAGE: Record<ClientKind, "json" | "bash"> = { generic: "json", claudeDesktop: "json", cursor: "json", claudeCode: "bash" }
/** the file a JSON snippet goes in, as its header names it */
export const FILE: Record<ClientKind, string | undefined> = { generic: undefined, claudeDesktop: "claude_desktop_config.json", cursor: "mcp.json", claudeCode: undefined }

const json = (v: unknown) => JSON.stringify(v, null, 2)
/** single quotes for a POSIX shell */
const sh = (s: string) => (/^[A-Za-z0-9_./:@%+=,-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`)

/** the snippet for `kind`, connecting to `url` with `token` (or `placeholder` when there is none) under the name `name` */
export function snippet(kind: ClientKind, url: string, token: string | null, placeholder: string, name = "steve"): string {
  const bearer = `Bearer ${token || placeholder}`
  switch (kind) {
    case "generic":
      return json({ mcpServers: { [name]: { type: "http", url, headers: { Authorization: bearer } } } })
    case "cursor":
      return json({ mcpServers: { [name]: { url, headers: { Authorization: bearer } } } })
    case "claudeDesktop":
      // Claude Desktop's config starts local commands: mcp-remote bridges them to the Hub's HTTP endpoint. The header
      // goes through an environment variable, so the space in "Bearer …" is not split into two arguments.
      return json({ mcpServers: { [name]: { command: "npx", args: ["-y", "mcp-remote", url, "--header", "Authorization:${STEVE_AUTH}"], env: { STEVE_AUTH: bearer } } } })
    case "claudeCode":
      return `claude mcp add --transport http ${sh(name)} ${sh(url)} --header ${sh(`Authorization: ${bearer}`)}`
  }
}

/** `/mcp` of the Hub, with the workspace named when the user has several (the token alone means their first) */
export function mcpUrl(endpoint: string, ws: string | null, several: boolean): string {
  return several && ws ? `${endpoint}?ws=${encodeURIComponent(ws)}` : endpoint
}
