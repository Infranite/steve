/* The harness's own session behind a Steve session, so the user can pick it up in the harness itself (ST-28).
 * The ACP session id is the harness's id: claude-agent-acp hands it to Claude Code as its session id
 * (~/.claude/projects/<cwd>/<id>.jsonl), codex-acp answers with the Codex thread id ($CODEX_HOME/sessions/…/rollout-<time>-<id>.jsonl).
 * An imported session has no ACP id; its import key names the transcript file, whose name carries the id.
 * Mirrors hub-rs/crates/steve-hub/src/resume.rs. */
import { closeSync, existsSync, openSync, readSync, readdirSync, statSync } from "node:fs"
import { basename, join } from "node:path"
import { homedir } from "node:os"

type Row = { harness: string; cwd: string; acp_session_id: string | null; import_key?: string | null }

export function nativeId(s: Row): string | null {
  if (s.acp_session_id) return s.acp_session_id
  const m = /^(cc|cx):(.*)$/.exec(s.import_key ?? "")
  if (!m) return null
  const stem = basename(m[2]).replace(/\.jsonl$/, "")
  return (m[1] === "cc" ? stem : stem.length >= 36 ? stem.slice(-36) : "") || null
}

const sh = (s: string) => (s && /^[\w\-./=:@%+,]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`)
const claudeDir = () => process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude")
const codexHome = () => process.env.CODEX_HOME || join(homedir(), ".codex")

export function resumeCommand(harness: string, id: string, cwd: string): string | null {
  const [v, dir, def, run] = harness === "claude-code" ? ["CLAUDE_CONFIG_DIR", claudeDir(), join(homedir(), ".claude"), `claude --resume ${sh(id)}`]
    : harness === "codex" ? ["CODEX_HOME", codexHome(), join(homedir(), ".codex"), `codex resume ${sh(id)}`] : []
  if (!run) return null
  return (cwd ? `cd ${sh(cwd)} && ` : "") + (dir === def ? "" : `${v}=${sh(dir!)} `) + run
}

const ls = (d: string) => { try { return readdirSync(d).sort().reverse() } catch { return [] } }
function transcriptFile(harness: string, id: string, cwd: string): string | null {
  if (harness === "claude-code") {
    const p = join(claudeDir(), "projects"), direct = join(p, cwd.replace(/[^a-zA-Z0-9]/g, "-"), id + ".jsonl")
    if (existsSync(direct)) return direct
    return ls(p).map((d) => join(p, d, id + ".jsonl")).find((f) => existsSync(f)) ?? null
  }
  if (harness === "codex") {
    const root = join(codexHome(), "sessions")
    for (const y of ls(root)) for (const m of ls(join(root, y))) for (const d of ls(join(root, y, m))) {
      const f = ls(join(root, y, m, d)).find((n) => n.startsWith("rollout-") && n.endsWith(`-${id}.jsonl`))
      if (f) return join(root, y, m, d, f)
    }
  }
  return null
}

/** the `sessionId`/`id` and `cwd` the transcript records in its first lines; nothing else is kept */
function recorded(file: string, harness: string): { id: string | null; cwd: string | null } {
  let id: string | null = null, cwd: string | null = null
  // the first 8 MB (a pasted screenshot makes a line of a megabyte or more); a line cut there does not parse
  const fd = openSync(file, "r"), buf = Buffer.alloc(Math.min(statSync(file).size, 8 << 20)), n = readSync(fd, buf, 0, buf.length, 0); closeSync(fd)
  const lines = buf.subarray(0, n).toString("utf8").split("\n", 50)
  for (const l of lines) {
    let v: any; try { v = JSON.parse(l) } catch { continue }
    if (harness === "codex") { if (v?.type === "session_meta") return { id: v.payload?.id ?? null, cwd: v.payload?.cwd ?? null }; continue }
    id ??= typeof v?.sessionId === "string" ? v.sessionId : null
    cwd ??= typeof v?.cwd === "string" ? v.cwd : null
    if (id && cwd) break
  }
  return { id, cwd }
}

export function resumeInfo(s: Row) {
  const id = nativeId(s), cwd = s.cwd ?? "", cwd_exists = !!cwd && existsSync(cwd) && statSync(cwd).isDirectory()
  if (!id) return { harness: s.harness, id: null, cwd, cwd_exists, command: null, transcript: null }
  const f = transcriptFile(s.harness, id, cwd)
  let transcript = null
  if (f) {
    const r = recorded(f, s.harness)
    transcript = { path: f, updated: Math.floor(statSync(f).mtimeMs), id: r.id, cwd: r.cwd, matches: (!r.id || r.id === id) && (!r.cwd || !cwd || r.cwd === cwd) }
  }
  return { harness: s.harness, id, cwd, cwd_exists, command: resumeCommand(s.harness, id, cwd), transcript }
}
