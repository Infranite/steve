// What finishing an upgrade takes, per installation. No imports: node --test loads this directly.

/** How the Hub was installed (Hub `upgrades/install.rs`). Paths are only sent to the instance manager. */
export type InstallKind = "managed" | "local_release" | "release_script" | "source" | "external"
export type Install = { kind: InstallKind; home?: string; release?: string | null; repo?: string | null }
export type GuideStep = { title: string; command?: string; note?: string }
export type Guide = { label: string; summary: string; steps: GuideStep[]; backup: string; recovery: string[] }

export const installLabel: Record<InstallKind, string> = {
  managed: "托管安装", local_release: "本机签名构建", release_script: "发布脚本部署", source: "源码运行", external: "外部部署",
}

const sh = (s: string) => /^[\w@%+=:,./~-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`

/** Commands for an installation Steve does not switch itself, with this machine's paths filled in. */
export function upgradeGuide(install: Install, version: string): Guide {
  const v = version.replace(/^v/, ""), tag = `v${v}`
  const home = install.home ? sh(install.home) : "~/.steve"
  const repo = install.repo ? sh(install.repo) : "<Steve 源码目录>"
  const verify: GuideStep = { title: "回到本页核对", note: `点「我已完成升级，核对版本」，确认后端、前端都是 ${v} 且服务健康。` }
  switch (install.kind) {
    case "local_release": return {
      label: "本机签名构建（ops/local-release.py）",
      summary: "这台机器的 Hub 用本机签名身份和 steve-keyhelper 读取保险箱，不能直接换成 GitHub Release 的通用预编译包。请用同一个 Release 标签在本机构建，再由脚本切换。",
      steps: [
        { title: "获取 Release 标签", command: `git -C ${repo} fetch --tags origin && git -C ${repo} rev-parse --verify ${tag}^{commit}`, note: "第二条命令输出一个提交号，说明标签已在本地。" },
        { title: "在本机构建并签名", command: `python3 ${repo}/ops/local-release.py build ${tag}`, note: `生成 ${home}/releases/${v}-local-<提交前 7 位>。构建不影响正在运行的服务。` },
        { title: "切换到新版本", command: `python3 ${repo}/ops/local-release.py deploy ${home}/releases/${v}-local-*`, note: "依次检查数据库兼容、等待运行中的任务停止（最长 10 分钟）、停止服务并备份数据库、在维护模式试启动新版本，最后切换并启动。期间页面会短暂断开。" },
        verify,
      ],
      backup: `部署脚本停止服务后把数据库备份到 ${home}/backups，并用 quick_check 校验；备份文件名写在 ${home}/logs/release/local-${v}-<时间>.json。`,
      recovery: [
        "切换前任一步失败（兼容检查、等待任务、备份、试启动）：脚本放回备份并启动旧版本，不需要手动处理。按结果文件里的 error 处理后重试。",
        `切换后失败（退出码 3）：新版本保留，数据库可能已迁移，不会自动回退。先看 ${home}/logs/hub.log；必须回到旧版本时运行 python3 ${repo}/ops/local-release.py deploy ${home}/releases/${install.release ? sh(install.release) : "<旧版本目录>"}，数据库已被迁移时会被拒绝，此时用结果文件里的备份恢复。`,
      ],
    }
    case "release_script": return {
      label: "发布脚本部署（ops/release.sh）",
      summary: "这台机器由 launchd 运行 ops/release.sh 构建的版本。升级方式是把源码仓库的 main 更新到包含这个 Release 标签，再用发布脚本发布。",
      steps: [
        { title: "更新 main", command: `git -C ${repo} checkout main && git -C ${repo} pull --ff-only --tags origin main`, note: `再运行 git -C ${repo} merge-base --is-ancestor ${tag} main，没有输出也没有报错说明 main 已包含 ${tag}。` },
        { title: "发布", command: `zsh ${repo}/ops/release.sh main`, note: "等待运行中的任务停止，备份数据库，切换 current 并重启，然后做健康检查；前端单独切换。期间页面会短暂断开。" },
        verify,
      ],
      backup: `发布脚本在切换前把数据库备份到 ${home}/backups，并在 ${home}/releases/history.jsonl 记录备份文件和前后数据库版本。`,
      recovery: [
        `健康检查失败：脚本自动回到上一个版本（旧版本仍能打开数据库时），结果写在 ${home}/releases/last.json。`,
        `需要手动回退：zsh ${repo}/ops/release.sh --rollback，默认回到上一个版本；数据库已被新版本迁移时会被拒绝，此时用 ${home}/backups 里的备份恢复。`,
      ],
    }
    case "source": return {
      label: "源码运行",
      summary: "Hub 直接从源码构建运行，没有安装器替你切换。按下面的步骤备份、更新源码、构建，再重启你自己的 Hub 进程。",
      steps: [
        { title: "备份数据库", command: `mkdir -p ${home}/backups && sqlite3 ${home}/data/steve.sqlite3 ".backup ${home}/backups/before-${v}.sqlite3"`, note: "Hub 运行时也能得到一致的副本。" },
        { title: "切到 Release 标签", command: `git -C ${repo} fetch --tags origin && git -C ${repo} checkout ${tag}`, note: "在自己的分支上工作时，也可以把标签合并进来。" },
        { title: "构建前后端", command: `cd ${repo} && npm ci && npm -w web run build && cargo build --release --manifest-path hub-rs/Cargo.toml -p steve-hub` },
        { title: "重启 Hub", note: "停止当前 Hub 进程，用新构建重新启动。" },
        verify,
      ],
      backup: `源码运行不会自动备份，第 1 步把数据库复制到 ${home}/backups/before-${v}.sqlite3。`,
      recovery: [`启动失败：停止新进程，git checkout 回原来的提交重新构建；新版本已迁移数据库时，先停止 Hub，再用 ${home}/backups/before-${v}.sqlite3 覆盖 ${home}/data/steve.sqlite3。`],
    }
    default: return {
      label: "外部部署",
      summary: "Steve 判断不出这台机器由谁管理文件和进程（例如容器或其他部署系统），不会自己改动。请在部署系统里把版本换成这个 Release。",
      steps: [
        { title: "备份数据", note: `停止服务后复制 ${home}/data，或用 sqlite3 的 .backup 在运行时备份。` },
        { title: `部署 ${tag}`, note: "使用这个 Release 的升级包或镜像。前后端必须来自同一个 Release。" },
        { title: "重启服务", note: "按部署系统的方式重启。" },
        verify,
      ],
      backup: "外部部署不会自动备份，请在第 1 步自行备份。",
      recovery: ["失败时在部署系统里回退到原来的版本；新版本已迁移数据库时，旧版本会拒绝打开，需要先恢复第 1 步的备份。"],
    }
  }
}

/** The managed supervisor's journal phases, as steps a person can follow. */
export const upgradeStages = [
  { label: "下载并校验", phases: ["staging", "ready"] },
  { label: "等待任务停止", phases: ["draining"] },
  { label: "停止旧版本", phases: ["stopping"] },
  { label: "备份数据库", phases: ["backing_up"] },
  { label: "切换并重启", phases: ["restarting"] },
  { label: "验证新版本", phases: ["verifying", "committed"] },
  { label: "核对版本与健康", phases: ["succeeded"] },
] as const
export const stageIndex = (phase: string) => upgradeStages.findIndex(s => (s.phases as readonly string[]).includes(phase))

export type Health = { ok?: boolean; version?: string; web_version?: string; drain?: boolean | null; recovery?: { state?: string } | null }
export type VerifyItem = { key: "health" | "hub" | "web" | "page"; label: string; ok: boolean; detail: string }
const same = (a = "", b = "") => !!a && a.replace(/^v/, "") === b.replace(/^v/, "")

/** After the restart: what the Hub reports now, against the version that was installed. */
export function verifyUpgrade(target: string, health: Health | null, loadedWeb: string): { items: VerifyItem[]; done: boolean; needsReload: boolean } {
  const v = target.replace(/^v/, "")
  const healthy = !!health?.ok && health.drain !== true && health.recovery?.state !== "failed"
  const items: VerifyItem[] = [
    { key: "health", label: "服务健康", ok: healthy, detail: !health ? "暂时连不上服务" : !health.ok ? "健康检查未通过" : health.drain ? "仍处于维护状态，暂不接新任务" : health.recovery?.state === "failed" ? "中断任务恢复失败，请查看 Hub 日志" : health.recovery?.state === "partial" ? "正常；部分中断任务需要手动处理" : "正常" },
    { key: "hub", label: "后端 Hub", ok: same(health?.version, v), detail: health?.version || "未知" },
    { key: "web", label: "服务的前端", ok: same(health?.web_version, v), detail: health?.web_version || "未知" },
    { key: "page", label: "本页前端", ok: same(loadedWeb, v), detail: loadedWeb || "未知" },
  ]
  const served = items.slice(0, 3).every(i => i.ok)
  return { items, done: items.every(i => i.ok), needsReload: served && !items[3].ok }
}
