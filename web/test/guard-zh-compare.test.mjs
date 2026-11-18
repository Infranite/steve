/* Existing Chinese comparisons are a shrinking baseline. Display text, prompts and logs are
 * content; branch on domain code values instead. STEVE_ZH_BLESS=1 node --test
 * web/test/guard-zh-compare.test.mjs rewrites the baseline. */
import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync, readdirSync, writeFileSync } from "node:fs"
import { join, relative } from "node:path"
import { fileURLToPath } from "node:url"
import ts from "typescript"

const root = fileURLToPath(new URL("..", import.meta.url))
const list = join(root, "test/guard-zh-compare.txt")
const methods = new Set(["includes", "startsWith", "endsWith", "indexOf", "match", "replace", "split"])
const operators = new Set([
  ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken,
  ts.SyntaxKind.EqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsToken,
])
const han = (s) => /\p{Script=Han}/u.test(s)
const files = (dir) => readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
  const p = join(dir, e.name)
  if (["src/i18n/locales", "src/components/affect", "src/gen"].includes(relative(root, p).replaceAll("\\", "/"))) return []
  return e.isDirectory() ? files(p) : /\.(ts|tsx)$/.test(e.name) ? [p] : []
})
const unwrap = (n) => {
  while (ts.isParenthesizedExpression(n) || ts.isAsExpression(n) || ts.isNonNullExpression(n) || ts.isSatisfiesExpression(n)) n = n.expression
  return n
}
// Only literal/template operands count, not arbitrary literals inside a function call.
const literals = (n) => {
  n = unwrap(n)
  if (ts.isStringLiteral(n) || ts.isNoSubstitutionTemplateLiteral(n)) return han(n.text) ? [n.text] : []
  if (ts.isTemplateExpression(n)) {
    const parts = [n.head.text, ...n.templateSpans.map((s) => s.literal.text)]
    return parts.filter(han)
  }
  return []
}
const scan = (src, filename = "fixture.tsx") => {
  const ast = ts.createSourceFile(filename, src, ts.ScriptTarget.Latest, true,
    filename.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  assert.equal(ast.parseDiagnostics.length, 0, `${filename}: invalid TypeScript`)
  const found = new Map()
  const add = (values) => { for (const s of values) { const key = JSON.stringify(s); found.set(key, (found.get(key) ?? 0) + 1) } }
  const visit = (n) => {
    if (ts.isBinaryExpression(n) && operators.has(n.operatorToken.kind)) {
      add(literals(n.left)); add(literals(n.right))
    }
    if (ts.isCaseClause(n)) add(literals(n.expression))
    if (ts.isCallExpression(n)) {
      let receiver, method
      if (ts.isPropertyAccessExpression(n.expression)) {
        receiver = n.expression.expression; method = n.expression.name.text
      } else if (ts.isElementAccessExpression(n.expression) && ts.isStringLiteral(n.expression.argumentExpression)) {
        receiver = n.expression.expression; method = n.expression.argumentExpression.text
      }
      if (methods.has(method)) for (const arg of n.arguments) add(literals(arg))
      if (method === "includes" && ts.isArrayLiteralExpression(unwrap(receiver))) {
        for (const element of unwrap(receiver).elements) add(literals(element))
      }
    }
    ts.forEachChild(n, visit)
  }
  visit(ast)
  return found
}

const differences = (now, listed) => {
  const problems = []
  for (const [k, n] of now) if (n !== (listed.get(k) ?? 0)) problems.push(`${k}: ${n}, listed ${listed.get(k) ?? 0}`)
  for (const [k, n] of listed) if (!now.has(k)) problems.push(`${k}: gone, listed ${n}; shorten the list`)
  return problems
}

test("Chinese comparisons only shrink", () => {
  const now = new Map()
  for (const f of files(join(root, "src"))) {
    for (const [literal, n] of scan(readFileSync(f, "utf8"), f)) now.set(`${relative(root, f).replaceAll("\\", "/")}\t${literal}`, n)
  }
  if (process.env.STEVE_ZH_BLESS === "1") {
    const text = [...now].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([k, n]) => `${k}\t${n}\n`).join("")
    writeFileSync(list, "# Existing Chinese comparisons; replace display text with domain code values. Only shrinks.\n# file under web<TAB>JSON-escaped literal<TAB>count.\n# Rewrite: STEVE_ZH_BLESS=1 node --test web/test/guard-zh-compare.test.mjs\n" + text)
    return
  }
  const listed = new Map(readFileSync(list, "utf8").split("\n").filter((l) => l && !l.startsWith("#")).map((l) => {
    const parts = l.split("\t")
    assert.equal(parts.length, 3, `bad baseline row: ${l}`)
    assert.ok(Number.isSafeInteger(Number(parts[2])) && Number(parts[2]) > 0, `bad count: ${l}`)
    return [`${parts[0]}\t${parts[1]}`, Number(parts[2])]
  }))
  assert.deepEqual(differences(now, listed), [], "中文比较守卫：改成代码值，或在第 4 波前确有必要时用 STEVE_ZH_BLESS=1 重写并在 PR 里说明。已消失或减少的条目必须缩短清单。")
})

test("AST covers branches, templates and array membership while leaving display content alone", () => {
  const source = [
    'x === "相等"; "不等" !== x; x == "双等"; x != "双不等";',
    'switch (x) { case "分支": break; }',
    'x.includes("包含"); x.startsWith("开头"); x.endsWith("结尾"); x.indexOf("位置");',
    'x.match("匹配"); x.replace("替换", "输出"); x.split("分隔");',
    '["同义", `关键词`].includes(x); x["includes"]("索引调用");',
    'x === `模板${id}末尾`; x === "\\u6d4b\\u8bd5"; x === "𠀀";',
    'format("显示"); console.log("日志"); t("翻译"); ordinary("参数");',
    'const data = { text: "字段" }; const prompt = `提示词${id}`;',
    '// x === "注释"',
    'x === make("构造参数"); const jsx = <p>展示</p>;',
  ].join("\n")
  assert.deepEqual([...scan(source)].sort(), [
    "相等", "不等", "双等", "双不等", "分支", "包含", "开头", "结尾", "位置", "匹配",
    "替换", "输出", "分隔", "同义", "关键词", "索引调用", "模板", "末尾", "测试", "𠀀",
  ].map((s) => [JSON.stringify(s), 1]).sort())
  assert.equal(han("かな한글"), false)
  assert.ok(differences(new Map([["new", 1]]), new Map()).length)
  assert.ok(differences(new Map(), new Map([["gone", 1]])).length)
  assert.ok(differences(new Map([["count", 2]]), new Map([["count", 1]])).length)
})
