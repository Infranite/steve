/* SKILL.md as people paste it: optional YAML front matter between two `---` lines (`name`, `description`; plain,
 * quoted or a folded / literal block), then the Markdown body. Only what a Bot's skill keeps is read; other keys go. */

export type SkillMd = { name: string; description: string; content: string }

const unquote = (v: string) => {
  const s = v.trim()
  if (s.length >= 2 && s.startsWith('"') && s.endsWith('"')) { try { return JSON.parse(s) as string } catch { return s.slice(1, -1) } }
  if (s.length >= 2 && s.startsWith("'") && s.endsWith("'")) return s.slice(1, -1).replace(/''/g, "'")
  return s
}

/** a pasted SKILL.md → its name, description and body; without front matter the whole text is the body */
export function parseSkillMd(text: string): SkillMd {
  const src = text.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n")
  const m = /^---[ \t]*\n([\s\S]*?)\n---[ \t]*(?:\n|$)/.exec(src)
  if (!m) return { name: "", description: "", content: src.trim() }
  const lines = m[1].split("\n"), out: Record<string, string> = {}
  for (let i = 0; i < lines.length; i++) {
    const kv = /^([A-Za-z0-9_-]+):[ \t]*(.*)$/.exec(lines[i])
    if (!kv) continue
    let v = kv[2].trim()
    if (v === "" || /^[>|][+-]?$/.test(v)) {
      const block: string[] = []
      while (i + 1 < lines.length && (/^\s/.test(lines[i + 1]) || lines[i + 1] === "")) block.push(lines[++i].trim())
      v = block.filter(Boolean).join(" ")
    } else v = unquote(v)
    out[kv[1]] = v
  }
  return { name: out.name ?? "", description: (out.description ?? "").replace(/\s+/g, " ").trim(), content: src.slice(m[0].length).trim() }
}

/** a name every harness takes as a directory: lower-case letters, digits and single hyphens, at most 64 */
export const SKILL_NAME = /^[a-z0-9]+(-[a-z0-9]+)*$/
export const skillNameOk = (n: string) => n.length <= 64 && SKILL_NAME.test(n)
/** what a typed or pasted name becomes: lower case, other characters as single hyphens */
export const skillName = (n: string) => n.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64).replace(/-+$/, "")
