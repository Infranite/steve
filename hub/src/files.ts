/* Attachments: validation by content (not by the name or the claimed type), storage under data/files,
 * and handing them to agents (copied into the session's working directory, images also inline in the prompt). */
import { createHash } from "node:crypto"
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { config } from "./config.js"
import { files, fileRef, uid, type FileRef, type FileRow } from "./repo.js"

export const MAX_FILE = 20 * 1024 * 1024
export const MAX_FILES = 10
/** image types a model takes inline; everything else only goes by path */
export const INLINE_IMAGES = ["image/png", "image/jpeg", "image/gif", "image/webp"]
/** the Messages API takes base64 images up to 5 MB; larger ones are read from disk by the agent */
const MAX_INLINE = 3_750_000

type Kind = { mime: string; ext: string[] }
const T: Record<string, Kind> = {
  png: { mime: "image/png", ext: ["png"] },
  jpeg: { mime: "image/jpeg", ext: ["jpg", "jpeg"] },
  gif: { mime: "image/gif", ext: ["gif"] },
  webp: { mime: "image/webp", ext: ["webp"] },
  pdf: { mime: "application/pdf", ext: ["pdf"] },
  zip: { mime: "application/zip", ext: ["zip", "docx", "xlsx", "pptx", "jar", "apk", "ipa", "epub"] },
  text: { mime: "text/plain; charset=utf-8", ext: [] },
}
const starts = (b: Buffer, sig: number[] | string, at = 0) => { const s = typeof sig === "string" ? Buffer.from(sig, "latin1") : Buffer.from(sig); return b.length >= at + s.length && b.subarray(at, at + s.length).equals(s) }
const utf8 = new TextDecoder("utf-8", { fatal: true })
/** the type of the bytes, or null when it is not one Steve accepts (executables, archives other than zip, svg/html are text at most) */
export function sniff(b: Buffer): Kind | null {
  if (starts(b, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return T.png
  if (starts(b, [0xff, 0xd8, 0xff])) return T.jpeg
  if (starts(b, "GIF87a") || starts(b, "GIF89a")) return T.gif
  if (starts(b, "RIFF") && starts(b, "WEBP", 8)) return T.webp
  if (starts(b, "%PDF-")) return T.pdf
  if (starts(b, [0x50, 0x4b, 0x03, 0x04]) || starts(b, [0x50, 0x4b, 0x05, 0x06])) return T.zip
  /* text: valid UTF-8 without NUL bytes (logs, code, csv, markdown…); always served as a download */
  if (!b.includes(0)) { try { utf8.decode(b); return T.text } catch { /* not text */ } }
  return null
}

/** a file name that is safe on disk, in a header and in a prompt: no directories, control or bidi characters, reserved punctuation or leading dots */
export function cleanName(raw: string | null | undefined, kind: Kind): string {
  let n = String(raw ?? "").normalize("NFC").replace(/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g, "")
  n = (n.split(/[\\/]/).pop() ?? "").replace(/[<>:"|?*`$]/g, "_").replace(/\s+/g, " ").replace(/^[.\s]+/, "").trim()
  let dot = n.lastIndexOf("."), ext = dot > 0 ? n.slice(dot + 1).toLowerCase() : ""
  /* the extension has to say what the bytes are (a PNG named .exe becomes .exe.png); text keeps its own, or gets .txt */
  if (kind.ext.length && !kind.ext.includes(ext)) { n = (n || "file") + "." + kind.ext[0]; dot = n.lastIndexOf("."); ext = kind.ext[0] }
  if (!kind.ext.length && !ext) { n = (n || "file") + ".txt"; dot = n.lastIndexOf("."); ext = "txt" }
  if (!n || n.startsWith(".")) n = "file" + (ext ? "." + ext : "")
  if (n.length > 120) { const tail = n.slice(n.lastIndexOf(".")); n = n.slice(0, 120 - tail.length) + tail }
  return n
}

const err = (msg: string, status: number) => Object.assign(new Error(msg), { status })
/** check an upload; throws with an HTTP status (413 too large, 415 not allowed, 400 empty) */
export function validate(b: Buffer, name: string | null | undefined) {
  if (!b.length) throw err("文件是空的", 400)
  if (b.length > MAX_FILE) throw err(`文件太大，最多 ${MAX_FILE / 1024 / 1024} MB`, 413)
  const kind = sniff(b)
  if (!kind) throw err("不支持这种文件：只能上传图片（PNG/JPEG/GIF/WebP）、PDF、文本/日志或 zip", 415)
  return { mime: kind.mime, name: cleanName(name, kind) }
}

export const blobPath = (f: Pick<FileRow, "workspace_id" | "sha256" | "id">) => join(config.dataDir, "files", f.workspace_id, f.sha256.slice(0, 2), f.id)

/** validate and store; the same bytes under the same name in a workspace are stored once */
export function store(ws: string, b: Buffer, name: string | null | undefined, by: string): FileRow {
  const v = validate(b, name), sha256 = createHash("sha256").update(b).digest("hex")
  const dup = files.same(ws, sha256, v.name); if (dup && existsSync(blobPath(dup))) return dup
  const row = { id: uid("fil"), workspace_id: ws, sha256, name: v.name, mime: v.mime, size: b.length, created_by: by }
  const p = blobPath(row); mkdirSync(join(p, ".."), { recursive: true }); writeFileSync(p, b, { mode: 0o600 })
  return files.create(row)
}

/** attachment ids from a request → descriptors; every id must be a file of this workspace */
export function refs(ws: string, ids: string[] | undefined): FileRef[] {
  return [...new Set(ids ?? [])].map((id) => { const f = files.get(id); if (!f || f.workspace_id !== ws) throw err("没有这个附件", 404); return fileRef(f) })
}

export const isInlineImage = (mime: string) => INLINE_IMAGES.includes(mime)
const kb = (n: number) => (n >= 1024 * 1024 ? (n / 1024 / 1024).toFixed(1) + " MB" : Math.max(1, Math.round(n / 1024)) + " KB")

/** copy attachments into <cwd>/.steve/attachments (ignored by git) and say where they are; images that fit also go inline */
export function forAgent(cwd: string, fs: FileRef[], inlineImages: boolean): { note: string; images: { type: "image"; data: string; mimeType: string }[] } {
  if (!fs.length) return { note: "", images: [] }
  const dir = join(cwd, ".steve", "attachments"); mkdirSync(dir, { recursive: true })
  const ign = join(dir, ".gitignore"); if (!existsSync(ign)) writeFileSync(ign, "*\n")
  const lines: string[] = [], images: { type: "image"; data: string; mimeType: string }[] = []
  for (const r of fs) {
    const f = files.get(r.id); if (!f) continue
    const rel = `.steve/attachments/${f.id.slice(4)}-${f.name}`, dst = join(cwd, rel)
    if (!existsSync(dst)) copyFileSync(blobPath(f), dst)
    const inline = inlineImages && isInlineImage(f.mime) && f.size <= MAX_INLINE
    if (inline) images.push({ type: "image", data: readBase64(f), mimeType: f.mime })
    lines.push(`- ${rel}（${f.mime.split(";")[0]}，${kb(f.size)}${inline ? "，图片也直接附在这条消息里" : ""}）`)
  }
  return { note: `\n\n用户附带了 ${lines.length} 个附件，已保存在工作目录里（绝对路径前缀 ${cwd}/），需要时直接读取：\n${lines.join("\n")}`, images }
}
const readBase64 = (f: FileRow) => readFileSync(blobPath(f)).toString("base64")
