import { useEffect, useRef } from "react"
import { hubNavigationAllowed, nativeBridge } from "@/hub"
import { NativeShellSession, shellBridge, type ShellContext } from "@/native-shell"
import { landAttentionTarget } from "@/native-attention-target"
import { AttentionVisibilityPublisher } from "@/native-attention-visible"
import { st, switchWorkspace } from "@/store"
import { getSheet, go } from "@/ui"

/** App-level lifecycle: keyed pages can unmount without disposing pending navigation or focus listeners. */
export function useNativeShell(context: Omit<ShellContext, "visible" | "focused">, addHubVisible = false) {
  const session = useRef<NativeShellSession | null>(null)
  const attention = useRef<AttentionVisibilityPublisher | null>(null)
  const latest = useRef(context)
  const committedSheet = useRef(getSheet())
  const addHub = useRef(addHubVisible)
  const sheet = getSheet()
  useEffect(() => {
    latest.current = context
    committedSheet.current = sheet
    addHub.current = addHubVisible
    const value = { ...context, focused: document.hasFocus(), visible: document.visibilityState === "visible" }
    session.current?.update(value)
    attention.current?.update({ ...value, ready: value.ready && !sheet && !addHubVisible })
  }, [context.workspaceID, context.workspaceName, context.route, context.title, context.ready, context.authenticated, sheet, addHubVisible])
  useEffect(() => {
    const bridge = shellBridge(nativeBridge())
    if (!bridge) return
    // Native's legacy fallback tests this marker after document load, never a stream-dependent state.
    window.__STEVE_SHELL_CONTEXT__ = true
    const publisher = bridge.can.includes("shell.attention.visible") ? new AttentionVisibilityPublisher(bridge, async () => { await native.publish() }) : null
    attention.current = publisher
    const native = new NativeShellSession(bridge, {
      landTarget: publisher ? async (route, id, workspace, signal, active) => {
        await landAttentionTarget(route, id, workspace, signal, active)
        if (!active()) throw new Error("Navigation scope changed")
        await native.publish()
        await publisher.flush()
      } : undefined,
      allowed: hubNavigationAllowed,
      hasWorkspace: (id) => st.workspaces.some((w) => w.id === id),
      selectWorkspace: switchWorkspace,
      navigate: go,
      actionOpen: (route) => route === "#/hub/add"
        ? addHub.current || (committedSheet.current?.kind === "hubs" && committedSheet.current.payload?.add === true)
        : route === "#/hub/pair" && committedSheet.current?.kind === "pairClient",
    })
    session.current = native
    const visibility = () => {
      const value = { ...latest.current, focused: document.hasFocus(), visible: document.visibilityState === "visible" }
      native.update(value); publisher?.update({ ...value, ready: value.ready && !committedSheet.current && !addHub.current })
    }
    visibility()
    const invalidate = () => { native.invalidate(); publisher?.invalidate() }
    window.addEventListener("steve:shell-invalidated", invalidate)
    window.addEventListener("focus", visibility)
    window.addEventListener("blur", visibility)
    document.addEventListener("visibilitychange", visibility)
    return () => {
      window.removeEventListener("steve:shell-invalidated", invalidate)
      native.stop(); publisher?.stop()
      session.current = null; attention.current = null
      window.__STEVE_SHELL_CONTEXT__ = false
      window.removeEventListener("focus", visibility)
      window.removeEventListener("blur", visibility)
      document.removeEventListener("visibilitychange", visibility)
    }
  }, [])
}

declare global { interface Window { __STEVE_SHELL_CONTEXT__?: boolean } }
