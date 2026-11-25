/* Single TanStack Query hook for Hub data reads across all views and modules.
 * Scopes cache entries by workspace/Hub (`scopeKey()`), refetches on Hub SSE
 * events (debounced), and routes expired sessions (`Unauthenticated`) to sign-in. */
import { useCallback, useRef } from "react"
import { useQuery, type QueryKey } from "@tanstack/react-query"
import { HubError, Unauthenticated } from "@/api"
import { qc, scopeKey } from "@/data/sync"
import { emit, st, useHubEvents } from "@/store"

export type HubQueryOptions = {
  /** Hub SSE event types that trigger a refetch. */
  events?: readonly string[]
  /** Debounce window (ms) for SSE-triggered refetches; defaults to 150ms. */
  debounceMs?: number
  /** Disable SSE refetching without changing the events array (e.g. historical stats ranges). */
  live?: boolean
  /** Periodic polling interval in ms (e.g. node probe refresh). */
  pollMs?: number
  /** When false, the query does not execute. Defaults to true. */
  enabled?: boolean
}

export type HubQuery<T> = {
  data: T | undefined
  error: (Error & { status?: number }) | null
  /** HTTP status code when `error` is a `HubError`, otherwise 0. */
  status: number
  /** True while the first load for the current key is pending (or redirecting to sign-in). */
  loading: boolean
  /** Epoch ms when `data` was last updated (0 before first success). */
  updatedAt: number
  /** Re-run the query now. */
  reload: () => Promise<void>
  /** Replace or update cached data for the current query key without refetching. */
  set: (v: T | ((prev: T | undefined) => T)) => void
}

const EMPTY_EVENTS: readonly string[] = []
const QUERY_GC_MS = 5 * 60_000

/** Builds the workspace-scoped TanStack Query key for a Hub query. */
export const hubQueryKey = (key: readonly unknown[], scope = scopeKey()): QueryKey => ["hub", scope, ...key]

export function useHubQuery<T>(
  key: readonly unknown[],
  fn: (signal: AbortSignal) => Promise<T>,
  options: HubQueryOptions = {},
): HubQuery<T> {
  const { events = EMPTY_EVENTS, debounceMs = 150, live = true, pollMs, enabled = true } = options
  const fullKey = hubQueryKey(key)
  const fnRef = useRef(fn)
  fnRef.current = fn
  const keyRef = useRef(fullKey)
  keyRef.current = fullKey

  const q = useQuery<T, Error>({
    queryKey: fullKey,
    queryFn: async ({ signal }) => {
      try {
        return await fnRef.current(signal)
      } catch (e) {
        if (e instanceof Unauthenticated) {
          st.login = e.login ?? "/auth/login"
          emit()
        }
        throw e
      }
    },
    enabled,
    staleTime: 0,
    gcTime: QUERY_GC_MS,
    retry: false,
    refetchOnWindowFocus: false,
    refetchInterval: pollMs && pollMs > 0 ? pollMs : false,
  }, qc)

  const refetchRef = useRef(q.refetch)
  refetchRef.current = q.refetch

  const reload = useCallback(async () => {
    await refetchRef.current()
  }, [])

  const set = useCallback((v: T | ((prev: T | undefined) => T)) => {
    qc.setQueryData<T>(keyRef.current, v as T | ((old: T | undefined) => T))
  }, [])

  const subscribed = live && enabled && events.length > 0
  useHubEvents(subscribed ? events : EMPTY_EVENTS, () => {
    if (subscribed) void refetchRef.current()
  }, debounceMs)

  const unauth = q.error instanceof Unauthenticated
  const error = unauth ? null : (q.error as (Error & { status?: number }) | null)
  const status = error instanceof HubError ? error.status : error?.status ?? 0

  return {
    data: q.data,
    error,
    status,
    loading: enabled && (q.isPending || unauth),
    updatedAt: q.dataUpdatedAt,
    reload,
    set,
  }
}
