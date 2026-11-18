//! Chinese text must not become a code value. This ratchet lists existing comparisons, not
//! display text, prompts or logs. Replace each entry with a code value in the owning domain.
//! STEVE_ZH_BLESS=1 cargo test -p steve-hub --test zh_compare rewrites the sorted baseline.
#[path = "node_boundary/scan.rs"]
#[allow(dead_code)]
mod scan;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::parse::Parser;
use syn::visit::{self, Visit};
use syn::{Expr, Item, Lit, Meta, Token};

type Counts = BTreeMap<(String, String), usize>;
const METHODS: &[&str] = &[
    "starts_with",
    "ends_with",
    "contains",
    "strip_prefix",
    "strip_suffix",
    "find",
    "rfind",
    "split",
    "split_once",
    "eq",
    "eq_ignore_ascii_case",
    "replace",
    "trim_start_matches",
    "trim_end_matches",
];

fn han(s: &str) -> bool {
    // Unicode Script=Han (including compatibility ideographs and supplementary planes).
    static HAN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\p{Han}").unwrap());
    HAN.is_match(s)
}

fn test_meta(meta: &Meta) -> bool {
    match meta {
        Meta::Path(p) => p.is_ident("test"),
        Meta::NameValue(n) if n.path.is_ident("feature") => {
            matches!(&n.value, Expr::Lit(l) if matches!(&l.lit, Lit::Str(s) if ["fake", "testkit"].contains(&s.value().as_str())))
        }
        Meta::List(l) => {
            let children = l
                .parse_args_with(syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated)
                .unwrap_or_default();
            if l.path.is_ident("all") {
                children.iter().any(test_meta)
            } else if l.path.is_ident("any") {
                !children.is_empty() && children.iter().all(test_meta)
            } else {
                false
            }
        }
        _ => false,
    }
}

fn test_attrs(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") && a.parse_args::<Meta>().is_ok_and(|m| test_meta(&m)))
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Value {
    literals: Vec<String>,
    table: bool,
}
type Constants = BTreeMap<String, Value>;

fn resolve(path: &syn::Path, constants: &Constants, scope: &str) -> Value {
    let parts = path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>();
    let mut base = scope
        .split("::")
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let mut from = 0;
    if matches!(parts.first().map(String::as_str), Some("self" | "super")) {
        // Functions have lexical constants but do not add a Rust module namespace.
        if let Some(i) = base.iter().position(|p| p.starts_with('@')) {
            base.truncate(i);
        }
    }
    match parts.first().map(String::as_str) {
        Some("crate") => {
            base.clear();
            from = 1;
        }
        Some("self" | "Self") => from = 1,
        Some("super") => {
            while parts.get(from).is_some_and(|s| s == "super") {
                base.pop();
                from += 1;
            }
        }
        _ => {}
    }
    loop {
        let key = base
            .iter()
            .copied()
            .chain(parts[from..].iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("::");
        if let Some(v) = constants.get(&key) {
            return v.clone();
        }
        if from > 0 || base.pop().is_none() {
            break;
        }
    }
    // A uniquely named imported constant needs no type checker. Ambiguous names must not
    // mistake another module's display text for this module's ASCII constant.
    let suffix = format!("::{}", parts.last().unwrap());
    let mut candidates = constants.iter().filter(|(k, _)| k.ends_with(&suffix));
    match (candidates.next(), candidates.next()) {
        (Some((_, v)), None) => v.clone(),
        _ => Value::default(),
    }
}

fn value(expr: &Expr, constants: &Constants, scope: &str) -> Value {
    match expr {
        Expr::Lit(l) => match &l.lit {
            Lit::Str(s) if han(&s.value()) => Value {
                literals: vec![s.value()],
                ..Value::default()
            },
            _ => Value::default(),
        },
        Expr::Path(p) => resolve(&p.path, constants, scope),
        Expr::Macro(m) if m.mac.path.is_ident("concat") => Value {
            literals: operand_literals(expr),
            ..Value::default()
        },
        Expr::Reference(r) => value(&r.expr, constants, scope),
        Expr::Paren(p) => value(&p.expr, constants, scope),
        Expr::Group(g) => value(&g.expr, constants, scope),
        // Preserve the source table through arbitrary iterator adapters (including adapters
        // taking closures), so a later consumer cannot hide the original const/static table.
        Expr::MethodCall(m) => {
            let v = value(&m.receiver, constants, scope);
            if v.table { v } else { Value::default() }
        }
        Expr::Tuple(t) => Value {
            literals: t
                .elems
                .iter()
                .flat_map(|e| value(e, constants, scope).literals)
                .collect(),
            ..Value::default()
        },
        Expr::Array(a) => Value {
            literals: a
                .elems
                .iter()
                .flat_map(|e| expression_literals(e, constants, scope, true))
                .collect(),
            table: true,
        },
        _ => Value::default(),
    }
}

fn tokens(input: syn::parse::ParseStream<'_>, out: &mut Vec<String>) -> syn::Result<()> {
    while !input.is_empty() {
        if input.peek(syn::Lit) {
            if let Lit::Str(s) = input.parse::<Lit>()? {
                if han(&s.value()) {
                    out.push(s.value());
                }
            }
        } else if input.peek(syn::token::Paren) {
            let inner;
            syn::parenthesized!(inner in input);
            tokens(&inner, out)?;
        } else if input.peek(syn::token::Bracket) {
            let inner;
            syn::bracketed!(inner in input);
            tokens(&inner, out)?;
        } else if input.peek(syn::token::Brace) {
            let inner;
            syn::braced!(inner in input);
            tokens(&inner, out)?;
        } else {
            input.step(|c| {
                c.token_tree()
                    .map(|(_, next)| ((), next))
                    .ok_or_else(|| c.error("token"))
            })?;
        }
    }
    Ok(())
}

/// String literals anywhere under a logic operand. General macro bodies stay opaque;
/// format!/concat! are string constructors only when their result is itself used by logic.
fn operand_literals(expr: &Expr) -> Vec<String> {
    expression_literals(expr, &Constants::new(), "", false)
}
fn expression_literals(
    expr: &Expr,
    constants: &Constants,
    scope: &str,
    resolve_constants: bool,
) -> Vec<String> {
    struct Literals<'a> {
        out: Vec<String>,
        constants: &'a Constants,
        scope: &'a str,
        resolve_constants: bool,
    }
    impl<'ast> Visit<'ast> for Literals<'_> {
        fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
            if self.resolve_constants {
                self.out
                    .extend(resolve(&p.path, self.constants, self.scope).literals);
            }
        }
        fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
            if han(&s.value()) {
                self.out.push(s.value());
            }
        }
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let name = m
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            if name == "matches" {
                (|input: syn::parse::ParseStream<'_>| tokens(input, &mut self.out))
                    .parse2(m.tokens.clone())
                    .expect("logic operand tokens");
            } else if ["format", "concat"].contains(&name.as_str()) {
                let args = syn::punctuated::Punctuated::<Expr, Token![,]>::parse_terminated
                    .parse2(m.tokens.clone())
                    .expect("string constructor arguments");
                // Visit temporary ASTs with a fresh visitor, preserving opacity of ordinary macros.
                for arg in args {
                    self.out.extend(expression_literals(
                        &arg,
                        self.constants,
                        self.scope,
                        self.resolve_constants,
                    ));
                }
            }
        }
    }
    let mut out = Literals {
        out: vec![],
        constants,
        scope,
        resolve_constants,
    };
    out.visit_expr(expr);
    out.out
}
const ITERATORS: &[&str] = &[
    "any",
    "all",
    "find",
    "find_map",
    "position",
    "rposition",
    "filter",
    "filter_map",
    "map",
    "contains",
];

struct Guard<'a> {
    constants: &'a Constants,
    scope: String,
    definitions: Constants,
    collect: bool,
    literals: Vec<String>,
}
impl<'a> Guard<'a> {
    fn new(constants: &'a Constants, collect: bool, scope: String) -> Self {
        Self {
            constants,
            scope,
            definitions: Constants::new(),
            collect,
            literals: vec![],
        }
    }
    fn key(&self, name: &str) -> String {
        if self.scope.is_empty() {
            name.to_owned()
        } else {
            format!("{}::{name}", self.scope)
        }
    }
    fn argument(&mut self, e: &Expr) {
        self.literals
            .extend(value(e, self.constants, &self.scope).literals);
    }
    fn table(&mut self, e: &Expr) {
        let v = value(e, self.constants, &self.scope);
        if v.table {
            self.literals.extend(v.literals);
        }
    }
    fn define(&mut self, name: &syn::Ident, e: &Expr) {
        if self.collect {
            let v = value(e, self.constants, &self.scope);
            self.definitions.insert(self.key(&name.to_string()), v);
        }
    }
}
impl<'ast> Visit<'ast> for Guard<'_> {
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let old = self.scope.clone();
        if let syn::Type::Path(t) = &*i.self_ty {
            if let Some(s) = t.path.segments.last() {
                self.scope = self.key(&s.ident.to_string());
            }
        }
        visit::visit_item_impl(self, i);
        self.scope = old;
    }
    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        let old = self.scope.clone();
        self.scope = self.key(&i.ident.to_string());
        visit::visit_item_mod(self, i);
        self.scope = old;
    }
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        let old = self.scope.clone();
        self.scope = self.key(&format!("@{}", i.sig.ident));
        visit::visit_item_fn(self, i);
        self.scope = old;
    }
    fn visit_item(&mut self, i: &'ast Item) {
        if !test_attrs(item_attrs(i)) && !matches!(i, Item::Mod(m) if m.ident == "tests") {
            visit::visit_item(self, i);
        }
    }
    fn visit_impl_item(&mut self, i: &'ast syn::ImplItem) {
        let attrs = match i {
            syn::ImplItem::Const(i) => &i.attrs,
            syn::ImplItem::Fn(i) => &i.attrs,
            syn::ImplItem::Type(i) => &i.attrs,
            syn::ImplItem::Macro(i) => &i.attrs,
            _ => return,
        };
        if !test_attrs(attrs) {
            visit::visit_impl_item(self, i);
        }
    }
    fn visit_trait_item(&mut self, i: &'ast syn::TraitItem) {
        let attrs = match i {
            syn::TraitItem::Const(i) => &i.attrs,
            syn::TraitItem::Fn(i) => &i.attrs,
            syn::TraitItem::Type(i) => &i.attrs,
            syn::TraitItem::Macro(i) => &i.attrs,
            _ => return,
        };
        if !test_attrs(attrs) {
            visit::visit_trait_item(self, i);
        }
    }
    fn visit_item_const(&mut self, i: &'ast syn::ItemConst) {
        self.define(&i.ident, &i.expr);
        visit::visit_item_const(self, i);
    }
    fn visit_item_static(&mut self, i: &'ast syn::ItemStatic) {
        self.define(&i.ident, &i.expr);
        visit::visit_item_static(self, i);
    }
    fn visit_impl_item_const(&mut self, i: &'ast syn::ImplItemConst) {
        self.define(&i.ident, &i.expr);
        visit::visit_impl_item_const(self, i);
    }
    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        if !self.collect && matches!(b.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) {
            self.literals.extend(operand_literals(&b.left));
            self.literals.extend(operand_literals(&b.right));
        }
        visit::visit_expr_binary(self, b);
    }
    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        if self.collect {
            visit::visit_arm(self, a);
            return;
        }
        struct Pattern(Vec<String>);
        impl<'ast> Visit<'ast> for Pattern {
            fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
                if han(&s.value()) {
                    self.0.push(s.value());
                }
            }
        }
        let mut p = Pattern(vec![]);
        p.visit_pat(&a.pat);
        self.literals.extend(p.0);
        visit::visit_arm(self, a);
    }
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if self.collect {
            visit::visit_expr_call(self, c);
            return;
        }
        if let Expr::Path(p) = &*c.func {
            let parts = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>();
            if parts.last().is_some_and(|s| s == "new")
                && parts
                    .iter()
                    .rev()
                    .nth(1)
                    .is_some_and(|s| ["Regex", "RegexBuilder"].contains(&s.as_str()))
            {
                for arg in &c.args {
                    let direct = value(arg, self.constants, &self.scope).literals;
                    self.literals.extend(if direct.is_empty() {
                        operand_literals(arg)
                    } else {
                        direct
                    });
                }
            }
        }
        visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if self.collect {
            visit::visit_expr_method_call(self, m);
            return;
        }
        let name = m.method.to_string();
        if METHODS.contains(&name.as_str()) {
            for arg in &m.args {
                self.argument(arg);
            }
            // A literal receiver is also text being parsed, e.g. "一二三四五六".find(c).
            let receiver = value(&m.receiver, &Constants::new(), "");
            if !receiver.table {
                self.literals.extend(receiver.literals);
            }
        }
        if METHODS.contains(&name.as_str()) || ITERATORS.contains(&name.as_str()) {
            self.table(&m.receiver);
        }
        visit::visit_expr_method_call(self, m);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if self.collect {
            return;
        }
        if m.path.segments.last().is_some_and(|s| s.ident == "matches") {
            // Token scanning also covers nested patterns and raw literals in matches!.
            (|input: syn::parse::ParseStream<'_>| tokens(input, &mut self.literals))
                .parse2(m.tokens.clone())
                .expect("matches tokens");
        }
    }
}

fn file_scope(rel: &str) -> String {
    let Some((_, src)) = rel.split_once("/src/") else {
        return String::new();
    };
    let path = src.trim_end_matches(".rs");
    let mut parts = path.split('/').collect::<Vec<_>>();
    if parts
        .last()
        .is_some_and(|p| ["lib", "main", "mod"].contains(p))
    {
        parts.pop();
    }
    parts.join("::")
}

fn scan_files(files: &[(String, syn::File)]) -> Counts {
    let mut constants = Constants::new();
    // Resolve aliases as well as direct literals, independent of declaration/file order.
    loop {
        let mut next = constants.clone();
        for (rel, file) in files {
            let mut g = Guard::new(&constants, true, file_scope(rel));
            g.visit_file(file);
            next.extend(g.definitions);
        }
        if next == constants {
            break;
        }
        constants = next;
    }
    let mut out = Counts::new();
    for (rel, file) in files {
        let mut g = Guard::new(&constants, false, file_scope(rel));
        g.visit_file(file);
        for literal in g.literals {
            *out.entry((rel.clone(), serde_json::to_string(&literal).unwrap()))
                .or_default() += 1;
        }
    }
    out
}
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn found() -> Counts {
    let root = crates_dir();
    let mut out = Counts::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        if !entry.path().join("Cargo.toml").exists() {
            continue;
        }
        let files = scan::crate_sources(&root, &entry.file_name().to_string_lossy())
            .into_iter()
            .map(|s| {
                let raw = std::fs::read_to_string(root.join(&s.rel)).unwrap();
                let file = syn::parse_file(&raw).unwrap_or_else(|e| panic!("{}: {e}", s.rel));
                (s.rel, file)
            })
            .collect::<Vec<_>>();
        out.extend(scan_files(&files));
    }
    out
}

#[test]
fn chinese_comparisons_only_shrink() {
    let now = found();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/zh_compare.txt");
    if std::env::var("STEVE_ZH_BLESS").as_deref() == Ok("1") {
        let mut text = String::from(
            "# Existing Chinese comparisons; replace display text with domain code values. Only shrinks.\n# file under hub-rs/crates<TAB>JSON-escaped literal<TAB>count.\n# Rewrite: STEVE_ZH_BLESS=1 cargo test -p steve-hub --test zh_compare\n",
        );
        for ((file, literal), n) in now {
            text.push_str(&format!("{file}\t{literal}\t{n}\n"));
        }
        std::fs::write(path, text).unwrap();
        return;
    }
    let listed: Counts = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let parts: Vec<_> = l.split('\t').collect();
            assert_eq!(parts.len(), 3, "bad baseline row: {l}");
            (
                (parts[0].to_owned(), parts[1].to_owned()),
                parts[2].parse().unwrap(),
            )
        })
        .collect();
    let mut problems = vec![];
    for (k, n) in &now {
        let was = listed.get(k).copied().unwrap_or(0);
        if *n != was {
            problems.push(format!("{} {}: {n}, listed {was}", k.0, k.1));
        }
    }
    for (k, n) in &listed {
        if !now.contains_key(k) {
            problems.push(format!(
                "{} {}: gone, listed {n}; shorten the list",
                k.0, k.1
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "中文比较守卫：改成代码值，或在第 4 波前确有必要时用 STEVE_ZH_BLESS=1 重写并在 PR 里说明。已消失或减少的条目必须缩短清单。\n{}",
        problems.join("\n")
    );
}

#[test]
fn ast_covers_branches_constants_tables_and_skips_display_and_tests() {
    let source = r##"
        const WORD: &str = "常量";
        static WORDS: &[&str] = &["同义", "关键词"];
        fn f(s: &str) {
            if s == r#"原样"# || "反向" != s {}
            match s { "分支" | "组合" => (), _ => () }
            match Some(s) { Some("嵌套") => (), _ => () }
            let _ = matches!(Some(s), Some(r"宏" | "组合宏"));
            s.starts_with(WORD); s.contains("直接"); s.trim_end_matches("裁尾");
            WORDS.contains(&s); WORDS.iter().any(|w| s.ends_with(w)); WORDS.iter().copied().find(|w| s.starts_with(w));
            format!("显示"); tracing::info!("日志"); t!("翻译"); ordinary("参数");
            let _ = Thing { text: "字段" }; let _ = "提示词";
        }
        #[cfg(test)] fn unit() { if s == "测试函数" {} }
        mod tests { fn unit() { if s == "测试模块" {} } }
        #[cfg(feature = "fake")] mod fake { fn f() { s.contains("假数据"); } }
        struct A; impl A { #[cfg(test)] fn f() { s.contains("测试方法"); } }
    "##;
    let now = scan_files(&[("fixture".into(), syn::parse_file(source).unwrap())]);
    let literals: BTreeMap<_, _> = now
        .into_iter()
        .map(|((_, s), n)| (serde_json::from_str::<String>(&s).unwrap(), n))
        .collect();
    let expected = [
        ("原样", 1),
        ("反向", 1),
        ("分支", 1),
        ("组合", 1),
        ("嵌套", 1),
        ("宏", 1),
        ("组合宏", 1),
        ("常量", 1),
        ("直接", 1),
        ("裁尾", 1),
        ("同义", 3),
        ("关键词", 3),
    ];
    assert_eq!(
        literals,
        expected
            .into_iter()
            .map(|(s, n)| (s.to_string(), n))
            .collect()
    );
    assert!(han("𠀀"));
    assert!(han("﨑"));
    assert!(!han("かな한글"));
}

#[test]
fn constants_are_collected_across_files_without_confusing_same_named_values() {
    let files = [
        ("crate/src/use_words.rs", r#"fn f(s: &str) { s.strip_prefix(crate::words::ALIAS); crate::words::TABLE.iter().any(|w| s.contains(w)); }"#),
        ("crate/src/words.rs", r#"pub const ALIAS: &str = WORD; pub const WORD: &str = "跨文件"; pub const TABLE: &[&str] = &["表项"];"#),
        ("crate/src/tuples.rs", r#"const TABLE: &[(&str, &str)] = &[("code", "标签")]; fn f(s: &str) { TABLE.iter().find(|(code, _)| *code == s); TABLE.iter().any(|row| row.1 == s); TABLE.iter().find(|(_, label)| s.starts_with(label)); TABLE.contains(&(s, s)); }"#),
        ("crate/src/other.rs", r#"const WORD: &str = "ascii"; fn f(s: &str) { s.contains(WORD); }"#),
        ("crate/src/associated.rs", r#"struct A; impl A { const WORD: &str = "关联"; fn f(s: &str) { s.contains(Self::WORD); } } struct B; impl B { const WORD: &str = "ascii"; fn f(s: &str) { s.contains(Self::WORD); } }"#),
    ].map(|(p, s)| (p.to_owned(), syn::parse_file(s).unwrap()));
    let now = scan_files(&files);
    assert_eq!(
        now,
        [
            (
                ("crate/src/associated.rs".to_owned(), "\"关联\"".to_owned()),
                1
            ),
            (("crate/src/tuples.rs".to_owned(), "\"标签\"".to_owned()), 4),
            (
                ("crate/src/use_words.rs".to_owned(), "\"跨文件\"".to_owned()),
                1
            ),
            (
                ("crate/src/use_words.rs".to_owned(), "\"表项\"".to_owned()),
                1
            ),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn expanded_logic_contexts_cover_nested_operands_iterators_and_regexes() {
    let source = r##"
        const WORDS: &[(&str, &[&str])] = &[("code", &["词表"])];
        static STATIC: &[&str] = &[concat!("静态表")];
        const PATTERN: &str = "常量正则";
        const BUILT: &[Row] = &[row("code", "构造表字段")];
        fn f(s: &str) {
            if s == format!("格式{}", 1) || Some(s) != Some("包裹") {}
            if (s, s) == ("元组".to_string(), "转换".to_string()) {}
            "汉字接收者".find(s);
            WORDS.into_iter().copied().cloned().filter(|_| true).filter_map(Some)
                .map(|x| x).all(|_| true);
            BUILT.iter().map(|r| r.code);
            WORDS.iter().find_map(Some); WORDS.iter().position(|_| true);
            WORDS.iter().rposition(|_| true); STATIC.iter().any(|_| true);
            Regex::new("正则"); regex::bytes::RegexBuilder::new(r"构建器");
            Regex::new(PATTERN);
            static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(concat!("延迟", "拼接")).unwrap());
            if s == format!("外层{}", opaque!("宏内忽略")) {}
            ordinary("函数参数忽略"); format!("显示忽略"); t!("翻译忽略");
            RegexLike::new("非正则忽略"); db("SELECT 'SQL忽略'");
            if s == opaque!("宏比较忽略") {}
        }
    "##;
    let now = scan_files(&[("fixture".into(), syn::parse_file(source).unwrap())]);
    let literals: BTreeMap<_, _> = now
        .into_iter()
        .map(|((_, s), n)| (serde_json::from_str::<String>(&s).unwrap(), n))
        .collect();
    let expected = [
        ("构造表字段", 1),
        ("格式{}", 1),
        ("包裹", 1),
        ("元组", 1),
        ("转换", 1),
        ("汉字接收者", 1),
        ("词表", 7),
        ("静态表", 1),
        ("正则", 1),
        ("构建器", 1),
        ("常量正则", 1),
        ("延迟", 1),
        ("拼接", 1),
        ("外层{}", 1),
    ];
    assert_eq!(
        literals,
        expected
            .into_iter()
            .map(|(s, n)| (s.to_owned(), n))
            .collect()
    );
}
