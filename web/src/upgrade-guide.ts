// What finishing an upgrade takes, per installation. node --test loads this directly, so imports carry .ts extensions.
import { t } from "./i18n/index.ts"
import type { UpdateOperation, UpdateProgress, UpdateState } from "@/updates"

/** How the Hub was installed (Hub `upgrades/install.rs`). Paths are only sent to the instance manager. */
export type InstallKind = "managed" | "local_release" | "legacy_macos_service" | "release_script" | "source" | "external"
export type LegacyMacosServiceKind = "run_sh" | "run_managed_sh"
export type CapabilityReasonCode = "legacy_macos_service"
export type Install = { kind: InstallKind; home?: string; release?: string | null; repo?: string | null; legacy_service?: LegacyMacosServiceKind | null }
export type GuideStep = { title: string; command?: string; note?: string }
export type Guide = { label: string; summary: string; steps: GuideStep[]; backup: string; recovery: string[] }

/** the name of an installation kind, in the UI language */
export const installLabel = (kind: InstallKind) => t(`pages:guide.install.${kind}`)

/** What it takes before Settings · 关于 can upgrade this installation itself (a local_release install says
 * what it lacks in the Hub's reason). */
export const enableOneClick = (kind: InstallKind): string | undefined =>
  kind === "release_script" ? t(`pages:guide.enable.${kind}`) : undefined

const sh = (s: string) => /^[\w@%+=:,./~-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`

/** The native CLI provides the same read-only maintenance entry when the Hub is unavailable. */
export const serviceCommands = (home: string) => ["status", "doctor"].map(action =>
  `steve hub ${action} --home ${sh(home)}`)

/** One-shot macOS migration command using only the Rust binary inside the new release package. */
export function macosOneShotUpgradeCommand(home: string, version?: string | null, repository?: string | null): string {
  const h = sh(home)
  const v = version?.trim().replace(/^v/, "")
  if (v) {
    const repo = repository?.trim() || "Infranite/steve"
    return `rm -rf /tmp/steve-upgrade && mkdir -p /tmp/steve-upgrade && curl -fsSL https://github.com/${repo}/releases/download/v${v}/steve-${v}-aarch64-apple-darwin.tar.gz | tar -xz -C /tmp/steve-upgrade && /tmp/steve-upgrade/bin/steve-hub --local-release upgrade-macos --home ${h}`
  }
  return `brew upgrade steve && "$(brew --prefix steve)/libexec/bin/steve-hub" --local-release upgrade-macos --home ${h}`
}

/** Manual rollback command restoring v0.12.0 release symlinks and original LaunchAgent plist. */
export const macosRollbackCommand = (home: string) =>
  `${sh(home)}/current/bin/steve-hub --local-release rollback-macos --home ${sh(home)}`

/** Commands for an installation Steve does not switch itself, with this machine's paths filled in. */
export function upgradeGuide(install: Install, version: string, repository?: string): Guide {
  const v = version.replace(/^v/, ""), tag = v ? `v${v}` : ""
  const rawHome = install.home || "~/.steve"
  const home = sh(rawHome)
  const repo = install.repo ? sh(install.repo) : t("pages:guide.repoPlaceholder")
  const verify: GuideStep = { title: t("pages:guide.verify.title"), note: t("pages:guide.verify.note", { v }) }
  switch (install.kind) {
    case "legacy_macos_service": return {
      label: t("pages:guide.legacyMacos.label"),
      summary: t("pages:guide.legacyMacos.summary"),
      steps: [
        {
          title: t("pages:guide.legacyMacos.upgrade"),
          command: macosOneShotUpgradeCommand(rawHome, v, repository),
          note: t("pages:guide.legacyMacos.upgradeNote"),
        },
        ...(v ? [verify] : []),
      ],
      backup: t("pages:guide.legacyMacos.backup", { home }),
      recovery: [
        t("pages:guide.legacyMacos.autoRollback"),
        t("pages:guide.legacyMacos.manualRollback", { command: macosRollbackCommand(rawHome) }),
      ],
    }
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
        { title: t("pages:guide.script.main"), command: `steve hub doctor --home ${home}`, note: t("pages:guide.script.mainNote", { tag }) },
        { title: t("pages:guide.script.release"), command: `steve setup --home ${home}`, note: t("pages:guide.script.releaseNote") },
        verify,
      ],
      backup: t("pages:guide.script.backup", { home }),
      recovery: [
        t("pages:guide.script.health", { home }),
        t("pages:guide.script.manual", { home, command: `steve hub restart --home ${home}` }),
      ],
    }
    case "source": return {
      label: t("pages:guide.source.label"),
      summary: t("pages:guide.source.summary"),
      steps: [
        { title: t("pages:guide.source.backup"), command: `STEVE_HOME=${home} ${home}/current/bin/steve-hub --admin backup --reason pre-upgrade`, note: t("pages:guide.source.backupNote") },
        { title: t("pages:guide.source.checkout"), command: `git -C ${repo} fetch --tags origin && git -C ${repo} checkout ${tag}`, note: t("pages:guide.source.checkoutNote") },
        { title: t("pages:guide.source.build"), command: `cd ${repo} && npm ci && npm -w web run build && cargo build --release --manifest-path hub-rs/Cargo.toml -p steve-hub` },
        { title: t("pages:guide.source.restart"), note: t("pages:guide.source.restartNote") },
        verify,
      ],
      backup: t("pages:guide.source.backupText", { file: `${home}/backups/restic` }),
      recovery: [t("pages:guide.source.recovery")],
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
export const stageLabel = (key: (typeof upgradeStages)[number]["key"]) =>
  upgradeStages.find(s => s.key === key)!.label
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
/** A failure after the irreversible commit boundary: the new version and database stay in place. */
export const committedFailure = (op: UpdateOperation) =>
  op.phase === "failed" && op.error_code === "committed_startup_failed"
