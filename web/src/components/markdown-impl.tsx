import { getItems } from "@/store"
import { lazy, Suspense, useEffect, useMemo, useRef, useState, type ComponentProps, type ReactNode } from "react"
import { Streamdown, defaultComponents, defaultRehypePlugins, defaultRemarkPlugins, useIsCodeFenceIncomplete, type Components, type DefaultComponents, type DiagramPlugin } from "streamdown"
import { cjk } from "@streamdown/cjk"
import { code } from "@streamdown/code"
import remarkBreaks from "remark-breaks"
import { cn } from "cn"
import { A2uiPending, useA2uiNested } from "@/a2ui/host"
import { useT } from "@/i18n"
import { currentHub } from "@/hub"
import { historicalReplyMarker, historicalReplyPath, markdownImageSource, replyDeliveryUrl, replyFileReason, replyImageHint, type MarkdownImageSource } from "@/markdown-images"
import { Button } from "@/components/ui/button"

/* A2UI blocks (ST-46) draw with the app's controls and the stats chart: loaded the first time a reply carries one */
const A2uiBlock = lazy(() => import("@/a2ui/render").then((m) => ({ default: m.A2uiBlock })))

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
        if (!getItems().some((i) => i.id === m[0])) continue
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
  const t = useT(), ref = useRef<HTMLIFrameElement>(null), [h, setH] = useState(160)
  useEffect(() => {
    const on = (e: MessageEvent) => { if (e.source === ref.current?.contentWindow && typeof e.data?.steveHtmlHeight === "number") setH(Math.min(560, Math.max(60, e.data.steveHtmlHeight))) }
    addEventListener("message", on); return () => removeEventListener("message", on)
  }, [])
  return <iframe ref={ref} title={t("misc:markdown.htmlPreview")} sandbox="allow-scripts" referrerPolicy="no-referrer" srcDoc={src + SIZE} style={{ height: h }} className="block w-full bg-white" data-html-preview />
}
type CodeProps = ComponentProps<DefaultComponents["code"]>
function HtmlBlock(props: CodeProps) {
  const t = useT(), streaming = useIsCodeFenceIncomplete(), [tab, setTab] = useState<"preview" | "code">("preview")
  const showCode = streaming || tab === "code"
  return (
    <div className="my-4 overflow-hidden rounded-xl border" data-html-block>
      <div className="flex items-center gap-1 border-b bg-muted/60 px-2 py-1">
        <span className="mr-auto px-1 font-mono text-[12px] text-muted-foreground">html</span>
        {(["preview", "code"] as const).map((k) => (
          <button key={k} type="button" disabled={streaming} aria-pressed={!showCode === (k === "preview")} onClick={() => setTab(k)}
            className="rounded-md px-2 py-0.5 type-footnote text-muted-foreground hover:text-foreground disabled:opacity-50 aria-pressed:bg-background aria-pressed:text-foreground">{t(`misc:markdown.${k}`)}</button>
        ))}
      </div>
      {showCode ? <div className="[&>[data-streamdown=code-block]]:my-0 [&>[data-streamdown=code-block]]:rounded-none [&>[data-streamdown=code-block]]:border-0"><defaultComponents.code {...props} /></div>
        : <HtmlPreview src={text(props.children)} />}
    </div>
  )
}

/* tables open to their full height in the page's own scroll; only a wide table scrolls, sideways. Streamdown's default
   frames them with a toolbar and caps them at 300px with a scrollbar inside, which nests scrolling in the message.
   A very long table shows its first rows and a button for the rest */
const TABLE_FOLD = 30, TABLE_SHOW = 20
function Table({ node, children, className, ...rest }: ComponentProps<DefaultComponents["table"]>) {
  const t = useT(), rows = countRows(node), [open, setOpen] = useState(false), fold = rows > TABLE_FOLD && !open
  return (
    <div className="my-3" data-md-table>
      <div className="overflow-x-auto" data-folded={fold || undefined}>
        <table {...rest} className={className}>{children}</table>
      </div>
      {fold && <button type="button" onClick={() => setOpen(true)} className="mt-1 rounded-md px-2 py-1 type-footnote text-muted-foreground hover:bg-muted hover:text-foreground">{t("misc:markdown.moreRows", { count: rows - TABLE_SHOW })}</button>}
    </div>
  )
}
type Hast = { type: string; tagName?: string; children?: Hast[]; properties?: Record<string, unknown> }
const countRows = (n?: Hast): number => !n ? 0 : n.tagName === "tbody" ? (n.children ?? []).filter((c) => c.tagName === "tr").length
  : (n.children ?? []).reduce((s, c) => s + countRows(c), 0)

/** a ```a2ui block: the interface it describes (a placeholder while it is written); inside an interface, just code */
function A2uiCode(props: CodeProps) {
  const streaming = useIsCodeFenceIncomplete(), nested = useA2uiNested()
  const code = <defaultComponents.code {...props} />
  if (nested) return code
  return <Suspense fallback={<A2uiPending loading={!streaming} />}><A2uiBlock src={text(props.children)} streaming={streaming} code={code} /></Suspense>
}

type ImageProps = ComponentProps<DefaultComponents["img"]>
function MarkdownImage({ source, reason, ...props }: ImageProps & { source: MarkdownImageSource | null; reason?: ReturnType<typeof replyFileReason> }) {
  const t = useT(), root = useRef<HTMLSpanElement>(null), [failed, setFailed] = useState(false), [attempt, setAttempt] = useState(0)
  // Cached failures may have completed before React attached the event handlers.
  useEffect(() => {
    const img = root.current?.querySelector("img")
    if (img?.complete && !img.naturalWidth) setFailed(true)
  }, [attempt])
  return <span ref={root} className="inline-block max-w-full align-middle [&_[data-streamdown=image-wrapper]]:max-w-full" data-md-image>
    {source && <span hidden={failed}>
      <defaultComponents.img {...props} key={attempt} src={source.src} className={cn("h-auto", props.className)}
        // CORS with credentials also permits the Hub's CORP-protected images in a separately served client.
        crossOrigin={source.authenticated ? "use-credentials" : props.crossOrigin}
        onLoad={(e) => { setFailed(false); props.onLoad?.(e) }} onError={(e) => { setFailed(true); props.onError?.(e) }} />
    </span>}
    {(!source || failed) && <span className="my-3 inline-flex max-w-full flex-wrap items-center gap-2 rounded-md border px-3 py-2 type-footnote text-muted-foreground" data-md-image-error>
      <span role="status" className="min-w-0 break-words">
        {props.alt && <span className="mr-2 font-medium text-foreground">{props.alt}</span>}
        {reason ? t(`misc:markdown.fileUnavailable.${reason}`) : t(source ? "misc:markdown.imageFailed" : "misc:markdown.imageUnavailable")}
      </span>
      {source && <Button type="button" variant="outline" size="xs" onClick={(e) => { e.preventDefault(); e.stopPropagation(); setFailed(false); setAttempt((n) => n + 1) }}>{t("common:action.retry")}</Button>}
    </span>}
  </span>
}
function Image(props: ImageProps) {
  const reason = replyFileReason(props.src)
  const source = !reason && typeof props.src === "string" ? markdownImageSource(props.src, currentHub()) : null
  return <MarkdownImage key={source?.src ?? String(props.src)} {...props} source={source} reason={reason} />
}
const hasImage = (n?: Hast): boolean => n?.tagName === "img" || !!n?.children?.some(hasImage)

function Link({ href, children, node: _n, target: _t, rel: _r, ...rest }: ComponentProps<DefaultComponents["a"]>) {
  const t = useT(), reason = replyFileReason(href)
  if (reason) return <span data-md-file-unavailable className="break-words">{children} <span className="type-footnote text-muted-foreground">({t(`misc:markdown.fileUnavailable.${reason}`)})</span></span>
  if (href?.startsWith("#/")) return <a {...rest} href={href} onClick={(e) => e.stopPropagation()} className="font-medium tabular-nums text-blue hover:underline" data-ref={/^#\/items\/(.+)/.exec(href)?.[1]}>{children}</a>
  const delivery = replyDeliveryUrl(href, currentHub())
  if (delivery) return <a {...rest} href={delivery} onClick={(e) => e.stopPropagation()} className="font-medium text-blue underline underline-offset-2" data-reply-delivery>{children}</a>
  return <defaultComponents.a {...rest} href={href}>{children}</defaultComponents.a>
}

function remarkHistoricalReplyFiles() {
  const walk = (n: MdNode) => {
    // Definitions are evaluated by the parser: rewrite file-shaped definitions, not literal text.
    if ((n.type === "link" || n.type === "image" || n.type === "definition") && n.url && historicalReplyPath(n.url, n.type === "image")) n.url = historicalReplyMarker
    for (const c of n.children ?? []) walk(c)
  }
  return (tree: MdNode) => walk(tree)
}

const components: Components = {
  table: Table,
  img: Image,
  // Streamdown's image has block controls. Mixed text/images cannot live inside a <p>.
  p: ({ node, children, ...props }) => hasImage(node) ? <div {...props}>{children}</div> : <defaultComponents.p {...props} node={node}>{children}</defaultComponents.p>,
  /* in-app links (item refs) stay in this tab: drop the target/rel rehype-harden adds to every link */
  a: Link,
  code: (props) => !("data-block" in props) ? <defaultComponents.code {...props} />
    : /\blanguage-html\b/.test(props.className ?? "") ? <HtmlBlock {...props} />
    : /\blanguage-a2ui\b/.test(props.className ?? "") ? <A2uiCode {...props} /> : <defaultComponents.code {...props} />,
}
/* cjk: CommonMark does not close `**` between a full-width mark and a Han character (`**“执行中”。**看…`), which leaves
 * the asterisks showing in Chinese replies */
const plugins = { cjk, code, mermaid }
const remarkPlugins = [...Object.values(defaultRemarkPlugins), remarkBreaks, remarkHtmlAsText, remarkItemRefs]
/* Streamdown's defaults minus rehype-raw: sanitize + harden still apply to what markdown itself produces */
function rehypeReplyImageHints() {
  const walk = (n: Hast) => {
    if (n.tagName === "img" && n.properties && typeof n.properties.src === "string") n.properties.src = replyImageHint(n.properties.src)
    for (const c of n.children ?? []) walk(c)
  }
  return (tree: Hast) => walk(tree)
}
const rehypePlugins = [defaultRehypePlugins.sanitize, rehypeReplyImageHints, defaultRehypePlugins.harden]

/** `streaming`: the text is still being written (unclosed markdown is completed, a caret follows the end) */
/* a document of a delivered package: its relative images and links point into the package; an absolute URL stays */
const ABSOLUTE = /^([a-z][a-z0-9+.-]*:|\/\/|#)/i
function remarkRebase(resolve: (ref: string) => string | null) {
  return () => {
    const walk = (n: MdNode) => {
      if ((n.type === "image" || n.type === "link" || n.type === "definition") && n.url && !ABSOLUTE.test(n.url)) n.url = resolve(n.url) ?? ""
      for (const c of n.children ?? []) walk(c)
    }
    return (tree: MdNode) => walk(tree)
  }
}

export default function MarkdownImpl({ text, className, streaming, resolve, replyFiles }: { text: string; className?: string; streaming?: boolean; replyFiles?: boolean; resolve?: (ref: string) => string | null }) {
  const remark = useMemo(() => resolve ? [...remarkPlugins, remarkRebase(resolve)] : replyFiles ? [...remarkPlugins, remarkHistoricalReplyFiles] : remarkPlugins, [resolve, replyFiles])
  return (
    <Streamdown className={cn("md", className)} plugins={plugins} components={components} remarkPlugins={remark} rehypePlugins={rehypePlugins}
      shikiTheme={["github-light", "github-dark"]} controls={{ mermaid: { panZoom: false }, table: false }} tableMaxHeight="none"
      isAnimating={!!streaming} caret={streaming ? "circle" : undefined}>{String(text ?? "")}</Streamdown>
  )
}
