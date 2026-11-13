/* First-run onboarding, shown in the main conversation until it is finished or skipped: Steve introduces itself,
 * shows the runtimes and projects it really found, and ends with the first meeting (the instance's owner) or a first
 * task (anyone else). Built on affectUI's Stepper Form; every row comes from the store, nothing is made up. */
import { StepperForm, StepperSuccess, type Step, type StepperLabels } from "@/components/affect/stepper-form-clean"
import { Button } from "@/components/ui/button"
import { emit, finishOnboarding, meet, skipOnboarding, st, toast, useStore } from "@/store"
import { go } from "@/ui"
import { homeTilde } from "@/resume"
import { useT } from "@/i18n"
import { cn } from "cn"
import { ArrowUpRightIcon } from "lucide-react"
import type { ReactNode } from "react"
import type pages from "@/i18n/locales/zh-CN/pages"

type V = Record<string, never>
type Raw = keyof typeof pages.chat.onboard
/** the tour's string keys, plural forms by their base name */
type K = Raw extends infer R ? R extends `${infer B}_${"one" | "other"}` ? B : R : never

/** A link out of the tour to the page that shows more; the tour stays where it was */
function Out({ to, id, children }: { to: string; id: string; children: ReactNode }) {
  return <Button type="button" size="sm" variant="outline" className="self-start" onClick={() => go(to)} data-onboard-opt={id}>{children}<ArrowUpRightIcon /></Button>
}

/** One row of a short list: a status dot, the name, and a muted line on the right. A path (`path`) is set in mono and
 * cut at its start, so the folder name at the end, the part that tells similar projects apart, stays visible. */
function Row({ ok, name, sub, path, data }: { ok?: boolean; name: string; sub?: string; path?: boolean; data?: Record<string, string> }) {
  return (
    <li className="flex min-h-9 min-w-0 items-center gap-2.5 px-3 py-2" {...data}>
      {ok !== undefined && <span className={cn("size-1.5 shrink-0 rounded-full", ok ? "bg-success" : "bg-attention")} aria-hidden />}
      {/* the name stays whole up to 60% of the row; the muted line gives way first */}
      <span className="min-w-0 max-w-[60%] shrink-0 truncate type-callout font-medium text-foreground">{name}</span>
      {sub && (path
        /* rtl moves the ellipsis to the start; the marks keep the path's own left-to-right order */
        ? <span dir="rtl" className="ml-auto min-w-0 shrink truncate text-left type-code text-muted-foreground" title={sub}>{"\u200e" + sub + "\u200e"}</span>
        : <span className="ml-auto min-w-0 shrink truncate type-footnote text-muted-foreground">{sub}</span>)}
    </li>
  )
}
const List = ({ children }: { children: ReactNode }) => <ul className="flex flex-col divide-y divide-border/70 overflow-hidden rounded-lg bg-muted/50">{children}</ul>

export function Onboarding() {
  useStore()
  const t = useT()
  if (st.onboard < 0) return null
  const k = (s: K, o?: Record<string, unknown>) => String(t(`pages:chat.onboard.${s}`, o as never))
  const rts = st.runtimes, ok = rts.filter((r) => r.ok).map((r) => r.name)
  const ws = st.workspace, projects = st.projects.filter((p) => !p.archived && (!ws || p.workspace_id === ws.id))
  const shown = projects.slice(0, 4)

  const steps: Step<V>[] = [
    { id: "hello", title: k("start"), heading: k("helloHead"), description: k("hello"),
      render: () => null,
    },
    { id: "runtime", title: k("runtime"), heading: k("runtimeHead"), description: ok.length ? k("runtimes", { first: ok[0] }) : k("noRuntime"),
      render: () => <div className="flex flex-col gap-3">
        {rts.length > 0 && <List>{rts.map((r) => <Row key={r.id} ok={r.ok} name={r.name} data={{ "data-onboard-runtime": r.id }}
          sub={r.ok ? (r.models.length ? k("models", { count: r.models.length }) : r.note) : (r.error || r.install_hint || k("unavailable"))} />)}</List>}
        <Out to="#/devices" id="devices">{k("devices")}</Out>
      </div>,
    },
    { id: "project", title: k("project"), heading: k("projectHead"), description: k("projects", { space: ws?.name, count: projects.length }),
      render: () => <div className="flex flex-col gap-3">
        {shown.length > 0 && <List>{shown.map((p) => <Row key={p.id} name={p.name} sub={homeTilde(p.path)} path data={{ "data-onboard-project": p.id }} />)}
          {projects.length > shown.length && <li className="px-3 py-2 type-footnote text-muted-foreground">{k("moreProjects", { count: projects.length - shown.length })}</li>}</List>}
        <Out to="#/projects" id="seeProjects">{k("seeProjects")}</Out>
      </div>,
    },
    /* the instance's owner ends with the first meeting; anyone else with a first task */
    st.owner
      ? { id: "meet", title: k("meetStep"), heading: k("meetHead"), description: k("meet"), render: () => null }
      : { id: "first", title: k("firstTask"), heading: k("firstHead"), description: k("first"), render: () => null },
  ]
  const labels: Partial<StepperLabels> = {
    continue: k("ok"), back: t("common:action.back"), backTo: (s) => k("backTo", { step: s }),
    stepOf: (n, total) => k("stepOf", { n, total }), allDone: () => k("done"), progress: k("progress"),
    done: k("srDone"), current: k("srCurrent"), finished: k("done"), failed: k("failed"), check: k("check"),
    successTitle: k("done"), successDesc: "",
  }
  const last = st.onboard >= steps.length - 1
  return (
    /* lined up with the messages and the item cards above: their px-1 inset and the cards' 560px */
    <div className="flex flex-col px-1 anim-in [--ui-stpc-label-tracking:0] [--ui-stpc-max-width:560px]" data-onboard-card data-onboard={st.onboard}>
      <StepperForm<V>
        className="ui-steve-onboard"
        steps={steps}
        initialValues={{}}
        step={st.onboard}
        onStepChange={(n) => { st.onboard = n; emit() }}
        labels={labels}
        submitLabel={st.owner ? k("meetGo") : k("finish")}
        submittingLabel={st.owner ? k("meetGo") : k("finish")}
        aside={<Button type="button" size="sm" variant="ghost" className="text-muted-foreground" onClick={skipOnboarding} data-onboard-skip>{last && st.owner ? k("later") : k("skip")}</Button>}
        onComplete={async () => { if (st.owner) await meet(); else { finishOnboarding(); toast(k("done")) } }}
        success={() => <StepperSuccess title={k("done")} />}
      />
    </div>
  )
}
