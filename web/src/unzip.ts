/* reading a zip (Word, Excel and PowerPoint files are zips of XML) with the browser's own inflate: no library.
 * Stored and deflated entries; no zip64, no encryption — Office files need neither. */

export type Zip = { names: string[]; bytes: (name: string) => Promise<Uint8Array | null>; text: (name: string) => Promise<string | null> }

async function inflate(data: Uint8Array): Promise<Uint8Array> {
  const s = new Blob([data as BlobPart]).stream().pipeThrough(new DecompressionStream("deflate-raw"))
  return new Uint8Array(await new Response(s).arrayBuffer())
}

export function unzip(buf: ArrayBuffer): Zip {
  const b = new Uint8Array(buf), v = new DataView(buf)
  /* the end of central directory record: within the last 64 KB (its comment) */
  let end = -1
  for (let i = b.length - 22; i >= Math.max(0, b.length - 65_557); i--) if (v.getUint32(i, true) === 0x06054b50) { end = i; break }
  if (end < 0) throw new Error("不是有效的 zip 文件")
  const count = v.getUint16(end + 10, true)
  let p = v.getUint32(end + 16, true)
  const entries = new Map<string, { method: number; size: number; at: number }>()
  const dec = new TextDecoder()
  for (let n = 0; n < count && p + 46 <= b.length && v.getUint32(p, true) === 0x02014b50; n++) {
    const method = v.getUint16(p + 10, true), size = v.getUint32(p + 20, true)
    const nl = v.getUint16(p + 28, true), xl = v.getUint16(p + 30, true), cl = v.getUint16(p + 32, true), at = v.getUint32(p + 42, true)
    entries.set(dec.decode(b.subarray(p + 46, p + 46 + nl)), { method, size, at })
    p += 46 + nl + xl + cl
  }
  const bytes = async (name: string) => {
    const e = entries.get(name.replace(/^\//, ""))
    if (!e || v.getUint32(e.at, true) !== 0x04034b50) return null
    const start = e.at + 30 + v.getUint16(e.at + 26, true) + v.getUint16(e.at + 28, true)
    const raw = b.subarray(start, start + e.size)
    return e.method === 0 ? raw : e.method === 8 ? inflate(raw) : null
  }
  return { names: [...entries.keys()], bytes, text: async (name) => { const x = await bytes(name); return x && dec.decode(x) } }
}

/** a relationship target (`../media/a.png` from `ppt/slides/`) as a path in the zip */
export function resolve(dir: string, target: string): string {
  if (target.startsWith("/")) return target.slice(1)
  const parts = dir.split("/").filter(Boolean)
  for (const s of target.split("/")) { if (s === "..") parts.pop(); else if (s && s !== ".") parts.push(s) }
  return parts.join("/")
}
