import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { BotToolPins } from "../../src/caps/tool-pins"
const server = { id: "m1", name: "Team Calendar", transport: "http" as const, command: null, args: [], env: [], url: "https://calendar.example.test/mcp", headers: [], created: 1, updated: 1, enabled: true }
const agent = { id: "a1", name: "planner", tool: "steve_a2a_planner", card_url: "https://agent.example.test/.well-known/agent-card.json", title: "Trip Planner", description: "Plan a journey", url: "https://agent.example.test/a2a", version: null, provider: null, protocol: null, skills: [], auth: { scheme: "none" as const }, has_secret: false, fetched: 1, created: 1, updated: 1, enabled: true }
createRoot(document.getElementById("root")!).render(<main className="h-svh overflow-y-auto p-4 sm:p-8"><div className="mx-auto max-w-4xl"><BotToolPins bot="writer" caps={{ mcp: [server, { ...server, id: "m2", name: "Disabled Calendar", enabled: false }], skills: [], a2a: [agent] }} /></div></main>)
