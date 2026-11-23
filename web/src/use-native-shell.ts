import { useEffect, useRef } from "react"
import { hubNavigationAllowed, nativeBridge } from "@/hub"
import { NativeShellSession, nativeHubChrome, shellBridge, type ShellContext } from "@/native-shell"
import { landAttentionTarget } from "@/native-attention-target"
import { AttentionVisibilityPublisher } from "@/native-attention-visible"
import { st, switchWorkspace, useStoreSelector } from "@/store"
import { getSheet, getTheme, go } from "@/ui"

/** App-level lifecycle: keyed pages can unmount without disposing pending navigation or focus listeners. */
export function useNativeShell(context: Omit<ShellContext, "visible" | "focused" | "appearance">, addHubVisible = false) {
  const appearance = useRef<ShellContext["appearance"]>(undefined)
  const refreshAppearance = useRef<(() => void) | null>(null)
  const session = useRef<NativeShellSession | null>(null)
  const attention = useRef<AttentionVisibilityPublisher | null>(null)
  const latest = useRef(context)
  const committedSheet = useRef(getSheet())
  const addHub = useRef(addHubVisible)
  const sheet = useStoreSelector(getSheet)
  const themePreference = useStoreSelector(getTheme)
  useEffect(() => {
    latest.current = context
    refreshAppearance.current?.()
    committedSheet.current = sheet
    addHub.current = addHubVisible
    const value = { ...context, ...(appearance.current ? { appearance: appearance.current } : {}), focused: document.hasFocus(), visible: document.visibilityState === "visible" }
    session.current?.update(value)
    attention.current?.update({ ...value, ready: value.ready && !sheet && !addHubVisible })
  }, [context.workspaceID, context.workspaceName, context.route, context.title, context.ready, context.authenticated, context.language, sheet, addHubVisible, themePreference])
  useEffect(() => {
    const bridge = shellBridge(nativeBridge())
    if (!bridge) return
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
        : route === "#/hub/pair" && committedSheet.current?.kind === "addDevice" && committedSheet.current.payload?.kind === "client",
    })
    session.current = native
    const value = () => ({ ...latest.current, ...(appearance.current ? { appearance: appearance.current } : {}), focused: document.hasFocus(), visible: document.visibilityState === "visible" })
    const visibility = () => {
      refreshAppearance.current?.()
      const context = value()
      native.update(context); publisher?.update({ ...context, ready: context.ready && !committedSheet.current && !addHub.current })
    }
    let appearanceObserver: MutationObserver | undefined
    let appearanceQueued = false
    let live = true
    if (nativeHubChrome(bridge)) {
      // Canvas normalizes computed CSS colors (including user themes using oklch/color()) to sRGB.
      const canvas = document.createElement("canvas")
      canvas.width = canvas.height = 1
      const color = canvas.getContext("2d", { willReadFrequently: true })
      const readAppearance = () => {
        if (!color) return
        color.clearRect(0, 0, 1, 1)
        color.fillStyle = getComputedStyle(document.body).backgroundColor
        color.fillRect(0, 0, 1, 1)
        const [r, g, b, alpha] = color.getImageData(0, 0, 1, 1).data
        // Only claim an exact opaque match; never invent a background behind transparent Web content.
        appearance.current = alpha === 255 ? { dark: document.documentElement.classList.contains("dark"), background: [r, g, b], followSystem: getTheme() === "system" } : undefined
      }
      refreshAppearance.current = readAppearance
      readAppearance()
      appearanceObserver = new MutationObserver(() => {
        if (appearanceQueued) return
        appearanceQueued = true
        // WKWebView pauses animation frames while hidden; theme metadata must not depend on painting.
        queueMicrotask(() => {
          appearanceQueued = false
          if (!live) return
          readAppearance()
          native.update(value()) // Coalesced by metadata, never by streamed page content.
        })
      })
      for (const element of [document.documentElement, document.body]) {
        appearanceObserver.observe(element, { attributes: true, attributeFilter: ["class", "style"] })
      }
      // User-selected theme manifests replace a stylesheet without changing the route or root class.
      // Observe only head, not streamed page content.
      appearanceObserver.observe(document.head, { childList: true, characterData: true, subtree: true })
    }
    visibility()
    const invalidate = () => { native.invalidate(); publisher?.invalidate() }
    window.addEventListener("steve:shell-invalidated", invalidate)
    window.addEventListener("focus", visibility)
    window.addEventListener("blur", visibility)
    document.addEventListener("visibilitychange", visibility)
    return () => {
      live = false
      appearanceObserver?.disconnect()
      refreshAppearance.current = null
      appearance.current = undefined
      window.removeEventListener("steve:shell-invalidated", invalidate)
      native.stop(); publisher?.stop()
      session.current = null; attention.current = null
      window.removeEventListener("focus", visibility)
      window.removeEventListener("blur", visibility)
      document.removeEventListener("visibilitychange", visibility)
    }
  }, [])
}
