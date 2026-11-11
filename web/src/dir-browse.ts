/* The directory chooser of a new project, without its drawing: where a path being typed is looked up, what of a
 * directory's list is shown for it, what the chooser says when it shows none, and which Nodes the project can be on. */
import type { RBrowse, RNode } from "@/api"

/** `~/a/b` or `/a/b` as the steps down to it */
export const crumbs = (p: string) => {
  const abs = p.startsWith("/"), segs = p.split("/").filter(Boolean)
  return [...(abs ? [{ name: "/", path: "/" }] : []), ...segs.map((name, i) => ({ name, path: (abs ? "/" : "") + segs.slice(0, i + 1).join("/") }))]
}

/** where a typed path is looked at: the directory itself when it is one; while its last part is still being typed,
 *  its parent, with that part as what narrows the list. `git` is known only of a directory that is there and named
 *  in full: an empty path shows the home directory without choosing it */
export async function lookUp(browse: (path: string) => Promise<RBrowse>, path: string): Promise<{ at: RBrowse; typed: string; git: boolean | null }> {
  let at = await browse(path.trim()), typed = ""
  if (!at.exists && at.parent != null) { typed = at.path.slice(at.path.lastIndexOf("/") + 1); at = await browse(at.parent) }
  return { at, typed, git: path.trim() && !typed && at.exists ? at.git : null }
}

/** the directories whose name starts with what is typed, whatever its case */
export const narrowed = (at: RBrowse, typed: string) => at.dirs.filter((x) => x.name.toLowerCase().startsWith(typed.toLowerCase()))

/** why no directory is listed, as the key of its sentence (`pages:projects.*`); "" when some are */
export const emptyNote = (at: RBrowse, typed: string) =>
  !at.exists ? "browseMissing" : at.denied ? "browseDenied" : !at.dirs.length ? "browseEmpty" : !narrowed(at, typed).length ? "browseNoMatch" : ""

/** the name a project takes from its directory until it is given one: the last part of the path, 40 characters at
 *  most; "" for the home directory and the root, which name nothing */
export const nameOf = (path: string) => { const base = path.slice(path.lastIndexOf("/") + 1); return base && base !== "~" ? base.slice(0, 40) : "" }

/** the Nodes as a new project lists them: each once; the Hub's own (if the list has it) apart from the others,
 *  and a revoked one not at all */
export function projectNodes(nodes: RNode[]): { local: RNode | undefined; remote: RNode[] } {
  const each = [...new Map(nodes.map((x) => [x.node_id, x])).values()]
  return { local: each.find((x) => x.local), remote: each.filter((x) => !x.local && x.state !== "revoked") }
}
