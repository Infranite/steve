import { Badge } from "@/components/affect/badge-clean"
import { HarnessMark } from "@/components/harness-mark"
import { Button } from "@/components/ui/button"
import { SetSelect } from "@/settings-controls"
import { st, useStore } from "@/store"
import { closeSheet } from "@/ui"
import { useT } from "@/i18n"
import { linkedHubDevice } from "@/hub-management-model"
import type { RRuntime } from "@/api"
import { permissionModes, runtimeReason, runtimeState, supportsPermission } from "@/runtime-status"

export const runtimeSetupHref = () => {
  const hub = linkedHubDevice(st.devices)
  return hub ? "#/devices/nodes/local" : "#/devices/nodes"
}

export function RuntimeSetupLink() {
  const t = useT()
  return <Button size="sm" variant="link" render={<a href={runtimeSetupHref()} onClick={closeSheet} />} data-runtime-setup-link>{t("shell:runtimeSetup.manage")}</Button>
}

export function RuntimeBadge({ runtime }: { runtime?: RRuntime }) {
  const t = useT(), state = runtimeState(runtime)
  return <Badge dot live={state === "installing"} tone={state === "ready" && runtime?.ok ? "success" : state === "installing" ? "info" : state === "unavailable" || (state === "ready" && !runtime?.ok) ? "warning" : "neutral"} data-runtime-state={state} aria-label={t(runtime?.ok ? "shell:devices.available" : "shell:devices.unavailable")}>
    {t(`shell:runtimeSetup.state.${state}`)}
  </Badge>
}

/** Every entry comes from the registry, including unavailable entries, with the same setup path. */
export function RuntimeSelect({ value, onChange, testId, eligible }: { value: string; onChange: (id: string) => void; testId: string; eligible?: string[] }) {
  useStore()
  const t = useT()
  const options = st.runtimes.map(r => ({ v: r.id, l: `${r.name} · ${t("shell:runtimeSetup.localState", { state: t(`shell:runtimeSetup.state.${runtimeState(r)}`) })}`, icon: <HarnessMark harness={r.id} />,
    description: runtimeReason(r) || (!r.ok ? t("manage:form.unavailable", { name: r.name }) : undefined) || (eligible && !eligible.includes(r.id) ? t("shell:runtimeSetup.notForPurpose") : undefined), disabled: !!eligible && !eligible.includes(r.id) }))
  if (value && !options.some(o => o.v === value)) options.push({ v: value, l: `${value} · ${t("shell:runtimeSetup.state.unknown")}`, icon: <HarnessMark harness={value} />, description: t("shell:runtimeSetup.unknown"), disabled: !!eligible && !eligible.includes(value) })
  return <SetSelect wide label={t("shell:devices.runtimes")} testId={testId} value={value} onChange={onChange} options={options} />
}

export function RuntimeAvailability({ harness }: { harness: string }) {
  useStore()
  const t = useT(), r = st.runtimes.find(r => r.id === harness)
  return <div className="flex min-w-0 flex-col gap-1 type-footnote text-muted-foreground" data-runtime-availability={harness}>
    <span className="flex flex-wrap items-center gap-2"><span>{t("shell:runtimeSetup.localSample")} · {r?.name ?? harness}</span><RuntimeBadge runtime={r} /></span>
    <span>{t("shell:runtimeSetup.targetPending")}</span>
    {!r?.ok && <>
      <span className="break-words whitespace-pre-wrap">{runtimeReason(r) || t(r ? "shell:runtimeSetup.probeUnavailable" : "shell:runtimeSetup.unknown")}</span>
      {r?.setup?.help && <span className="break-words whitespace-pre-wrap">{r.setup.help}</span>}
      <span className="self-start"><RuntimeSetupLink /></span>
    </>}
  </div>
}

/** A stored permission is never silently changed when switching to a more limited adapter. */
export function RuntimePermissionNotice({ harness, mode }: { harness: string; mode: string }) {
  useStore()
  const t = useT(), r = st.runtimes.find(r => r.id === harness)
  if (!r || r.modes == null || !r.ok && !r.modes.length) return <p className="type-footnote text-muted-foreground" data-runtime-permission-unknown>{t("shell:runtimeSetup.permissionPending")}</p>
  if (supportsPermission(r, mode)) return null
  const modes = (r.modes ?? []).map(m => t(`shell:devices.harness.mode.${m}`, { defaultValue: m })).join(t("shell:list.sep"))
  return <p role="alert" className="break-words rounded-lg bg-attention/10 px-3 py-2 type-callout text-attention" data-runtime-permission>
    {t("shell:runtimeSetup.localSample")} · {t("shell:runtimeSetup.unsupportedMode", { name: r.name, mode: t(`shell:devices.harness.mode.${permissionModes[mode as keyof typeof permissionModes] ?? mode}`, { defaultValue: mode }), modes: modes || t("shell:runtimeSetup.noModes") })}
  </p>
}
