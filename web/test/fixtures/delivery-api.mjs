/* Isolated HTTP fixture for delivery browser checks. No installed Hub, Node or user data is accessed. */
import { createServer } from "node:http"
import { randomUUID } from "node:crypto"
import { crc32 } from "node:zlib"

const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aWQAAAABJRU5ErkJggg==", "base64")
function zip(files) {
  const data = [], central = []; let offset = 0
  for (const [name, body] of Object.entries(files)) {
    const b = Buffer.from(body), n = Buffer.from(name), crc = crc32(b)
    const local = Buffer.alloc(30); local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6)
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(b.length, 18); local.writeUInt32LE(b.length, 22); local.writeUInt16LE(n.length, 26)
    const entry = Buffer.alloc(46); entry.writeUInt32LE(0x02014b50); entry.writeUInt16LE(20, 4); entry.writeUInt16LE(20, 6); entry.writeUInt16LE(0x0800, 8)
    entry.writeUInt32LE(crc, 16); entry.writeUInt32LE(b.length, 20); entry.writeUInt32LE(b.length, 24); entry.writeUInt16LE(n.length, 28); entry.writeUInt32LE(offset, 42)
    data.push(local, n, b); central.push(entry, n); offset += local.length + n.length + b.length
  }
  const directory = Buffer.concat(central), end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(central.length / 2, 8); end.writeUInt16LE(central.length / 2, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16)
  return Buffer.concat([...data, directory, end])
}
function pdf() {
  let doc = "%PDF-1.4\n", offsets = [0]
  const objects = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>", "<< /Length 0 >>\nstream\n\nendstream"]
  for (const [i, obj] of objects.entries()) { offsets.push(Buffer.byteLength(doc)); doc += `${i + 1} 0 obj\n${obj}\nendobj\n` }
  const xref = Buffer.byteLength(doc)
  doc += `xref\n0 5\n0000000000 65535 f \n${offsets.slice(1).map((n) => String(n).padStart(10, "0") + " 00000 n \n").join("")}trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`
  return Buffer.from(doc)
}
const text = "# Delivered report\n\nPersistent **Markdown** from a real HTTP endpoint.\n"
export const longName = "delivery_".repeat(18) + "最终交付.md"
const design = {
  "pages/index.html": { mime: "text/html", body: '<!doctype html><html><head><link rel="stylesheet" href="../assets/theme.css"></head><body><h1>Design entry</h1><img alt="Bundled image" src="../assets/image.png"><a href="details.html">Open details</a><script>try { parent.document.body.dataset.escaped = "yes" } catch { document.body.dataset.isolated = "yes" }</script></body></html>' },
  "pages/details.html": { mime: "text/html", body: '<!doctype html><h1>Design details</h1><a href="index.html">Back</a>' },
  "assets/theme.css": { mime: "text/css", body: "h1 { color: rgb(20, 100, 180) }" },
  "assets/image.png": { mime: "image/png", body: png },
}
const files = {
  md: { name: longName, mime: "text/markdown", body: text },
  image: { name: "image.png", mime: "image/png", body: png },
  pdf: { name: "paper.pdf", mime: "application/pdf", body: pdf() },
  html: { name: "design.zip", mime: "application/zip", body: zip(Object.fromEntries(Object.entries(design).map(([p, f]) => [p, f.body]))), entrypoint: "pages/index.html" },
  office: { name: "report.docx", mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", body: zip({ "word/document.xml": '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Delivered Office document</w:t></w:r></w:p></w:body></w:document>' }) },
  binary: { name: "data.bin", mime: "application/octet-stream", body: "binary fixture" },
}

export async function deliveryFixture() {
  const requests = [], shares = new Map(), streams = new Set()
  const created = Date.now() - 60_000
  const workspace = { id: "fixture", name: "Test workspace", description: "", item_prefix: "ST", sort: 0, created, updated: created }
  const item = { id: "ST-100", workspace_id: workspace.id, no: 100, title: "Delivered files", status: "done", stage: "", bot_id: null, project_id: null, cwd: null, branch: null, plan: [], now: "", accept_mode: null, priority: "", archived: false, star: false, due: null, snooze: null,
    source: ["chat", "chat"], parent: null, created, updated: created, goal: "Stable delivery", goals: 1, tags: [], deps: [], sessionIds: [], open: [], queued: 0, live: false }
  const state = { version: "fixture", web_version: "fixture", workspace, workspaces: [workspace], items: [item], decisions: [], recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [], runtimes: [] }
  const json = (res, value, status = 200) => { res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", "x-steve-cursor": "1" }); res.end(JSON.stringify(value)) }
  const server = createServer(async (req, res) => {
    const origin = `http://${req.headers.host}`, u = new URL(req.url, origin), path = u.pathname
    requests.push({ method: req.method, path, ws: req.headers["x-steve-ws"], query: u.search, cookie: req.headers.cookie })
    if (path === "/auth/hello") return json(res, { name: "steve", version: "fixture", api: 1, login: { token: false, oidc: true } })
    if (path === "/auth/dev") { res.writeHead(302, { "set-cookie": "fixture-auth=1; HttpOnly; SameSite=Lax; Path=/", location: u.searchParams.get("back") || "/" }); return res.end() }
    const publicPath = /^\/api\/delivery-shares\/([^/]+)(.*)$/.exec(path)
    const loggedIn = (req.headers.cookie || "").includes("fixture-auth=1")
    if (!publicPath && !loggedIn) return json(res, { error: "unauthenticated", login: "/auth/dev", token: false }, 401)
    if (path === "/api/state") return json(res, state)
    if (path === "/api/plugins/themes") return json(res, { themes: [] })
    if (path === "/api/yolo" || path === "/api/yolo/actions") return json(res, { windows: [], on: false, mode: "off", scopes: [], counts: {} })
    if (path === "/api/stream") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": keepalive\n\n"); streams.add(res); req.on("close", () => streams.delete(res)); return }
    if (path === "/api/items/ST-100") return json(res, { ...item, goalsList: [], timeline: [], decisions: [], dependents: [], annotations: [], artifacts: [
      { id: "artifact-md", kind: "file", title: longName, location: null, delivery_id: "md", created },
      { id: "artifact-html", kind: "file", title: "HTML design bundle", location: null, delivery_id: "html", created },
      { id: "artifact-old", kind: "file", title: "Older workspace report", location: "workspace/old.md", created },
    ] })
    if (path === "/api/items/ST-100/artifacts/artifact-old/preview") return json(res, { type: "text", name: "old.md", path: "workspace/old.md", lang: "markdown", text: "# Legacy workspace report", truncated: false })
    let id, rest, token
    if (publicPath) {
      token = decodeURIComponent(publicPath[1]); const s = shares.get(token)
      if (!s || s.revoked_at || s.expires_at <= Date.now()) return json(res, { error: "Share unavailable" }, 410)
      id = s.id; rest = publicPath[2]
    } else {
      const m = /^\/api\/deliveries\/([^/]+)(.*)$/.exec(path)
      if (!m) return json(res, { error: "Unknown fixture route: " + path }, 404)
      id = decodeURIComponent(m[1]); rest = m[2]
    }
    const file = files[id]
    if (!file) return json(res, { error: "Delivery not found" }, 404)
    const root = token ? `/api/delivery-shares/${encodeURIComponent(token)}` : `/api/deliveries/${encodeURIComponent(id)}`
    const meta = () => ({ id, item_id: item.id, title: id === "md" ? "Delivered report" : file.name, name: file.name, mime: file.mime, size: Buffer.byteLength(file.body), created, entrypoint: file.entrypoint ?? null,
      url: origin + (token ? `/shares/${token}` : `/deliveries/${id}`), download_url: root + "/raw", content_url: root + (file.entrypoint ? "/content/" : "/content/file"), access_warning: id === "html" ? "Fixture address is reachable only in this isolated development check." : null,
      ...(token ? { expires_at: shares.get(token).expires_at } : {}),
    })
    if (!rest) return json(res, meta())
    if (!token && rest === "/shares") {
      if (req.method === "GET") return json(res, [...shares].filter(([, s]) => s.id === id).map(([token, s]) => ({ token, url: origin + `/shares/${token}`, expires_at: s.expires_at, revoked_at: s.revoked_at })))
      if (req.method === "POST") {
        let input = ""; for await (const chunk of req) input += chunk
        const { expires_at } = JSON.parse(input)
        if (!Number.isFinite(expires_at) || expires_at <= Date.now()) return json(res, { error: "Expiry must be in the future" }, 400)
        const token = randomUUID(); shares.set(token, { id, expires_at, revoked_at: null })
        return json(res, { token, url: origin + `/shares/${token}`, expires_at })
      }
    }
    if (!token && rest.startsWith("/shares/") && req.method === "DELETE") {
      const s = shares.get(decodeURIComponent(rest.slice(8)))
      if (!s || s.id !== id) return json(res, { error: "Not found" }, 404)
      s.revoked_at = Date.now(); return json(res, { ok: true })
    }
    if (rest === "/raw") {
      res.writeHead(200, { "content-type": file.mime, "content-disposition": "attachment; filename=original", "cache-control": "no-store" }); return res.end(file.body)
    }
    if (rest.startsWith("/content/")) {
      const resource = rest.slice(9)
      let body = file.body, mime = file.mime
      if (id === "html") {
        if (!design[resource]) return json(res, { error: "Resource not found" }, 404)
        ;({ body, mime } = design[resource])
      }
      res.writeHead(200, { "content-type": mime, "cache-control": "no-store", "x-content-type-options": "nosniff", ...(mime === "text/html" ? { "content-security-policy": token ? "sandbox allow-scripts" : "sandbox allow-same-origin" } : {}) }); return res.end(body)
    }
    json(res, { error: "Unknown delivery route" }, 404)
  })
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
  return { url: `http://127.0.0.1:${server.address().port}`, requests, shares, close: async () => { for (const s of streams) s.end(); server.closeAllConnections(); await new Promise((r) => server.close(r)) } }
}
