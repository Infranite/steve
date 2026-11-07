// What finishing an upgrade takes, per installation. node --test loads this directly, so imports carry .ts extensions.
import { t } from "./i18n/index.ts"
import type { UpdateOperation, UpdateProgress, UpdateState } from "@/updates"

/** How the Hub was installed (Hub `upgrades/install.rs`). Paths are only sent to the instance manager. */
export type InstallKind = "managed" | "local_release" | "release_script" | "source" | "external"
export type Install = { kind: InstallKind; home?: string; release?: string | null; repo?: string | null }
export type GuideStep = { title: string; command?: string; note?: string }
export type Guide = { label: string; summary: string; steps: GuideStep[]; backup: string; recovery: string[] }

/** the name of an installation kind, in the UI language */
export const installLabel = (kind: InstallKind) => t(`pages:guide.install.${kind}`)

/** What it takes before Settings · 关于 can upgrade this installation itself (a local_release install says
 * what it lacks in the Hub's reason). */
export const enableOneClick = (kind: InstallKind): string | undefined =>
  kind === "release_script" ? t(`pages:guide.enable.${kind}`) : undefined

const sh = (s: string) => /^[\w@%+=:,./~-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`

/** The release package carries the same read-only maintenance entry when the Hub is unavailable. */
export const serviceCommands = (home: string) => ["status", "doctor"].map(action =>
  `python3 ${sh(`${home}/current/ops/steve-service.py`)} hub ${action} --home ${sh(home)}`)

/** Commands for an installation Steve does not switch itself, with this machine's paths filled in. */
export function upgradeGuide(install: Install, version: string): Guide {
  const v = version.replace(/^v/, ""), tag = `v${v}`
  const home = install.home ? sh(install.home) : "~/.steve"
  const repo = install.repo ? sh(install.repo) : t("pages:guide.repoPlaceholder")
  const verify: GuideStep = { title: t("pages:guide.verify.title"), note: t("pages:guide.verify.note", { v }) }
  switch (install.kind) {
    case "local_release": return {
      label: t("pages:guide.local.label"),
      summary: t("pages:guide.local.summary"),
      steps: [
        { title: t("pages:guide.local.prepare"), note: t("pages:guide.local.prepareNote") },
        { title: t("pages:guide.local.deploy"), note: t("pages:guide.local.deployNote") },
        verify,
      ],
      backup: t("pages:guide.local.backup", { home, file: `${home}/logs/release/local-${v}-${t("pages:guide.timePlaceholder")}.json` }),
      recovery: [
        t("pages:guide.local.before"),
        t("pages:guide.local.after", { home }),
      ],
    }
    case "release_script": return {
      label: t("pages:guide.script.label"),
      summary: t("pages:guide.script.summary"),
      steps: [
        { title: t("pages:guide.script.main"), command: `git -C ${repo} checkout main && git -C ${repo} pull --ff-only --tags origin main`, note: t("pages:guide.script.mainNote", { command: `git -C ${repo} merge-base --is-ancestor ${tag} main`, tag }) },
        { title: t("pages:guide.script.release"), command: `zsh ${repo}/ops/release.sh main`, note: t("pages:guide.script.releaseNote") },
        verify,
      ],
      backup: t("pages:guide.script.backup", { home }),
      recovery: [
        t("pages:guide.script.health", { home }),
        t("pages:guide.script.manual", { home, command: `zsh ${repo}/ops/release.sh --rollback` }),
      ],
    }
    case "source": return {
      label: t("pages:guide.source.label"),
      summary: t("pages:guide.source.summary"),
      steps: [
        { title: t("pages:guide.source.backup"), command: `mkdir -p ${home}/backups && sqlite3 ${home}/data/steve.sqlite3 ".backup ${home}/backups/before-${v}.sqlite3"`, note: t("pages:guide.source.backupNote") },
        { title: t("pages:guide.source.checkout"), command: `git -C ${repo} fetch --tags origin && git -C ${repo} checkout ${tag}`, note: t("pages:guide.source.checkoutNote") },
        { title: t("pages:guide.source.build"), command: `cd ${repo} && npm ci && npm -w web run build && cargo build --release --manifest-path hub-rs/Cargo.toml -p steve-hub` },
        { title: t("pages:guide.source.restart"), note: t("pages:guide.source.restartNote") },
        verify,
      ],
      backup: t("pages:guide.source.backupText", { file: `${home}/backups/before-${v}.sqlite3` }),
      recovery: [t("pages:guide.source.recovery", { file: `${home}/backups/before-${v}.sqlite3`, db: `${home}/data/steve.sqlite3` })],
    }
    default: return {
      label: t("pages:guide.external.label"),
      summary: t("pages:guide.external.summary"),
      steps: [
        { title: t("pages:guide.external.backup"), note: t("pages:guide.external.backupNote", { dir: `${home}/data` }) },
        { title: t("pages:guide.external.deploy", { tag }), note: t("pages:guide.external.deployNote") },
        { title: t("pages:guide.external.restart"), note: t("pages:guide.external.restartNote") },
        verify,
      ],
      backup: t("pages:guide.external.backupText"),
      recovery: [t("pages:guide.external.recovery")],
    }
  }
}

/** The upgrade journal's phases (the managed supervisor's, or a local build's runner), as steps a person can follow. */
const stage = <K extends string, P extends readonly string[]>(key: K, phases: P) => ({ key, phases, get label() { return t(`pages:guide.stage.${key}` as "pages:guide.stage.staging") } })
export const upgradeStages = [
  stage("staging", ["staging", "ready"] as const),
  stage("draining", ["draining"] as const),
  stage("stopping", ["stopping"] as const),
  stage("backingUp", ["backing_up"] as const),
  stage("restarting", ["restarting"] as const),
  stage("verifying", ["verifying", "committed"] as const),
  stage("succeeded", ["succeeded"] as const),
] as const
/** Old local_release receipts really did compile; only their history keeps that label. */
export const stageLabel = (key: (typeof upgradeStages)[number]["key"], runnerKind?: string) =>
  key === "staging" && runnerKind === "local_release" ? t("pages:guide.stage.stagingSource") : upgradeStages.find(s => s.key === key)!.label
export const stageIndex = (phase: string) => upgradeStages.findIndex(s => (s.phases as readonly string[]).includes(phase))

/** Only the measured download has a percentage. Missing or inconsistent byte counts stay unknown. */
export function downloadPercent(progress?: UpdateProgress): number | undefined {
  const d = progress?.step === "downloading" ? progress.download : undefined
  if (!d || !Number.isSafeInteger(d.received_bytes) || !Number.isSafeInteger(d.total_bytes)
    || d.total_bytes <= 0 || d.received_bytes < 0 || d.received_bytes > d.total_bytes) return undefined
  return Math.floor(d.received_bytes / d.total_bytes * 100)
}

export type Health = { ok?: boolean; version?: string; web_version?: string; drain?: boolean | null; recovery?: { state?: string } | null }
export type VerifyItem = { key: "health" | "hub" | "web" | "page"; label: string; ok: boolean; detail: string }
const same = (a = "", b = "") => !!a && a.replace(/^v/, "") === b.replace(/^v/, "")

/** After the restart: what the Hub reports now, against the version that was installed. */
export function verifyUpgrade(target: string, health: Health | null, loadedWeb: string): { items: VerifyItem[]; done: boolean; needsReload: boolean } {
  const v = target.replace(/^v/, "")
  const healthy = !!health?.ok && health.drain !== true && health.recovery?.state !== "failed"
  const c = (k: string) => t(`pages:guide.check.${k}` as "pages:guide.check.ok")
  const items: VerifyItem[] = [
    { key: "health", label: c("health"), ok: healthy, detail: !health ? c("down") : !health.ok ? c("failed") : health.drain ? c("drain") : health.recovery?.state === "failed" ? c("recoveryFailed") : health.recovery?.state === "partial" ? c("partial") : c("ok") },
    { key: "hub", label: c("hub"), ok: same(health?.version, v), detail: health?.version || c("unknown") },
    { key: "web", label: c("web"), ok: same(health?.web_version, v), detail: health?.web_version || c("unknown") },
    { key: "page", label: c("page"), ok: same(loadedWeb, v), detail: loadedWeb || c("unknown") },
  ]
  const served = items.slice(0, 3).every(i => i.ok)
  return { items, done: items.every(i => i.ok), needsReload: served && !items[3].ok }
}

/** A version check older than this is repeated when the instance manager opens the page (the Hub caches it 10 minutes). */
export const CHECK_STALE_MS = 3600_000
export const checkStale = (state: Pick<UpdateState, "capability" | "check" | "operation">, now = Date.now()) =>
  state.capability.owner === true && !(state.operation && !["succeeded", "rolled_back", "failed"].includes(state.operation.phase)) && (!state.check || now - state.check.checked_at > CHECK_STALE_MS)
/** A failure after the switch: the new version stays (supervisor and ops/local-release.py both say so). */
export const committedFailure = (op: UpdateOperation) => op.phase === "failed" && !!op.error && /升级已提交|Upgrade committed/.test(op.error)
