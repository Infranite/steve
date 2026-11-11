import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/dir-browse.ts")
const dir = (path, names = [], more = {}) => ({
  path, parent: path === "/" ? null : path.slice(0, path.lastIndexOf("/")) || "/", exists: true, denied: false, git: false, more: false,
  dirs: names.map((name) => ({ name, path: `${path}/${name}`, git: name === "app" })), ...more,
})
/** a machine with these directories; anything else is not there, and says where its parent would be */
const machine = (dirs) => {
  const asked = []
  const browse = async (path) => {
    asked.push(path)
    const at = path === "" ? "~" : path.replace(/\/$/, "") || "/"
    return dirs[at] ?? { ...dir(at), exists: false, dirs: [] }
  }
  return { asked, browse }
}

test("a path is the steps down to it, from the home directory or from the root", () => {
  assert.deepEqual(m.crumbs("~/code/app"), [{ name: "~", path: "~" }, { name: "code", path: "~/code" }, { name: "app", path: "~/code/app" }])
  assert.deepEqual(m.crumbs("/srv/app"), [{ name: "/", path: "/" }, { name: "srv", path: "/srv" }, { name: "app", path: "/srv/app" }])
  assert.deepEqual(m.crumbs("/"), [{ name: "/", path: "/" }])
})

test("a directory named in full is looked at itself, and whether it is a checkout is known", async () => {
  const pc = machine({ "~/code": dir("~/code", ["app", "notes"]), "~/code/app": dir("~/code/app", ["src"], { git: true }) })
  const seen = await m.lookUp(pc.browse, " ~/code/app ")
  assert.deepEqual(pc.asked, ["~/code/app"])
  assert.deepEqual([seen.at.path, seen.typed, seen.git], ["~/code/app", "", true])
  const plain = await m.lookUp(pc.browse, "~/code")
  assert.equal(plain.git, false)
  assert.deepEqual(plain.at.dirs.map((x) => x.name), ["app", "notes"])
})

test("a last part still being typed shows its parent, narrowed, and chooses nothing yet", async () => {
  const pc = machine({ "~/code": dir("~/code", ["app", "Apple", "notes"], { git: true }) })
  const seen = await m.lookUp(pc.browse, "~/code/ap")
  assert.deepEqual(pc.asked, ["~/code/ap", "~/code"])
  assert.equal(seen.typed, "ap")
  assert.equal(seen.git, null, "the parent being a checkout says nothing of the directory being typed")
  assert.deepEqual(m.narrowed(seen.at, seen.typed).map((x) => x.name), ["app", "Apple"])
  assert.equal(m.emptyNote(seen.at, seen.typed), "")
  assert.equal(m.emptyNote(seen.at, "zz"), "browseNoMatch")
})

test("an empty path shows the home directory without choosing it", async () => {
  const pc = machine({ "~": dir("~", ["code"], { parent: "/home" }) })
  const seen = await m.lookUp(pc.browse, "  ")
  assert.deepEqual(pc.asked, [""])
  assert.equal(seen.at.path, "~")
  assert.equal(seen.git, null)
})

test("a path with nothing there, a directory the system keeps closed and an empty one each say so", async () => {
  // neither the directory nor its parent: what the parent answers is shown, and that is "not there"
  const pc = machine({})
  const gone = await m.lookUp(pc.browse, "/nowhere/deep")
  assert.deepEqual(pc.asked, ["/nowhere/deep", "/nowhere"])
  assert.equal(m.emptyNote(gone.at, gone.typed), "browseMissing")
  assert.equal(gone.git, null)
  // the root has no parent to fall back to
  const none = await m.lookUp(async () => ({ ...dir("/"), exists: false }), "/")
  assert.equal(none.typed, "")
  assert.equal(m.emptyNote(dir("~/Documents", [], { denied: true }), ""), "browseDenied")
  assert.equal(m.emptyNote(dir("~/empty"), ""), "browseEmpty")
  assert.equal(m.emptyNote(dir("~/code", ["app"]), ""), "")
})

test("a project is named after its directory until it has a name of its own", () => {
  assert.equal(m.nameOf("~/code/my-app"), "my-app")
  assert.equal(m.nameOf("/srv/" + "x".repeat(60)), "x".repeat(40))
  assert.equal(m.nameOf("~"), "")
  assert.equal(m.nameOf("/"), "")
})

test("the Hub's own Node is apart from the others, each listed once and a revoked one not at all", () => {
  const node = (node_id, more = {}) => ({ node_id, name: node_id, fingerprint: "", state: "active", linked: true, local: false, ...more })
  const { local, remote } = m.projectNodes([node("nod_hub", { local: true }), node("nod_a"), node("nod_a"), node("nod_b", { state: "revoked" }), node("nod_c", { state: "paused" })])
  assert.equal(local.node_id, "nod_hub")
  assert.deepEqual(remote.map((x) => x.node_id), ["nod_a", "nod_c"])
  assert.deepEqual(m.projectNodes([]), { local: undefined, remote: [] })
})
