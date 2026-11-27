/* External MCP client configuration snippets rendered from declarative templates in
 * `steve-harness-spec` (`src/gen/harness-meta.json`). Built from the Hub's configured `/mcp`
 * endpoint and the user's access token (or placeholder). */
import harnessMeta from "../gen/harness-meta.json" with { type: "json" }

type ClientSpec = {
  id: string
  language: "json" | "bash"
  file?: string
  wrap?: boolean
  template?: unknown
  command_template?: string
}

const SPECS = harnessMeta.mcp_clients as readonly ClientSpec[]
const BY_ID: Record<string, ClientSpec> = Object.fromEntries(SPECS.map((s) => [s.id, s]))

export type ClientKind = string
export const CLIENTS: readonly ClientKind[] = SPECS.map((s) => s.id)
/** what a snippet is: JSON for a config file, or a shell command */
export const LANGUAGE: Record<ClientKind, "json" | "bash"> = Object.fromEntries(SPECS.map((s) => [s.id, s.language]))
/** the file a JSON snippet goes in, as its header names it */
export const FILE: Record<ClientKind, string | undefined> = Object.fromEntries(SPECS.map((s) => [s.id, s.file]))
/** whether the snippet wraps lines in the code viewer */
export const WRAP: Record<ClientKind, boolean> = Object.fromEntries(SPECS.map((s) => [s.id, !!s.wrap]))

const json = (v: unknown) => JSON.stringify(v, null, 2)
/** single quotes for a POSIX shell */
const sh = (s: string) => (/^[A-Za-z0-9_./:@%+=,-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`)

function replaceTokens(text: string, vars: Record<string, string>): string {
  return text.replace(/\{\{(\w+)\}\}/g, (m, k: string) => (Object.hasOwn(vars, k) ? vars[k] : m))
}

function renderJsonTemplate(node: unknown, vars: Record<string, string>): unknown {
  if (typeof node === "string") return replaceTokens(node, vars)
  if (Array.isArray(node)) return node.map((item) => renderJsonTemplate(item, vars))
  if (node && typeof node === "object") {
    const out: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
      out[replaceTokens(k, vars)] = renderJsonTemplate(v, vars)
    }
    return out
  }
  return node
}

/** the snippet for `kind`, connecting to `url` with `token` (or `placeholder` when there is none) under the name `name` */
export function snippet(kind: ClientKind, url: string, token: string | null, placeholder: string, name = "steve"): string {
  const spec = BY_ID[kind] ?? SPECS[0]
  const bearer = `Bearer ${token || placeholder}`
  if (spec.language === "bash" && spec.command_template) {
    return replaceTokens(spec.command_template, {
      name: sh(name),
      url: sh(url),
      bearer: sh(bearer),
      auth_header: sh(`Authorization: ${bearer}`),
    })
  }
  return json(renderJsonTemplate(spec.template, { name, url, bearer }))
}

/** `/mcp` of the Hub, with the workspace named when the user has several (the token alone means their first) */
export function mcpUrl(endpoint: string, ws: string | null, several: boolean): string {
  return several && ws ? `${endpoint}?ws=${encodeURIComponent(ws)}` : endpoint
}
