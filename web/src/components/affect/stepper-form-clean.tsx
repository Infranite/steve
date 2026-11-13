/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type FormEvent,
  type InputHTMLAttributes,
  type ReactNode,
} from "react";
import { reducedMotion } from "@/lib/motion";

/*
 * Stepper Form (Clean) – a multi-step flow for onboarding, checkout or setup wizards, in a single self-contained file.
 * The clean style: one white card with hairline shadows. On top a thin progress bar per step with small uppercase
 * labels; finished steps swap their number for a blue check that draws in. Back and Continue sit in the card's last row.
 * Each step slides in from the side you're heading to, and the card glides to the new step's height.
 * Continue checks the step: invalid fields shake once and say what's wrong; Enter continues, Back keeps everything.
 * The last step submits, then a small success state takes over. On narrow screens the steps collapse
 * to "Step 2 of 4 · Team".
 * Building blocks for the steps come along: StepperField, StepperInput and StepperSuccess. (Steve: the pulled
 * StepperTags, StepperChoices and StepperReview blocks and their styles were removed, nothing here used them.)
 * The styles are injected below, so no extra files are needed.
 */

export type StepErrors = Partial<Record<string, string>>;

export type StepContext<V> = {
  values: V;
  /** Merges a patch into the values – shared by every step, so Back keeps what was entered */
  setValues: (patch: Partial<V>) => void;
  /** Messages of this step – only filled after a Continue that didn't pass */
  errors: StepErrors;
  /** Jumps to a step by id – from a later step, Continue then returns to where you came from */
  goTo: (id: string) => void;
  /** Same as pressing Continue */
  next: () => void;
};

export type Step<V> = {
  id: string;
  /** Short label for the step list, e.g. "Team" */
  title: string;
  /** Heading inside the card – defaults to the title */
  heading?: string;
  /** A line under the heading */
  description?: string;
  render: (ctx: StepContext<V>) => ReactNode;
  /** Returns messages by field name; nothing (or an empty object) means the step is fine */
  validate?: (values: V) => StepErrors | null | undefined;
};

export type StepperFormProps<V extends Record<string, unknown>> = {
  steps: Step<V>[];
  initialValues: V;
  /** Called with all values on the last step; return a promise to show "Creating…" until it settles (throw to show an error) */
  onComplete?: (values: V) => void | Promise<void>;
  /** Index of the first step shown */
  initialStep?: number;
  /** Controlled step index – pair it with onStepChange */
  step?: number;
  onStepChange?: (step: number) => void;
  /** Label of the last button */
  submitLabel?: string;
  /** Label while onComplete runs */
  submittingLabel?: string;
  /** Words the form itself shows (Steve: translated through i18n) – defaults to English */
  labels?: Partial<StepperLabels>;
  /** Extra controls in the footer, between Back and the primary button (Steve: skip the tour) */
  aside?: ReactNode;
  /** What shows once onComplete is done – defaults to a short "All set" */
  success?: (values: V, reset: () => void) => ReactNode;
  className?: string;
};

export type StepperLabels = {
  continue: string; back: string; backTo: (title: string) => string; stepOf: (n: string, total: string) => string; allDone: (total: string) => string
  progress: string; done: string; current: string; finished: string; failed: string; check: string; successTitle: string; successDesc: string
};
const EN: StepperLabels = {
  continue: "Continue", back: "Back", backTo: (t) => `Back to ${lower(t)}`, stepOf: (n, total) => `Step ${n} of ${total}`, allDone: (total) => `All ${total} steps done`,
  progress: "Progress", done: " (done)", current: " (current)", finished: "All done", failed: "Something went wrong – try again", check: "Check this step",
  successTitle: "All set", successDesc: "Everything is saved. You can change it any time.",
};

/* ---------- Helpers ---------- */

const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(" ");
const hasErrors = (e: StepErrors | null | undefined) => !!e && Object.values(e).some(Boolean);
const text = (v: unknown) => (v == null ? "" : String(v));
const fmt = (x: number) => x.toLocaleString("en-US");
/** "Billing Details" → "billing details", but "API & SSO" stays as it is */
const lower = (t: string) => t.replace(/\p{Lu}\p{Ll}+/gu, (w) => w.toLowerCase());
const reduced = () => reducedMotion();

/** A short, calm shake – Web Animations, so it can run again on the same element without remounting it */
function shake(el: HTMLElement | null) {
  if (!el || reduced() || !el.animate) return;
  el.animate(
    [{ transform: "none" }, { transform: "translateX(-4px)" }, { transform: "translateX(4px)" }, { transform: "translateX(-2px)" }, { transform: "none" }],
    { duration: 340, easing: "ease-out" },
  );
}

/* ---------- Icons ---------- */

const Glyph = ({ d, className }: { d: string; className?: string }) => (
  <svg viewBox="0 0 24 24" className={cx("ui-stpc__glyph", className)} aria-hidden="true">
    <path d={d} />
  </svg>
);
const icons = {
  check: "M5.5 12.5l4 4L18.5 8",
  close: "M7 7l10 10M17 7 7 17",
  back: "M14.5 6l-6 6 6 6",
};

/** A check that draws itself in – pathLength 1 makes the dash math independent of the path */
const DrawCheck = ({ className }: { className?: string }) => (
  <svg viewBox="0 0 24 24" className={cx("ui-stpc__draw", className)} aria-hidden="true">
    <path d={icons.check} pathLength={1} />
  </svg>
);

/* ---------- Contexts for the building blocks ---------- */

type PaneState = { errors: StepErrors; shake: number; report: (name: string, message: string | null) => void };
const PaneContext = createContext<PaneState>({ errors: {}, shake: 0, report: () => {} });

type FieldState = { id: string; invalid: boolean; describedBy?: string; flag: (message: string | null) => void };
const FieldContext = createContext<FieldState | null>(null);

/* ---------- The form ---------- */

export function StepperForm<V extends Record<string, unknown>>({
  steps,
  initialValues,
  onComplete,
  initialStep = 0,
  step,
  onStepChange,
  submitLabel = "Finish",
  submittingLabel = "Creating…",
  success,
  labels,
  aside,
  className,
}: StepperFormProps<V>) {
  const L = { ...EN, ...labels };
  const n = steps.length;
  // A missing, negative or too large index lands on the nearest step
  const safe = (i: number) => (Number.isFinite(i) ? Math.max(0, Math.min(Math.max(n - 1, 0), Math.trunc(i))) : 0);
  const [own, setOwn] = useState(initialStep);
  const index = safe(step ?? own);
  const [values, setValuesState] = useState<V>(initialValues);
  const valuesRef = useRef(values);
  const [reached, setReached] = useState(index);
  const furthest = Math.max(reached, index);
  const [attempted, setAttempted] = useState<Record<number, boolean>>({});
  const [shakes, setShakes] = useState(0);
  const [returnTo, setReturnTo] = useState<number | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState("");
  const [done, setDone] = useState(false);
  const [announce, setAnnounce] = useState("");

  const formRef = useRef<HTMLFormElement>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const btnInner = useRef<HTMLSpanElement>(null);
  const moved = useRef(false);
  // Nothing slides in on the first render
  const navigated = useRef(false);
  // Problems a building block found on its own, e.g. a typed email that isn't one yet
  const local = useRef<StepErrors>({});
  const report = useCallback((name: string, message: string | null) => {
    local.current = { ...local.current, [name]: message ?? undefined };
  }, []);
  const uid = useId();

  /* Which pane shows, and the one sliding out – derived during render, so both are there in the same frame */
  const viewIndex = done ? n : index;
  const [view, setView] = useState<{ index: number; from: number | null; dir: 1 | -1 }>({ index: viewIndex, from: null, dir: 1 });
  if (view.index !== viewIndex) setView({ index: viewIndex, from: view.index, dir: viewIndex > view.index ? 1 : -1 });

  useEffect(() => {
    if (view.from === null) return;
    const t = window.setTimeout(() => setView((v) => ({ ...v, from: null })), 420);
    return () => clearTimeout(t);
  }, [view.index, view.from]);

  const setValues = useCallback((patch: Partial<V>) => {
    valuesRef.current = { ...valuesRef.current, ...patch };
    setValuesState(valuesRef.current);
  }, []);

  const go = (to: number) => {
    const next = Math.max(0, Math.min(n - 1, to));
    if (next === index) return;
    moved.current = true;
    navigated.current = true;
    local.current = {};
    if (step === undefined) setOwn(next);
    onStepChange?.(next);
    setReached((r) => Math.max(r, next));
    setSubmitError("");
    setAnnounce(`${L.stepOf(fmt(next + 1), fmt(n))}: ${text(steps[next].title)}`);
  };

  /** Checks the current step; shakes and announces the first problem when it doesn't pass */
  const check = () => {
    const errs = { ...local.current, ...steps[index].validate?.(valuesRef.current) };
    if (!hasErrors(errs)) {
      setAttempted((a) => ({ ...a, [index]: false }));
      return true;
    }
    setAttempted((a) => ({ ...a, [index]: true }));
    setShakes((s) => s + 1);
    setAnnounce(Object.values(errs).find(Boolean) ?? L.check);
    return false;
  };

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (submitting || done || !check()) return;
    if (index < n - 1) {
      if (returnTo !== null && returnTo > index) {
        setReturnTo(null);
        go(returnTo);
      } else go(index + 1);
      return;
    }
    setSubmitting(true);
    setSubmitError("");
    try {
      await onComplete?.(valuesRef.current);
      moved.current = true;
      navigated.current = true;
      setDone(true);
      setAnnounce(L.finished);
    } catch (err) {
      const message = err instanceof Error && err.message ? err.message : L.failed;
      setSubmitError(message);
      setAnnounce(message);
    } finally {
      setSubmitting(false);
    }
  };

  const back = () => {
    setReturnTo(null);
    go(index - 1);
  };

  /** By position, so steps that share an id still reach the right one */
  const goToIndex = (to: number) => {
    if (to < 0 || to >= n || to === index) return;
    // Forward jumps need the current step to pass first
    if (to > index && !check()) return;
    setReturnTo(to < index ? index : null);
    go(to);
  };
  const goTo = (id: string) => goToIndex(steps.findIndex((s) => s.id === id));

  const reset = () => {
    valuesRef.current = initialValues;
    setValuesState(initialValues);
    setAttempted({});
    setReturnTo(null);
    setReached(safe(initialStep));
    setDone(false);
    setSubmitError("");
    moved.current = true;
    navigated.current = true;
    local.current = {};
    if (step === undefined) setOwn(safe(initialStep));
    onStepChange?.(safe(initialStep));
  };

  // Focus follows the new step: its first field, else its heading (never on the first render)
  useEffect(() => {
    if (!moved.current) return;
    moved.current = false;
    const pane = viewport.current?.querySelector<HTMLElement>('[data-pane="current"]');
    const field = pane?.querySelector<HTMLElement>('input:not([type="hidden"]):not(:disabled), textarea, select');
    (field ?? pane?.querySelector<HTMLElement>(".ui-stpc__heading"))?.focus({ preventScroll: true });
  }, [viewIndex]);

  // After a failed Continue the first field with a problem gets the focus
  useEffect(() => {
    if (!shakes) return;
    viewport.current?.querySelector<HTMLElement>('[data-pane="current"] [aria-invalid="true"]')?.focus({ preventScroll: true });
  }, [shakes]);

  // The card glides to the height of the step that's coming in
  useLayoutEffect(() => {
    const box = viewport.current;
    const pane = box?.querySelector<HTMLElement>('[data-pane="current"]');
    if (!box || !pane) return;
    const first = !box.dataset.ready;
    const set = () => (box.style.height = `${pane.offsetHeight}px`);
    if (first) box.style.transition = "none";
    set();
    if (first) {
      box.dataset.ready = "1";
      void box.offsetWidth;
      box.style.transition = "";
    }
    const ro = new ResizeObserver(set);
    ro.observe(pane);
    return () => ro.disconnect();
  }, [viewIndex]);

  // The primary button glides to the width of its new label
  useLayoutEffect(() => {
    const box = btn.current;
    const inner = btnInner.current;
    if (!box || !inner) return;
    const set = () => (box.style.width = `${inner.offsetWidth}px`);
    set();
    const ro = new ResizeObserver(set);
    ro.observe(inner);
    return () => ro.disconnect();
  }, []);

  // Nothing to show without steps – every hook has run by now
  if (!n) return null;
  const current = steps[index];
  const errors = attempted[index] ? current.validate?.(values) ?? {} : {};
  const valid = !hasErrors(current.validate?.(values));
  const last = index === n - 1;
  const label = submitting ? submittingLabel : last ? submitLabel : returnTo !== null && returnTo > index ? L.backTo(text(steps[returnTo].title)) : L.continue;

  const ctx: StepContext<V> = { values, setValues, errors, goTo, next: () => formRef.current?.requestSubmit() };

  const renderPane = (i: number) => {
    if (i >= n) {
      return success ? success(values, reset) : <StepperSuccess title={L.successTitle} description={L.successDesc} />;
    }
    const s = steps[i];
    const paneCtx = i === index ? ctx : { ...ctx, errors: {} };
    return (
      <>
        <h2 className="ui-stpc__heading" tabIndex={-1} id={`${uid}-h${i}`} dir="auto">
          {s.heading ?? s.title}
        </h2>
        {s.description && (
          <p className="ui-stpc__desc" dir="auto">
            {s.description}
          </p>
        )}
        {(() => {
          // A step with nothing to render is just its heading and line, without the body's top gap
          const body = s.render(paneCtx);
          return body == null || body === false ? null : <div className="ui-stpc__body">{body}</div>;
        })()}
      </>
    );
  };

  const panes = view.from !== null && view.from !== view.index ? [view.from, view.index] : [view.index];

  return (
    <div className={cx("ui-stpc", className)} style={{ "--ui-stpc-n": n } as CSSProperties}>
      {/* React 19 hoists this into <head> once, no matter how many forms render */}
      <style href="ui-stepper-form-clean" precedence="default">
        {css}
      </style>

      <div className="ui-stpc__head">
        {/* Wide: the full list of steps. Narrow: kept for screen readers, replaced on screen by the compact line */}
        <div className="ui-stpc__steps">
          <ol className="ui-stpc__list" aria-label={L.progress}>
          {steps.map((s, i) => {
            const state = done || (i < furthest && i !== index) ? "done" : i === index ? "current" : "upcoming";
            const canJump = !done && !submitting && i !== index && i <= furthest;
            const inner = (
              <>
                <span className="ui-stpc__bar" aria-hidden="true" />
                <span className="ui-stpc__mark">
                  {state === "done" ? <DrawCheck key="check" /> : <span key="num">{i + 1}</span>}
                </span>
                <span className="ui-stpc__step-label" dir="auto">
                  {s.title}
                </span>
                <span className="ui-stpc__sr">{state === "done" ? L.done : state === "current" ? L.current : ""}</span>
              </>
            );
            return (
              <li key={`${s.id}#${i}`} className="ui-stpc__step" data-state={state} aria-current={!done && i === index ? "step" : undefined}>
                {canJump ? (
                  <button type="button" className="ui-stpc__step-btn" title={text(s.title) || undefined} onClick={() => goToIndex(i)}>
                    {inner}
                  </button>
                ) : (
                  <span className="ui-stpc__step-btn" title={text(s.title) || undefined}>
                    {inner}
                  </span>
                )}
              </li>
            );
          })}
          </ol>
        </div>

        <div className="ui-stpc__compact" aria-hidden="true">
          <span className="ui-stpc__compact-text" key={done ? "done" : index}>
            {done ? (
              <>{L.allDone(fmt(n))}</>
            ) : (
              <>
                {L.stepOf(fmt(index + 1), fmt(n))}
                <span className="ui-stpc__compact-dot">·</span>
                <span className="ui-stpc__compact-title" dir="auto">
                  {current.title}
                </span>
              </>
            )}
          </span>
        </div>
      </div>

      <form ref={formRef} className="ui-stpc__card" noValidate onSubmit={submit} aria-labelledby={done ? undefined : `${uid}-h${index}`}>
        <div ref={viewport} className="ui-stpc__viewport">
          {panes.map((i) => {
            const isCurrent = i === view.index;
            return (
              <div
                key={i}
                className="ui-stpc__pane"
                data-pane={isCurrent ? "current" : "leaving"}
                data-dir={view.dir}
                data-first={!navigated.current || undefined}
                inert={!isCurrent}
                aria-hidden={!isCurrent || undefined}
              >
                <PaneContext.Provider value={{ errors: isCurrent && i === index ? errors : {}, shake: isCurrent ? shakes : 0, report }}>
                  {renderPane(i)}
                </PaneContext.Provider>
              </div>
            );
          })}
        </div>

        {/* The footer folds away once everything is done */}
        <div className="ui-stpc__foot-fold" data-hidden={done || undefined}>
          <div className="ui-stpc__foot" inert={done}>
            <button type="button" className="ui-stpc__btn ui-stpc__btn-ghost" data-hidden={index === 0 || undefined} disabled={index === 0 || submitting} onClick={back}>
              <Glyph d={icons.back} />
              {L.back}
            </button>
            {aside && <span className="ui-stpc__aside">{aside}</span>}
            {submitError && (
              <span className="ui-stpc__submit-error" role="alert" dir="auto">
                {submitError}
              </span>
            )}
            <button
              ref={btn}
              type="submit"
              className="ui-stpc__btn ui-stpc__btn-primary"
              aria-disabled={!valid || submitting || undefined}
              data-busy={submitting || undefined}
            >
              <span ref={btnInner} className="ui-stpc__btn-inner">
                {submitting && <span className="ui-stpc__spinner" aria-hidden="true" />}
                <span className="ui-stpc__btn-label" key={label} title={label}>
                  {label}
                </span>
              </span>
            </button>
          </div>
        </div>
      </form>

      <span className="ui-stpc__sr" aria-live="polite">
        {announce}
      </span>
    </div>
  );
}

/* ---------- Building blocks ---------- */

export type StepperFieldProps = {
  /** Matches the key in the step's validate result */
  name: string;
  label: string;
  hint?: ReactNode;
  optional?: boolean;
  children: ReactNode;
};

/** A label, the control and an inline message that folds in – the control shakes when Continue finds a problem */
export function StepperField({ name, label, hint, optional, children }: StepperFieldProps) {
  const { errors, shake: shakes, report } = useContext(PaneContext);
  const [local, setLocal] = useState<string | null>(null);
  const [localShake, setLocalShake] = useState(0);
  const id = useId();
  const control = useRef<HTMLDivElement>(null);
  const message = errors[name] ?? local ?? "";
  // Keeps the last text while the message folds away
  const lastMessage = useRef(message);
  if (message) lastMessage.current = message;
  const invalid = !!message;

  useEffect(() => {
    if (shakes && (errors[name] || local)) shake(control.current);
  }, [shakes]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (localShake) shake(control.current);
  }, [localShake]);

  const flag = useCallback((m: string | null) => {
    setLocal(m);
    report(name, m);
    if (m) setLocalShake((s) => s + 1);
  }, [name, report]);

  return (
    <div className="ui-stpc__field" data-invalid={invalid || undefined}>
      <label className="ui-stpc__label" htmlFor={id} dir="auto">
        <span className="ui-stpc__label-text">{label}</span>
        {optional && <span className="ui-stpc__optional">Optional</span>}
      </label>
      <div ref={control} className="ui-stpc__control">
        <FieldContext.Provider value={{ id, invalid, describedBy: invalid ? `${id}-msg` : hint ? `${id}-hint` : undefined, flag }}>
          {children}
        </FieldContext.Provider>
      </div>
      <div className="ui-stpc__fold" data-open={invalid || undefined}>
        <div>
          <p className="ui-stpc__msg" id={`${id}-msg`} key={lastMessage.current} dir="auto">
            {lastMessage.current}
          </p>
        </div>
      </div>
      {hint && (
        <div className="ui-stpc__fold" data-open={!invalid || undefined}>
          <div>
            <p className="ui-stpc__hint" id={`${id}-hint`} dir="auto">
              {hint}
            </p>
          </div>
        </div>
      )}
    </div>
  );
}

export type StepperInputProps = Omit<InputHTMLAttributes<HTMLInputElement>, "prefix"> & {
  /** Fixed text before the value, e.g. "acme.com/" */
  prefix?: string;
};

/** A text field wired to its StepperField – label, message and aria come from there */
export function StepperInput({ prefix, className, ...rest }: StepperInputProps) {
  const field = useContext(FieldContext);
  return (
    <div className={cx("ui-stpc__input", className)} data-invalid={field?.invalid || undefined}>
      {prefix && (
        <span className="ui-stpc__prefix" aria-hidden="true" title={prefix}>
          {prefix}
        </span>
      )}
      <input
        id={field?.id}
        aria-invalid={field?.invalid || undefined}
        aria-describedby={field?.describedBy}
        autoComplete="off"
        spellCheck={false}
        {...rest}
      />
    </div>
  );
}

/** The success state: a check pops in, a heading and a short line, plus optional actions */
export function StepperSuccess({ title, description, children }: { title: ReactNode; description?: ReactNode; children?: ReactNode }) {
  return (
    <div className="ui-stpc__success">
      <span className="ui-stpc__success-check">
        <DrawCheck />
      </span>
      <h2 className="ui-stpc__heading" tabIndex={-1}>
        {title}
      </h2>
      {description && <p className="ui-stpc__desc">{description}</p>}
      {children && <div className="ui-stpc__success-actions">{children}</div>}
    </div>
  );
}

/* ---------- Styles ---------- */


const css = /* css */ `
/* Colors and motion as variables – override them on .ui-stpc */
.ui-stpc {
  --ui-stpc-card: var(--aui-surface, #ffffff);
  --ui-stpc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-stpc-strong: var(--aui-ink, #000000);
  --ui-stpc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-stpc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-stpc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-stpc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-stpc-line: rgba(0, 0, 0, 0.06);
  --ui-stpc-fill: rgba(0, 0, 0, 0.06);
  --ui-stpc-fill-strong: rgba(0, 0, 0, 0.1);
  --ui-stpc-wash: var(--aui-subtle, rgba(0, 0, 0, 0.03));
  --ui-stpc-wash-soft: rgba(0, 0, 0, 0.015);
  --ui-stpc-accent: var(--aui-blue, rgba(0, 96, 226, 0.765));
  --ui-stpc-accent-soft: color-mix(in srgb, var(--ui-stpc-accent) 10%, transparent);
  --ui-stpc-accent-fill: color-mix(in srgb, var(--ui-stpc-accent) 6%, transparent);
  --ui-stpc-accent-line: color-mix(in srgb, var(--ui-stpc-accent) 40%, transparent);
  --ui-stpc-ok: var(--aui-success, #1e8a4f);
  --ui-stpc-ok-soft: color-mix(in srgb, var(--aui-green, #5fc88a) 10%, transparent);
  --ui-stpc-danger: var(--aui-red, #f04438);
  --ui-stpc-danger-soft: color-mix(in srgb, var(--aui-red, #f04438) 55%, transparent);
  --ui-stpc-chip: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-stpc-chip-hover: color-mix(in srgb, var(--ui-stpc-chip) 88%, var(--ui-stpc-card));
  --ui-stpc-on-chip: var(--aui-on-primary, #fcfcfc);
  --ui-stpc-white-hover: color-mix(in srgb, var(--ui-stpc-ink) 4%, var(--ui-stpc-card));
  --ui-stpc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-stpc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-stpc-shadow-hover: 0 0 0 0.5px rgba(0, 0, 0, 0.12), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-stpc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  position: relative;
  box-sizing: border-box;
  container-type: inline-size;
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  width: 100%;
  max-width: var(--ui-stpc-max-width, 520px); /* Steve: a host can match its own card width */
  border-radius: var(--aui-radius-lg, 10px);
  background: var(--ui-stpc-card);
  box-shadow: var(--aui-shadow-card, var(--ui-stpc-shadow));
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: calc(13px * var(--type-scale, 1));
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-stpc-ink);
  -webkit-font-smoothing: antialiased;
}
.ui-stpc *, .ui-stpc *::before, .ui-stpc *::after { box-sizing: border-box; }
:where(.ui-stpc) button:where([class*="ui-stpc__"]) { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:where(.ui-stpc) svg { display: block; flex: none; }
:where(.ui-stpc) :is(h2, p, dl, dd, ul, ol) { margin: 0; }
.ui-stpc button:focus-visible { outline: 2px solid var(--ui-stpc-focus); outline-offset: 2px; }
/* 16px icons with thin ~1.1px lines */
.ui-stpc__glyph { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.65; stroke-linecap: round; stroke-linejoin: round; }
.ui-stpc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* A check that draws in */
.ui-stpc__draw { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 2.2; stroke-linecap: round; stroke-linejoin: round; }
.ui-stpc__draw path { stroke-dasharray: 1; stroke-dashoffset: 1; animation: ui-stpc-draw 360ms var(--ui-stpc-ease) 80ms forwards; }

/* ---- The steps on top: a thin bar per step, a small uppercase label under it ---- */
.ui-stpc__head { padding: 16px 20px 0; }
.ui-stpc__steps { position: relative; }
.ui-stpc__list { display: grid; grid-template-columns: repeat(var(--ui-stpc-n), minmax(0, 1fr)); gap: 8px; padding: 0; list-style: none; }
.ui-stpc__step { position: relative; display: grid; min-width: 0; }
.ui-stpc__step-btn {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  align-items: center;
  column-gap: 6px;
  row-gap: 6px;
  min-width: 0;
  margin: -4px;
  padding: 4px 4px 2px;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  text-align: left;
  cursor: default;
  transition: background-color 100ms ease;
}
button.ui-stpc__step-btn { cursor: pointer; }
button.ui-stpc__step-btn:hover { background: var(--ui-stpc-wash); }
/* The bar fills from the left once the step is reached */
.ui-stpc__bar { position: relative; grid-column: 1 / -1; height: 2px; overflow: hidden; border-radius: 1px; background: var(--ui-stpc-line); }
.ui-stpc__bar::after {
  content: "";
  position: absolute;
  inset: 0;
  border-radius: inherit;
  background: var(--ui-stpc-accent);
  transform: scaleX(0);
  transform-origin: left center;
  transition: transform 360ms var(--ui-stpc-ease);
}
.ui-stpc__step:not([data-state="upcoming"]) .ui-stpc__bar::after { transform: none; }
/* The number, or the check once the step is done – same small slot, so the label never moves */
.ui-stpc__mark {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: flex-start;
  min-width: 12px;
  height: 16px;
  font-size: calc(10px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 16px;
  font-variant-numeric: tabular-nums;
  color: var(--ui-stpc-faint);
  transition: color 200ms ease;
}
.ui-stpc__mark > span { animation: ui-stpc-in 260ms var(--ui-stpc-ease) both; }
.ui-stpc__mark .ui-stpc__draw { width: 12px; height: 12px; stroke-width: 2.4; }
.ui-stpc__step[data-state="current"] .ui-stpc__mark { color: var(--ui-stpc-accent); }
.ui-stpc__step[data-state="done"] .ui-stpc__mark { color: var(--ui-stpc-accent); }
.ui-stpc__step-label {
  overflow: hidden;
  min-width: 0;
  font-size: calc(10px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 16px;
  letter-spacing: var(--ui-stpc-label-tracking, 0.5px);
  text-transform: uppercase;
  white-space: nowrap;
  text-overflow: ellipsis;
  color: var(--ui-stpc-muted);
  transition: color 200ms ease;
}
.ui-stpc__step[data-state="done"] .ui-stpc__step-label { color: var(--ui-stpc-text); }
.ui-stpc__step[data-state="current"] .ui-stpc__step-label { color: var(--ui-stpc-strong); }
button.ui-stpc__step-btn:hover .ui-stpc__step-label { color: var(--ui-stpc-ink); }

/* Compact line for narrow screens: "STEP 2 OF 4 · TEAM" above the bars */
.ui-stpc__compact { display: none; }
.ui-stpc__compact-text {
  display: flex;
  min-width: 0;
  font-size: calc(10px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 16px;
  letter-spacing: var(--ui-stpc-label-tracking, 0.5px);
  text-transform: uppercase;
  font-variant-numeric: tabular-nums;
  color: var(--ui-stpc-muted);
  white-space: nowrap;
  animation: ui-stpc-in 280ms var(--ui-stpc-ease) both;
}
.ui-stpc__compact-dot { margin: 0 6px; color: var(--ui-stpc-faint); }
.ui-stpc__compact-title { overflow: hidden; text-overflow: ellipsis; color: var(--ui-stpc-strong); }

/* ---- The step content: only this part clips and glides to each step's height ---- */
.ui-stpc__card { display: grid; min-width: 0; }
/* The panes carry a little padding so focus rings aren't cut */
.ui-stpc__viewport { position: relative; overflow: hidden; transition: height 360ms var(--ui-stpc-ease); }
.ui-stpc__pane { padding: 20px 20px 20px; animation: ui-stpc-enter 360ms var(--ui-stpc-ease) both; }
.ui-stpc__pane[data-dir="-1"] { animation-name: ui-stpc-enter-back; }
.ui-stpc__pane[data-first] { animation: none; }
.ui-stpc__pane[data-pane="leaving"] { position: absolute; top: 0; left: 0; right: 0; pointer-events: none; animation: ui-stpc-leave 280ms var(--ui-stpc-ease) both; }
.ui-stpc__pane[data-pane="leaving"][data-dir="-1"] { animation-name: ui-stpc-leave-back; }
.ui-stpc__heading { font-size: calc(15px * var(--type-scale, 1)); font-weight: 500; line-height: 24px; color: var(--ui-stpc-strong); outline: none; overflow-wrap: anywhere; }
.ui-stpc__desc { color: var(--ui-stpc-text); text-wrap: pretty; white-space: pre-line; overflow-wrap: anywhere; }
/* Steve: one shrinkable column, so a long unbreakable line (a path) is cut inside the card instead of widening it */
.ui-stpc__body { display: grid; grid-template-columns: minmax(0, 1fr); gap: 16px; margin-top: 16px; }

/* Fields */
.ui-stpc__field { display: grid; min-width: 0; }
.ui-stpc__label { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; margin: 0 0 6px 2px; font-size: calc(12px * var(--type-scale, 1)); font-weight: 500; line-height: 16px; color: var(--ui-stpc-text); }
.ui-stpc__label-text { min-width: 0; overflow-wrap: anywhere; }
.ui-stpc__optional { flex: none; font-weight: 400; color: var(--ui-stpc-muted); }
.ui-stpc__control { min-width: 0; }
.ui-stpc__input {
  display: flex;
  align-items: center;
  min-width: 0;
  height: 32px;
  border-radius: calc(var(--aui-radius, 7px) + 1px);
  background: var(--ui-stpc-card);
  box-shadow: var(--ui-stpc-shadow);
  transition: box-shadow 120ms ease;
  cursor: text;
}
.ui-stpc__input:hover { box-shadow: var(--ui-stpc-shadow-hover); }
/* Typing in a field: no ring – the caret shows where you are, the edge only firms up a little */
.ui-stpc__input:focus-within { outline: none; box-shadow: var(--ui-stpc-shadow-hover); }
.ui-stpc__input[data-invalid] { box-shadow: 0 0 0 0.5px var(--ui-stpc-danger), 0 1px 2px 0 color-mix(in srgb, var(--aui-red, #f04438) 8%, transparent); }
.ui-stpc__input[data-invalid]:focus-within { box-shadow: 0 0 0 1px var(--ui-stpc-danger), 0 1px 2px 0 color-mix(in srgb, var(--aui-red, #f04438) 8%, transparent); }
.ui-stpc__input input {
  flex: 1;
  min-width: 0;
  height: 32px;
  margin: 0;
  padding: 0 10px;
  border: 0;
  outline: none;
  background: none;
  font: inherit;
  color: var(--ui-stpc-ink);
}
.ui-stpc__input input::placeholder { color: var(--ui-stpc-faint); }
/* A long prefix shortens, so the field always keeps room to type */
.ui-stpc__prefix { flex: 0 1 auto; min-width: 0; max-width: 60%; overflow: hidden; padding-left: 10px; color: var(--ui-stpc-muted); white-space: nowrap; text-overflow: ellipsis; user-select: none; }
.ui-stpc__prefix + input { padding-left: 0; }

/* Message and hint fold open and closed */
.ui-stpc__fold { display: grid; grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 260ms var(--ui-stpc-ease), opacity 200ms ease; }
.ui-stpc__fold[data-open] { grid-template-rows: 1fr; opacity: 1; }
.ui-stpc__fold > div { min-height: 0; overflow: hidden; }
.ui-stpc__msg, .ui-stpc__hint { padding: 6px 0 0 2px; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; text-wrap: pretty; overflow-wrap: anywhere; }
.ui-stpc__msg { color: var(--ui-stpc-danger); animation: ui-stpc-in 240ms var(--ui-stpc-ease) both; }
.ui-stpc__hint { color: var(--ui-stpc-muted); }

/* Success */
.ui-stpc__success { display: grid; justify-items: center; padding: 16px 0 8px; text-align: center; }
.ui-stpc__success-check {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  margin-bottom: 12px;
  border-radius: 50%;
  background: var(--ui-stpc-ok-soft);
  color: var(--ui-stpc-ok);
  animation: ui-stpc-pop 520ms cubic-bezier(0.34, 1.56, 0.64, 1) 120ms both;
}
.ui-stpc__success-check .ui-stpc__draw { width: 18px; height: 18px; stroke-width: 2; }
.ui-stpc__success-check .ui-stpc__draw path { animation-delay: 320ms; }
.ui-stpc__success .ui-stpc__heading { animation: ui-stpc-in 340ms var(--ui-stpc-ease) 220ms both; }
.ui-stpc__success .ui-stpc__desc { max-width: 40ch; text-wrap: balance; animation: ui-stpc-in 340ms var(--ui-stpc-ease) 290ms both; }
.ui-stpc__success-actions { display: flex; flex-wrap: wrap; justify-content: center; gap: 8px; margin-top: 16px; animation: ui-stpc-in 340ms var(--ui-stpc-ease) 360ms both; }

/* ---- The last row of the card: Back and Continue under a hairline; sticks to the bottom in long pages ---- */
.ui-stpc__foot-fold { position: sticky; bottom: 0; z-index: 3; display: grid; grid-template-rows: 1fr; border-radius: 0 0 var(--aui-radius-lg, 10px) var(--aui-radius-lg, 10px); background: var(--ui-stpc-card); transition: grid-template-rows 340ms var(--ui-stpc-ease), opacity 200ms ease; }
.ui-stpc__foot-fold[data-hidden] { grid-template-rows: 0fr; opacity: 0; }
.ui-stpc__foot { display: flex; align-items: center; gap: 12px; min-height: 0; overflow: hidden; padding: 0 12px; border-top: 0.5px solid var(--aui-border, var(--ui-stpc-line)); }
/* The row's height lives on a pseudo spacer so the fold can still close it to 0 */
.ui-stpc__foot::before { content: ""; flex: none; width: 0; height: 48px; margin-right: -12px; }
.ui-stpc__btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex: none;
  height: 28px;
  border-radius: calc(var(--aui-radius, 7px) - 1px);
  font-size: calc(13px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 20px;
  white-space: nowrap;
  transition: background-color 100ms ease, opacity 200ms ease, transform 120ms ease, width 300ms var(--ui-stpc-ease), visibility 200ms;
}
/* Secondary: white with the hairline shadow */
.ui-stpc__btn-ghost, .ui-stpc__btn-secondary { gap: 6px; padding: 0 10px 0 8px; background: var(--ui-stpc-card); box-shadow: var(--ui-stpc-shadow); color: var(--ui-stpc-strong); }
.ui-stpc__btn-secondary { padding: 0 10px; }
.ui-stpc__btn-ghost .ui-stpc__glyph { color: var(--ui-stpc-icon); }
.ui-stpc__btn-ghost:hover, .ui-stpc__btn-secondary:hover { background: var(--ui-stpc-white-hover); }
.ui-stpc__btn-ghost[data-hidden] { opacity: 0; visibility: hidden; }
/* Primary: the dark chip */
.ui-stpc__aside { display: flex; align-items: center; gap: 8px; margin-left: auto; }
.ui-stpc__aside + .ui-stpc__btn-primary { margin-left: 0; }
.ui-stpc__btn-primary { margin-left: auto; overflow: hidden; background: var(--ui-stpc-chip); color: var(--ui-stpc-on-chip); }
.ui-stpc__btn-primary:hover { background: var(--ui-stpc-chip-hover); }
.ui-stpc__btn:active { transform: scale(0.97); }
/* Not ready yet: dimmed, but still answers a press with the messages */
.ui-stpc__btn-primary[aria-disabled="true"] { opacity: 0.35; }
.ui-stpc__btn-primary[data-busy] { opacity: 0.7; cursor: progress; }
/* A long label shortens – the button never grows past the room Back leaves it */
.ui-stpc__btn-inner { display: inline-flex; flex: none; align-items: center; gap: 6px; max-width: calc(100cqw - 112px); padding: 0 10px; }
.ui-stpc__btn-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; animation: ui-stpc-in 240ms var(--ui-stpc-ease) both; }
.ui-stpc__spinner { width: 12px; height: 12px; border-radius: 50%; border: 1.5px solid currentColor; border-right-color: transparent; animation: ui-stpc-spin 700ms linear infinite; }
.ui-stpc__submit-error { flex: 1 1 auto; min-width: 0; overflow-wrap: anywhere; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; color: var(--ui-stpc-danger); animation: ui-stpc-in 240ms var(--ui-stpc-ease) both; }

@keyframes ui-stpc-in { from { opacity: 0; transform: translateY(3px); filter: blur(2px); } to { opacity: 1; transform: none; filter: none; } }
@keyframes ui-stpc-enter { from { opacity: 0; transform: translateX(24px); } to { opacity: 1; transform: none; } }
@keyframes ui-stpc-enter-back { from { opacity: 0; transform: translateX(-24px); } to { opacity: 1; transform: none; } }
@keyframes ui-stpc-leave { to { opacity: 0; transform: translateX(-24px); } }
@keyframes ui-stpc-leave-back { to { opacity: 0; transform: translateX(24px); } }
@keyframes ui-stpc-draw { to { stroke-dashoffset: 0; } }
@keyframes ui-stpc-pop { 0% { opacity: 0; transform: scale(0.4); } 60% { opacity: 1; transform: scale(1.08); } 100% { opacity: 1; transform: scale(1); } }
@keyframes ui-stpc-spin { to { transform: rotate(360deg); } }

/* Narrow (phones): the compact line over the bars */
@container (max-width: 420px) {
  .ui-stpc__head { padding: 14px 16px 0; }
  .ui-stpc__compact { display: flex; margin-bottom: 8px; }
  /* Many steps: the gaps shrink so every bar still fits – with a few steps they stay 8px */
  .ui-stpc__list { gap: min(8px, calc((100cqw - 32px) / (var(--ui-stpc-n) * 4))); }
  .ui-stpc__step-btn { row-gap: 0; padding-bottom: 4px; }
  .ui-stpc__mark, .ui-stpc__step-label { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
  .ui-stpc__pane { padding: 16px 16px 16px; }
  .ui-stpc__foot { padding: 0 12px; }
}
/* Touch screens: 16px fields (no zoom on iOS) and taller targets */
@media (hover: none) and (pointer: coarse) {
  .ui-stpc__input input, .ui-stpc__prefix { font-size: calc(16px * var(--type-scale, 1)); }
  .ui-stpc__input, .ui-stpc__input input { height: 40px; }
  .ui-stpc__btn { height: 36px; }
  .ui-stpc__foot::before { height: 56px; }
  .ui-stpc__step-btn { padding-top: 10px; padding-bottom: 8px; margin: -10px -4px -8px; }
}
/* Steve: narrow, the labels hide and only the 2px bars show; the button back to a finished step keeps a 28px touch
 * target through padding cancelled by a negative margin. After the touch rule so it also wins on phones. */
@container (max-width: 420px) {
  .ui-stpc__step-btn { padding: 13px 4px; margin: -13px -4px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-stpc *, .ui-stpc *::before, .ui-stpc *::after { animation-duration: 1ms !important; animation-delay: 0ms !important; transition: none !important; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-stpc {
  --ui-stpc-card: var(--aui-surface, #1c1c1e);
  --ui-stpc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-stpc-strong: var(--aui-ink, #ffffff);
  --ui-stpc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-stpc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-stpc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.3));
  --ui-stpc-icon: var(--aui-muted, rgba(255, 255, 255, 0.5));
  --ui-stpc-line: rgba(255, 255, 255, 0.08);
  --ui-stpc-fill: rgba(255, 255, 255, 0.08);
  --ui-stpc-fill-strong: rgba(255, 255, 255, 0.16);
  --ui-stpc-wash: var(--aui-subtle, rgba(255, 255, 255, 0.04));
  --ui-stpc-wash-soft: rgba(255, 255, 255, 0.025);
  --ui-stpc-accent: var(--aui-blue, #4fb0fe);
  --ui-stpc-accent-soft: color-mix(in srgb, var(--ui-stpc-accent) 16%, transparent);
  --ui-stpc-accent-fill: color-mix(in srgb, var(--ui-stpc-accent) 10%, transparent);
  --ui-stpc-accent-line: color-mix(in srgb, var(--ui-stpc-accent) 50%, transparent);
  --ui-stpc-ok: var(--aui-green, #5fc88a);
  --ui-stpc-ok-soft: color-mix(in srgb, var(--aui-green, #7dcf9f) 14%, transparent);
  --ui-stpc-danger: var(--aui-red, #ff6b5f);
  --ui-stpc-chip: var(--aui-primary, rgba(255, 255, 255, 0.9));
  --ui-stpc-chip-hover: color-mix(in srgb, var(--ui-stpc-chip) 88%, var(--ui-stpc-card));
  --ui-stpc-on-chip: var(--aui-on-primary, #111113);
  --ui-stpc-white-hover: color-mix(in srgb, var(--ui-stpc-ink) 5%, var(--ui-stpc-card));
  --ui-stpc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
  --ui-stpc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
  --ui-stpc-shadow-hover: 0 0 0 0.5px rgba(255, 255, 255, 0.16), 0 1px 2px 0 rgba(0, 0, 0, 0.4);
}
`;
