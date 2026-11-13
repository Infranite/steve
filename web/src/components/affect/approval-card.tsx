/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";

/*
 * Approval Card – an AI agent asks before it does something consequential, in a single self-contained file.
 * A header with the action, a risk badge and a preview (recipients, key-value details), then
 * "Always allow this action" in a white card; Approve (⌘↵) / Deny sit on the gray tray below it.
 * An optional countdown ("Expires in 0:42") sits quietly in the tray and pauses while you hover or
 * focus the card. After a decision the tray footer folds away into a compact row: the check or cross
 * draws in and the height glides down.
 * The styles are injected below, so no extra files are needed.
 */

export type ApprovalStatus = "pending" | "approved" | "denied";
export type ApprovalRisk = "low" | "medium" | "high";
export type ApprovalDetail = { label: string; value: ReactNode };
export type DenyReason = "user" | "timeout";

export type ApprovalCardProps = {
  /** The action, e.g. "Send campaign email" */
  title: string;
  /** One line of context below the title */
  description?: ReactNode;
  /** Tinted badge in the header; leave out to hide it */
  risk?: ApprovalRisk;
  /** Icon in the header tile – defaults to a small shield */
  icon?: ReactNode;
  /** Key-value rows in the preview, e.g. To, Subject, Scheduled */
  details?: ApprovalDetail[];
  /** Anything to show above the details, e.g. a recipient list – children work too */
  preview?: ReactNode;
  children?: ReactNode;
  /** Controlled status */
  status?: ApprovalStatus;
  /** Starting status when uncontrolled */
  defaultStatus?: ApprovalStatus;
  /** Return a promise to show a spinner and lock the card until it settles; a rejection keeps it pending */
  onApprove?: (decision: { always: boolean }) => void | Promise<unknown>;
  onDeny?: (decision: { reason: DenyReason }) => void;
  /** Shows an "Edit" button */
  onEdit?: () => void;
  /** Seconds until the request expires and is denied with reason "timeout" */
  expiresIn?: number;
  /** Called when the countdown runs out (onDeny is called too) */
  onExpire?: () => void;
  /** Show the "Always allow this action" checkbox */
  allowAlways?: boolean;
  alwaysLabel?: string;
  approveLabel?: string;
  denyLabel?: string;
  editLabel?: string;
  /** Show the ⌘↵ hint on the buttons (the shortcuts work either way) */
  shortcuts?: boolean;
  /** Small label on the gray tray above the card, e.g. "Step 4 · gmail.send" */
  label?: ReactNode;
  /** Where the buttons sit: on the gray tray below the card (default) or inside the white card */
  actions?: "tray" | "card";
  /** Steve: the words of the resolved row */
  resolvedLabels?: { approved: string; denied: string; expired: string };
  /** Steve: attributes on the buttons (test hooks) */
  approveAttrs?: Record<string, string | undefined>;
  denyAttrs?: Record<string, string | undefined>;
  /** Steve: attributes on the card's root (test hooks, the decision's ids) */
  attrs?: Record<string, string | undefined>;
  className?: string;
};

const RISK_LABEL: Record<ApprovalRisk, string> = { low: "Low", medium: "Medium", high: "High" };

export function ApprovalCard({
  title,
  description,
  risk,
  icon,
  details,
  preview,
  children,
  status: statusProp,
  defaultStatus = "pending",
  onApprove,
  onDeny,
  onEdit,
  expiresIn,
  onExpire,
  allowAlways = true,
  alwaysLabel = "Always allow this action",
  approveLabel = "Approve",
  denyLabel = "Deny",
  editLabel = "Edit",
  shortcuts = true,
  label,
  actions = "tray",
  resolvedLabels = { approved: "Approved", denied: "Denied", expired: "Expired" },
  approveAttrs,
  denyAttrs,
  attrs,
  className,
}: ApprovalCardProps) {
  const id = useId();
  const [inner, setInner] = useState<ApprovalStatus>(defaultStatus);
  const status = statusProp ?? inner;
  const resolved = status !== "pending";

  const [always, setAlways] = useState(false);
  const [busy, setBusy] = useState(false);
  const [reason, setReason] = useState<DenyReason>("user");
  const [decidedAt, setDecidedAt] = useState<number | null>(() => (resolved ? Date.now() : null));
  const [announce, setAnnounce] = useState("");
  const [hover, setHover] = useState(false);
  const [focusIn, setFocusIn] = useState(false);
  const [mod, setMod] = useState("⌘");

  // Status changes are handled during render, so the leaving view and the new one animate in the same frame
  const [prev, setPrev] = useState(status);
  const [leaving, setLeaving] = useState(false);
  const [entering, setEntering] = useState(false);
  if (prev !== status) {
    setPrev(status);
    if (status === "pending") {
      setLeaving(false);
      setEntering(true);
      setAlways(false);
      setBusy(false);
      setReason("user");
      setDecidedAt(null);
      setAnnounce("");
    } else {
      setLeaving(prev === "pending");
      setEntering(false);
      setDecidedAt(Date.now());
      setAnnounce(`${status === "approved" ? resolvedLabels.approved : reason === "timeout" ? resolvedLabels.expired : resolvedLabels.denied}: ${title}`);
    }
  }

  const root = useRef<HTMLDivElement>(null);
  const row = useRef<HTMLDivElement>(null);
  const approveBtn = useRef<HTMLButtonElement>(null);
  const rowFocused = useRef(false);

  // ⌘ on Apple devices, Ctrl elsewhere
  useEffect(() => {
    const p = (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform ?? navigator.platform ?? "";
    if (!/mac|iphone|ipad|ipod/i.test(p || navigator.userAgent)) setMod("Ctrl");
  }, []);

  /* ---------- Decisions ---------- */

  const approve = async () => {
    if (resolved || busy) return;
    const result = onApprove?.({ always });
    if (result && typeof (result as Promise<unknown>).then === "function") {
      setBusy(true);
      try {
        await result;
      } catch {
        setBusy(false);
        return;
      }
      setBusy(false);
    }
    if (statusProp === undefined) setInner("approved");
  };

  const deny = (why: DenyReason = "user") => {
    if (resolved || (busy && why === "user")) return;
    setReason(why);
    if (why === "timeout") onExpire?.();
    onDeny?.({ reason: why });
    if (statusProp === undefined) setInner("denied");
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (resolved || busy || e.defaultPrevented) return;
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void approve();
    } else if (e.key === "Escape") {
      /* Steve: Esc only lets go of the card – a final deny takes the Deny button, so "get me out" never refuses by accident */
      e.preventDefault();
      (document.activeElement as HTMLElement | null)?.blur?.();
    }
  };

  /* ---------- Countdown ---------- */

  // NaN, Infinity and 0 mean no countdown
  const total = typeof expiresIn === "number" && Number.isFinite(expiresIn) && expiresIn > 0 ? expiresIn * 1000 : 0;
  const left = useRef(total);
  const [secondsLeft, setSecondsLeft] = useState(Math.ceil(total / 1000));
  const expire = useRef(() => {});
  useLayoutEffect(() => {
    expire.current = () => deny("timeout");
  });

  // A fresh request gets the full time again
  useEffect(() => {
    if (status !== "pending") return;
    left.current = total;
    setSecondsLeft(Math.ceil(total / 1000));
  }, [status, total]);

  const paused = hover || focusIn || busy;
  useEffect(() => {
    if (!total || resolved || paused) return;
    let last = performance.now();
    let raf = 0;
    const tick = (now: number) => {
      left.current -= now - last;
      last = now;
      setSecondsLeft(Math.max(0, Math.ceil(left.current / 1000)));
      if (left.current <= 0) {
        expire.current();
        return;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [total, resolved, paused]);

  /* ---------- Height morph ---------- */

  const lastH = useRef(0);
  const animating = useRef(false);
  const mounted = useRef(false);

  // Remember the settled height, so a status change knows where to animate from
  useEffect(() => {
    const el = root.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (!animating.current) lastH.current = el.offsetHeight;
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useLayoutEffect(() => {
    const el = root.current;
    if (!el) return;
    if (!mounted.current) {
      mounted.current = true;
      lastH.current = el.offsetHeight;
      return;
    }

    // Keep focus in the card: on the resolved row, or back on Approve for a new request
    const active = document.activeElement;
    if (resolved && active && el.contains(active)) row.current?.focus({ preventScroll: true });
    if (!resolved && rowFocused.current) approveBtn.current?.focus({ preventScroll: true });
    rowFocused.current = false;

    const from = lastH.current;
    el.style.height = "";
    const to = el.offsetHeight;
    if (!from || from === to) return;
    animating.current = true;
    el.style.height = `${from}px`;
    void el.offsetHeight;
    el.style.height = `${to}px`;
    const t = window.setTimeout(() => {
      el.style.height = "";
      animating.current = false;
      lastH.current = el.offsetHeight;
    }, 560);
    return () => {
      window.clearTimeout(t);
      el.style.height = "";
      animating.current = false;
    };
  }, [resolved]);

  // "just now" turns into "2 min ago" over time
  const [, setNow] = useState(0);
  useEffect(() => {
    if (!resolved) return;
    const t = window.setInterval(() => setNow((n) => n + 1), 30000);
    return () => window.clearInterval(t);
  }, [resolved]);

  /* ---------- Render ---------- */

  const titleId = `${id}-title`;
  const rowTitleId = `${id}-row`;
  const descId = `${id}-desc`;
  const timerId = `${id}-timer`;
  const body = preview ?? children;
  const resolvedLabel =
    status === "approved" ? resolvedLabels.approved : reason === "timeout" ? resolvedLabels.expired : resolvedLabels.denied;
  const describedBy = [description ? descId : null, total && !resolved ? timerId : null].filter(Boolean).join(" ");

  // The countdown and the decision – on the tray below the card, or inside it with actions="card"
  const foot = (
    <div className="ui-ap__foot">
      {total > 0 && (
        <span className="ui-ap__expires" data-paused={paused || undefined} data-low={secondsLeft <= 10 || undefined} aria-hidden="true">
          <svg viewBox="0 0 16 16">
            <circle cx="8" cy="8" r="6" />
            <path d="M8 5v3.2l2 1.3" />
          </svg>
          Expires in {clock(secondsLeft)}
        </span>
      )}
      <div className="ui-ap__actions">
        {onEdit && (
          <button
            type="button"
            className="ui-ap__btn ui-ap__btn--ghost"
            aria-disabled={busy || undefined}
            onClick={() => !busy && onEdit()}
          >
            {editLabel}
          </button>
        )}
        <button
          type="button"
          className="ui-ap__btn ui-ap__btn--quiet ui-ap__deny"
          aria-disabled={busy || undefined}
          onClick={() => deny("user")}
          {...denyAttrs}
        >
          {denyLabel}
        </button>
        <button
          ref={approveBtn}
          type="button"
          className="ui-ap__btn ui-ap__btn--primary ui-ap__approve"
          aria-keyshortcuts="Meta+Enter Control+Enter"
          aria-disabled={busy || undefined}
          onClick={() => void approve()}
          {...approveAttrs}
        >
          {approveLabel}
          {busy ? (
            <svg className="ui-ap__spinner" viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="8" cy="8" r="6" />
            </svg>
          ) : (
            shortcuts && (
              <kbd className="ui-ap__kbd" aria-hidden="true">
                {mod === "⌘" ? "⌘↵" : "Ctrl ↵"}
              </kbd>
            )
          )}
        </button>
      </div>
    </div>
  );

  return (
    <div
      ref={root}
      className={["ui-ap", className].filter(Boolean).join(" ")}
      role="group"
      aria-labelledby={resolved ? rowTitleId : titleId}
      aria-describedby={describedBy || undefined}
      aria-busy={busy || undefined}
      data-status={status}
      data-busy={busy || undefined}
      data-label={label ? "" : undefined}
      data-actions={actions}
      {...attrs}
      onKeyDown={onKeyDown}
      onPointerEnter={() => setHover(true)}
      onPointerLeave={() => setHover(false)}
      onFocus={() => setFocusIn(true)}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocusIn(false);
      }}
    >
      {/* React 19 hoists this into <head> once, no matter how many cards render */}
      <style href="ui-approval-card" precedence="default">
        {css}
      </style>

      {/* The label sits on the gray tray, above the card */}
      {label && (
        <div className="ui-ap__label" title={typeof label === "string" ? label : undefined}>
          {label}
        </div>
      )}

      {(!resolved || leaving) && (
        <div
          className="ui-ap__pending"
          data-leaving={leaving || undefined}
          data-entering={entering || undefined}
          inert={leaving}
          aria-hidden={leaving || undefined}
          onAnimationEnd={(e) => {
            if (e.target !== e.currentTarget) return;
            if (leaving) setLeaving(false);
            if (entering) setEntering(false);
          }}
        >
          <div className="ui-ap__card">
            <div className="ui-ap__head">
              <span className="ui-ap__icon" aria-hidden="true">
                {icon ?? (
                  <svg viewBox="0 0 24 24">
                    <path d="M12 3 5 5.8v5.4c0 4.3 2.9 8.2 7 9.8 4.1-1.6 7-5.5 7-9.8V5.8L12 3Z" />
                    <path d="m9.2 12.2 2 2 3.8-4" />
                  </svg>
                )}
              </span>
              <div className="ui-ap__titles">
                <h3 className="ui-ap__title" dir="auto" title={title} id={leaving ? undefined : titleId}>
                  {title}
                </h3>
                {description && (
                  <p className="ui-ap__desc" dir="auto" title={typeof description === "string" ? description : undefined} id={leaving ? undefined : descId}>
                    {description}
                  </p>
                )}
              </div>
              {risk && (
                <span className="ui-ap__risk" data-risk={risk}>
                  <span className="ui-ap__risk-dot" aria-hidden="true" />
                  {RISK_LABEL[risk]}
                  <span className="ui-ap__risk-word"> risk</span>
                </span>
              )}
            </div>

            {(body || details?.length) && (
              <div className="ui-ap__preview">
                {body && <div className="ui-ap__custom">{body}</div>}
                {details?.length ? (
                  <dl className="ui-ap__details">
                    {details.map((d, i) => (
                      <div className="ui-ap__detail" key={d.label + i}>
                        <dt dir="auto" title={d.label}>{d.label}</dt>
                        <dd dir="auto">{d.value}</dd>
                      </div>
                    ))}
                  </dl>
                ) : null}
              </div>
            )}

            {allowAlways && (
              <label className="ui-ap__always">
                <span className="ui-ap__box">
                  <input
                    type="checkbox"
                    checked={always}
                    disabled={busy}
                    onChange={(e) => setAlways(e.target.checked)}
                  />
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <path d="M4 8.3 6.8 11 12 5.2" />
                  </svg>
                </span>
                <span className="ui-ap__always-label">{alwaysLabel}</span>
              </label>
            )}
            {actions === "card" && foot}
          </div>

          {actions === "tray" && foot}

          {total > 0 && (
            <span className="ui-ap__sr" id={leaving ? undefined : timerId}>
              {`Expires in ${spoken(secondsLeft)} unless approved. The timer pauses while the card is hovered or focused.`}
            </span>
          )}
        </div>
      )}

      {resolved && (
        <div
          ref={row}
          className="ui-ap__card ui-ap__resolved"
          tabIndex={-1}
          data-status={status}
          onFocus={() => (rowFocused.current = true)}
          onBlur={(e) => {
            // A blur from being removed (the card resets) keeps the flag, so focus can return to Approve
            if (e.relatedTarget || e.currentTarget.isConnected) rowFocused.current = false;
          }}
        >
          <span className="ui-ap__mark" aria-hidden="true">
            <svg viewBox="0 0 16 16">
              {status === "approved" ? (
                <path d="M4.2 8.4 6.9 11 11.8 5.6" pathLength={1} />
              ) : (
                <path d="M5.2 5.2 10.8 10.8M10.8 5.2 5.2 10.8" pathLength={1} />
              )}
            </svg>
          </span>
          <span className="ui-ap__row-title" dir="auto" title={title} id={rowTitleId}>
            {title}
          </span>
          <span className="ui-ap__row-status">
            {resolvedLabel}
            {status === "approved" && always && <span className="ui-ap__row-extra"> · Always</span>}
            {decidedAt !== null && <span className="ui-ap__row-time"> · {ago(decidedAt)}</span>}
          </span>
        </div>
      )}

      <span className="ui-ap__sr" role="status" aria-live="polite">
        {announce}
      </span>
    </div>
  );
}

/** 0:42, 2:05 – and 1:05:00 once it runs past an hour */
function clock(s: number) {
  const pad = (n: number) => String(n).padStart(2, "0");
  if (s < 3600) return `${Math.floor(s / 60)}:${pad(s % 60)}`;
  return `${Math.floor(s / 3600)}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
}

/** For screen readers: 42 seconds, 5 minutes, 26 hours */
function spoken(s: number) {
  const [n, unit] = s < 120 ? [s, "second"] : s < 7200 ? [Math.round(s / 60), "minute"] : [Math.round(s / 3600), "hour"];
  return `${n} ${unit}${n === 1 ? "" : "s"}`;
}

/** just now, 4 min ago, 2 h ago */
function ago(t: number) {
  const s = Math.floor((Date.now() - t) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-ap */
.ui-ap {
  --ui-ap-tray: var(--aui-subtle, #f0f0f0);
  --ui-ap-bg: var(--aui-surface, #ffffff);
  --ui-ap-ink: var(--aui-ink, #171717);
  --ui-ap-text: var(--aui-muted, #6b6b6b);
  --ui-ap-muted: var(--aui-muted, #8a8a8a);
  --ui-ap-line: var(--aui-border, #ececec);
  --ui-ap-soft: var(--aui-subtle, #fafafa);
  --ui-ap-tile: var(--aui-subtle, #f4f4f4);
  --ui-ap-hover: color-mix(in srgb, var(--aui-ink, #171717) 8%, transparent);
  --ui-ap-primary: var(--aui-primary, #171717);
  --ui-ap-primary-hover: color-mix(in srgb, var(--aui-primary, #171717) 88%, var(--aui-ink, #000));
  --ui-ap-primary-ink: var(--aui-on-primary, #ffffff);
  --ui-ap-primary-kbd: rgba(255, 255, 255, 0.14);
  --ui-ap-primary-kbd-ink: rgba(255, 255, 255, 0.72);
  --ui-ap-kbd: color-mix(in srgb, var(--aui-ink, #000) 6%, transparent);
  --ui-ap-box: var(--aui-border-strong, #d4d4d4);
  --ui-ap-box-hover: var(--aui-muted, #b5b5b5);
  --ui-ap-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-ap-timer-low: var(--aui-warning, #b25e00);
  --ui-ap-low-bg: var(--aui-subtle, #f2f2f2);
  --ui-ap-low-ink: var(--aui-muted, #6b6b6b);
  --ui-ap-med-bg: var(--aui-warning-soft, #fbf5e2);
  --ui-ap-med-ink: var(--aui-warning, #8f6c00);
  --ui-ap-high-bg: var(--aui-danger-soft, #fdedeb);
  --ui-ap-high-ink: var(--aui-danger, #c4320a);
  --ui-ap-ok-bg: var(--aui-success-soft, #e9f6ee);
  --ui-ap-ok-ink: var(--aui-success, #1e8a4f);
  --ui-ap-no-bg: var(--aui-subtle, #f2f2f2);
  --ui-ap-no-ink: var(--aui-muted, #6b6b6b);
  --ui-ap-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  position: relative;
  box-sizing: border-box;
  width: 100%;
  min-width: 0;
  overflow: hidden;
  container-type: inline-size;
  padding: 4px;
  border-radius: calc(var(--aui-radius-lg, 16px) + 4px);
  background: var(--ui-ap-tray);
  color: var(--ui-ap-text);
  font-family: var(--aui-font, "Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 1.45;
  letter-spacing: -0.006em;
  text-align: left;
  -webkit-font-smoothing: antialiased;
  transition: height 520ms var(--ui-ap-ease);
}
.ui-ap *,
.ui-ap *::before,
.ui-ap *::after {
  box-sizing: border-box;
}

/* The white card – the pending request and the resolved row both live in one */
.ui-ap__card {
  position: relative;
  border-radius: var(--aui-radius-lg, 16px);
  background: var(--ui-ap-bg);
  box-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-ap-line), 0 1px 2px rgba(0, 0, 0, 0.04));
}

/* ---------- Pending ---------- */
.ui-ap__pending > .ui-ap__card {
  padding: 16px;
}
.ui-ap__pending[data-leaving] {
  position: absolute;
  top: 4px;
  left: 4px;
  right: 4px;
  pointer-events: none;
  animation: ui-ap-out 280ms var(--ui-ap-ease) both;
}
.ui-ap__pending[data-entering] {
  animation: ui-ap-in 420ms var(--ui-ap-ease) 80ms both;
}
.ui-ap[data-busy] .ui-ap__pending {
  pointer-events: none;
}

.ui-ap__head {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: start;
  gap: 12px;
}
.ui-ap__icon {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  border-radius: 9px;
  background: var(--ui-ap-tile);
  box-shadow: inset 0 0 0 1px var(--ui-ap-line);
  color: var(--ui-ap-ink);
}
.ui-ap__icon svg {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.ui-ap__titles {
  min-width: 0;
  padding-top: 1px;
}
.ui-ap__title {
  margin: 0;
  overflow: hidden;
  color: var(--ui-ap-ink);
  font-size: 14px;
  font-weight: 550;
  line-height: 20px;
  letter-spacing: -0.011em;
  text-align: start;
  white-space: nowrap;
  text-overflow: ellipsis;
}
/* Secondary context: one line, the whole of it in the tooltip */
.ui-ap__desc {
  margin: 1px 0 0;
  overflow: hidden;
  font-size: 13px;
  line-height: 18px;
  text-align: start;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.ui-ap__risk {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 22px;
  margin-top: 1px;
  padding: 0 9px 0 8px;
  border-radius: 999px;
  background: var(--ui-ap-low-bg);
  color: var(--ui-ap-low-ink);
  font-size: 12px;
  font-weight: 500;
  line-height: 1;
  white-space: nowrap;
}
.ui-ap__risk-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
  opacity: 0.8;
}
.ui-ap__risk-word {
  white-space: pre;
}
.ui-ap__risk[data-risk="medium"] {
  background: var(--ui-ap-med-bg);
  color: var(--ui-ap-med-ink);
}
.ui-ap__risk[data-risk="high"] {
  background: var(--ui-ap-high-bg);
  color: var(--ui-ap-high-ink);
}

/* Preview: custom content and key-value rows in one soft panel */
.ui-ap__preview {
  display: grid;
  gap: 12px;
  max-height: 360px;
  overflow-y: auto;
  overscroll-behavior: contain;
  margin-top: 14px;
  padding: 12px;
  border-radius: calc(var(--aui-radius-lg, 16px) - 6px);
  background: var(--ui-ap-soft);
  box-shadow: inset 0 0 0 1px var(--ui-ap-line);
}
.ui-ap__custom {
  min-width: 0;
}
.ui-ap__custom + .ui-ap__details {
  padding-top: 12px;
  border-top: 1px solid var(--ui-ap-line);
}
.ui-ap__details {
  display: grid;
  /* Long labels take at most 40% (and 180px) – the values are what you approve */
  grid-template-columns: fit-content(min(40%, 180px)) minmax(0, 1fr);
  gap: 6px 16px;
  margin: 0;
}
.ui-ap__detail {
  display: contents;
}
.ui-ap__detail dt {
  min-width: 0;
  overflow: hidden;
  color: var(--ui-ap-muted);
  /* Wraps between words only – a word too long for the column ends in an ellipsis, the full label is in the tooltip */
  overflow-wrap: normal;
  text-overflow: ellipsis;
}
.ui-ap__detail dd {
  min-width: 0;
  margin: 0;
  overflow-x: auto;
  color: var(--ui-ap-ink);
  text-align: start;
  overflow-wrap: anywhere;
}

/* "Always allow" closes the card */
.ui-ap__always {
  display: flex;
  width: fit-content;
  max-width: 100%;
  margin-top: 14px;
  align-items: flex-start;
  gap: 8px;
  min-width: 0;
  cursor: pointer;
  user-select: none;
}
.ui-ap__always-label {
  min-width: 0;
  overflow-wrap: anywhere;
}
.ui-ap__box {
  position: relative;
  display: grid;
  flex: none;
  margin-top: 1.5px;
  width: 16px;
  height: 16px;
}
.ui-ap__box input {
  appearance: none;
  -webkit-appearance: none;
  width: 16px;
  height: 16px;
  margin: 0;
  border-radius: 5px;
  background: var(--ui-ap-bg);
  box-shadow: inset 0 0 0 1px var(--ui-ap-box);
  cursor: pointer;
  transition: background-color 160ms ease, box-shadow 160ms ease;
}
.ui-ap__box input:hover {
  box-shadow: inset 0 0 0 1px var(--ui-ap-box-hover);
}
.ui-ap__box input:checked {
  background: var(--ui-ap-primary);
  box-shadow: inset 0 0 0 1px var(--ui-ap-primary);
}
.ui-ap__box input:focus-visible {
  outline: 2px solid var(--ui-ap-focus);
  outline-offset: 2px;
}
.ui-ap__box svg {
  position: absolute;
  inset: 0;
  width: 16px;
  height: 16px;
  fill: none;
  stroke: var(--ui-ap-primary-ink);
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
  opacity: 0;
  transform: scale(0.6);
  transition: opacity 160ms ease, transform 260ms var(--ui-ap-ease);
  pointer-events: none;
}
.ui-ap__box input:checked + svg {
  opacity: 1;
  transform: none;
}

/* Optional label on the gray tray above the card */
.ui-ap__label {
  overflow: hidden;
  padding: 6px 8px 4px;
  color: var(--ui-ap-muted);
  font-size: 12.5px;
  line-height: 16px;
  white-space: nowrap;
  text-overflow: ellipsis;
}
/* The leaving request is laid over the resolved row – below the label when there is one */
.ui-ap[data-label] .ui-ap__pending[data-leaving] { top: 30px; }

/* actions="card": the buttons close the white card instead of sitting on the tray */
.ui-ap__card > .ui-ap__foot { min-height: 0; margin-top: 14px; padding: 0; }

/* The tray below the card: countdown on the left, buttons on the right */
.ui-ap__foot {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px 12px;
  min-height: 44px;
  padding: 6px 2px 2px 10px;
}
.ui-ap__expires {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  color: var(--ui-ap-muted);
  font-size: 12.5px;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  transition: color 300ms ease;
}
.ui-ap__expires svg {
  width: 14px;
  height: 14px;
  flex: none;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.5;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.ui-ap__expires[data-low] {
  color: var(--ui-ap-timer-low);
}
.ui-ap__actions {
  display: flex;
  flex: 0 1 auto;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 4px;
  min-width: 0;
  margin-left: auto;
}
.ui-ap__btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  height: 34px;
  padding: 0 12px;
  border: 0;
  border-radius: var(--aui-radius, 10px);
  font: inherit;
  font-size: 13px;
  font-weight: 500;
  line-height: 1;
  white-space: nowrap;
  cursor: pointer;
  -webkit-tap-highlight-color: transparent;
  transition: background-color 160ms ease, box-shadow 160ms ease, transform 160ms var(--ui-ap-ease), opacity 160ms ease;
}
.ui-ap__btn:active,
.ui-ap__btn[data-pressed] {
  transform: scale(0.97);
}
.ui-ap__btn:focus-visible {
  outline: 2px solid var(--ui-ap-focus);
  outline-offset: 2px;
}
.ui-ap__btn--primary {
  padding-right: 8px;
  background: var(--ui-ap-primary);
  color: var(--ui-ap-primary-ink);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
}
.ui-ap__btn--primary:hover,
.ui-ap__btn--primary[data-pressed] {
  background: var(--ui-ap-primary-hover);
}
/* Quiet: transparent on the tray, a soft fill on hover */
.ui-ap__btn--quiet {
  padding-right: 8px;
  background: transparent;
  color: var(--ui-ap-ink);
}
.ui-ap__btn--quiet:hover,
.ui-ap__btn--quiet[data-pressed] {
  background: var(--ui-ap-hover);
}
.ui-ap__btn--ghost {
  background: transparent;
  color: var(--ui-ap-text);
}
.ui-ap__btn--ghost:hover {
  background: var(--ui-ap-hover);
  color: var(--ui-ap-ink);
}
.ui-ap[data-busy] .ui-ap__btn:not(.ui-ap__approve),
.ui-ap[data-busy] .ui-ap__always {
  opacity: 0.5;
}
.ui-ap[data-busy] .ui-ap__approve {
  cursor: progress;
}

.ui-ap__kbd {
  display: inline-grid;
  place-items: center;
  min-width: 20px;
  height: 20px;
  padding: 0 5px;
  border-radius: calc(var(--aui-radius-sm, 7px) - 1px);
  background: var(--ui-ap-kbd);
  color: var(--ui-ap-muted);
  font: inherit;
  font-size: 11px;
  font-weight: 500;
  line-height: 1;
}
.ui-ap__btn--primary .ui-ap__kbd {
  background: var(--ui-ap-primary-kbd);
  color: var(--ui-ap-primary-kbd-ink);
}
.ui-ap__spinner {
  width: 20px;
  height: 14px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-dasharray: 26 38;
  transform-origin: 50% 50%;
  animation: ui-ap-spin 800ms linear infinite;
}

/* ---------- Resolved row ---------- */
.ui-ap__resolved {
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 48px;
  padding: 12px 14px;
  outline: none;
  animation: ui-ap-in 420ms var(--ui-ap-ease) 120ms both;
}
.ui-ap__resolved:focus-visible {
  outline: 2px solid var(--ui-ap-focus);
  outline-offset: -2px;
}
.ui-ap__mark {
  display: grid;
  flex: none;
  place-items: center;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  background: var(--ui-ap-ok-bg);
  color: var(--ui-ap-ok-ink);
  animation: ui-ap-pop 460ms var(--ui-ap-ease) 140ms both;
}
.ui-ap__resolved[data-status="denied"] .ui-ap__mark {
  background: var(--ui-ap-no-bg);
  color: var(--ui-ap-no-ink);
}
.ui-ap__mark svg {
  width: 14px;
  height: 14px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.ui-ap__mark path {
  stroke-dasharray: 1;
  stroke-dashoffset: 1;
  animation: ui-ap-draw 420ms var(--ui-ap-ease) 300ms forwards;
}
.ui-ap__row-title {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  color: var(--ui-ap-ink);
  font-size: 13.5px;
  font-weight: 500;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.ui-ap__row-status {
  flex: none;
  color: var(--ui-ap-muted);
  white-space: nowrap;
}

.ui-ap__sr {
  position: absolute;
  width: 1px;
  height: 1px;
  margin: -1px;
  padding: 0;
  overflow: hidden;
  clip: rect(0 0 0 0);
  white-space: nowrap;
  border: 0;
}

/* Narrow cards: buttons share the full width, the hints go */
@container (max-width: 400px) {
  .ui-ap__actions {
    flex: 1 1 100%;
    margin-left: 0;
  }
  .ui-ap__foot {
    padding-left: 2px;
  }
  .ui-ap__expires {
    padding-left: 8px;
  }
  /* Equal widths – unless a long label needs more, then it takes a row of its own */
  .ui-ap__actions .ui-ap__btn {
    flex: 1 1 0;
    padding: 0 10px;
  }
  .ui-ap__kbd {
    display: none;
  }
}
@container (max-width: 340px) {
  .ui-ap__pending > .ui-ap__card { padding: 14px; }
  .ui-ap__head { gap: 10px; }
  .ui-ap__risk-word { display: none; }
  .ui-ap__row-time { display: none; }
  .ui-ap__details { column-gap: 12px; }
}
/* Touch screens have no shortcuts */
@media (hover: none) {
  .ui-ap__kbd { display: none; }
}

@keyframes ui-ap-out {
  to { opacity: 0; transform: translateY(-6px) scale(0.985); filter: blur(2px); }
}
@keyframes ui-ap-in {
  from { opacity: 0; transform: translateY(4px); filter: blur(2px); }
  to { opacity: 1; transform: none; filter: none; }
}
@keyframes ui-ap-pop {
  from { opacity: 0; transform: scale(0.5); }
  to { opacity: 1; transform: none; }
}
@keyframes ui-ap-draw {
  to { stroke-dashoffset: 0; }
}
@keyframes ui-ap-spin {
  to { transform: rotate(360deg); }
}

@media (prefers-reduced-motion: reduce) {
  .ui-ap,
  .ui-ap *,
  .ui-ap *::before,
  .ui-ap *::after {
    animation-duration: 1ms !important;
    animation-delay: 0ms !important;
    transition-duration: 1ms !important;
  }
  .ui-ap__spinner { animation: ui-ap-spin 1.6s linear infinite !important; }
  .ui-ap__mark path { stroke-dashoffset: 0; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-ap {
  /* Steve: the colors above follow tokens that already switch with dark mode */
  --ui-ap-box-hover: rgba(255, 255, 255, 0.32);
}
`;
