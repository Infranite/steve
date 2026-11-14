import { useEffect, useRef, useState } from "react"
import { useT } from "@/i18n"
import { notificationsApi, minuteTime, timeMinute, validQuiet, validTimezone, type NotificationPolicy, type NotificationTarget, type NotificationsClient } from "@/api/notifications"
import { errText } from "@/api/flow"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/affect/switch-clean"
import { Select } from "@/components/affect/select-clean"
import { TextField } from "@/components/affect/text-field-clean"
import { TimePicker } from "@/components/affect/time-picker-clean"
import { Empty, Loading, Notice, Panel, Row, Section } from "@/ui"

/* Separate resources: an unavailable channel list must not hide the inbox or prevent disabling consent.
 * A retry has a real pending state, and late responses cannot overwrite a later retry/unmounted scope. */
function useResource<T>(load: () => Promise<T>) {
  const [data, setData] = useState<T>(), [error, setError] = useState<unknown>(), [loading, setLoading] = useState(true)
  const seq = useRef(0)
  const reload = async () => {
    const n = ++seq.current
    setLoading(true); setError(undefined)
    try { const value = await load(); if (n === seq.current) setData(value) }
    catch (e) { if (n === seq.current) setError(e) }
    finally { if (n === seq.current) setLoading(false) }
  }
  useEffect(() => { void reload(); return () => { seq.current++ } }, [])
  return { data, error, loading, reload }
}

export function NotificationSettingsPane({ workspace }: { workspace: string }) {
  const t = useT()
  const [client] = useState(() => notificationsApi.forWorkspace(workspace))
  const policy = useResource(client.policy), inbox = useResource(client.inbox)
  const identity = useResource(client.me), targets = useResource(client.targets)
  const [seen, setSeen] = useState<Set<string>>(new Set()), [busy, setBusy] = useState<string | null>(null)
  const [seenError, setSeenError] = useState<{ id: string; error: unknown } | null>(null)
  const markSeen = async (id: string) => {
    if (busy) return
    setBusy(id); setSeenError(null)
    try { await client.seen(id); setSeen((old) => new Set([...old, id])) }
    catch (error) { setSeenError({ id, error }) }
    finally { setBusy(null) }
  }
  const failed = (error: unknown, retry: () => void, label: string) => <div role="alert" className="flex flex-wrap items-center gap-3 p-4 text-sm">
    <p className="min-w-0 flex-1 break-words">{label} · {errText(error)}</p>
    <Button size="sm" variant="outline" onClick={retry}>{t("common:action.retry")}</Button>
  </div>
  const me = identity.data?.is_me && identity.data.status === "active" ? identity.data : undefined
  return <div data-personal-notifications className="flex min-w-0 flex-col gap-6">
    {/* Keep the enabled hover colored, not affectUI's default black/white; this pane only. */}
    <style href="notification-switch-hover" precedence="default">{"[data-personal-notifications] .ui-swc { --ui-swc-on-hover: var(--aui-primary-hover); }"}</style>
    <Notice>{t("manage:notify.scope")}</Notice>
    <Section label={t("manage:notify.policy")} right={<Button size="sm" variant="outline" disabled={targets.loading} onClick={() => void targets.reload()}>{t("manage:notify.refreshTargets")}</Button>}>
      {policy.loading ? <Loading /> : policy.error ? failed(policy.error, () => void policy.reload(), t("manage:notify.loadPolicyFailed")) : policy.data && <PolicyForm client={client} initial={policy.data} party={me?.id} name={me?.name} targetsKnown={!targets.loading && !targets.error && !identity.loading && !identity.error} targets={!targets.loading && !targets.error ? targets.data?.targets.filter((target) => target.party_id === me?.id) ?? [] : []} />}
      <div className="mt-3 text-sm text-muted-foreground">
        {identity.loading ? <p role="status">{t("manage:notify.identityLoading")}</p> : identity.error ? failed(identity.error, () => void identity.reload(), t("manage:notify.identityFailed")) : !me ? <p>{t("manage:notify.identityMissing")}</p> : null}
        {targets.loading ? <p role="status">{t("manage:notify.routesLoading")}</p> : targets.error ? failed(targets.error, () => void targets.reload(), t("manage:notify.routesFailed")) : null}
      </div>
    </Section>
    <Section label={t("manage:notify.inbox")} note={t("manage:notify.inboxSub")} right={<Button size="sm" variant="outline" disabled={inbox.loading} onClick={() => void inbox.reload()}>{t("common:action.refresh")}</Button>}>
      {inbox.loading ? <Loading /> : inbox.error ? failed(inbox.error, () => void inbox.reload(), t("manage:notify.loadInboxFailed")) : !inbox.data?.length ? <Empty title={t("manage:notify.empty")} sub={t("manage:notify.emptySub")} /> : <Panel>
        {inbox.data.map((n) => {
          const read = n.seen_at !== null || seen.has(n.id)
          const state = n.state === "waiting" ? t("manage:notify.waiting") : n.state === "held" ? t("manage:notify.held") : n.state === "submitted" ? t("manage:notify.submitted") : n.state === "cancelled" ? t("manage:notify.cancelled") : n.state === "purged" ? t("manage:notify.purged") : t("manage:notify.unknownState", { state: n.state })
          return <article key={n.id} data-personal-notification={n.id} className="flex min-w-0 flex-col gap-3 border-b border-border p-4 last:border-b-0">
            <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
              <time dateTime={new Date(n.created).toISOString()}>{new Date(n.created).toLocaleString(t("manage:notify.locale"))}</time>
              <span>{state} · {t(read ? "manage:notify.read" : "manage:notify.unread")}</span>
            </div>
            <p className="whitespace-pre-wrap text-sm [overflow-wrap:anywhere]">{n.body ?? t("manage:notify.bodyPurged")}</p>
            {!read && <div><Button size="sm" variant="outline" disabled={busy !== null} data-personal-seen onClick={() => void markSeen(n.id)}>{t(busy === n.id ? "manage:notify.marking" : "manage:notify.markRead")}</Button></div>}
            {seenError?.id === n.id && failed(seenError.error, () => void markSeen(n.id), t("manage:notify.seenFailed"))}
          </article>
        })}
      </Panel>}
    </Section>
  </div>
}

const targetKey = (target: { party_id: string | null; space_id: string | null }) => JSON.stringify([target.party_id, target.space_id])
function PolicyForm({ client, initial, party, name, targets, targetsKnown }: { client: NotificationsClient; initial: NotificationPolicy; party?: string; name?: string; targets: NotificationTarget[]; targetsKnown: boolean }) {
  const t = useT()
  const [draft, setDraft] = useState(initial), [saved, setSaved] = useState(initial)
  const [saving, setSaving] = useState(false), [error, setError] = useState<unknown>(), [success, setSuccess] = useState(false)
  const quiet = draft.quiet_start !== null || draft.quiet_end !== null
  const patch = (p: Partial<NotificationPolicy>) => { setDraft((v) => ({ ...v, ...p })); setSuccess(false); setError(undefined) }
  const zoneValid = validTimezone(draft.timezone), quietValid = validQuiet(draft.quiet_start, draft.quiet_end)
  // Only this caller-scoped endpoint proves membership and current E1 bot reachability. /spaces
  // counterpart is a first-member/history fallback, not an exhaustive membership/route proof.
  const target = targets.find((candidate) => targetKey(candidate) === targetKey(draft))
  const canSave = zoneValid && quietValid && (!draft.enabled || (party && (draft.mode !== "now" || target)))
  const save = async () => {
    if (saving || !canSave) return
    const next: NotificationPolicy = { ...draft, timezone: draft.timezone.trim(), party_id: draft.mode === "now" ? draft.party_id : party ?? (draft.enabled ? null : saved.party_id), space_id: draft.mode === "now" ? draft.space_id : null }
    setSaving(true); setError(undefined); setSuccess(false)
    try { await client.save(next); setSaved(next); setDraft(next); setSuccess(true) }
    catch (e) { setError(e) }
    finally { setSaving(false) }
  }
  const timeLabels = { show: t("manage:notify.showTimes"), hide: t("manage:notify.hideTimes"), times: t("manage:notify.times") }
  return <form data-notification-policy onSubmit={(e) => { e.preventDefault(); void save() }} className="flex min-w-0 flex-col gap-4">
    <fieldset disabled={saving} className="min-w-0">
      <Panel>
        <Row title={t("manage:notify.enabled")} sub={t("manage:notify.enabledSub")}><Switch aria-label={t("manage:notify.enabled")} data-notify-enabled checked={draft.enabled} onChange={(e) => patch({ enabled: e.currentTarget.checked })} /></Row>
        <Row title={t("manage:notify.mode")} className="max-sm:flex-col max-sm:items-stretch">
          <Select aria-label={t("manage:notify.mode")} data-notify-mode className="w-52 max-w-full max-sm:w-full" value={draft.mode} onChange={(mode) => patch({ mode: mode as NotificationPolicy["mode"] })} options={[
            { value: "now", label: t("manage:notify.now"), disabled: !targets.length },
            { value: "digest", label: t("manage:notify.digest") }, { value: "log", label: t("manage:notify.log") },
          ]} />
        </Row>
        <Row title={t("manage:notify.recipient")} sub={t("manage:notify.recipientSub")}><span className="text-sm">{party ? name?.trim() || t("manage:notify.selfIdentity") : t("manage:notify.identityMissing")}</span></Row>
        {draft.mode === "now" && <Row title={t("manage:notify.target")} className="max-sm:flex-col max-sm:items-stretch">
          <Select aria-label={t("manage:notify.target")} data-notify-target className="w-64 max-w-full max-sm:w-full" placeholder={t("manage:notify.chooseTarget")} value={target || draft.space_id ? targetKey(draft) : null} onChange={(key) => { const next = targets.find((candidate) => targetKey(candidate) === key); if (next) patch({ party_id: next.party_id, space_id: next.space_id }) }} options={[
            ...(!target && draft.space_id ? [{ value: targetKey(draft), label: t("manage:notify.savedTargetUnavailable"), disabled: true }] : []),
            ...targets.map((candidate) => ({ value: targetKey(candidate), label: candidate.space_name.trim() || t("manage:notify.unnamedConversation"), description: `${candidate.party_name.trim() || t("manage:notify.selfIdentity")} · ${candidate.channel}` })),
          ]} />
        </Row>}
      </Panel>
      <p className="mt-3 text-sm text-muted-foreground" data-notify-mode-scope>{t(draft.mode === "now" ? "manage:notify.nowScope" : draft.mode === "digest" ? "manage:notify.digestScope" : "manage:notify.logScope")}</p>
      {targetsKnown && !targets.length && <p className="mt-2 text-sm text-muted-foreground" data-notify-no-targets>{t("manage:notify.noTargets")}</p>}
      {draft.mode === "now" && !target && <p role="alert" className="mt-2 text-sm text-destructive">{t(draft.space_id ? "manage:notify.savedTargetUnavailable" : "manage:notify.chooseTarget")}</p>}
      <div className="mt-4 flex flex-col gap-4 rounded-xl border border-border p-4">
        <TextField label={t("manage:notify.timezone")} hint={t("manage:notify.timezoneSub")} value={draft.timezone} data-notify-timezone onChange={(e) => patch({ timezone: e.currentTarget.value })} error={!zoneValid ? t("manage:notify.zoneInvalid") : undefined} />
        <Switch label={t("manage:notify.quietOn")} description={t("manage:notify.quietSub")} data-notify-quiet checked={quiet} onChange={(e) => patch(e.currentTarget.checked ? { quiet_start: 22 * 60, quiet_end: 7 * 60 } : { quiet_start: null, quiet_end: null })} />
        {quiet && <div className="grid min-w-0 gap-4 sm:grid-cols-2" data-notify-quiet-times>
          <TimePicker label={t("manage:notify.quietStart")} value={minuteTime(draft.quiet_start)} onChange={(v) => patch({ quiet_start: timeMinute(v) })} hourCycle={24} locale={t("manage:notify.locale")} labels={timeLabels} />
          <TimePicker label={t("manage:notify.quietEnd")} value={minuteTime(draft.quiet_end)} onChange={(v) => patch({ quiet_end: timeMinute(v) })} hourCycle={24} locale={t("manage:notify.locale")} labels={timeLabels} />
          {!quietValid && <p role="alert" className="text-sm text-destructive sm:col-span-2">{t("manage:notify.quietInvalid")}</p>}
        </div>}
      </div>
    </fieldset>
    {draft.enabled && !saved.enabled && <Notice><span data-notify-consent>{t("manage:notify.consent")}</span></Notice>}
    {error != null && <p role="alert" className="text-sm text-destructive [overflow-wrap:anywhere]" data-notify-save-error>{t("manage:notify.saveFailed", { error: errText(error) })}</p>}
    <div className="flex flex-wrap items-center gap-3">
      <Button type="submit" disabled={saving || !canSave} data-notify-save>{t(saving ? "manage:notify.saving" : "common:action.save")}</Button>
      <span role="status" className="text-sm text-muted-foreground" data-notify-save-status>{success ? t("common:state.saved") : t(saved.enabled ? "manage:notify.savedEnabled" : "manage:notify.savedDisabled")}</span>
    </div>
  </form>
}
