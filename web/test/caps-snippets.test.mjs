import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/caps/snippets.ts")
const URL = "https://hub.example.test/mcp"

test("every client's configuration carries the Hub's /mcp and the token as a bearer header", () => {
  const g = JSON.parse(m.snippet("generic", URL, "stt_abc", "<token>"))
  assert.deepEqual(g, { mcpServers: { steve: { type: "http", url: URL, headers: { Authorization: "Bearer stt_abc" } } } })
  const c = JSON.parse(m.snippet("cursor", URL, "stt_abc", "<token>"))
  assert.deepEqual(c.mcpServers.steve, { url: URL, headers: { Authorization: "Bearer stt_abc" } })
  const d = JSON.parse(m.snippet("claudeDesktop", URL, "stt_abc", "<token>"))
  assert.equal(d.mcpServers.steve.command, "npx")
  assert.deepEqual(d.mcpServers.steve.args, ["-y", "mcp-remote", URL, "--header", "Authorization:${STEVE_AUTH}"])
  assert.equal(d.mcpServers.steve.env.STEVE_AUTH, "Bearer stt_abc")
  assert.equal(m.snippet("claudeCode", URL, "stt_abc", "<token>"), `claude mcp add --transport http steve ${URL} --header 'Authorization: Bearer stt_abc'`)
})

test("without a token the placeholder stands in, and a URL with a query is quoted for the shell", () => {
  assert.match(m.snippet("generic", URL, null, "<your access token>"), /Bearer <your access token>/)
  const u = m.mcpUrl("http://127.0.0.1:8440/mcp", "ws_2", true)
  assert.equal(u, "http://127.0.0.1:8440/mcp?ws=ws_2")
  assert.equal(m.mcpUrl("http://127.0.0.1:8440/mcp", "ws_2", false), "http://127.0.0.1:8440/mcp")
  assert.match(m.snippet("claudeCode", u, "stt_x", "-"), /'http:\/\/127\.0\.0\.1:8440\/mcp\?ws=ws_2'/)
})
