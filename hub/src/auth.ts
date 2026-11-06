/* Login: OIDC against the lab's Authentik (owner group is enforced there). Browsers only hold an opaque cookie.
 * The first subject to log in becomes the workspace owner; later unknown subjects are refused until invited. */
import { Hono, type Context, type Next } from "hono"
import { getCookie, setCookie, deleteCookie } from "hono/cookie"
import { randomBytes } from "node:crypto"
import * as oidc from "openid-client"
import { config } from "./config.js"
import { users, authSessions, audit } from "./repo.js"

const SESSION = "steve_session", LOGIN = "steve_login", TTL = 30 * 24 * 3600_000
const secure = config.publicUrl.startsWith("https://")
const logins = new Map<string, { verifier: string; state: string; nonce: string; back: string; origin: string; at: number }>()
/** the configured origin matching the Host the browser used; anything else falls back to the primary origin */
function originOf(c: Context) {
  const host = (c.req.header("x-forwarded-host") ?? c.req.header("host") ?? "").split(",")[0].trim().toLowerCase()
  return config.publicUrls.find((u) => new URL(u).host === host) ?? config.publicUrls[0] ?? config.publicUrl
}
let cfg: oidc.Configuration | null = null
async function oidcConfig() {
  if (cfg) return cfg
  if (!config.oidc.issuer || !config.oidc.clientId) throw new Error("OIDC 未配置")
  cfg = await oidc.discovery(new URL(config.oidc.issuer), config.oidc.clientId, config.oidc.clientSecret)
  return cfg
}
function startSession(c: Context, userId: string) {
  const token = randomBytes(32).toString("base64url")
  authSessions.create(userId, token, TTL)
  setCookie(c, SESSION, token, { httpOnly: true, secure, sameSite: "Lax", path: "/", maxAge: TTL / 1000 })
}
function login(sub: string, name: string, email: string | null) {
  const u = users.bySubject(sub); if (u) return u.id
  if (users.count() > 0) return null
  const id = users.create(sub, name, email, "owner"); audit(id, "user.create", id, { role: "owner" }); return id
}

export const authRoutes = new Hono()
authRoutes.get("/login", async (c) => {
  const conf = await oidcConfig()
  const verifier = oidc.randomPKCECodeVerifier(), state = oidc.randomState(), nonce = oidc.randomNonce(), browser = randomBytes(16).toString("base64url")
  const origin = originOf(c)
  logins.set(browser, { verifier, state, nonce, back: c.req.query("back") ?? "/", origin, at: Date.now() })
  setCookie(c, LOGIN, browser, { httpOnly: true, secure, sameSite: "Lax", path: "/auth", maxAge: 600 })
  const url = oidc.buildAuthorizationUrl(conf, { redirect_uri: `${origin}/auth/callback`, scope: "openid profile", code_challenge: await oidc.calculatePKCECodeChallenge(verifier), code_challenge_method: "S256", state, nonce })
  return c.redirect(url.href)
})
authRoutes.get("/callback", async (c) => {
  const browser = getCookie(c, LOGIN), l = browser ? logins.get(browser) : undefined
  if (!l || Date.now() - l.at > 600_000) return c.text("登录已过期，请重新登录", 400)
  logins.delete(browser!); deleteCookie(c, LOGIN, { path: "/auth" })
  try {
    const conf = await oidcConfig()
    const here = new URL(c.req.url); const cb = new URL(`${l.origin}/auth/callback${here.search}`)
    const t = await oidc.authorizationCodeGrant(conf, cb, { pkceCodeVerifier: l.verifier, expectedState: l.state, expectedNonce: l.nonce })
    const cl = t.claims()!
    const id = login(String(cl.sub), String(cl.name ?? cl.preferred_username ?? cl.email ?? "我"), cl.email ? String(cl.email) : null)
    if (!id) return c.text("这个账号还没有被邀请使用 Steve", 403)
    startSession(c, id); return c.redirect(isLocalPath(l.back) ? l.back : "/")
  } catch (e) { console.error("oidc callback", (e as Error).message); return c.text("登录失败", 400) }
})
authRoutes.post("/logout", (c) => { const t = getCookie(c, SESSION); if (t) authSessions.drop(t); deleteCookie(c, SESSION, { path: "/" }); return c.json({ ok: true }) })
/** one leading "/", not "//" or "/\\" (both mean another host), no control characters */
export const isLocalPath = (b: string) => /^\/(?![/\\])/.test(b) && !/[\u0000-\u001f\u007f-\u009f]/.test(b)
/* development: loopback-only one-click login, disabled unless STEVE_DEV_LOGIN=1 */
authRoutes.get("/dev", (c) => {
  const ip = (c.env as any)?.incoming?.socket?.remoteAddress ?? ""
  if (!config.devLogin || !/^(127\.|::1|::ffff:127\.)/.test(ip)) return c.text("not found", 404)
  const id = login("dev:local", "本机", null) ?? users.bySubject("dev:local")?.id
  if (!id) return c.text("已有所有者，本机登录不可用", 403)
  startSession(c, id); return c.redirect("/")
})

export async function requireUser(c: Context, next: Next) {
  const t = getCookie(c, SESSION), u = t ? authSessions.user(t) : undefined
  if (!u) return c.json({ error: "unauthenticated", login: config.oidc.issuer ? "/auth/login" : config.devLogin ? "/auth/dev" : null }, 401)
  c.set("user" as never, u as never); await next()
}
