import assert from "node:assert/strict"
import test from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"
import React from "react"
import { renderToStaticMarkup } from "react-dom/server"
import zhCommon from "../src/i18n/locales/zh-CN/common.ts"
import enCommon from "../src/i18n/locales/en/common.ts"

function loadTs(relPath, mocks) {
  const source = readFileSync(new URL(relPath, import.meta.url), "utf8")
  const code = ts.transpileModule(source, {
    fileName: relPath,
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2023,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText
  const mod = { exports: {} }
  const req = (spec) => {
    if (spec in mocks) return mocks[spec]
    throw new Error(`Unexpected import in test harness: ${spec}`)
  }
  runInNewContext(code, { module: mod, exports: mod.exports, require: req, console, URL, encodeURIComponent })
  return mod.exports
}

const actorMod = loadTs("../src/actor.ts", {
  "@/api": { fileUrl: (id) => `/api/files/${encodeURIComponent(id)}` },
})

const {
  avatarSrc,
  hasUploadedAvatar,
  actorSeed,
  memberActor,
  partyActor,
  resolveActor,
  disambiguateActors,
  formatActorName,
} = actorMod

function lookupI18n(dict, key, vars = {}) {
  const path = key.replace(/^common:/, "").split(".")
  let cur = dict
  for (const seg of path) cur = cur?.[seg]
  assert.equal(typeof cur, "string", `missing i18n key ${key}`)
  return cur.replace(/\{\{(\w+)\}\}/g, (_, k) => String(vars[k] ?? ""))
}

function loadActorAvatar({ lang = "zh-CN", st }) {
  const dict = lang === "zh-CN" ? zhCommon : enCommon
  const jsxRuntime = {
    jsx: (type, props, key) => React.createElement(type, key !== undefined ? { ...props, key } : props),
    jsxs: (type, props, key) => React.createElement(type, key !== undefined ? { ...props, key } : props),
    Fragment: React.Fragment,
  }
  const icon = (name) => (props) => React.createElement("svg", { "data-icon": name, ...props })
  return loadTs("../src/components/actor-avatar.tsx", {
    "react/jsx-runtime": jsxRuntime,
    "lucide-react": {
      BuildingIcon: icon("building"),
      BotIcon: icon("bot"),
      CpuIcon: icon("cpu"),
      UsersIcon: icon("users"),
    },
    cn: { cn: (...xs) => xs.filter(Boolean).join(" ") },
    "@/components/ui/avatar": {
      Avatar: ({ name, src, seed, size, shape, icon: glyph, ...rest }) =>
        React.createElement(
          "span",
          {
            "data-aui-avatar": shape,
            "data-seed": seed,
            "data-size": size,
            "data-name": name,
            ...rest,
          },
          src ? React.createElement("img", { src, alt: "" }) : glyph ?? React.createElement("span", null, (name || "?")[0]),
        ),
    },
    "@/components/affect/tooltip-clean": {
      Tooltip: ({ content, children }) =>
        React.createElement("span", { "data-tooltip-wrap": content }, children),
    },
    "@/robot": {
      Robot: ({ k, size, live }) =>
        React.createElement("span", { "data-robot": k, "data-size": size, "data-live": live ? "1" : undefined }),
    },
    "@/store": { st },
    "@/i18n": { useT: () => (k, vars) => lookupI18n(dict, k, vars) },
    "@/actor": actorMod,
  }).ActorAvatar
}

const ME = {
  user_id: "usr_owner",
  login: "lin.zx",
  name: "林知行",
  party_id: "ct_lin",
  avatar: { file: "file_lin_photo" },
  subtitle: "负责人 · 星河",
  email: "lin@example.test",
  role: "owner",
}

const MEMBER_A = {
  user_id: "usr_zhou_fe",
  login: "zhou.fe",
  name: "周明",
  party_id: "ct_zhou_fe",
  avatar: null,
  subtitle: "前端 · 星河",
  email: "zhou.fe@example.test",
  role: "member",
}

const MEMBER_B = {
  user_id: "usr_zhou_be",
  login: "zhou.be",
  name: "周明",
  party_id: "ct_zhou_be",
  avatar: null,
  subtitle: "后端 · 星河",
  email: "zhou.be@example.test",
  role: "member",
}

const BOTS = {
  steve: { n: "Steve", main: true, role: "总管" },
  code: { n: "开发", main: false, role: "写代码" },
}

test("avatarSrc prefers Hub-uploaded files, allows safe URLs, and blocks unsafe schemes", () => {
  assert.equal(avatarSrc({ file: "f_123", url: "https://cdn.example.test/a.png" }), "/api/files/f_123")
  assert.equal(avatarSrc({ url: "https://cdn.example.test/a.png" }), "https://cdn.example.test/a.png")
  assert.equal(avatarSrc({ url: "/local/avatar.png" }), "/local/avatar.png")
  assert.equal(avatarSrc({ url: "data:image/png;base64,AA==" }), "data:image/png;base64,AA==")
  assert.equal(avatarSrc({ url: "javascript:alert(1)" }), undefined)
  assert.equal(avatarSrc(null), undefined)
  assert.equal(hasUploadedAvatar({ file: "f_123" }), true)
  assert.equal(hasUploadedAvatar({ url: "https://cdn.example.test/a.png" }), false)
})

test("resolveActor resolves person, legacy me, bots, yolo, and system consistently", () => {
  const ctx = { me: ME, members: [ME, MEMBER_A, MEMBER_B], bots: BOTS, systemName: "系统" }

  const byUser = resolveActor("usr_owner", ctx)
  const byMe = resolveActor("me", ctx)
  const byParty = resolveActor("ct_lin", ctx)
  assert.equal(byUser.kind, "person")
  assert.equal(byUser.name, "林知行")
  assert.equal(byUser.id, "ct_lin")
  assert.equal(actorSeed(byUser), "ct_lin")
  assert.deepEqual(byMe, byUser, "legacy 'me' resolves to the exact same person identity as user_id")
  assert.deepEqual(byParty, byUser, "party_id resolves to the exact same person identity as user_id")

  const a = resolveActor("usr_zhou_fe", ctx)
  const b = resolveActor("usr_zhou_be", ctx)
  assert.equal(a.name, "周明")
  assert.equal(b.name, "周明")
  assert.notEqual(actorSeed(a), actorSeed(b), "same-named people get distinct stable color seeds")

  const codeBot = resolveActor("code", ctx)
  assert.equal(codeBot.kind, "bot")
  assert.equal(codeBot.botId, "code")
  assert.equal(codeBot.name, "开发 Bot")

  const yolo = resolveActor("yolo", ctx)
  assert.equal(yolo.kind, "bot")
  assert.equal(yolo.botId, "steve")
  assert.equal(yolo.name, "Steve")

  const sys = resolveActor("system", ctx)
  assert.equal(sys.kind, "system")
  assert.equal(sys.name, "系统")
})

test("disambiguateActors appends distinguishing context only when display names collide", () => {
  const map = disambiguateActors([
    { key: "ct_lin", name: "林知行", subtitle: "负责人 · 星河", fallbackId: "ct_lin" },
    { key: "ct_zhou_fe", name: "周明", subtitle: "前端 · 星河", fallbackId: "ct_zhou_fe" },
    { key: "ct_zhou_be", name: "周明", subtitle: "后端 · 星河", fallbackId: "ct_zhou_be" },
    { key: "ct_chen_1", name: "陈晨", subtitle: "设计", fallbackId: "ct_chen_1111" },
    { key: "ct_chen_2", name: "陈晨", subtitle: "设计", fallbackId: "ct_chen_2222" },
    { key: "ct_lu_1", name: "陆远", fallbackId: "usr_aaa1" },
    { key: "ct_lu_2", name: "陆远", fallbackId: "usr_bbb2" },
  ])
  assert.equal(map.get("ct_lin"), undefined, "unique name needs no disambiguator")
  assert.equal(map.get("ct_zhou_fe"), "前端 · 星河")
  assert.equal(map.get("ct_zhou_be"), "后端 · 星河")
  assert.equal(map.get("ct_chen_1"), "设计 · #1111", "colliding subtitles fall back to subtitle + id tail")
  assert.equal(map.get("ct_chen_2"), "设计 · #2222")
  assert.equal(map.get("ct_lu_1"), "#aaa1", "missing subtitles fall back to id tail")
  assert.equal(map.get("ct_lu_2"), "#bbb2")
  assert.equal(formatActorName("周明", map.get("ct_zhou_fe")), "周明 (前端 · 星河)")
})

test("ActorAvatar renders person photo/initials, bot robot, system badge, org/team, and localized tooltips", () => {
  const st = { me: ME, members: [ME, MEMBER_A, MEMBER_B], bots: BOTS }
  const ActorAvatarZh = loadActorAvatar({ lang: "zh-CN", st })
  const ActorAvatarEn = loadActorAvatar({ lang: "en", st })

  // Person with uploaded picture (display name is person name, never login handle)
  const meHtml = renderToStaticMarkup(React.createElement(ActorAvatarZh, { actorId: "usr_owner", size: 28, account: true }))
  assert.match(meHtml, /data-actor-kind="person"/)
  assert.match(meHtml, /data-actor-id="ct_lin"/)
  assert.match(meHtml, /data-account-avatar=""/)
  assert.match(meHtml, /src="\/api\/files\/file_lin_photo"/)
  assert.match(meHtml, /data-tooltip-wrap="林知行 · 人"/)
  assert.doesNotMatch(meHtml, /lin\.zx/, "login handle must never leak into avatar name/tooltip")

  // Same person resolved from contact party produces identical seed & image src
  const partyHtml = renderToStaticMarkup(
    React.createElement(ActorAvatarZh, {
      actor: partyActor({ id: ME.party_id, name: ME.name, avatar: ME.avatar, isMe: true }),
      size: 28,
    }),
  )
  assert.match(partyHtml, /data-seed="ct_lin"/)
  assert.match(partyHtml, /src="\/api\/files\/file_lin_photo"/)

  // Duplicate-named collaborators render distinct seeds and disambiguated hover tooltip
  const zhouFeHtml = renderToStaticMarkup(React.createElement(ActorAvatarZh, { actorId: "usr_zhou_fe", disambiguator: "前端 · 星河", size: 20 }))
  const zhouBeHtml = renderToStaticMarkup(React.createElement(ActorAvatarZh, { actorId: "usr_zhou_be", disambiguator: "后端 · 星河", size: 20 }))
  assert.match(zhouFeHtml, /data-seed="ct_zhou_fe"/)
  assert.match(zhouBeHtml, /data-seed="ct_zhou_be"/)
  assert.match(zhouFeHtml, /data-tooltip-wrap="周明（前端 · 星河） · 人"/)
  assert.match(zhouBeHtml, /data-tooltip-wrap="周明（后端 · 星河） · 人"/)

  // Bot renders agent-robot-avatar
  const botHtml = renderToStaticMarkup(React.createElement(ActorAvatarZh, { actorId: "code", size: 20 }))
  assert.match(botHtml, /data-actor-kind="bot"/)
  assert.match(botHtml, /data-robot="code"/)
  assert.match(botHtml, /data-tooltip-wrap="开发 Bot · Bot"/)

  // System renders system icon without redundant "系统 · 系统"
  const sysZh = renderToStaticMarkup(React.createElement(ActorAvatarZh, { actorId: "system", size: 18 }))
  assert.match(sysZh, /data-actor-kind="system"/)
  assert.match(sysZh, /data-icon="cpu"/)
  assert.match(sysZh, /data-tooltip-wrap="系统"/)

  const sysEn = renderToStaticMarkup(React.createElement(ActorAvatarEn, { actorId: "system", size: 18 }))
  assert.match(sysEn, /data-tooltip-wrap="System"/)

  // Org and Team render square shape with building/users glyphs
  const orgHtml = renderToStaticMarkup(
    React.createElement(ActorAvatarEn, { actor: partyActor({ id: "org_1", name: "Nova Labs", type: "org" }), size: 32 }),
  )
  assert.match(orgHtml, /data-aui-avatar="square"/)
  assert.match(orgHtml, /data-icon="building"/)
  assert.match(orgHtml, /data-tooltip-wrap="Nova Labs · Organization"/)

  // tooltip={false} omits tooltip wrapper while preserving aria-label
  const noTip = renderToStaticMarkup(React.createElement(ActorAvatarEn, { actor: memberActor(ME, true), size: 24, tooltip: false }))
  assert.doesNotMatch(noTip, /data-tooltip-wrap/)
  assert.match(noTip, /aria-label="林知行 · Person"/)
})
