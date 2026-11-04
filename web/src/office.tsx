import { unzip, resolve, type Zip } from "@/unzip"
import { Segmented } from "@/ui"
import { cn } from "cn"
import { useEffect, useState, type ReactNode } from "react"

/* Word, Excel and PowerPoint files (docx/xlsx/pptx) read in the page: the text, headings, lists, tables and pictures of a
 * document, the cells of each sheet, the text, tables and pictures of each slide. Layout, fonts and charts are not
 * reproduced — the download keeps the original. Each part says where it is (`data-ann-at`) so a note on selected text
 * can name it. */

type Run = { t: string; b?: boolean; i?: boolean; u?: boolean; s?: boolean; href?: string } | { img: string } | { br: true }
type Para = { kind: "p"; style: string; list?: number; runs: Run[] }
type Block = Para | { kind: "table"; rows: Block[][][] }
type Sheet = { name: string; rows: string[][]; cols: number; cut: boolean }
type Slide = { n: number; title: string; parts: ({ kind: "text"; paras: { level: number; runs: Run[] }[] } | { kind: "img"; src: string } | { kind: "table"; rows: string[][] })[] }
export type OfficeDoc = { type: "docx"; blocks: Block[] } | { type: "xlsx"; sheets: Sheet[] } | { type: "pptx"; slides: Slide[] }

const ROWS_MAX = 1000, COLS_MAX = 60

const xml = (t: string | null) => t ? new DOMParser().parseFromString(t, "application/xml") : null
const kids = (e: Element | Document | null | undefined, name?: string) => e ? Array.from(e.children).filter((c) => !name || c.localName === name) : []
const kid = (e: Element | null | undefined, name: string) => kids(e, name)[0] ?? null
const all = (e: Element | Document | null | undefined, name: string) => e ? Array.from(e.getElementsByTagNameNS("*", name)) : []
const attr = (e: Element | null | undefined, name: string) => { if (!e) return null; for (const a of Array.from(e.attributes)) if (a.localName === name) return a.value; return null }
/** a relationship id (`r:id`): an element may also have a plain `id` (a slide's number) */
const R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
const rid = (e: Element | null | undefined) => e?.getAttributeNS(R, "id") || attr(e, "id") || ""
/** a toggle like `<w:b/>`, off when its value says so */
const on = (e: Element | null) => !!e && !["0", "false", "none"].includes(attr(e, "val") ?? "")

const MIME: Record<string, string> = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", bmp: "image/bmp", webp: "image/webp", svg: "image/svg+xml", tif: "image/tiff", tiff: "image/tiff" }

/** `{id: {target, external}}` of a part's relationships */
async function rels(z: Zip, part: string) {
  const dir = part.split("/").slice(0, -1).join("/"), file = part.split("/").pop()
  const d = xml(await z.text(`${dir ? dir + "/" : ""}_rels/${file}.rels`))
  const m = new Map<string, { target: string; external: boolean }>()
  for (const r of all(d, "Relationship")) {
    const external = attr(r, "TargetMode") === "External", t = attr(r, "Target") ?? ""
    m.set(attr(r, "Id") ?? "", { target: external ? t : resolve(dir, t), external })
  }
  return m
}

/** pictures become object URLs, released with the view */
type Pics = { urls: string[]; get: (z: Zip, path: string) => Promise<string | null> }
const pics = (): Pics => {
  const urls: string[] = []
  return { urls, get: async (z, path) => {
    const ext = path.split(".").pop()?.toLowerCase() ?? "", mime = MIME[ext]
    if (!mime) return null
    const b = await z.bytes(path); if (!b) return null
    const u = URL.createObjectURL(new Blob([b as BlobPart], { type: mime })); urls.push(u); return u
  } }
}

/* ── Word ── */
async function docx(z: Zip, px: Pics): Promise<Block[]> {
  const doc = xml(await z.text("word/document.xml"))
  if (!doc) throw new Error("没有找到文档正文（word/document.xml）")
  const rel = await rels(z, "word/document.xml")
  const names = new Map<string, string>()
  for (const s of all(xml(await z.text("word/styles.xml")), "style")) names.set(attr(s, "styleId") ?? "", (attr(kid(s, "name"), "val") ?? "").toLowerCase())
  const styleOf = (p: Element) => {
    const id = attr(kid(kid(p, "pPr"), "pStyle"), "val") ?? "", n = names.get(id) ?? id.toLowerCase()
    if (n === "title") return "title"
    const h = /heading\s*(\d)/.exec(n)
    return h ? `h${Math.min(6, +h[1])}` : "p"
  }
  async function runs(e: Element, href?: string): Promise<Run[]> {
    const out: Run[] = []
    for (const c of kids(e)) {
      if (c.localName === "r") {
        const pr = kid(c, "rPr"), fmt = { b: on(kid(pr, "b")), i: on(kid(pr, "i")), u: on(kid(pr, "u")), s: on(kid(pr, "strike")), href }
        for (const x of kids(c)) {
          if (x.localName === "t") out.push({ t: x.textContent ?? "", ...fmt })
          else if (x.localName === "tab") out.push({ t: "\t", ...fmt })
          else if (x.localName === "br" || x.localName === "cr") out.push({ br: true })
          else if (x.localName === "drawing" || x.localName === "pict") {
            for (const bl of [...all(x, "blip"), ...all(x, "imagedata")]) {
              const r = rel.get(attr(bl, "embed") ?? attr(bl, "id") ?? "")
              const u = r && !r.external ? await px.get(z, r.target) : null
              if (u) out.push({ img: u })
            }
          }
        }
      } else if (c.localName === "hyperlink") {
        const r = rel.get(rid(c))
        out.push(...await runs(c, r?.external && /^https?:/i.test(r.target) ? r.target : href))
      } else if (["ins", "smartTag", "fldSimple", "sdt", "sdtContent"].includes(c.localName)) out.push(...await runs(c, href))
    }
    return out
  }
  async function blocks(e: Element | null): Promise<Block[]> {
    const out: Block[] = []
    for (const c of kids(e)) {
      if (c.localName === "p") {
        const num = kid(kid(c, "pPr"), "numPr")
        out.push({ kind: "p", style: styleOf(c), list: num ? +(attr(kid(num, "ilvl"), "val") ?? 0) : undefined, runs: await runs(c) })
      } else if (c.localName === "tbl") {
        const rows: Block[][][] = []
        for (const tr of kids(c, "tr")) { const cells: Block[][] = []; for (const tc of kids(tr, "tc")) cells.push(await blocks(tc)); rows.push(cells) }
        out.push({ kind: "table", rows })
      } else if (c.localName === "sdt") out.push(...await blocks(kid(c, "sdtContent")))
    }
    return out
  }
  return blocks(all(doc, "body")[0] ?? null)
}

/* ── Excel ── */
const col = (ref: string) => { let n = 0; for (const ch of ref.replace(/\d+$/, "").toUpperCase()) n = n * 26 + ch.charCodeAt(0) - 64; return n - 1 }
export const colName = (n: number) => { let s = ""; for (n++; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s; return s }
/** built-in date formats, and custom ones that have a date or time part outside quotes */
const isDateFormat = (id: number, code?: string) => (id >= 14 && id <= 22) || (id >= 45 && id <= 47) || (!!code && /[ymdh]/i.test(code.replace(/"[^"]*"|\[[^\]]*\]|\\./g, "")))
const fromSerial = (v: number) => {
  const d = new Date(Math.round((v - 25569) * 86400000)), p = (n: number) => String(n).padStart(2, "0")
  const day = `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}`
  return v % 1 ? `${v >= 1 ? day + " " : ""}${p(d.getUTCHours())}:${p(d.getUTCMinutes())}` : day
}
async function xlsx(z: Zip): Promise<Sheet[]> {
  const wb = xml(await z.text("xl/workbook.xml"))
  if (!wb) throw new Error("没有找到工作簿（xl/workbook.xml）")
  const rel = await rels(z, "xl/workbook.xml")
  const shared = all(xml(await z.text("xl/sharedStrings.xml")), "si").map((si) => all(si, "t").filter((t) => t.parentElement?.localName !== "rPh").map((t) => t.textContent ?? "").join(""))
  const st = xml(await z.text("xl/styles.xml"))
  const codes = new Map(all(st, "numFmt").map((f) => [+(attr(f, "numFmtId") ?? 0), attr(f, "formatCode") ?? ""]))
  const dates = kids(all(st, "cellXfs")[0], "xf").map((x) => { const id = +(attr(x, "numFmtId") ?? 0); return isDateFormat(id, codes.get(id)) })
  const out: Sheet[] = []
  for (const s of all(wb, "sheet")) {
    const r = rel.get(rid(s)), d = r && xml(await z.text(r.target))
    if (!d) continue
    const rows: string[][] = []; let cols = 0, cut = false
    for (const row of all(d, "row")) {
      const y = +(attr(row, "r") ?? rows.length + 1) - 1
      if (y >= ROWS_MAX) { cut = true; break }
      for (const c of kids(row, "c")) {
        const x = col(attr(c, "r") ?? "A")
        if (x >= COLS_MAX) { cut = true; continue }
        const t = attr(c, "t"), v = kid(c, "v")?.textContent ?? ""
        const val = t === "s" ? shared[+v] ?? "" : t === "inlineStr" ? all(c, "t").map((e) => e.textContent ?? "").join("") : t === "b" ? (v === "1" ? "TRUE" : "FALSE")
          : !t && v && dates[+(attr(c, "s") ?? 0)] && Number.isFinite(+v) ? fromSerial(+v) : v
        if (!val) continue
        while (rows.length <= y) rows.push([])
        rows[y][x] = val; cols = Math.max(cols, x + 1)
      }
    }
    out.push({ name: attr(s, "name") ?? `Sheet${out.length + 1}`, rows, cols, cut })
  }
  return out
}

/* ── PowerPoint ── */
async function pptx(z: Zip, px: Pics): Promise<Slide[]> {
  const pres = xml(await z.text("ppt/presentation.xml"))
  if (!pres) throw new Error("没有找到演示文稿（ppt/presentation.xml）")
  const rel = await rels(z, "ppt/presentation.xml")
  const slides: Slide[] = []
  for (const id of all(pres, "sldId")) {
    const path = rel.get(rid(id))?.target, d = path && xml(await z.text(path))
    if (!path || !d) continue
    const srel = await rels(z, path)
    const slide: Slide = { n: slides.length + 1, title: "", parts: [] }
    const shapes: { y: number; x: number; e: Element }[] = []
    const walk = (e: Element | null) => { for (const c of kids(e)) {
      if (c.localName === "grpSp") walk(c)
      else if (["sp", "pic", "graphicFrame"].includes(c.localName)) { const off = all(c, "off")[0]; shapes.push({ y: +(attr(off, "y") ?? 0), x: +(attr(off, "x") ?? 0), e: c }) }
    } }
    walk(all(d, "spTree")[0] ?? null)
    shapes.sort((a, b) => a.y - b.y || a.x - b.x)
    for (const { e } of shapes) {
      if (e.localName === "sp") {
        const ph = attr(all(e, "ph")[0], "type") ?? ""
        const paras = all(e, "p").filter((p) => p.parentElement?.localName === "txBody").map((p) => ({
          level: +(attr(kid(p, "pPr"), "lvl") ?? 0),
          runs: kids(p).flatMap((r): Run[] => r.localName === "br" ? [{ br: true }] : ["r", "fld"].includes(r.localName) ? [{ t: kid(r, "t")?.textContent ?? "", b: attr(kid(r, "rPr"), "b") === "1", i: attr(kid(r, "rPr"), "i") === "1" }] : []),
        })).filter((p) => p.runs.some((r) => "t" in r && r.t.trim()))
        if (!paras.length) continue
        if ((ph === "title" || ph === "ctrTitle") && !slide.title) slide.title = paras.map((p) => p.runs.map((r) => "t" in r ? r.t : " ").join("")).join(" ")
        else slide.parts.push({ kind: "text", paras })
      } else if (e.localName === "pic") {
        const r = srel.get(attr(all(e, "blip")[0], "embed") ?? ""), u = r && !r.external ? await px.get(z, r.target) : null
        if (u) slide.parts.push({ kind: "img", src: u })
      } else {
        const tbl = all(e, "tbl")[0]
        if (tbl) slide.parts.push({ kind: "table", rows: kids(tbl, "tr").map((tr) => kids(tr, "tc").map((tc) => all(tc, "t").map((t) => t.textContent ?? "").join(""))) })
      }
    }
    slides.push(slide)
  }
  return slides
}

export async function readOffice(buf: ArrayBuffer, name: string, px: Pics): Promise<OfficeDoc> {
  const z = unzip(buf), ext = name.split(".").pop()?.toLowerCase()
  if (ext === "docx") return { type: "docx", blocks: await docx(z, px) }
  if (ext === "xlsx") return { type: "xlsx", sheets: await xlsx(z) }
  if (ext === "pptx") return { type: "pptx", slides: await pptx(z, px) }
  throw new Error(`不认识的格式：${name}`)
}

/* ── rendering ── */
function Runs({ runs }: { runs: Run[] }) {
  return <>{runs.map((r, n) => "br" in r ? <br key={n} />
    : "img" in r ? <img key={n} src={r.img} alt="" className="my-2 inline-block max-h-[50vh] max-w-full rounded" />
    : (() => { const t = <span key={n} className={cn(r.b && "font-semibold", r.i && "italic", r.u && "underline", r.s && "line-through", "whitespace-pre-wrap")}>{r.t}</span>
      return r.href ? <a key={n} href={r.href} target="_blank" rel="noopener noreferrer" className="text-blue underline">{t}</a> : t })())}</>
}
const H: Record<string, string> = { title: "text-[26px] font-semibold leading-tight mt-2 mb-3", h1: "text-[22px] font-semibold mt-5 mb-2", h2: "text-[19px] font-semibold mt-4 mb-1.5", h3: "text-[17px] font-semibold mt-3 mb-1", h4: "font-semibold mt-3 mb-1", h5: "font-semibold mt-2", h6: "font-semibold mt-2" }
function Blocks({ blocks }: { blocks: Block[] }) {
  return <>{blocks.map((b, n) => b.kind === "table"
    ? <div key={n} className="my-3 overflow-x-auto"><table className="w-full border-collapse type-footnote"><tbody>{b.rows.map((r, k) => <tr key={k}>{r.map((c, j) => <td key={j} className="border px-2 py-1 align-top"><Blocks blocks={c} /></td>)}</tr>)}</tbody></table></div>
    : <p key={n} className={cn("min-h-[1lh] break-words", H[b.style] ?? "my-1.5", b.list != null && "relative pl-5 before:absolute before:left-1.5 before:content-['•'] before:text-muted-foreground")} style={b.list ? { marginLeft: b.list * 20 } : undefined}><Runs runs={b.runs} /></p>)}</>
}
function Sheets({ sheets }: { sheets: Sheet[] }) {
  const [at, setAt] = useState(sheets[0]?.name ?? "")
  const s = sheets.find((x) => x.name === at) ?? sheets[0]
  if (!s) return <div className="py-8 text-center type-footnote text-muted-foreground">工作簿里没有工作表</div>
  return (
    <div className="flex min-w-0 flex-col gap-2" data-office-sheets>
      {sheets.length > 1 && <div className="overflow-x-auto"><Segmented size="sm" value={s.name} onChange={setAt} options={sheets.map((x) => x.name)} /></div>}
      <div className="max-h-[60vh] overflow-auto rounded-xl border" data-ann-at={`工作表 ${s.name}`} data-office-sheet={s.name}>
        {!s.rows.length ? <div className="py-8 text-center type-footnote text-muted-foreground">这张表是空的</div> :
        <table className="border-collapse type-footnote">
          <thead className="sticky top-0 z-[1] bg-muted"><tr><th className="sticky left-0 bg-muted px-2 py-1" />{Array.from({ length: s.cols }, (_, n) => <th key={n} className="min-w-16 border-l px-2 py-1 text-center font-normal text-muted-foreground">{colName(n)}</th>)}</tr></thead>
          <tbody>{s.rows.map((r, y) => <tr key={y} className="border-t">
            <td className="sticky left-0 bg-muted px-2 py-1 text-right tabular-nums text-muted-foreground">{y + 1}</td>
            {Array.from({ length: s.cols }, (_, x) => <td key={x} className={cn("max-w-80 border-l px-2 py-1 align-top whitespace-pre-wrap break-words", /^-?[\d,.]+%?$/.test(r[x] ?? "") && "text-right tabular-nums")}>{r[x] ?? ""}</td>)}
          </tr>)}</tbody>
        </table>}
      </div>
      {s.cut && <div className="rounded-lg bg-muted/60 px-3 py-2 type-footnote text-muted-foreground">表格较大，只显示了前 {ROWS_MAX} 行、{COLS_MAX} 列</div>}
    </div>
  )
}
function Slides({ slides }: { slides: Slide[] }) {
  if (!slides.length) return <div className="py-8 text-center type-footnote text-muted-foreground">演示文稿里没有幻灯片</div>
  return (
    <div className="flex flex-col gap-3" data-office-slides>
      {slides.map((s) => (
        <section key={s.n} className="rounded-xl border px-5 py-4" data-ann-at={`第 ${s.n} 张幻灯片`} data-office-slide={s.n}>
          <div className="mb-2 type-footnote tabular-nums text-muted-foreground">{s.n} / {slides.length}</div>
          {s.title && <h3 className="mb-2 text-[19px] font-semibold leading-snug">{s.title}</h3>}
          {s.parts.map((p, n) => p.kind === "img" ? <img key={n} src={p.src} alt="" className="my-2 max-h-[45vh] max-w-full rounded" />
            : p.kind === "table" ? <div key={n} className="my-2 overflow-x-auto"><table className="w-full border-collapse type-footnote"><tbody>{p.rows.map((r, k) => <tr key={k}>{r.map((c, j) => <td key={j} className="border px-2 py-1 align-top">{c}</td>)}</tr>)}</tbody></table></div>
            : <div key={n} className="my-1.5">{p.paras.map((x, k) => <p key={k} className={cn("break-words", x.level > 0 || p.paras.length > 1 ? "relative pl-5 before:absolute before:left-1.5 before:content-['•'] before:text-muted-foreground" : "")} style={x.level ? { marginLeft: x.level * 20 } : undefined}><Runs runs={x.runs} /></p>)}</div>)}
          {!s.title && !s.parts.length && <div className="type-footnote text-tertiary">这张幻灯片没有文字或图片</div>}
        </section>
      ))}
    </div>
  )
}

/** fetches the file and renders it; failures say what went wrong and leave the download */
export function OfficeView({ url, name, fallback }: { url: string; name: string; fallback: (reason: string) => ReactNode }) {
  const [doc, setDoc] = useState<OfficeDoc | null>(null), [err, setErr] = useState("")
  useEffect(() => {
    let live = true; const px = pics()
    setDoc(null); setErr("")
    fetch(url, { credentials: "same-origin" }).then((r) => { if (!r.ok) throw new Error(`读取失败（${r.status}）`); return r.arrayBuffer() })
      .then((b) => readOffice(b, name, px)).then((d) => live && setDoc(d), (e: Error) => live && setErr(e.message || "解析失败"))
    return () => { live = false; px.urls.forEach((u) => URL.revokeObjectURL(u)) }
  }, [url, name])
  if (err) return <>{fallback(`${name} 没能在这里打开：${err}。可以下载后用本地应用查看`)}</>
  if (!doc) return <div className="py-16 text-center type-footnote text-muted-foreground">正在读取 {name}…</div>
  return (
    <div className="min-w-0" data-office={doc.type}>
      {doc.type === "docx" ? <article className="max-h-[60vh] overflow-auto rounded-xl border px-5 py-4 type-body" data-ann-at="">{doc.blocks.length ? <Blocks blocks={doc.blocks} /> : <div className="type-footnote text-muted-foreground">文档是空的</div>}</article>
        : doc.type === "xlsx" ? <Sheets sheets={doc.sheets} /> : <Slides slides={doc.slides} />}
    </div>
  )
}
