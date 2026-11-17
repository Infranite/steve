import type { NativeBridge } from "./hub"

/** Opaque rendered body background in sRGB bytes, not a fixed native palette. */
export type ShellAppearance = { dark: boolean; background: [number, number, number]; followSystem?: boolean }

/** Page context only. The trusted native document supplies Hub identity; never send credentials or counts. */
export type ShellContext = {
  workspaceID: string | null
  workspaceName: string | null
  route: string
  title: string
  ready: boolean
  authenticated: boolean
  focused: boolean
  visible: boolean
  appearance?: ShellAppearance
}
export type ShellNavigation = { id: string; hubID: string; workspaceID?: string; route: string; targetID?: string }
const OPERATIONS = ["shell.context", "shell.navigation.next", "shell.navigation.ack"]
const HUB_SWITCH_OPERATIONS = ["shell.hub-switch.next", "shell.hub-switch.ack"]
export const SHELL_POLL_MS = 2000
const NAVIGATION_TIMEOUT_MS = 10000

export function shellBridge(bridge: NativeBridge | null): NativeBridge | null {
  return bridge && OPERATIONS.every((op) => bridge.can.includes(op)) ? bridge : null
}
/** Only the Hub switcher is duplicated by the Mac toolbar. Business navigation/actions stay in Web. */
export const nativeHubChrome = (bridge: NativeBridge | null): boolean =>
  !!shellBridge(bridge) && bridge!.platform === "macos" && bridge!.can.includes("shell.chrome.hub")

/** Navigation is deliberately narrower than ordinary browser URLs. Keep roots aligned with App.tsx. */
export function shellRoute(raw: unknown): string | null {
  if (typeof raw !== "string" || raw.length > 4096) return null
  const route = raw.startsWith("/") ? "#" + raw : raw
  if (!route.startsWith("#/") || /[\\\u0000-\u001f\u007f]/u.test(route)) return null
  let decoded: string
  try { decoded = decodeURIComponent(route.slice(1)) } catch { return null }
  if (/[#\\\u0000-\u001f\u007f]/u.test(decoded)) return null
  const path = decoded.split("?")[0], parts = path.slice(1).split("/")
  if (path.includes("//") || parts.some((p) => p === "." || p === "..")) return null
  const roots = ["", "c", "feed", "inbox", "items", "projects", "sessions", "settings", "bots", "capabilities", "design", "knowledge", "devices", "github", "integrations", "channels", "auto", "signals", "mail", "accounts", "vault", "yolo", "stats", "contacts", "orgs", "teams", "publications", "more", "hub"]
  if (!roots.includes(parts[0])) return null
  // Redirect-only legacy roots are not native destinations. Hub actions are the two explicit modal routes.
  if (parts[0] === "hub" && (parts.length !== 2 || !["add", "pair"].includes(parts[1]) || route.includes("?"))) return null
  if (parts[0] === "auto" && ["acc", "space"].includes(parts[1])) return null
  return route
}

export function shellDestination(route: string): string {
  if (route === "#/hub/add" || route === "#/hub/pair") return "#/"
  if (route === "#/settings/hub") return "#/devices/hub"
  if (route === "#/settings/access") return "#/devices/clients"
  return route
}

export function attentionTargetRoute(route: string, targetID: unknown): boolean {
  if (typeof targetID !== "string" || !targetID || targetID.length > 512 || /[\u0000-\u001f\u007f]/u.test(targetID)) return false
  return route === "#/" || route === "#/inbox"
    || route === `#/feed?entry=${encodeURIComponent(targetID)}`
    || route === `#/c/${encodeURIComponent("chan:" + targetID)}`
}

export type ShellPort = {
  allowed(): boolean
  hasWorkspace(id: string): boolean
  selectWorkspace(id: string): Promise<boolean>
  navigate(route: string): void
  /** Hub actions redirect to #/; success means their sheet is actually displayed. */
  actionOpen(route: string): boolean
  /** Resolves only after the scoped entity is rendered and its visibility report reaches native. */
  landTarget?(route: string, targetID: string, workspace: string, signal: AbortSignal, active: () => boolean): Promise<void>
}

/** One session per mounted App, not per keyed route. Polls never overlap; context reports coalesce by value. */
export class NativeShellSession {
  private context: ShellContext | null = null
  private published = ""
  private publishing: Promise<void> | null = null
  private live = true
  private revision = 0
  private authGeneration = 0
  private timer: ReturnType<typeof setTimeout> | undefined
  private waiters = new Set<() => void>()
  private polling = false
  private landing: AbortController | undefined

  private bridge: NativeBridge
  private port: ShellPort
  constructor(bridge: NativeBridge, port: ShellPort) { this.bridge = bridge; this.port = port }

  update(context: ShellContext) {
    if (!this.live) return
    const previous = this.context
    if (previous && (previous.workspaceID !== context.workspaceID || previous.route !== context.route || !context.ready || !context.authenticated)) this.landing?.abort()
    if (previous?.authenticated !== context.authenticated) this.authGeneration++
    if (previous?.authenticated !== context.authenticated || previous?.workspaceID !== context.workspaceID || previous?.ready !== context.ready) this.revision++
    this.context = context
    this.waiters.forEach((wake) => wake())
    void this.publish().catch(() => {}) // A failed bridge is retried at the next poll/change, never an unhandled rejection.
    this.schedule()
  }

  invalidate() {
    this.published = ""
    void this.publish().catch(() => {})
  }

  private schedule() {
    if (this.timer) clearTimeout(this.timer)
    this.timer = undefined
    if (!this.live || this.polling || !this.context) return
    this.timer = setTimeout(() => { this.timer = undefined; void this.poll() }, SHELL_POLL_MS)
  }

  /** Drain the most recent value before ack, so native can verify the actual rendered destination. */
  async publish(): Promise<void> {
    if (this.publishing) { await this.publishing; return this.publish() }
    const send = async () => {
      while (this.live && this.context) {
        const value = JSON.stringify(this.context)
        if (value === this.published) return
        await this.bridge.call("shell.context", { ...this.context })
        this.published = value
      }
    }
    this.publishing = send()
    try { await this.publishing } finally { this.publishing = null }
  }

  private waitFor(test: () => boolean): Promise<void> {
    return new Promise((resolve, reject) => {
      const finish = (error?: Error) => {
        clearTimeout(timer); this.waiters.delete(wake)
        if (error) reject(error); else resolve()
      }
      const wake = () => {
        if (!this.live) { finish(new Error("Navigation cancelled")); return }
        try { if (test()) finish() } catch (e) { finish(e instanceof Error ? e : new Error("Navigation failed")) }
      }
      const timer = setTimeout(() => finish(new Error("Navigation did not become ready")), NAVIGATION_TIMEOUT_MS)
      this.waiters.add(wake); wake()
    })
  }

  /** Public for deterministic bridge tests; production uses the non-overlapping low-cadence scheduler. */
  async poll(): Promise<void> {
    if (!this.live || this.polling || !this.context) return
    this.polling = true
    const revision = this.revision, auth = this.authGeneration
    let intent: ShellNavigation | null = null
    try {
      await this.publish()
      // Native menus may ask to leave while the app is loading/offline. No context/ready
      // fallback may turn an attached editor's silence into approval.
      if (HUB_SWITCH_OPERATIONS.every((op) => this.bridge.can.includes(op))) {
        const request: unknown = await this.bridge.call("shell.hub-switch.next")
        if (!this.live || revision !== this.revision) return
        if (request != null) {
          if (typeof request !== "object" || !("id" in request) || typeof request.id !== "string" ||
              !("hubID" in request) || typeof request.hubID !== "string") return
          const approved = this.port.allowed()
          if (!this.live || revision !== this.revision) return
          await this.bridge.call("shell.hub-switch.ack", { id: request.id, approved })
          return
        }
      }
      if (!this.context?.ready) return
      const raw: unknown = await this.bridge.call("shell.navigation.next")
      if (!this.live || revision !== this.revision) return // Late response from a different workspace/login/readiness.
      if (raw == null) return
      if (typeof raw !== "object" || !("id" in raw) || typeof raw.id !== "string") return
      intent = raw as ShellNavigation
      const route = shellRoute(intent.route)
      if (!route || typeof intent.hubID !== "string" || (intent.workspaceID !== undefined && typeof intent.workspaceID !== "string")) throw new Error("Unsupported navigation target")
      if (intent.targetID !== undefined && (!this.port.landTarget || !attentionTargetRoute(route, intent.targetID))) throw new Error("Unsupported deep navigation target")
      const hubs = await this.bridge.call("hubs.list")
      if (!this.live || revision !== this.revision) return
      if (hubs?.hubs?.find((h: { current?: boolean }) => h.current)?.id !== intent.hubID) throw new Error("Navigation belongs to another Hub")
      const addingHub = route === "#/hub/add"
      if (!this.context!.authenticated && !addingHub) throw new Error("Sign in before opening this destination")
      const workspace = intent.workspaceID ?? this.context!.workspaceID
      if ((!workspace && !addingHub) || (workspace && !this.port.hasWorkspace(workspace))) throw new Error("Workspace is not available")
      const active = () => {
        if (!this.live || auth !== this.authGeneration || (!addingHub && !this.context?.authenticated)) throw new Error("Navigation scope changed")
      }
      if (workspace && workspace !== this.context!.workspaceID) {
        // Selection itself clears/unmounts the current editor: ask before that side effect.
        active()
        if (!this.port.allowed()) throw new Error("Navigation is blocked by an active edit")
        if (!await this.port.selectWorkspace(workspace)) throw new Error("Workspace could not be selected")
        await this.waitFor(() => { active(); return this.context?.workspaceID === workspace && this.context.ready })
      }
      active()
      if (!this.context?.ready || this.context.workspaceID !== workspace) throw new Error("Workspace is not ready")
      if (!this.port.allowed()) throw new Error("Navigation is blocked by an active edit")
      this.port.navigate(route)
      await this.waitFor(() => {
        active()
        if (this.context?.workspaceID !== workspace || !this.context.ready) throw new Error("Workspace changed during navigation")
        return route.startsWith("#/hub/") ? this.context.route === "#/" && this.port.actionOpen(route) : this.context.route === shellDestination(route)
      })
      if (intent.targetID !== undefined) {
        const controller = new AbortController(); this.landing = controller
        const stillHere = () => this.live && !controller.signal.aborted && auth === this.authGeneration
          && !!this.context?.authenticated && this.context.ready && this.context.workspaceID === workspace
          && this.context.route === shellDestination(route)
        await this.port.landTarget!(route, intent.targetID, workspace!, controller.signal, stillHere)
        if (!stillHere()) throw new Error("Navigation target changed")
      }
      await this.publish()
      active()
      if (!this.context?.ready || this.context.workspaceID !== workspace || (route.startsWith("#/hub/") ? !this.port.actionOpen(route) : this.context.route !== shellDestination(route))) throw new Error("Navigation target changed")
      await this.bridge.call("shell.navigation.ack", { id: intent.id, ok: true })
    } catch (e) {
      // A stale document/account must not acknowledge an intent issued to its replacement.
      if (intent && this.live && auth === this.authGeneration) {
        await this.bridge.call("shell.navigation.ack", { id: intent.id, ok: false, error: e instanceof Error ? e.message : "Navigation failed" }).catch(() => {})
      }
    } finally {
      this.landing?.abort(); this.landing = undefined
      this.polling = false
      this.schedule()
    }
  }

  stop() {
    this.live = false
    this.landing?.abort()
    if (this.timer) clearTimeout(this.timer)
    this.waiters.forEach((wake) => wake())
  }
}
