import { useEffect, type RefObject } from "react"
import { isNotWired } from "@/api"
import { attentionApi } from "@/api/attention"
import { currentHub } from "@/hub"
import { st } from "@/store"
import { replySnapshot, visibleReply, visibleReplyIDs } from "@/thread/attention-read"

/** Reading a final reply is personal state; opening the window or receiving its notification is not. */
export function useReplyReading(scroller: RefObject<HTMLDivElement | null>, conversation?: string) {
  const workspace = st.workspace?.id, hub = currentHub(), ready = st.ready, login = st.login
  useEffect(() => {
    const root = scroller.current
    if (conversation || !workspace || !ready || login || !root) return
    const controller = new AbortController(), client = attentionApi.forWorkspace(workspace)
    let snapshot: Awaited<ReturnType<typeof replySnapshot>> | undefined, fetched = 0, busy = false, unsupported = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const active = () => !controller.signal.aborted && st.ready && !st.login && st.workspace?.id === workspace && currentHub() === hub
    const viewing = () => active() && document.visibilityState === "visible" && document.hasFocus()
    const check = async () => {
      if (busy || unsupported || !viewing()) return
      busy = true
      try {
        if (!snapshot || Date.now() - fetched >= 15_000) {
          snapshot = await replySnapshot(client, workspace, controller.signal)
          fetched = Date.now()
        }
        if (!viewing()) return
        const through = visibleReply(snapshot.entries, visibleReplyIDs(root))
        if (!through) return
        await client.read(snapshot.snapshot, through.target.entity_id, controller.signal)
        if (!active()) return
        snapshot.entries = snapshot.entries.filter((e) => e.order === undefined || e.order > through.order!)
        window.dispatchEvent(new Event("steve-attention-read"))
      } catch (e) {
        if (isNotWired(e)) unsupported = true
        // Failed/expired snapshots retry on the next visible check, without clearing an unread count.
        snapshot = undefined
      } finally { busy = false }
    }
    const later = () => { clearTimeout(timer); timer = setTimeout(() => void check(), 200) }
    root.addEventListener("scroll", later, { passive: true })
    document.addEventListener("visibilitychange", later); window.addEventListener("focus", later)
    const observer = new MutationObserver(later); observer.observe(root, { childList: true, subtree: true })
    const interval = setInterval(() => void check(), 15_000)
    later()
    return () => {
      controller.abort(); clearTimeout(timer); clearInterval(interval); observer.disconnect()
      root.removeEventListener("scroll", later); document.removeEventListener("visibilitychange", later); window.removeEventListener("focus", later)
    }
  }, [workspace, hub, ready, login, conversation, scroller])
}
