// The retry scenarios use Pi's native accounting (no transcript-tail timer). Its registry entry
// requires HTTP MCP, unlike the generic scripted adapter. Keep that handshake contract here;
// the scripted turns never call an MCP server, so no provider or HTTP service is needed.
const write = process.stdout.write.bind(process.stdout)
process.stdout.write = (chunk, ...args) => {
  // fake-acp.js writes each JSON-RPC message as one complete line, plus malformed startup
  // output to exercise the client's parser. Preserve that output too.
  let message
  try { message = JSON.parse(chunk) } catch { return write(chunk, ...args) }
  if (message.result?.agentCapabilities) {
    message.result.agentCapabilities.mcpCapabilities = { http: true }
  }
  return write(JSON.stringify(message) + "\n", ...args)
}

await import("../../../steve-acp/tests/fake-acp.js")
