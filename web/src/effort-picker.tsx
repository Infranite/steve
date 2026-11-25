/* Canonical five-level reasoning effort picker and session mapping formatter.
 * Storage, Hub APIs, and the UI speak only Steve's five canonical levels; this component annotates each
 * level with what the selected harness/model actually runs (from the Hub's capability `effort_map`) and
 * greys out levels the selected model does not support. */
import type { EffortMapping, EffortMode, ReasoningEffort } from "@/gen"
import { useCatalog, type Catalog } from "@/model-picker"
import { SetSelect } from "@/settings-controls"
import { useRuntimes } from "@/store"
import { useT } from "@/i18n"

export const CANONICAL_EFFORTS: readonly ReasoningEffort[] = ["default", "low", "medium", "high", "max"] as const

export function isCanonicalEffort(v: string | null | undefined): v is ReasoningEffort {
  return v != null && (CANONICAL_EFFORTS as readonly string[]).includes(v)
}

export function normalizeEffort(v: string | null | undefined): ReasoningEffort {
  return isCanonicalEffort(v) ? v : "default"
}

type T = ReturnType<typeof useT>

/** Short localized name of a canonical effort level (`低` / `中` / `高` / `最高` / `跟随模型默认`). */
export function effortShortLabel(t: T, level: ReasoningEffort): string {
  return t(`manage:effort.${level}`)
}

/** Resolve the 5-level `EffortMapping[]` for `(harness, model)` from catalog + runtime capability data.
 *  Returns `null` when capability data is not yet available (unknown), so the user is not blocked from picking. */
export function resolveEffortMap(
  model: string | null | undefined,
  catalog: Catalog | null | undefined,
  harnessMap?: EffortMapping[] | null,
  runtimeMap?: EffortMapping[] | null,
  effortMode?: EffortMode | null,
): EffortMapping[] | null {
  const m = model?.trim() || ""
  if (m) {
    const entry = catalog?.models.find((x) => x.id === m)
    if (entry?.effort_map?.length) return entry.effort_map
    if (effortMode === "unsupported") return catalog?.effort_map ?? harnessMap ?? runtimeMap ?? null
    return null
  }
  const defModel = catalog?.default?.trim()
  if (defModel) {
    const entry = catalog?.models.find((x) => x.id === defModel)
    if (entry?.effort_map?.length) return entry.effort_map
  }
  if (catalog?.effort_map?.length) return catalog.effort_map
  if (harnessMap?.length) return harnessMap
  if (runtimeMap?.length) return runtimeMap
  return null
}

function optionDescription(t: T, entry: EffortMapping | undefined): string | undefined {
  if (!entry || entry.requested === "default") return undefined
  if (!entry.supported) return t("manage:effort.optionUnsupported")
  if (entry.strategy === "acp_option" && entry.native_effort) {
    return t("manage:effort.mapsTo", { target: entry.native_effort })
  }
  if (entry.strategy === "model_variant" && entry.target_model) {
    return t("manage:effort.mapsTo", { target: entry.target_model })
  }
  return undefined
}

/** Format a session's recorded `EffortMapping` (or fallback canonical effort) for display in session headers/panels. */
export function formatEffortMapping(
  t: T,
  mapping: EffortMapping | null | undefined,
  harnessLabel?: string | null,
  fallback?: ReasoningEffort | null,
): string {
  if (mapping) {
    if (mapping.requested === "default") return t("views:session.effortDefault")
    const req = effortShortLabel(t, mapping.requested)
    if (mapping.strategy === "acp_option" && mapping.native_effort) {
      const target = harnessLabel ? `${harnessLabel} ${mapping.native_effort}` : mapping.native_effort
      return `${req} → ${target}`
    }
    if (mapping.strategy === "model_variant" && mapping.target_model) {
      return `${req} → ${mapping.target_model}`
    }
    return `${req} → ${t("views:session.effortModelDecides")}`
  }
  if (fallback && fallback !== "default") return effortShortLabel(t, fallback)
  return t("views:session.effortInherit")
}

export function EffortPicker({
  harness,
  model,
  value,
  onChange,
  testId,
  label,
  defaultLabel,
  harnessEffortMap,
  wide = true,
  className,
}: {
  harness: string
  model?: string | null
  value: ReasoningEffort
  onChange: (v: ReasoningEffort) => void
  testId?: string
  label?: string
  defaultLabel?: string
  harnessEffortMap?: EffortMapping[] | null
  wide?: boolean
  className?: string
}) {
  const t = useT()
  const runtimes = useRuntimes()
  const catalog = useCatalog(harness)
  const runtime = runtimes.find((r) => r.id === harness)
  const mode = catalog?.effort_mode ?? runtime?.effort_mode ?? null
  const effortMap = resolveEffortMap(model, catalog, harnessEffortMap, runtime?.effort_map, mode)
  const byLevel = effortMap ? new Map(effortMap.map((m) => [m.requested, m])) : null

  const options = CANONICAL_EFFORTS.map((level) => {
    const entry = byLevel?.get(level)
    const unsupported = level !== "default" && entry != null && !entry.supported
    return {
      v: level,
      l: level === "default" && defaultLabel ? defaultLabel : effortShortLabel(t, level),
      disabled: unsupported,
      description: optionDescription(t, entry),
    }
  })

  const explicit = byLevel
    ? (["low", "medium", "high", "max"] as const).map((k) => byLevel.get(k)).filter((x): x is EffortMapping => !!x)
    : []
  const allUnsupported = explicit.length === 4 && explicit.every((e) => !e.supported)
  const selected = byLevel?.get(value)
  const activeTarget = value !== "default" && selected?.supported
    ? (selected.strategy === "model_variant" ? selected.target_model : selected.native_effort)
    : null

  let note: string | null = null
  if (allUnsupported) {
    note = value !== "default"
      ? t("manage:effort.savedUnsupportedNote", { requested: effortShortLabel(t, value) })
      : t("manage:effort.modelUnsupportedNote")
  } else if (value !== "default" && selected && !selected.supported) {
    note = t("manage:effort.levelUnsupportedNote", { requested: effortShortLabel(t, value) })
  }

  return (
    <div className="flex min-w-0 flex-col gap-1.5" data-effort-picker={testId ?? harness}>
      <SetSelect
        wide={wide}
        className={className}
        testId={testId}
        label={label}
        value={value}
        onChange={(v) => onChange(normalizeEffort(v))}
        options={options}
      />
      {note && (
        <p className="type-footnote text-muted-foreground" data-effort-note="unsupported">
          {note}
        </p>
      )}
      {!note && activeTarget && (
        <p className="type-footnote text-muted-foreground" data-effort-note="mapped">
          {t("manage:effort.activeMapping", { target: activeTarget })}
        </p>
      )}
    </div>
  )
}
