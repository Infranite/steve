/* Existing Chinese comparisons are a shrinking baseline. Display text, prompts and logs are
 * content; branch on domain code values instead. STEVE_GUARD_BLESS=1 node --test
 * web/test/guard-zh-compare.test.mjs rewrites the baseline. */
import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync, readdirSync } from "node:fs"
import { join, relative } from "node:path"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { checkCounts, differences } from "./support/guard-ratchet.mjs"

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
// Literal pieces used by a logic operation (display-only expressions are never queried).
const literals = (n) => {
  n = unwrap(n)
  if (ts.isRegularExpressionLiteral(n)) return han(n.text) ? [n.text] : []
  if (ts.isStringLiteral(n) || ts.isNoSubstitutionTemplateLiteral(n)) return han(n.text) ? [n.text] : []
  if (ts.isTemplateExpression(n)) {
    const parts = [n.head.text, ...n.templateSpans.map((s) => s.literal.text)]
    return parts.filter(han)
  }
  return []
}
const nestedLiterals = (n) => {
  const direct = literals(n)
  if (ts.isTemplateExpression(unwrap(n))) return [...direct, ...unwrap(n).templateSpans.flatMap((s) => nestedLiterals(s.expression))]
  if (direct.length) return direct
  const found = []
  ts.forEachChild(n, (child) => { found.push(...nestedLiterals(child)) })
  return found
}
const iterators = new Set(["some", "every", "find", "findIndex", "findLastIndex", "filter", "map", "flatMap", "includes", "indexOf", "lastIndexOf"])
const scan = (src, filename = "fixture.tsx", checker) => {
  const ast = (checker ? program.getSourceFile(filename) : undefined) ?? ts.createSourceFile(filename, src, ts.ScriptTarget.Latest, true,
    filename.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  assert.equal(ast.parseDiagnostics.length, 0, `${filename}: invalid TypeScript`)
  const declarations = new Map()
  const collect = (n) => {
    if (ts.isVariableDeclaration(n) && ts.isIdentifier(n.name) && (n.parent.flags & ts.NodeFlags.Const)) declarations.set(n.name.text, n)
    ts.forEachChild(n, collect)
  }
  collect(ast)
  const table = (n, seen = new Set(), element = false) => {
    n = unwrap(n)
    if (ts.isArrayLiteralExpression(n)) return n.elements.flatMap((e) => table(e, seen, true))
    if (ts.isCallExpression(n) && ts.isPropertyAccessExpression(n.expression)) return table(n.expression.expression, seen, element)
    if (ts.isIdentifier(n)) {
      let symbol = checker?.getSymbolAtLocation(n)
      if (symbol?.flags & ts.SymbolFlags.Alias) symbol = checker.getAliasedSymbol(symbol)
      const decl = checker ? symbol?.valueDeclaration : declarations.get(n.text)
      if (decl && ts.isVariableDeclaration(decl) && (decl.parent.flags & ts.NodeFlags.Const) && decl.initializer && !seen.has(decl)) {
        const next = new Set(seen); next.add(decl); return table(decl.initializer, next, element)
      }
    }
    return element ? nestedLiterals(n) : []
  }
  const found = new Map()
  const add = (values) => { for (const s of values) { const key = JSON.stringify(s); found.set(key, (found.get(key) ?? 0) + 1) } }
  const visit = (n) => {
    if (ts.isBinaryExpression(n) && operators.has(n.operatorToken.kind)) {
      add(nestedLiterals(n.left)); add(nestedLiterals(n.right))
    }
    if (ts.isCaseClause(n)) add(literals(n.expression))
    if (ts.isCallExpression(n)) {
      let receiver, method
      if (ts.isPropertyAccessExpression(n.expression)) {
        receiver = n.expression.expression; method = n.expression.name.text
      } else if (ts.isElementAccessExpression(n.expression) && ts.isStringLiteral(n.expression.argumentExpression)) {
        receiver = n.expression.expression; method = n.expression.argumentExpression.text
      }
      if (methods.has(method)) {
        for (const arg of n.arguments) add(literals(arg))
        if (!ts.isArrayLiteralExpression(unwrap(receiver))) add(literals(receiver))
      }
      if (iterators.has(method)) add(table(receiver))
      if (ts.isIdentifier(n.expression) && n.expression.text === "RegExp") for (const arg of n.arguments) add(nestedLiterals(arg))
    }
    if (ts.isNewExpression(n) && ts.isIdentifier(n.expression) && n.expression.text === "RegExp") for (const arg of n.arguments ?? []) add(nestedLiterals(arg))
    ts.forEachChild(n, visit)
  }
  visit(ast)
  return found
}

let program
test("Chinese comparisons only shrink", () => {
  const paths = files(join(root, "src"))
  program = ts.createProgram(paths, { target: ts.ScriptTarget.Latest, jsx: ts.JsxEmit.Preserve, noEmit: true })
  const checker = program.getTypeChecker()
  const now = new Map()
  for (const f of paths) {
    for (const [literal, n] of scan(readFileSync(f, "utf8"), f, checker)) now.set(`${relative(root, f).replaceAll("\\", "/")}\t${literal}`, n)
  }
  checkCounts(now, {
    file: list, columns: 2,
    header: "# Existing Chinese comparisons; replace display text with domain code values. Only shrinks.\n# file under web<TAB>JSON-escaped literal<TAB>count.\n# Rewrite after removals: STEVE_GUARD_BLESS=1 node --test web/test/guard-zh-compare.test.mjs\n",
    help: "replace display text with domain code values; removals require STEVE_GUARD_BLESS=1",
  })
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
    "替换", "输出", "分隔", "同义", "关键词", "索引调用", "模板", "末尾", "测试", "𠀀", "构造参数",
  ].map((s) => [JSON.stringify(s), 1]).sort())
  assert.equal(han("かな한글"), false)
  assert.ok(differences(new Map([["new", 1]]), new Map()).length)
  assert.ok(differences(new Map(), new Map([["gone", 1]])).length)
  assert.ok(differences(new Map([["count", 2]]), new Map([["count", 1]])).length)
})


test("expanded logic covers nested operands, const table chains, receivers and regex construction", () => {
  const found = scan([
    'const WORDS = ["词表"]; const ALIAS = WORDS;',
    'ALIAS.map(x => x).filter(Boolean).some(Boolean); WORDS.every(Boolean);',
    'const LABEL = "字段别名"; const BUILT = [make("构造表字段"), LABEL]; BUILT.map(x => x);',
    'x === make("包裹"); x !== ["元组".toString(), `模板${id}`];',
    '"接收者".indexOf(x); "接收者包含".includes(x);',
    'new RegExp("正则"); RegExp(`构造${id}`); x.match(/匹配中文/u);',
    'ordinary("普通参数"); console.log("显示"); const prompt = "提示词";',
  ].join("\n"))
  assert.deepEqual([...found].sort(), [
    ["构造表字段", 1], ["字段别名", 1], ["词表", 4], ["包裹", 1], ["元组", 1], ["模板", 1], ["接收者", 1], ["接收者包含", 1],
    ["正则", 1], ["构造", 1], ["/匹配中文/u", 1],
  ].map(([s, n]) => [JSON.stringify(s), n]).sort())
})
