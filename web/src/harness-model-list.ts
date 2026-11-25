/** Pure, deterministic model ID parser, snapshot grouper, version sorter, and search filter
 * for the Node Harness tab. Never hardcodes a static model catalogue; new families group
 * automatically from structured model IDs while unrecognized IDs fall back to "other". */

export type ModelSource = "direct" | "passthrough" | "local" | "passthrough_local"

export type ParsedModelId = {
  raw: string
  /** Canonical identifier with date-snapshot suffix removed (preserves source/namespace prefix). */
  baseId: string
  /** Lowercase slug without source/namespace prefix or date snapshot. */
  slug: string
  /** Source inferred strictly from leading `passthrough/` and `local/` prefixes. */
  source: ModelSource
  /** Intermediate provider/namespace path between source prefix and slug, when present. */
  namespace: string | null
  /** Lowercase family identifier (e.g. "gpt", "glm", or "other" when unrecognized). */
  family: string
  /** Display name for recognized families (`null` when `family === "other"` so UI uses i18n). */
  familyLabel: string | null
  /** Numeric version tuple used for newest-to-oldest sorting inside a family. */
  version: number[]
  /** Normalized snapshot date stamp (e.g. "20251101" or "0605") when parsed from the ID. */
  snapshotDate: string | null
  /** True when the model carries a preview/beta/experimental channel marker. */
  isPreview: boolean
  /** Relative capability tier weight used only as a tie-breaker within the same version tuple. */
  tierRank: number
}

export type ModelSnapshotRef = {
  id: string
  date: string
}

export type GroupedModelEntry = {
  /** Primary ID shown for the row (undated alias when present, otherwise newest snapshot). */
  id: string
  baseId: string
  source: ModelSource
  family: string
  version: number[]
  isDefault: boolean
  isPreview: boolean
  tierRank: number
  latestSnapshotDate: string | null
  /** Additional date snapshots collapsed under this primary alias/version, newest first. */
  snapshots: ModelSnapshotRef[]
}

export type ModelFamilyGroup = {
  family: string
  /** Display name (`null` for `"other"` so UI renders localized label). */
  label: string | null
  latest: GroupedModelEntry
  /** Total raw reported model IDs inside this family (including grouped date snapshots). */
  rawCount: number
  entries: GroupedModelEntry[]
}

export type ModelFamilyChip = {
  family: string
  label: string | null
  latestId: string
  source: ModelSource
  rawCount: number
}

export type StructuredModelInventory = {
  totalRaw: number
  defaultModel: string | null
  familyChips: ModelFamilyChip[]
  groups: ModelFamilyGroup[]
}

export const OTHER_FAMILY = "other" as const

/** Generic control/router prefixes that must never form a guessed model family. */
const NON_FAMILY_HEADS = new Set([
  "auto",
  "default",
  "custom",
  "test",
  "fixture",
  "model",
  "models",
  "router",
  "proxy",
  "local",
  "passthrough",
  "internal",
  "system",
  "fallback",
  "agent",
  "classifier",
  "embed",
  "embedding",
  "embeddings",
  "unknown",
  "other",
  "none",
  "latest",
  "stable",
  "beta",
  "preview",
  "fast",
  "best",
  "small",
  "medium",
  "large",
])

/** Recognized capability/channel words that validate an unversioned `<family>-<tier>` alias. */
const TIER_OR_CHANNEL_TOKENS = new Set([
  "latest",
  "opus",
  "sonnet",
  "haiku",
  "pro",
  "flash",
  "mini",
  "nano",
  "lite",
  "plus",
  "max",
  "ultra",
  "air",
  "turbo",
  "thinking",
  "coder",
  "preview",
  "fast",
  "neon",
  "sol",
  "luna",
])

const PREVIEW_TOKENS = new Set(["preview", "beta", "alpha", "exp", "experimental", "rc"])

const HIGH_TIERS = new Set(["ultra", "max", "opus", "pro", "thinking", "neon"])
const MID_HIGH_TIERS = new Set(["sonnet", "plus", "large", "coder", "sol"])
const MID_LOW_TIERS = new Set(["medium", "air", "flash", "luna"])
const LOW_TIERS = new Set(["haiku", "mini", "small", "lite", "nano", "fast"])

/** Formatting-only display overrides for acronyms / compound brand casing. */
const ACRONYM_FAMILIES = new Set(["gpt", "glm", "llm"])
const SPECIAL_CASE_FAMILIES: Record<string, string> = {
  deepseek: "DeepSeek",
  minimax: "MiniMax",
  chatglm: "ChatGLM",
}

const SOURCE_ORDER: Record<ModelSource, number> = {
  direct: 0,
  passthrough: 1,
  local: 2,
  passthrough_local: 3,
}

const MAX_VERSION_PARTS = 3
const SOURCE_PREFIX_LIMIT = 2

export function formatFamilyLabel(family: string): string | null {
  if (family === OTHER_FAMILY || !family) return null
  if (ACRONYM_FAMILIES.has(family)) return family.toUpperCase()
  if (SPECIAL_CASE_FAMILIES[family]) return SPECIAL_CASE_FAMILIES[family]
  return family[0].toUpperCase() + family.slice(1)
}

function stripDateSnapshot(rawSlug: string): { baseSlugRaw: string; snapshotDate: string | null } {
  const lower = rawSlug.toLowerCase()
  // YYYYMMDD after hyphen or `@` (e.g. `claude-opus-4-5-20251101`, `claude-opus-4-5@20251101`)
  const ymdCompact = lower.match(/^(.+)[-@](20\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01]))$/)
  if (ymdCompact) {
    return {
      baseSlugRaw: rawSlug.slice(0, ymdCompact[1].length),
      snapshotDate: ymdCompact[2],
    }
  }
  // YYYY-MM-DD after hyphen (e.g. `gpt-4o-2024-11-20`, `gpt-5-2025-08-07`)
  const ymdDashed = lower.match(/^(.+)-(20\d{2})-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$/)
  if (ymdDashed) {
    return {
      baseSlugRaw: rawSlug.slice(0, ymdDashed[1].length),
      snapshotDate: `${ymdDashed[2]}${ymdDashed[3]}${ymdDashed[4]}`,
    }
  }
  // Preview/exp MM-DD suffix (e.g. `gemini-2.5-pro-preview-06-05`)
  const previewDashed = lower.match(/^(.+-(?:preview|exp))-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$/)
  if (previewDashed) {
    return {
      baseSlugRaw: rawSlug.slice(0, previewDashed[1].length),
      snapshotDate: `${previewDashed[2]}${previewDashed[3]}`,
    }
  }
  // Preview/exp MMDD infix before `-preview`/`-exp` (e.g. `kimi-k2-0905-preview`)
  const previewInfix = lower.match(/^(.+)-(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])(-(preview|exp))$/)
  if (previewInfix) {
    const stem = rawSlug.slice(0, previewInfix[1].length)
    const suffix = rawSlug.slice(rawSlug.length - previewInfix[4].length)
    return {
      baseSlugRaw: stem + suffix,
      snapshotDate: `${previewInfix[2]}${previewInfix[3]}`,
    }
  }
  return { baseSlugRaw: rawSlug, snapshotDate: null }
}

function isNonVersionDescriptor(token: string): boolean {
  // Parameter counts (`70b`, `32b`, `1.5b`), context sizes (`128k`, `1m`), quantization (`fp8`, `bf16`, `q4_k_m`, `a22b`)
  if (/^\d+(?:\.\d+)?[bkm]$/.test(token)) return true
  if (/^(?:fp\d+|bf\d+|int\d+|q\d[a-z0-9_]*|a\d+(?:\.\d+)?b|e\d+b)$/.test(token)) return true
  // Year / calendar stamps (`2025`, `20251101`, `0905`) or leading-zero numeric fragments (`05`, `06`)
  if (/^(?:20\d{2}|20\d{6}|0\d+|1[0-2][0-3]\d)$/.test(token)) return true
  return false
}

function parseVersionToken(token: string): number[] | null {
  if (isNonVersionDescriptor(token)) return null
  // Dotted version with optional single-letter suffix (e.g. `5.4`, `2.5`, `4.7`, `1.5.2`)
  const dotted = token.match(/^(\d{1,2})\.(\d{1,2})(?:\.(\d{1,2}))?[a-z]?$/)
  if (dotted) {
    const out = [Number(dotted[1]), Number(dotted[2])]
    if (dotted[3] !== undefined) out.push(Number(dotted[3]))
    return out
  }
  // Small integer version with optional `o` variant (`4`, `5`, `4o`)
  const smallInt = token.match(/^(\d{1,2})o?$/)
  if (smallInt) return [Number(smallInt[1])]
  // Single-letter generation marker (`k2.5`, `k2`, `v3.2`, `r1`, `m2.5`, `g2`)
  const gen = token.match(/^[kvrmg](\d{1,2})(?:\.(\d{1,2}))?$/)
  if (gen) {
    const out = [Number(gen[1])]
    if (gen[2] !== undefined) out.push(Number(gen[2]))
    return out
  }
  return null
}

function extractVersion(tokens: string[], attachedVersion: number[] | null): number[] {
  const version: number[] = attachedVersion ? [...attachedVersion] : []
  let collecting = version.length > 0
  for (const token of tokens) {
    if (version.length >= MAX_VERSION_PARTS) break
    const parsed = parseVersionToken(token)
    if (parsed) {
      if (collecting && version.length >= 2) break
      for (const n of parsed) {
        if (version.length < MAX_VERSION_PARTS) version.push(n)
      }
      collecting = true
    } else if (collecting) {
      // Once non-version words appear after version digits (e.g. `gpt-5.3-codex`), stop.
      break
    }
  }
  return version
}

function computeTierRank(tokens: string[]): number {
  if (tokens.some((t) => LOW_TIERS.has(t))) return 1
  if (tokens.some((t) => HIGH_TIERS.has(t))) return 5
  if (tokens.some((t) => MID_HIGH_TIERS.has(t))) return 4
  if (tokens.some((t) => MID_LOW_TIERS.has(t))) return 2
  return 3
}

/** Deterministically parse a raw model ID into source, family, canonical base ID, and version. */
export function parseModelId(rawInput: string): ParsedModelId {
  const raw = rawInput.trim()
  if (!raw) {
    return {
      raw: "",
      baseId: "",
      slug: "",
      source: "direct",
      namespace: null,
      family: OTHER_FAMILY,
      familyLabel: null,
      version: [],
      snapshotDate: null,
      isPreview: false,
      tierRank: 3,
    }
  }

  const segments = raw.split("/").filter(Boolean)
  let viaPassthrough = false
  let isLocal = false
  let consumed = 0
  const prefixSegments: string[] = []
  while (consumed < SOURCE_PREFIX_LIMIT && segments.length > 1) {
    const first = segments[0].toLowerCase()
    if (first === "passthrough" && !viaPassthrough) {
      viaPassthrough = true
      prefixSegments.push(segments.shift()!)
      consumed++
    } else if (first === "local" && !isLocal) {
      isLocal = true
      prefixSegments.push(segments.shift()!)
      consumed++
    } else {
      break
    }
  }

  const source: ModelSource =
    viaPassthrough && isLocal
      ? "passthrough_local"
      : viaPassthrough
        ? "passthrough"
        : isLocal
          ? "local"
          : "direct"

  const namespaceSegments = segments.length > 1 ? segments.slice(0, -1) : []
  const namespace = namespaceSegments.length ? namespaceSegments.join("/") : null
  const rawSlug = segments[segments.length - 1] ?? ""

  const { baseSlugRaw, snapshotDate } = stripDateSnapshot(rawSlug)
  const basePath = [...prefixSegments, ...namespaceSegments, baseSlugRaw].join("/")
  const slug = baseSlugRaw.toLowerCase()
  const tokens = slug.split(/[-_:]+/).filter(Boolean)
  const head = tokens[0] ?? ""

  let familyCandidate: string | null = null
  let attachedVersion: number[] | null = null
  let tailTokens: string[] = tokens.slice(1)

  // Attached family+version token (`qwen3-32b`, `llama3.3-70b`, `glm4.5`, `gpt4o`, `gemma3-27b`)
  const attached = head.match(/^([a-z]{2,})(\d{1,2}(?:\.\d{1,2})*)o?$/)
  if (attached && !NON_FAMILY_HEADS.has(attached[1])) {
    familyCandidate = attached[1]
    attachedVersion = attached[2].split(".").map(Number)
  } else if (/^[a-z]{2,}$/.test(head) && !NON_FAMILY_HEADS.has(head)) {
    familyCandidate = head
  }

  const version = familyCandidate ? extractVersion(tailTokens, attachedVersion) : []
  const allTailAreTiers =
    tailTokens.length > 0 && tailTokens.every((tok) => TIER_OR_CHANNEL_TOKENS.has(tok) || PREVIEW_TOKENS.has(tok))

  const recognized = familyCandidate !== null && (version.length > 0 || allTailAreTiers)
  const family = recognized && familyCandidate ? familyCandidate : OTHER_FAMILY
  const isPreview = tailTokens.some((tok) => PREVIEW_TOKENS.has(tok))
  const tierRank = computeTierRank(tailTokens)

  return {
    raw,
    baseId: basePath,
    slug,
    source,
    namespace,
    family,
    familyLabel: formatFamilyLabel(family),
    version,
    snapshotDate,
    isPreview,
    tierRank,
  }
}

/** Compare two numeric version tuples descending (newest first). Missing trailing parts compare as 0. */
export function compareVersionsDesc(a: readonly number[], b: readonly number[]): number {
  const hasA = a.length > 0
  const hasB = b.length > 0
  if (hasA !== hasB) return hasA ? -1 : 1
  const len = Math.max(a.length, b.length)
  for (let i = 0; i < len; i++) {
    const va = a[i] ?? 0
    const vb = b[i] ?? 0
    if (va !== vb) return vb - va
  }
  return 0
}

function compareDatesDesc(a: string | null, b: string | null): number {
  if (a === b) return 0
  if (!a) return 1
  if (!b) return -1
  return b.localeCompare(a)
}

function compareEntries(a: GroupedModelEntry, b: GroupedModelEntry): number {
  const v = compareVersionsDesc(a.version, b.version)
  if (v !== 0) return v
  if (a.tierRank !== b.tierRank) return b.tierRank - a.tierRank
  if (a.isPreview !== b.isPreview) return a.isPreview ? 1 : -1
  if (a.source !== b.source) return SOURCE_ORDER[a.source] - SOURCE_ORDER[b.source]
  const d = compareDatesDesc(a.latestSnapshotDate, b.latestSnapshotDate)
  if (d !== 0) return d
  return a.baseId.localeCompare(b.baseId)
}

/** Group raw model IDs by family, collapse date snapshots with their alias, and sort newest-to-oldest. */
export function structureHarnessModels(
  rawModels: readonly string[],
  defaultModel?: string | null,
): StructuredModelInventory {
  const trimmedDefault = defaultModel?.trim() || null
  const dedupedRaw: string[] = []
  const seenRaw = new Set<string>()
  for (const m of rawModels) {
    const trimmed = m.trim()
    if (!trimmed || seenRaw.has(trimmed)) continue
    seenRaw.add(trimmed)
    dedupedRaw.push(trimmed)
  }

  const parsed = dedupedRaw.map(parseModelId)

  // Step 1: Collapse date snapshots with their corresponding canonical alias (`baseId` + `source`).
  type Bucket = {
    alias: ParsedModelId | null
    snapshots: ParsedModelId[]
  }
  const byBase = new Map<string, Bucket>()
  for (const item of parsed) {
    const key = `${item.source}\0${item.baseId.toLowerCase()}`
    let bucket = byBase.get(key)
    if (!bucket) {
      bucket = { alias: null, snapshots: [] }
      byBase.set(key, bucket)
    }
    if (item.snapshotDate === null) {
      if (!bucket.alias) bucket.alias = item
    } else {
      bucket.snapshots.push(item)
    }
  }

  const entries: GroupedModelEntry[] = []
  for (const bucket of byBase.values()) {
    bucket.snapshots.sort((a, b) => compareDatesDesc(a.snapshotDate, b.snapshotDate) || a.raw.localeCompare(b.raw))
    const primary = bucket.alias ?? bucket.snapshots[0]
    const extraSnapshots = (bucket.alias ? bucket.snapshots : bucket.snapshots.slice(1)).map((s) => ({
      id: s.raw,
      date: s.snapshotDate ?? "",
    }))
    const allIds = [primary.raw, ...extraSnapshots.map((s) => s.id)]
    const isDefault = trimmedDefault !== null && allIds.includes(trimmedDefault)
    const latestSnapshotDate = bucket.snapshots[0]?.snapshotDate ?? null
    entries.push({
      id: primary.raw,
      baseId: primary.baseId,
      source: primary.source,
      family: primary.family,
      version: primary.version,
      isDefault,
      isPreview: primary.isPreview,
      tierRank: primary.tierRank,
      latestSnapshotDate,
      snapshots: extraSnapshots,
    })
  }

  // Step 2: Partition entries into family groups and sort each group newest-to-oldest.
  const byFamily = new Map<string, GroupedModelEntry[]>()
  for (const entry of entries) {
    const list = byFamily.get(entry.family)
    if (list) list.push(entry)
    else byFamily.set(entry.family, [entry])
  }

  const defaultFamily = trimmedDefault ? parseModelId(trimmedDefault).family : null

  const groups: ModelFamilyGroup[] = []
  for (const [family, list] of byFamily) {
    list.sort(compareEntries)
    const rawCount = list.reduce((acc, e) => acc + 1 + e.snapshots.length, 0)
    groups.push({
      family,
      label: formatFamilyLabel(family),
      latest: list[0],
      rawCount,
      entries: list,
    })
  }

  // Deterministic family order: default model's family first (if recognized), then direct families
  // alphabetically, then passthrough/local-only families alphabetically, and "other" always last.
  groups.sort((a, b) => {
    if (a.family === OTHER_FAMILY && b.family !== OTHER_FAMILY) return 1
    if (b.family === OTHER_FAMILY && a.family !== OTHER_FAMILY) return -1
    const aDef = defaultFamily !== null && defaultFamily !== OTHER_FAMILY && a.family === defaultFamily
    const bDef = defaultFamily !== null && defaultFamily !== OTHER_FAMILY && b.family === defaultFamily
    if (aDef !== bDef) return aDef ? -1 : 1
    const aDirect = a.entries.some((e) => e.source === "direct")
    const bDirect = b.entries.some((e) => e.source === "direct")
    if (aDirect !== bDirect) return aDirect ? -1 : 1
    return a.family.localeCompare(b.family)
  })

  // Summary chips: one latest chip per recognized family; if only "other" exists, show its first entry
  // so single-model or fixture harnesses still display their model cleanly on the card.
  const namedGroups = groups.filter((g) => g.family !== OTHER_FAMILY)
  const chipSource = namedGroups.length > 0 ? namedGroups : groups
  const familyChips: ModelFamilyChip[] = chipSource.map((g) => ({
    family: g.family,
    label: g.label,
    latestId: g.latest.id,
    source: g.latest.source,
    rawCount: g.rawCount,
  }))

  return {
    totalRaw: dedupedRaw.length,
    defaultModel: trimmedDefault,
    familyChips,
    groups,
  }
}

/** Filter structured family groups by a search query across IDs, snapshots, families, and sources. */
export function filterModelGroups(
  groups: readonly ModelFamilyGroup[],
  query: string,
): { groups: ModelFamilyGroup[]; matchedRawCount: number } {
  const needle = query.trim().toLowerCase()
  if (!needle) {
    return {
      groups: [...groups],
      matchedRawCount: groups.reduce((acc, g) => acc + g.rawCount, 0),
    }
  }
  const out: ModelFamilyGroup[] = []
  let matchedRawCount = 0
  for (const group of groups) {
    const familyMatches =
      group.family.toLowerCase().includes(needle) ||
      (group.label?.toLowerCase().includes(needle) ?? false)
    const entries = familyMatches
      ? group.entries
      : group.entries.filter((entry) => {
          if (entry.id.toLowerCase().includes(needle)) return true
          if (entry.baseId.toLowerCase().includes(needle)) return true
          if (entry.source.includes(needle)) return true
          return entry.snapshots.some((snap) => snap.id.toLowerCase().includes(needle))
        })
    if (!entries.length) continue
    const rawCount = entries.reduce((acc, e) => acc + 1 + e.snapshots.length, 0)
    matchedRawCount += rawCount
    out.push({
      ...group,
      latest: entries[0],
      rawCount,
      entries,
    })
  }
  return { groups: out, matchedRawCount }
}
