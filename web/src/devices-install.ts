/* A remote Node install (A6b) as the steps of a trail: what each step is doing right now, how far it is (bytes and
 * speed while fetching or sending, the phase of the setup on the target), how long it took, and where it failed.
 * Everything comes from the install the Hub keeps, so a page opened later shows the same. */
import type { RInstall, RInstallKind, RInstallProgress, RInstallStep } from "@/api"

type T = (key: string, options?: Record<string, unknown>) => string

/** the steps of every install, in order (the Hub's last, `done`, is the trail's end, not a step) */
export const KINDS: Exclude<RInstallKind, "done">[] = ["probe", "fetch", "send", "setup"]
/** what `steve setup node` goes through on the target */
export const SETUP_PHASES = ["install", "pair", "adapters", "start"] as const

export type InstallSubstep = { id: string; title: string; state: "done" | "running" | "pending"; detail?: string }
export type InstallTrailStep = {
  id: string
  title: string
  state: "done" | "running" | "pending" | "failed"
  /** ms, for a step that ended */
  duration?: number
  /** what a running step is doing, with its numbers */
  status?: string
  /** when a running step began, on this browser's clock */
  startedAt?: number
  substeps?: InstallSubstep[]
  /** 0..100, for a running step that moves a known number of bytes */
  percent?: number
}

/** 1.5 KB, 12.3 MB: one decimal under 100 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return ""
  const units = ["B", "KB", "MB", "GB"]
  let v = n, i = 0
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++ }
  return `${i === 0 || v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[i]}`
}

/** 4 s, 1:05, 1:02:03 */
export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s} s`
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = String(s % 60).padStart(2, "0")
  return h ? `${h}:${String(m).padStart(2, "0")}:${r}` : `${m}:${r}`
}

const percentOf = (p: RInstallProgress) => (p.total ? Math.min(100, Math.round((p.done / p.total) * 100)) : undefined)

/** "12.3 MB / 40.1 MB · 2.1 MB/s" */
export function bytesLine(p: RInstallProgress, t: T): string {
  const amount = p.total ? t("shell:devices.install.of", { done: formatBytes(p.done), total: formatBytes(p.total) }) : formatBytes(p.done)
  return p.rate ? `${amount} · ${formatBytes(p.rate)}/s` : amount
}

/** what a running step says it is doing */
export function statusOf(kind: RInstallKind, p: RInstallProgress | null | undefined, t: T): string {
  if (!p) return t("shell:devices.install.working")
  if (kind === "fetch") {
    if (p.phase === "download") return bytesLine(p, t)
    if (p.phase === "cached") return t("shell:devices.install.phase.cached")
    return t("shell:devices.install.phase.verifying")
  }
  if (kind === "send") return bytesLine(p, t)
  if (kind === "setup") return t(`shell:devices.install.phase.${SETUP_PHASES.includes(p.phase as never) ? p.phase : "install"}`)
  return t("shell:devices.install.working")
}

function substepsOf(s: RInstallStep, running: boolean, t: T): InstallSubstep[] | undefined {
  const p = s.progress
  const state = (i: number, at: number): InstallSubstep["state"] => (!running || i < at ? "done" : i === at ? "running" : "pending")
  if (s.kind === "fetch") {
    if (p?.phase === "cached") return [{ id: "cached", title: t("shell:devices.install.phase.cached"), state: state(0, 0), detail: p.total ? formatBytes(p.total) : undefined }]
    const at = p?.phase === "verify" ? 1 : 0
    return [
      { id: "download", title: t("shell:devices.install.phase.download"), state: state(0, at), detail: p ? (p.phase === "download" ? bytesLine(p, t) : p.total ? formatBytes(p.total) : undefined) : undefined },
      { id: "verify", title: t("shell:devices.install.phase.verify"), state: state(1, at) },
    ]
  }
  if (s.kind === "setup") {
    const at = Math.max(0, SETUP_PHASES.indexOf((p?.phase ?? "install") as never))
    return SETUP_PHASES.map((phase, i) => ({ id: phase, title: t(`shell:devices.install.phase.${phase}`), state: state(i, at) }))
  }
  return undefined
}

/** the trail of `op`, fetched at `receivedAt` (this browser's clock); steps not reached yet are pending */
export function installTrail(op: RInstall, receivedAt: number, t: T): InstallTrailStep[] {
  const reached = op.steps.filter(s => s.kind !== "done")
  const out: InstallTrailStep[] = reached.map((s, i) => {
    const last = i === reached.length - 1
    const running = last && op.state === "running"
    const failed = last && op.state === "failed"
    const elapsed = s.elapsed_ms ?? 0
    const step: InstallTrailStep = {
      id: s.kind,
      title: s.text || t(`shell:devices.install.kind.${s.kind}`),
      state: running ? "running" : failed ? "failed" : "done",
      substeps: substepsOf(s, running || failed, t),
    }
    if (running) {
      step.status = statusOf(s.kind, s.progress, t)
      step.startedAt = receivedAt - elapsed
      const pct = s.progress && (s.progress.phase === "download" || s.progress.phase === "send") ? percentOf(s.progress) : undefined
      if (pct !== undefined) step.percent = pct
    } else {
      step.duration = elapsed
    }
    return step
  })
  if (op.state === "running" || op.state === "failed" || op.state === "waiting")
    for (const kind of KINDS.slice(reached.length))
      out.push({ id: kind, title: t(`shell:devices.install.kind.${kind}`), state: "pending" })
  return out
}

/** where and why it failed: "Failed at “Sending …” after 1:14" */
export function failureOf(op: RInstall, t: T): { step: string; elapsed: string; error: string } | null {
  if (op.state !== "failed") return null
  const s = [...op.steps].reverse().find(x => x.kind === op.failed_step) ?? op.steps[op.steps.length - 1]
  return {
    step: s ? s.text || t(`shell:devices.install.kind.${s.kind}`) : "",
    elapsed: s ? formatElapsed(s.elapsed_ms ?? 0) : "",
    error: op.error ?? "",
  }
}

/** the install a page opened later shows: one that runs, else the newest one not yet done */
export function resumable(ops: RInstall[]): RInstall | null {
  return ops.find(o => o.state === "running") ?? (ops[0] && ops[0].state !== "done" ? ops[0] : null)
}
