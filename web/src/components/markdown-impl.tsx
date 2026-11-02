import { useEffect, useRef, useState, type ComponentProps, type ReactNode } from "react"
import { Streamdown, defaultComponents, defaultRehypePlugins, defaultRemarkPlugins, useIsCodeFenceIncomplete, type Components, type DefaultComponents, type DiagramPlugin } from "streamdown"
import { code } from "@streamdown/code"
import remarkBreaks from "remark-breaks"
import { cn } from "cn"
import { st } from "@/store"

/* mermaid is ~1 MB — load it the first time a diagram shows up, not with the app */
type Cfg = NonNullable<Parameters<DiagramPlugin["getMermaid"]>[0]>
const base: Cfg = { startOnLoad: false, securityLevel: "strict", fontFamily: "inherit", suppressErrorRendering: true }
let lib: Promise<typeof import("mermaid").default> | undefined
let cfg: Cfg = {}
const mermaid: DiagramPlugin = {
  name: "mermaid", type: "diagram", language: "mermaid",
  getMermaid(c) {
    if (c) cfg = c
    return {
      initialize(c) { cfg = c },
      async render(id, src) {
        const m = await (lib ??= import("mermaid").then((x) => x.default))
        m.initialize({ ...base, theme: document.documentElement.classList.contains("dark") ? "dark" : "neutral", ...cfg } as Parameters<typeof m.initialize>[0])
        return m.render(id, src)
      },
    }
  },
}

/* item numbers (ST-7) become links, except inside code and existing links */
type MdNode = { type: string; value?: string; url?: string; children?: MdNode[]; data?: Record<string, unknown> }
const REF = /\b[A-Z]{1,6}-\d+\b/g
const SKIP = new Set(["link", "linkReference", "inlineCode", "code", "html", "definition"])
function remarkItemRefs() {
  const walk = (n: MdNode) => {
    if (!n.children || SKIP.has(n.type)) return
    n.children = n.children.flatMap((c) => {
      if (c.type !== "text") { walk(c); return [c] }
      const v = c.value ?? "", out: MdNode[] = []
      let last = 0
      for (const m of v.matchAll(REF)) {
        if (!st.items.some((i) => i.id === m[0])) continue
        if (m.index > last) out.push({ type: "text", value: v.slice(last, m.index) })
        out.push({ type: "link", url: "#/items/" + m[0], children: [{ type: "text", value: m[0] }], data: { hProperties: { "data-ref": m[0] } } })
        last = m.index + m[0].length
      }
      if (!out.length) return [c]
      if (last < v.length) out.push({ type: "text", value: v.slice(last) })
      return out
    })
  }
  return (tree: MdNode) => walk(tree)
}

/* raw HTML in a message is shown as the text it is, never parsed into the app's DOM (html code blocks get the sandboxed preview instead) */
function remarkHtmlAsText() {
  const walk = (n: MdNode) => { for (const c of n.children ?? []) { if (c.type === "html") c.type = "text"; else walk(c) } }
  return (tree: MdNode) => walk(tree)
}

const text = (n: ReactNode): string => typeof n === "string" || typeof n === "number" ? String(n)
  : Array.isArray(n) ? n.map(text).join("") : n && typeof n === "object" && "props" in n ? text((n.props as { children?: ReactNode }).children) : ""

/* the page posts its height so the frame fits its content; the sandbox has no same-origin access */
const SIZE = `<script>(()=>{const b=document.body,p=()=>{const s=getComputedStyle(b);parent.postMessage({steveHtmlHeight:Math.ceil(b.getBoundingClientRect().height+parseFloat(s.marginTop)+parseFloat(s.marginBottom))},"*")};addEventListener("load",p);new ResizeObserver(p).observe(b)})()</script>`
function HtmlPreview({ src }: { src: string }) {
  const ref = useRef<HTMLIFrameElement>(null), [h, setH] = useState(160)
  useEffect(() => {
    const on = (e: MessageEvent) => { if (e.source === ref.current?.contentWindow && typeof e.data?.steveHtmlHeight === "number") setH(Math.min(560, Math.max(60, e.data.steveHtmlHeight))) }
    addEventListener("message", on); return () => removeEventListener("message", on)
  }, [])
  return <iframe ref={ref} title="HTML 预览" sandbox="allow-scripts" referrerPolicy="no-referrer" srcDoc={src + SIZE} style={{ height: h }} className="block w-full bg-white" data-html-preview />
}
type CodeProps = ComponentProps<DefaultComponents["code"]>
function HtmlBlock(props: CodeProps) {
  const streaming = useIsCodeFenceIncomplete(), [tab, setTab] = useState<"预览" | "代码">("预览")
  const showCode = streaming || tab === "代码"
  return (
    <div className="my-4 overflow-hidden rounded-xl border" data-html-block>
      <div className="flex items-center gap-1 border-b bg-muted/60 px-2 py-1">
        <span className="mr-auto px-1 font-mono text-[12px] text-muted-foreground">html</span>
        {(["预览", "代码"] as const).map((t) => (
          <button key={t} type="button" disabled={streaming} aria-pressed={!showCode === (t === "预览")} onClick={() => setTab(t)}
            className="rounded-md px-2 py-0.5 type-footnote text-muted-foreground hover:text-foreground disabled:opacity-50 aria-pressed:bg-background aria-pressed:text-foreground">{t}</button>
        ))}
      </div>
      {showCode ? <div className="[&>[data-streamdown=code-block]]:my-0 [&>[data-streamdown=code-block]]:rounded-none [&>[data-streamdown=code-block]]:border-0"><defaultComponents.code {...props} /></div>
        : <HtmlPreview src={text(props.children)} />}
    </div>
  )
}

const components: Components = {
  /* in-app links (item refs) stay in this tab: drop the target/rel rehype-harden adds to every link */
  a: ({ href, children, node: _n, target: _t, rel: _r, ...rest }) => href?.startsWith("#/")
    ? <a {...rest} href={href} onClick={(e) => e.stopPropagation()} className="font-medium tabular-nums text-blue hover:underline" data-ref={/^#\/items\/(.+)/.exec(href)?.[1]}>{children}</a>
    : <defaultComponents.a {...rest} href={href}>{children}</defaultComponents.a>,
  code: (props) => "data-block" in props && /\blanguage-html\b/.test(props.className ?? "") ? <HtmlBlock {...props} /> : <defaultComponents.code {...props} />,
}
const plugins = { code, mermaid }
const remarkPlugins = [...Object.values(defaultRemarkPlugins), remarkBreaks, remarkHtmlAsText, remarkItemRefs]
/* Streamdown's defaults minus rehype-raw: sanitize + harden still apply to what markdown itself produces */
const rehypePlugins = [defaultRehypePlugins.sanitize, defaultRehypePlugins.harden]

/** `streaming`: the text is still being written (unclosed markdown is completed, a caret follows the end) */
export default function MarkdownImpl({ text, className, streaming }: { text: string; className?: string; streaming?: boolean }) {
  return (
    <Streamdown className={cn("md", className)} plugins={plugins} components={components} remarkPlugins={remarkPlugins} rehypePlugins={rehypePlugins}
      shikiTheme={["github-light", "github-dark"]} controls={{ mermaid: { panZoom: false } }}
      isAnimating={!!streaming} caret={streaming ? "circle" : undefined}>{String(text ?? "")}</Streamdown>
  )
}
