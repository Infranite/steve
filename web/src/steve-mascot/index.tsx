/**
 * React entry point for Steve's charcoal mascot.
 * - Static (`live={false}`): synchronous cached SVG with zero hidden DOM or animation loop.
 * - Live (`live={true}`): pointer-following, cue-bus reactive, zero-CPU idle mascot.
 */

import { useEffect, useRef, useSyncExternalStore } from "react"
import { cn } from "cn"
import { onCue, type HeroCue } from "@/bus"
import { reducedMotion as isReducedMotion } from "@/lib/motion"
import { SteveMascotController } from "./controller.ts"
import { renderStaticSvg } from "./static-svg.ts"
import type { ThemeMode } from "./types.ts"

export { getMascotMetrics } from "./scheduler.ts"
export { clearStaticCache, getStaticCacheStats, renderAdaptiveFaviconSvg, renderStaticSvg } from "./static-svg.ts"
export type { MascotMetrics, SustainedState, ThemeMode } from "./types.ts"

interface EnvSnapshot {
  readonly theme: ThemeMode
  readonly reduced: boolean
}

let envListeners = new Set<() => void>()
let envObserver: MutationObserver | null = null
let envMedia: MediaQueryList | null = null
let cachedEnv: EnvSnapshot = readEnv()

function readEnv(): EnvSnapshot {
  const dark = typeof document !== "undefined" && document.documentElement.classList.contains("dark")
  return { theme: dark ? "dark" : "light", reduced: isReducedMotion() }
}

function notifyEnv(): void {
  const next = readEnv()
  if (next.theme === cachedEnv.theme && next.reduced === cachedEnv.reduced) return
  cachedEnv = next
  for (const fn of envListeners) fn()
}

function subscribeEnv(cb: () => void): () => void {
  envListeners.add(cb)
  if (typeof document !== "undefined" && !envObserver) {
    envObserver = new MutationObserver(notifyEnv)
    envObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] })
  }
  if (typeof window !== "undefined" && !envMedia && window.matchMedia) {
    envMedia = window.matchMedia("(prefers-reduced-motion: reduce)")
    envMedia.addEventListener("change", notifyEnv)
  }
  return () => {
    envListeners.delete(cb)
    if (envListeners.size === 0) {
      envObserver?.disconnect()
      envObserver = null
      envMedia?.removeEventListener("change", notifyEnv)
      envMedia = null
    }
  }
}

function getEnvSnapshot(): EnvSnapshot {
  const cur = readEnv()
  if (cur.theme !== cachedEnv.theme || cur.reduced !== cachedEnv.reduced) {
    cachedEnv = cur
  }
  return cachedEnv
}

function getServerEnvSnapshot(): EnvSnapshot {
  return { theme: "light", reduced: false }
}

function LiveSteveMascot({ size, className }: { size: number; className?: string }) {
  const env = useSyncExternalStore(subscribeEnv, getEnvSnapshot, getServerEnvSnapshot)
  const hostRef = useRef<HTMLSpanElement>(null)
  const ctrlRef = useRef<SteveMascotController | null>(null)
  const initialRef = useRef({ size, theme: env.theme, reduced: env.reduced })

  useEffect(() => {
    const host = hostRef.current
    if (!host) return
    const init = initialRef.current
    const ctrl = new SteveMascotController({
      container: host,
      size: init.size,
      theme: init.theme,
      reducedMotion: init.reduced,
    })
    ctrlRef.current = ctrl
    return () => {
      ctrl.dispose()
      ctrlRef.current = null
    }
  }, [])

  useEffect(() => {
    ctrlRef.current?.setSize(size)
  }, [size])

  useEffect(() => {
    ctrlRef.current?.setTheme(env.theme)
  }, [env.theme])

  useEffect(() => {
    ctrlRef.current?.setReducedMotion(env.reduced)
  }, [env.reduced])

  useEffect(() => {
    return onCue((c: HeroCue) => {
      ctrlRef.current?.applyCue(c)
    })
  }, [])

  return (
    <span
      ref={hostRef}
      aria-hidden
      className={cn("inline-flex shrink-0 select-none [&>svg]:block [&>svg]:size-full", className)}
      style={{ width: size, height: size }}
    />
  )
}

function StaticSteveMascot({ size, className }: { size: number; className?: string }) {
  const { theme } = useSyncExternalStore(subscribeEnv, getEnvSnapshot, getServerEnvSnapshot)
  const markup = renderStaticSvg({ size, theme })
  return (
    <span
      aria-hidden
      className={cn("inline-flex shrink-0 select-none [&>svg]:block [&>svg]:size-full", className)}
      style={{ width: size, height: size }}
      dangerouslySetInnerHTML={{ __html: markup }}
    />
  )
}

export function SteveMascot({
  size = 24,
  live = false,
  className,
}: {
  size?: number
  live?: boolean
  className?: string
}) {
  return live ? (
    <LiveSteveMascot size={size} className={className} />
  ) : (
    <StaticSteveMascot size={size} className={className} />
  )
}
