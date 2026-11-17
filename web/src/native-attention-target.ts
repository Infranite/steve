import { attentionApi } from "@/api/attention"
import { fillTo, jumpToMsg, threadMsg } from "@/thread/history"

function waitForElement(selector: string, signal: AbortSignal, active: () => boolean): Promise<HTMLElement> {
  return new Promise((resolve, reject) => {
    let observer: MutationObserver | undefined
    const finish = (error?: Error, element?: HTMLElement) => {
      observer?.disconnect(); clearTimeout(timeout); signal.removeEventListener("abort", abort)
      if (error) reject(error); else resolve(element!)
    }
    const abort = () => finish(new Error("Navigation cancelled"))
    const check = () => {
      if (signal.aborted || !active()) { abort(); return }
      const element = document.querySelector<HTMLElement>(selector)
      if (element) finish(undefined, element)
    }
    const timeout = setTimeout(() => finish(new Error("Target did not become available")), 12_000)
    observer = new MutationObserver(check); observer.observe(document.body, { childList: true, subtree: true, attributes: true })
    signal.addEventListener("abort", abort, { once: true }); check()
  })
}

/** Landing checks are read-only navigation. No read, approve, send or arbitrary URL command is issued here. */
export async function landAttentionTarget(route: string, targetID: string, workspace: string, signal: AbortSignal, active: () => boolean) {
  if (!targetID) return
  const escaped = CSS.escape(targetID)
  if (route.startsWith("#/feed")) {
    const row = await waitForElement(`[data-feed-target="${escaped}"] [data-feed-body][data-feed-ready=true]`, signal, active)
    row.scrollIntoView({ block: "start" }); return
  }
  if (route === "#/inbox") {
    const card = await waitForElement(`[data-decision-id="${escaped}"]`, signal, active)
    card.scrollIntoView({ block: "center" }); return
  }
  if (route.startsWith("#/c/")) {
    await waitForElement(`[data-chat-conversation="${escaped}"][data-chat-conversation-ready="true"]`, signal, active)
    return
  }
  if (route !== "#/") throw new Error("Unsupported attention target")
  const reply = await attentionApi.forWorkspace(workspace).reply(targetID, signal)
  if (!active() || signal.aborted) throw new Error("Navigation cancelled")
  if (reply.id !== targetID) throw new Error("Reply identity mismatch")
  if (!threadMsg(targetID)) await fillTo(reply.created, signal)
  if (!active() || signal.aborted) throw new Error("Navigation cancelled")
  await waitForElement(`[data-msg="${escaped}"]`, signal, active)
  jumpToMsg(targetID, reply.created)
  await waitForElement(`[data-msg="${escaped}"][data-flash]`, signal, active)
}
