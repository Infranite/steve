import type { NativeBridge } from "./hub"
import type { ShellContext } from "./native-shell"

export type VisibleAttentionTarget = { destination: "reply" | "pending" | "feed" | "channel"; entityID: string }

/** Intersect every scroll/clipping ancestor, not merely the browser viewport. */
export function onScreen(element: HTMLElement): boolean {
  let left = 0, top = 0, right = window.innerWidth, bottom = window.innerHeight
  const box = element.getBoundingClientRect()
  if (!box.width || !box.height) return false
  for (let parent: HTMLElement | null = element; parent; parent = parent.parentElement) {
    const style = getComputedStyle(parent)
    if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse" || style.opacity === "0") return false
    if (parent !== element) {
      const rect = parent.getBoundingClientRect()
      if (/(auto|scroll|hidden|clip)/.test(style.overflowX)) { left = Math.max(left, rect.left); right = Math.min(right, rect.right) }
      if (/(auto|scroll|hidden|clip)/.test(style.overflowY)) { top = Math.max(top, rect.top); bottom = Math.min(bottom, rect.bottom) }
    }
  }
  return right > left && bottom > top && box.right > left && box.left < right && box.bottom > top && box.top < bottom
}

export function visibleAttentionTargets(route: string): VisibleAttentionTarget[] {
  const targets = new Map<string, VisibleAttentionTarget>()
  const add = (destination: VisibleAttentionTarget["destination"], entityID: string | undefined, element: HTMLElement) => {
    if (entityID && onScreen(element)) targets.set(JSON.stringify([destination, entityID]), { destination, entityID })
  }
  if (route === "#/") for (const el of document.querySelectorAll<HTMLElement>("[data-attention-reply]")) add("reply", el.dataset.attentionReply, el)
  if (route === "#/inbox") for (const el of document.querySelectorAll<HTMLElement>("[data-decision-id]")) add("pending", el.dataset.decisionId, el)
  if (route.split("?")[0] === "#/feed") for (const el of document.querySelectorAll<HTMLElement>("[data-feed-entry] [data-feed-body][data-feed-ready=true]")) add("feed", el.closest<HTMLElement>("[data-feed-entry]")?.dataset.feedEntry, el)
  if (route.startsWith("#/c/")) for (const el of document.querySelectorAll<HTMLElement>("[data-chat-conversation][data-chat-conversation-ready=true]")) add("channel", el.dataset.chatConversation, el)
  // Native intentionally rejects oversized reports. Fail open for banners rather than hiding arbitrary targets.
  return targets.size <= 256 ? [...targets.values()] : []
}

/** Only the live trusted document publishes. No bodies, counts, account IDs or endpoints cross this bridge. */
export class AttentionVisibilityPublisher {
  private context: ShellContext | null = null
  private live = true
  private sent = ""
  private sending: Promise<void> | undefined
  private timer: ReturnType<typeof setTimeout> | undefined
  private observer: MutationObserver
  private bridge: NativeBridge
  private prepare: () => Promise<void>
  constructor(bridge: NativeBridge, prepare: () => Promise<void> = async () => {}) {
    this.bridge = bridge; this.prepare = prepare
    this.observer = new MutationObserver(this.later)
    this.observer.observe(document.body, { childList: true, subtree: true, attributes: true })
    document.addEventListener("scroll", this.later, true)
    document.addEventListener("visibilitychange", this.later)
    window.addEventListener("resize", this.later)
    window.addEventListener("focus", this.later)
    window.addEventListener("blur", this.later)
  }
  update(context: ShellContext) { this.context = context; this.later() }
  invalidate() { this.sent = ""; this.later() }
  private later = () => {
    if (!this.live) return
    clearTimeout(this.timer); this.timer = setTimeout(() => { void this.flush().catch(() => {}) }, 50)
  }
  async flush(): Promise<void> {
    if (this.sending) { await this.sending; return this.flush() }
    const send = async () => {
      while (this.live && this.context) {
        // Context changes clear native visibility; publish that context before its replacement report.
        await this.prepare()
        if (!this.live || !this.context) return
        const c = this.context
        const visible = c.visible && document.visibilityState === "visible"
        const value = { workspaceID: c.workspaceID, targets: c.ready && c.authenticated && visible ? visibleAttentionTargets(c.route) : [], focused: c.focused && document.hasFocus(), visible }
        const serialized = JSON.stringify([c.route, value])
        if (serialized === this.sent) return
        await this.bridge.call("shell.attention.visible", value)
        this.sent = serialized
      }
    }
    this.sending = send()
    try { await this.sending } finally { this.sending = undefined }
  }
  stop() {
    this.live = false; clearTimeout(this.timer); this.observer.disconnect()
    document.removeEventListener("scroll", this.later, true)
    document.removeEventListener("visibilitychange", this.later)
    window.removeEventListener("resize", this.later)
    window.removeEventListener("focus", this.later)
    window.removeEventListener("blur", this.later)
  }
}
