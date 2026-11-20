//! Runtime ratchet: move timers, TTL caches and wall-time policy into steve-runtime. This is a
//! syntax guard, not a Rust type checker: review `.earliest()` receivers in the inventory.
//! Scan module trees, not tests/examples or disconnected source files.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::parse::{ParseStream, Parser, discouraged::Speculative};
use syn::visit::{self, Visit};
use syn::{Expr, Item, Meta, Token};

type Counts = BTreeMap<(String, String, String), usize>;
const LIST: &str = "tests/runtime_guard.txt";
const LIST_HEADER: &str = "# Existing runtime policy outside steve-runtime; only shrinks.\n# Move timers to due_loop / every / clock, TTL maps to Cached, not-before deadlines to due::RowDeadlines and wall-time policy to tz.\n# Syntax scan of earliest() calls; timer loops count once per function.\n# file under hub-rs/crates<TAB>category<TAB>function (module/impl qualified)<TAB>count\n# Rewrite after explaining in the task card: STEVE_GUARD_BLESS=1 cargo test -p steve-hub --test runtime_guard\n# Known coverage boundary: steve-channels/src/lifecycle.rs supervise waits through its\n# private sleep_until wrapper; this guard does not follow helper calls.\n# wire/node_runtime uses its own Clock::sleep (implemented in clock.rs); this guard does\n# not resolve trait/custom-clock calls. Direct Tokio timers, explicit Interval types and\n# same-function interval construction plus unknown loop tick receivers are recognized.\n";
const HELP: &str = "use steve-runtime due_loop / every / clock / tz, Cached for TTL maps or due::RowDeadlines for not-before deadlines, or explain in the task card before rewriting with STEVE_GUARD_BLESS=1";

fn test_meta(meta: &Meta) -> bool {
    match meta {
        Meta::Path(p) => p.is_ident("test"),
        Meta::NameValue(n) if n.path.is_ident("feature") => {
            matches!(&n.value, Expr::Lit(l) if matches!(&l.lit, syn::Lit::Str(s) if ["fake", "testkit"].contains(&s.value().as_str())))
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
fn test_name(name: &str) -> bool {
    ["tests", "fake", "testkit"].contains(&name)
}
fn attrs(item: &Item) -> &[syn::Attribute] {
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
fn production(item: &Item) -> bool {
    !test_attrs(attrs(item)) && !matches!(item, Item::Mod(m) if test_name(&ident(&m.ident)))
}
fn ident(i: &syn::Ident) -> String {
    i.to_string().trim_start_matches("r#").to_owned()
}
fn path(p: &syn::Path) -> String {
    p.segments
        .iter()
        .map(|s| ident(&s.ident))
        .collect::<Vec<_>>()
        .join("::")
}
fn join(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.into()
    } else {
        format!("{scope}::{name}")
    }
}

#[derive(Clone, Default)]
struct Imports {
    names: BTreeMap<String, String>,
}
impl Imports {
    fn add(&mut self, prefix: &str, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(p) => self.add(&join(prefix, &ident(&p.ident)), &p.tree),
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    self.add(prefix, t);
                }
            }
            syn::UseTree::Name(n) => {
                let name = ident(&n.ident);
                let full = if name == "self" {
                    prefix.into()
                } else {
                    join(prefix, &name)
                };
                let name = full.rsplit("::").next().unwrap().to_owned();
                self.names.insert(name, full);
            }
            syn::UseTree::Rename(n) => {
                let full = if n.ident == "self" {
                    prefix.into()
                } else {
                    join(prefix, &ident(&n.ident))
                };
                self.names.insert(ident(&n.rename), full);
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    fn items(items: &[Item]) -> Self {
        let mut out = Self::default();
        for i in items.iter().filter(|i| production(i)) {
            if let Item::Use(u) = i {
                out.add("", &u.tree);
            }
        }
        out
    }
}

struct Source {
    rel: String,
    module: String,
    items: Vec<Item>,
}
// ordered_installers! is this repository's assembly DSL: its declarations are ordinary modules.
// This deliberately does not expand arbitrary Rust macros.
fn macro_parts<T: syn::parse::Parse>(
    input: ParseStream<'_>,
    found: &mut impl FnMut(T),
) -> syn::Result<()> {
    while !input.is_empty() {
        let fork = input.fork();
        if let Ok(part) = fork.parse::<T>() {
            input.advance_to(&fork);
            found(part);
        } else if input.peek(syn::token::Brace) {
            let inner;
            syn::braced!(inner in input);
            macro_parts(&inner, found)?;
        } else if input.peek(syn::token::Paren) {
            let inner;
            syn::parenthesized!(inner in input);
            macro_parts(&inner, found)?;
        } else if input.peek(syn::token::Bracket) {
            let inner;
            syn::bracketed!(inner in input);
            macro_parts(&inner, found)?;
        } else {
            input.step(|c| {
                c.token_tree()
                    .map(|(_, next)| ((), next))
                    .ok_or_else(|| c.error("macro token"))
            })?;
        }
    }
    Ok(())
}
// Keep inline module directories and #[path] semantics when following external modules.
fn modules(root: &Path, file: &Path, module: &str, dir: &Path, out: &mut Vec<Source>) {
    if file
        .strip_prefix(root)
        .unwrap()
        .components()
        .any(|c| test_name(&c.as_os_str().to_string_lossy()))
    {
        return;
    }
    let raw = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let parsed = syn::parse_file(&raw).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    if test_attrs(&parsed.attrs) {
        return;
    }
    module_items(
        root,
        file,
        module,
        dir,
        file.parent().unwrap(),
        parsed.items,
        out,
    );
}
fn module_items(
    root: &Path,
    file: &Path,
    module: &str,
    dir: &Path,
    path_dir: &Path,
    items: Vec<Item>,
    out: &mut Vec<Source>,
) {
    let mut expanded = vec![];
    for item in items.into_iter().filter(production) {
        if let Item::Macro(m) = &item {
            if m.mac.path.is_ident("ordered_installers") {
                (|input: ParseStream<'_>| {
                    macro_parts(input, &mut |module: syn::ItemMod| {
                        expanded.push(Item::Mod(module))
                    })
                })
                .parse2(m.mac.tokens.clone())
                .expect("macro module declarations");
            }
        }
        expanded.push(item);
    }
    let mut kept = vec![];
    for item in expanded.into_iter().filter(production) {
        if let Item::Mod(m) = &item {
            let name = ident(&m.ident);
            let filename = &name;
            let scope = join(module, &name);
            if let Some((_, children)) = &m.content {
                module_items(
                    root,
                    file,
                    &scope,
                    &dir.join(filename),
                    &dir.join(filename),
                    children.clone(),
                    out,
                );
            } else {
                let explicit = m.attrs.iter().find_map(|a| match &a.meta {
                    Meta::NameValue(n) if n.path.is_ident("path") => match &n.value {
                        Expr::Lit(l) => match &l.lit {
                            syn::Lit::Str(s) => Some(s.value()),
                            _ => None,
                        },
                        _ => None,
                    },
                    _ => None,
                });
                let child = if let Some(p) = explicit {
                    path_dir.join(p)
                } else {
                    [
                        dir.join(format!("{filename}.rs")),
                        dir.join(filename).join("mod.rs"),
                    ]
                    .into_iter()
                    .find(|p| p.exists())
                    .unwrap_or_else(|| panic!("{}: missing module {name}", file.display()))
                };
                let child_dir = if child.file_name().unwrap() == "mod.rs" {
                    child.parent().unwrap().to_owned()
                } else {
                    child.with_extension("")
                };
                modules(root, &child, &scope, &child_dir, out);
            }
        } else {
            kept.push(item);
        }
    }
    out.push(Source {
        rel: file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/"),
        module: module.into(),
        items: kept,
    });
}

struct Guard {
    imports: Vec<Imports>,
    bindings: Vec<BTreeMap<String, String>>,
    fields: BTreeMap<String, BTreeSet<String>>,
    owner: String,
    function: String,
    loops: usize,
    interval_created: bool,
    unknown_tick: bool,
    counts: BTreeMap<(String, String), usize>,
}
impl Guard {
    // Expand explicit imports in this lexical scope only. There is no module graph,
    // glob lookup or return-type inference; the bound also makes cyclic aliases harmless.
    fn resolve(&self, raw: &str) -> String {
        let mut resolved = raw.to_owned();
        let limit = self.imports.iter().map(|i| i.names.len()).sum::<usize>() + 1;
        for _ in 0..limit {
            let (head, rest) = resolved.split_once("::").unwrap_or((&resolved, ""));
            let Some(target) = self.imports.iter().rev().find_map(|i| i.names.get(head)) else {
                break;
            };
            if target == head || target.starts_with(&format!("{head}::")) {
                break;
            }
            resolved = if rest.is_empty() {
                target.clone()
            } else {
                join(target, rest)
            };
        }
        resolved.replacen("chrono::offset::Local", "chrono::Local", 1)
    }
    fn hit(&mut self, category: &str) {
        let key = (category.into(), self.function.clone());
        if category == "timer-loop" {
            self.counts.insert(key, 1);
        } else {
            *self.counts.entry(key).or_default() += 1;
        }
    }
    fn ty(&self, ty: &syn::Type) -> String {
        match ty {
            syn::Type::Path(t) => self.resolve(&path(&t.path)),
            syn::Type::Reference(t) => self.ty(&t.elem),
            _ => String::new(),
        }
    }
    fn value(&self, e: &Expr) -> String {
        match e {
            Expr::Path(p) => {
                let raw = path(&p.path);
                self.bindings
                    .iter()
                    .rev()
                    .find_map(|b| b.get(&raw).cloned())
                    .unwrap_or_else(|| self.resolve(&raw))
            }
            Expr::Call(c) => {
                let callable = self.value(&c.func);
                if ["tokio::time::interval", "tokio::time::interval_at"]
                    .contains(&callable.as_str())
                {
                    "tokio::time::Interval".into()
                } else {
                    String::new()
                }
            }
            Expr::Reference(r) => self.value(&r.expr),
            Expr::Paren(p) => self.value(&p.expr),
            Expr::Group(g) => self.value(&g.expr),
            Expr::Field(f) => {
                let name = match &f.member {
                    syn::Member::Named(n) => ident(n),
                    _ => return String::new(),
                };
                let owner = if matches!(&*f.base, Expr::Path(p) if p.path.is_ident("self")) {
                    self.owner.split(" as ").next().unwrap().to_owned()
                } else {
                    self.value(&f.base)
                };
                if self
                    .fields
                    .get(&owner)
                    .is_some_and(|fields| fields.contains(&name))
                {
                    "tokio::time::Interval".into()
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }
    fn bind_expr(&mut self, pat: &syn::Pat, expr: &Expr) {
        match (pat, expr) {
            (syn::Pat::Tuple(p), Expr::Tuple(e)) => {
                for (p, e) in p.elems.iter().zip(&e.elems) {
                    self.bind_expr(p, e);
                }
            }
            _ => self.bind(pat, self.value(expr)),
        }
    }
    fn bind(&mut self, pat: &syn::Pat, value: String) {
        match pat {
            syn::Pat::Ident(p) => {
                self.bindings
                    .last_mut()
                    .unwrap()
                    .insert(ident(&p.ident), value);
            }
            syn::Pat::Type(p) => self.bind(&p.pat, self.ty(&p.ty)),
            _ => {}
        }
    }
    fn function(&mut self, sig: &syn::Signature, body: Option<&syn::Block>) {
        let old = (
            self.function.clone(),
            self.loops,
            self.interval_created,
            self.unknown_tick,
        );
        self.interval_created = false;
        self.unknown_tick = false;
        self.function = join(
            if self.function == "<module>" {
                &self.owner
            } else {
                &self.function
            },
            &ident(&sig.ident),
        );
        self.loops = 0;
        self.bindings.push(BTreeMap::new());
        for arg in &sig.inputs {
            if let syn::FnArg::Typed(a) = arg {
                self.bind(&a.pat, self.ty(&a.ty));
            }
        }
        visit::visit_signature(self, sig);
        if let Some(b) = body {
            self.visit_block(b);
        }
        self.bindings.pop();
        // Pair syntax within the owning function, not inferred closure return values.
        // This covers local interval factories used by sibling async loops (spawn_jobs).
        if self.interval_created && self.unknown_tick {
            self.hit("timer-loop");
        }
        (
            self.function,
            self.loops,
            self.interval_created,
            self.unknown_tick,
        ) = old;
    }
}
impl<'ast> Visit<'ast> for Guard {
    fn visit_item(&mut self, i: &'ast Item) {
        if production(i) {
            visit::visit_item(self, i);
        }
    }
    fn visit_item_use(&mut self, _: &'ast syn::ItemUse) {}
    fn visit_item_macro(&mut self, i: &'ast syn::ItemMacro) {
        // Unexpanded definitions are not executed production code.
        if !i.mac.path.is_ident("macro_rules") {
            self.visit_macro(&i.mac);
        }
    }
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        self.function(&i.sig, Some(&i.block));
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let old = self.owner.clone();
        self.owner = match &*i.self_ty {
            syn::Type::Path(t) => path(&t.path),
            _ => "<impl>".into(),
        };
        if let Some((_, t, _)) = &i.trait_ {
            self.owner = format!("{} as {}", self.owner, path(t));
        }
        visit::visit_item_impl(self, i);
        self.owner = old;
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
    fn visit_field(&mut self, f: &'ast syn::Field) {
        if !test_attrs(&f.attrs) {
            visit::visit_field(self, f);
        }
    }
    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        if !test_attrs(&i.attrs) {
            self.function(&i.sig, Some(&i.block));
        }
    }
    fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
        let old = std::mem::replace(&mut self.owner, ident(&i.ident));
        visit::visit_item_trait(self, i);
        self.owner = old;
    }
    fn visit_trait_item_fn(&mut self, i: &'ast syn::TraitItemFn) {
        if !test_attrs(&i.attrs) {
            self.function(&i.sig, i.default.as_ref());
        }
    }
    fn visit_block(&mut self, b: &'ast syn::Block) {
        let mut env = Imports::default();
        for s in &b.stmts {
            if let syn::Stmt::Item(Item::Use(u)) = s {
                if !test_attrs(&u.attrs) {
                    env.add("", &u.tree);
                }
            }
        }
        self.imports.push(env);
        self.bindings.push(BTreeMap::new());
        visit::visit_block(self, b);
        self.bindings.pop();
        self.imports.pop();
    }
    fn visit_local(&mut self, l: &'ast syn::Local) {
        if test_attrs(&l.attrs) {
            return;
        }
        visit::visit_local(self, l);
        if let Some(init) = &l.init {
            self.bind_expr(&l.pat, &init.expr);
        } else {
            self.bind(&l.pat, String::new());
        }
    }
    fn visit_expr(&mut self, e: &'ast Expr) {
        let attrs = match e {
            Expr::Block(e) => &e.attrs,
            Expr::Loop(e) => &e.attrs,
            Expr::ForLoop(e) => &e.attrs,
            Expr::While(e) => &e.attrs,
            Expr::Call(e) => &e.attrs,
            Expr::MethodCall(e) => &e.attrs,
            Expr::Macro(e) => &e.attrs,
            _ => &[][..],
        };
        if !test_attrs(attrs) {
            visit::visit_expr(self, e);
        }
    }
    fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
        self.loops += 1;
        visit::visit_expr_loop(self, e);
        self.loops -= 1;
    }
    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.loops += 1;
        visit::visit_expr_while(self, e);
        self.loops -= 1;
    }
    fn visit_expr_for_loop(&mut self, e: &'ast syn::ExprForLoop) {
        self.loops += 1;
        visit::visit_expr_for_loop(self, e);
        self.loops -= 1;
    }
    fn visit_expr_closure(&mut self, e: &'ast syn::ExprClosure) {
        let loops = std::mem::replace(&mut self.loops, 0);
        self.bindings.push(BTreeMap::new());
        for pat in &e.inputs {
            self.bind(pat, String::new());
        }
        visit::visit_expr_closure(self, e);
        self.bindings.pop();
        self.loops = loops;
    }
    fn visit_expr_async(&mut self, e: &'ast syn::ExprAsync) {
        let loops = std::mem::replace(&mut self.loops, 0);
        visit::visit_expr_async(self, e);
        self.loops = loops;
    }
    fn visit_expr_assign(&mut self, e: &'ast syn::ExprAssign) {
        let value = self.value(&e.right);
        visit::visit_expr_assign(self, e);
        if let Expr::Path(p) = &*e.left {
            let name = path(&p.path);
            if let Some(scope) = self
                .bindings
                .iter_mut()
                .rev()
                .find(|b| b.contains_key(&name))
            {
                scope.insert(name, value);
            }
        }
    }
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        let callable = self.value(&c.func);
        if ["tokio::time::interval", "tokio::time::interval_at"].contains(&callable.as_str()) {
            self.interval_created = true;
        }
        if self.loops > 0
            && [
                "tokio::time::sleep",
                "tokio::time::sleep_until",
                "tokio::time::Interval::tick",
            ]
            .contains(&callable.as_str())
        {
            self.hit("timer-loop");
        }
        visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if m.method == "earliest" && m.args.is_empty() {
            self.hit("earliest");
        }
        if self.loops > 0 && m.method == "tick" {
            match self.value(&m.receiver).as_str() {
                "tokio::time::Interval" => self.hit("timer-loop"),
                "" => self.unknown_tick = true,
                _ => {}
            }
        }
        visit::visit_expr_method_call(self, m);
    }
    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        let resolved = self.value(&Expr::Path(p.clone()));
        if resolved == "chrono::Local" || resolved.starts_with("chrono::Local::") {
            self.hit("process-tz");
        }
        visit::visit_expr_path(self, p);
    }
    fn visit_type_path(&mut self, p: &'ast syn::TypePath) {
        if self.resolve(&path(&p.path)) == "chrono::Local" {
            self.hit("process-tz");
        }
        visit::visit_type_path(self, p);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        // ordered_installers! declarations were followed by the module walker above.
        if m.path.is_ident("ordered_installers") {
            return;
        }
        // This workspace's install! DSL wraps its body in the named function.
        if m.path.is_ident("install") {
            let (name, body) = (|input: ParseStream<'_>| {
                let name = input.parse::<syn::Ident>()?;
                while !input.peek(syn::token::Brace) {
                    input.step(|c| {
                        c.token_tree()
                            .map(|(_, next)| ((), next))
                            .ok_or_else(|| c.error("installer body"))
                    })?;
                }
                Ok((name, input.parse::<syn::Block>()?))
            })
            .parse2(m.tokens.clone())
            .expect("installer function");
            let sig = syn::parse_str::<syn::Signature>(&format!("fn {name}()")).unwrap();
            self.function(&sig, Some(&body));
            return;
        }
        // syn leaves macro bodies opaque. Read expressions and nested groups, so select!
        // branches (including their handlers) cannot hide sleep/tick or timezone policy.
        (|input: ParseStream<'_>| macro_parts(input, &mut |expr: Expr| self.visit_expr(&expr)))
            .parse2(m.tokens.clone())
            .expect("macro expressions");
    }
}

struct TtlGuard<'a> {
    imports: &'a BTreeMap<String, Imports>,
}
impl TtlGuard<'_> {
    // Match the upstream guard's bounded explicit-import expansion. TTL value
    // declarations additionally need crate/self/super paths to locate their types.
    fn resolve_in(&self, raw: &str, module: &str, _depth: usize) -> String {
        let mut resolved = raw.to_owned();
        for _ in 0..128 {
            let (head, rest) = resolved.split_once("::").unwrap_or((&resolved, ""));
            match head {
                "crate" => return format!("@::{rest}"),
                "self" => return format!("@::{}", join(module, rest)),
                "super" => {
                    return format!(
                        "@::{}",
                        join(
                            module.rsplit_once("::").map_or("", |(parent, _)| parent),
                            rest
                        )
                    );
                }
                _ => {}
            }
            let Some(target) = self.imports.get(module).and_then(|i| i.names.get(head)) else {
                break;
            };
            if target == head || target.starts_with(&format!("{head}::")) {
                break;
            }
            resolved = if rest.is_empty() {
                target.clone()
            } else {
                join(target, rest)
            };
        }
        resolved
    }
}

// Structural TTL inventory: lock-protected maps whose values contain a timestamp used
// for age checks. Expand aliases and display-value structs, but do not descend into a
// struct's own locks: those are independent lifecycle/authoritative state (connections,
// observations), rather than a cached value. This is deliberately a syntax guard;
// review the inventory when changing storage shapes, just like `.earliest()` receivers.
#[derive(Default)]
struct Ages {
    names: BTreeSet<String>,
    any: bool,
}
impl Ages {
    fn age(&mut self, expr: &Expr) {
        self.any = true;
        match expr {
            Expr::Path(p) => {
                self.names.insert(path(&p.path));
            }
            Expr::Field(f) => {
                self.names.insert(match &f.member {
                    syn::Member::Named(n) => ident(n),
                    syn::Member::Unnamed(n) => n.index.to_string(),
                });
            }
            Expr::Reference(r) => self.age(&r.expr),
            Expr::Paren(p) => self.age(&p.expr),
            _ => {}
        }
    }
}
impl<'ast> Visit<'ast> for Ages {
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if !test_attrs(&f.attrs) {
            visit::visit_impl_item_fn(self, f);
        }
    }

    fn visit_item(&mut self, item: &'ast Item) {
        if production(item) {
            visit::visit_item(self, item);
        }
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if m.method == "elapsed" {
            self.age(&m.receiver);
        }
        if [
            "saturating_sub",
            "duration_since",
            "saturating_duration_since",
        ]
        .iter()
        .any(|n| m.method == *n)
        {
            for arg in &m.args {
                self.age(arg);
            }
        }
        visit::visit_expr_method_call(self, m);
    }
    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        if matches!(b.op, syn::BinOp::Sub(_)) {
            self.age(&b.right);
        }
        visit::visit_expr_binary(self, b);
    }
}
type TtlTypes = BTreeMap<String, (String, bool, Vec<(Option<String>, syn::Type)>)>;
fn type_args(p: &syn::TypePath) -> Vec<&syn::Type> {
    p.path
        .segments
        .iter()
        .flat_map(|s| match &s.arguments {
            syn::PathArguments::AngleBracketed(a) => a
                .args
                .iter()
                .filter_map(|a| match a {
                    syn::GenericArgument::Type(t) => Some(t),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        })
        .collect()
}
fn contains_map(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(p) => {
            p.path
                .segments
                .last()
                .is_some_and(|s| s.ident == "HashMap" || s.ident == "BTreeMap")
                || type_args(p).into_iter().any(contains_map)
        }
        syn::Type::Tuple(t) => t.elems.iter().any(contains_map),
        syn::Type::Reference(r) => contains_map(&r.elem),
        _ => false,
    }
}
impl TtlGuard<'_> {
    fn ttl_type(
        &self,
        ty: &syn::Type,
        types: &TtlTypes,
        ages: &Ages,
        module: &str,
        stage: u8,
        field: Option<&str>,
        seen: &mut BTreeSet<String>,
    ) -> bool {
        match ty {
            syn::Type::Reference(r) => {
                self.ttl_type(&r.elem, types, ages, module, stage, field, seen)
            }
            syn::Type::Tuple(t) => {
                // Some caches stamp the entire map rather than each entry.
                let stamped_map = stage == 1
                    && t.elems.iter().any(|ty| contains_map(ty))
                    && t.elems
                        .iter()
                        .any(|ty| self.ttl_type(ty, types, ages, module, 2, None, seen));
                stamped_map
                    || t.elems.iter().enumerate().any(|(index, ty)| {
                        self.ttl_type(
                            ty,
                            types,
                            ages,
                            module,
                            stage,
                            field.or(Some(&index.to_string())),
                            seen,
                        )
                    })
            }
            syn::Type::Path(p) => {
                let resolved = self.resolve_in(&path(&p.path), module, 0);
                let name = resolved.rsplit("::").next().unwrap();
                let args = type_args(p);
                if stage == 2 && (name == "Instant" || name == "i64") {
                    return ages.any
                        && field.is_none_or(|f| {
                            f.bytes().all(|c| c.is_ascii_digit()) || ages.names.contains(f)
                        });
                }
                if stage == 2 && name == "u64" {
                    // Unsigned integers also represent sequence numbers. Require
                    // an age operation on the named field or tuple index. Signed
                    // millisecond timestamps retain the inventory's conservative
                    // match (including destructured tuples and Vec<i64>).
                    return field.is_some_and(|f| ages.names.contains(f));
                }
                if ["Mutex", "RwLock"].contains(&name) {
                    if stage == 2 && field.is_some() {
                        return false;
                    }
                    return args.iter().any(|t| {
                        self.ttl_type(
                            t,
                            types,
                            ages,
                            module,
                            if stage == 0 { 1 } else { stage },
                            field,
                            seen,
                        )
                    });
                }
                if ["HashMap", "BTreeMap"].contains(&name) {
                    return stage == 1
                        && args
                            .get(1)
                            .is_some_and(|t| self.ttl_type(t, types, ages, module, 2, None, seen));
                }
                if [
                    "Arc", "Box", "Option", "LazyLock", "OnceLock", "Vec", "VecDeque",
                ]
                .contains(&name)
                {
                    return args
                        .iter()
                        .any(|t| self.ttl_type(t, types, ages, module, stage, field, seen));
                }
                let key = resolved.trim_start_matches("@::");
                let key = if types.contains_key(key) {
                    key.to_owned()
                } else {
                    join(module, key)
                };
                if !seen.insert(key.clone()) {
                    return false;
                }
                let result = types.get(&key).is_some_and(|(scope, alias, fields)| {
                    // Inventory the declaration that owns the map, not every object
                    // transitively containing that owner (Core, Auth, etc.).
                    if !alias && stage != 2 {
                        return false;
                    }
                    fields.iter().any(|(name, ty)| {
                        self.ttl_type(
                            ty,
                            types,
                            ages,
                            scope,
                            stage,
                            name.as_deref().or(field),
                            seen,
                        )
                    })
                });
                seen.remove(&key);
                result
            }
            _ => false,
        }
    }
}
fn ttl_inventory(source: &Source, guard: &TtlGuard<'_>, types: &TtlTypes) -> Counts {
    let mut ages = Ages::default();
    for item in &source.items {
        ages.visit_item(item);
    }
    let mut out = Counts::new();
    let mut record = |name: String, ty: &syn::Type| {
        if guard.ttl_type(
            ty,
            types,
            &ages,
            &source.module,
            0,
            None,
            &mut BTreeSet::new(),
        ) {
            out.insert(
                (
                    source.rel.clone(),
                    "ttl-cache".into(),
                    join(&source.module, &name),
                ),
                1,
            );
        }
    };
    #[derive(Default)]
    struct LocalStatics(Vec<(String, syn::Type)>);
    impl<'ast> Visit<'ast> for LocalStatics {
        fn visit_item(&mut self, i: &'ast Item) {
            if production(i) {
                visit::visit_item(self, i);
            }
        }
        fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
            self.0.push((ident(&s.ident), *s.ty.clone()));
        }
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            if !test_attrs(&f.attrs) {
                visit::visit_impl_item_fn(self, f);
            }
        }
    }
    let mut statics = LocalStatics::default();
    for item in &source.items {
        statics.visit_item(item);
    }
    for (name, ty) in &statics.0 {
        record(name.clone(), ty);
    }
    for item in source.items.iter().filter(|i| production(i)) {
        match item {
            Item::Struct(s) => {
                for (i, f) in s
                    .fields
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| !test_attrs(&f.attrs))
                {
                    record(
                        join(
                            &ident(&s.ident),
                            &f.ident.as_ref().map_or(i.to_string(), ident),
                        ),
                        &f.ty,
                    );
                }
            }
            Item::Type(t) => record(ident(&t.ident), &t.ty),
            _ => {}
        }
    }
    out
}

fn scan_sources(sources: &[Source]) -> Counts {
    let envs: BTreeMap<_, _> = sources
        .iter()
        .map(|s| (s.module.clone(), Imports::items(&s.items)))
        .collect();
    let mut types = TtlTypes::new();
    for source in sources {
        for item in source.items.iter().filter(|i| production(i)) {
            let declaration = match item {
                Item::Struct(s) => Some((
                    &s.ident,
                    false,
                    s.fields
                        .iter()
                        .filter(|f| !test_attrs(&f.attrs))
                        .map(|f| (f.ident.as_ref().map(ident), f.ty.clone()))
                        .collect(),
                )),
                Item::Enum(e) => Some((
                    &e.ident,
                    false,
                    e.variants
                        .iter()
                        .filter(|v| !test_attrs(&v.attrs))
                        .flat_map(|v| v.fields.iter())
                        .filter(|f| !test_attrs(&f.attrs))
                        .map(|f| (f.ident.as_ref().map(ident), f.ty.clone()))
                        .collect(),
                )),
                Item::Type(t) => Some((&t.ident, true, vec![(None, *t.ty.clone())])),
                _ => None,
            };
            if let Some((name, alias, fields)) = declaration {
                types.insert(
                    join(&source.module, &ident(name)),
                    (source.module.clone(), alias, fields),
                );
            }
        }
    }
    let mut out = Counts::new();
    for source in sources {
        let mut g = Guard {
            imports: vec![Imports::items(&source.items)],
            bindings: vec![BTreeMap::new()],
            fields: BTreeMap::new(),
            owner: String::new(),
            function: "<module>".into(),
            loops: 0,
            interval_created: false,
            unknown_tick: false,
            counts: BTreeMap::new(),
        };
        // Only directly declared Interval fields in this source's structs are needed.
        // In particular, do not infer fields through other modules or function returns.
        for item in &source.items {
            if let Item::Struct(s) = item {
                let fields = s
                    .fields
                    .iter()
                    .filter(|f| !test_attrs(&f.attrs))
                    .filter(|f| g.ty(&f.ty) == "tokio::time::Interval")
                    .filter_map(|f| f.ident.as_ref().map(ident))
                    .collect();
                g.fields.insert(ident(&s.ident), fields);
            }
        }
        out.extend(ttl_inventory(source, &TtlGuard { imports: &envs }, &types));
        for item in &source.items {
            g.visit_item(item);
        }
        for ((category, function), count) in g.counts {
            let key = (
                source.rel.clone(),
                category.clone(),
                join(&source.module, &function),
            );
            if category == "timer-loop" {
                out.insert(key, 1);
            } else {
                *out.entry(key).or_default() += count;
            }
        }
    }
    out
}
fn found() -> Counts {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap();
    // Cargo's target inventory also covers new binary roots, without scanning fake tools,
    // examples, benches, integration tests, build scripts or disconnected source files.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
        ])
        .arg(root.join("../Cargo.toml"))
        .output()
        .expect("cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).expect("metadata JSON");
    let mut out = Counts::new();
    for package in meta["packages"].as_array().expect("packages") {
        if package["name"] == "steve-runtime" {
            continue;
        }
        for target in package["targets"].as_array().expect("targets") {
            let kinds = target["kind"].as_array().expect("target kinds");
            if !kinds.iter().any(|k| {
                matches!(
                    k.as_str(),
                    Some("lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" | "bin")
                )
            }) || target["required-features"]
                .as_array()
                .is_some_and(|features| {
                    features
                        .iter()
                        .any(|f| matches!(f.as_str(), Some("fake" | "testkit")))
                })
            {
                continue;
            }
            let file = Path::new(target["src_path"].as_str().expect("target source"));
            let mut sources = vec![];
            modules(&root, file, "", file.parent().unwrap(), &mut sources);
            // Shared lib/bin modules are the same call site, not two violations.
            for (key, count) in scan_sources(&sources) {
                out.insert(key, count);
            }
        }
    }
    out
}
#[path = "support/ratchet.rs"]
mod ratchet;
#[path = "support/ratchet_tests.rs"]
mod ratchet_tests;
#[path = "support/test_sleep.rs"]
mod test_sleep;

fn inventory(counts: &Counts) -> ratchet::Counts {
    counts
        .iter()
        .map(|((file, category, function), n)| {
            (vec![file.clone(), category.clone(), function.clone()], *n)
        })
        .collect()
}
fn differences(now: &Counts, listed: &Counts) -> Vec<String> {
    ratchet::differences(&inventory(now), &inventory(listed), HELP)
}
#[test]
fn runtime_policy_only_shrinks() {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(LIST);
    ratchet::check(inventory(&found()), &file, 3, LIST_HEADER, HELP);
}

fn fixture_counts(code: &str) -> Counts {
    scan_sources(&[Source {
        rel: "fixture.rs".into(),
        module: String::new(),
        items: syn::parse_file(code).unwrap().items,
    }])
}

#[test]
fn aliases_macros_types_and_test_exclusions() {
    let fixture = r#"
        use tokio as rt;
        use rt::time::{self as timers, sleep as nap, Interval as Ticker};
        use chrono::{self as dates, Local as Here};
        use unrelated::unrelated;
        fn imported_value_namespace() { unrelated::unrelated(); }
        struct Stream { ping: Ticker }
        impl Stream {
            async fn next(mut self) { loop { rt::select! { _ = self.ping.tick() => {} } } }
            #[cfg(test)] async fn unit() { loop { nap(d).await; } }
        }
        async fn sleep_loop() { loop { nap(d).await; timers::sleep_until(t).await; } }
        async fn raw_path() { loop { tokio::r#time::r#sleep(d).await; } }
        async fn interval_loop() {
            let mut t = timers::interval_at(at, d);
            let mut alias = t;
            for _ in rows { rt::select! { _ = alias.tick() => { Here::now(); } } }
            while ready { nap(d).await; }
        }
        async fn typed(t: &mut Ticker) { while ready { t.tick().await; } }
        async fn field(s: &mut Stream) { loop { s.ping.tick().await; } }
        async fn block_import() { use tokio::time::sleep; loop { sleep(d).await; } Here::now(); }
        fn zone() { dates::Local::today(); convert(&Here); Here.from_local_datetime(&d).earliest(); }
        fn normal(other: Other) { loop { other.tick(); } let s = "chrono::Local::now()"; }
        async fn once() { nap(d).await; timers::interval(d); }
        #[cfg(test)] fn unit() { Here::now(); }
        #[cfg(all(unix, feature = "fake"))] fn fake() { Here::now(); }
        #[cfg(feature = "testkit")] fn kit() { Here::now(); }
        #[cfg(any(test, unix))] fn production_too() { Here::now(); }
        macro_rules! unused { () => { fn generated() { Here::now(); } } }
    "#;
    let result = fixture_counts(fixture);
    let actual: BTreeMap<_, _> = result
        .into_iter()
        .map(|((_, category, function), count)| ((category, function), count))
        .collect();
    let expected = [
        ("timer-loop", "Stream::next", 1),
        ("timer-loop", "sleep_loop", 1),
        ("timer-loop", "raw_path", 1),
        ("timer-loop", "interval_loop", 1),
        ("timer-loop", "typed", 1),
        ("timer-loop", "field", 1),
        ("timer-loop", "block_import", 1),
        ("earliest", "zone", 1),
        ("process-tz", "zone", 3),
        ("process-tz", "interval_loop", 1),
        ("process-tz", "block_import", 1),
        ("process-tz", "production_too", 1),
    ]
    .into_iter()
    .map(|(c, f, n)| ((c.into(), f.into()), n))
    .collect();
    assert_eq!(actual, expected);
}

#[test]
fn additions_removals_and_count_changes_fail_the_ratchet() {
    let key = ("fixture/src/lib.rs".into(), "earliest".into(), "f".into());
    let original = Counts::from([(key.clone(), 1)]);
    for changed in [Counts::new(), Counts::from([(key, 2)])] {
        assert!(!differences(&changed, &original).is_empty());
        assert!(!differences(&original, &changed).is_empty());
    }
    assert!(differences(&original, &original).is_empty());
}

#[test]
fn module_tree_follows_path_attributes_and_assembly_dsl_without_test_sources() {
    let fixture = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let root = fixture.path();
    let src = root.join("fixture/src");
    std::fs::create_dir_all(src.join("inline")).unwrap();
    for (file, code) in [
        (
            "lib.rs",
            r#"
            mod shared;
            ordered_installers! { MODULES: fn(); mod generated; install, }
            mod r#type;
            mod inline { #[path = "renamed.rs"] mod child; }
            #[cfg(test)] mod missing_unit;
            #[cfg(feature = "fake")] mod missing_fake;
            #[cfg(feature = "testkit")] mod missing_kit;
            mod tests;
        "#,
        ),
        (
            "shared.rs",
            r#"
            use tokio::time::{sleep as nap, Interval as Ticker};
            async fn f(t: &mut Ticker) { loop { t.tick().await; nap(d).await; } }
            async fn g() { use tokio::time::sleep; loop { sleep(d).await; } }
        "#,
        ),
        (
            "inline/renamed.rs",
            r#"
            use chrono::Local;
            fn zone() { Local::now(); }
        "#,
        ),
        (
            "generated.rs",
            r#"install! { install(a, b) [input] => [output] {
            let (mut refresh, mut health) = (tokio::time::interval(d), tokio::time::interval(d));
            tokio::join!(async { loop { refresh.tick().await; } }, async { loop { health.tick().await; } });
        }}"#,
        ),
        ("type.rs", "fn clean() {}"),
        ("tests.rs", "deliberately invalid Rust: must not be parsed"),
        (
            "disconnected.rs",
            "deliberately invalid Rust: must not be parsed",
        ),
    ] {
        std::fs::write(src.join(file), code).unwrap();
    }
    let mut sources = vec![];
    modules(root, &src.join("lib.rs"), "", &src, &mut sources);
    assert_eq!(sources.len(), 6);
    let actual = scan_sources(&sources);
    let expected = [
        ("fixture/src/shared.rs", "timer-loop", "shared::f", 1),
        ("fixture/src/shared.rs", "timer-loop", "shared::g", 1),
        (
            "fixture/src/generated.rs",
            "timer-loop",
            "generated::install",
            1,
        ),
        (
            "fixture/src/inline/renamed.rs",
            "process-tz",
            "inline::child::zone",
            1,
        ),
    ]
    .into_iter()
    .map(|(file, category, function, count)| {
        ((file.into(), category.into(), function.into()), count)
    })
    .collect();
    assert_eq!(actual, expected);
}

#[test]
fn callbacks_do_not_inherit_the_outer_loop() {
    let actual = fixture_counts(
        r#"
        async fn once_in_callbacks() {
            loop {
                let callback = || tokio::time::sleep(d);
                let future = async { tokio::time::sleep(d).await; };
            }
        }
        fn timer_in_closure() {
            let callback = || { loop { tokio::time::sleep(d); } };
        }
        fn timer_in_async() {
            let future = async { loop { tokio::time::sleep(d).await; } };
        }
    "#,
    );
    let functions: BTreeSet<_> = actual.into_keys().map(|(_, _, f)| f).collect();
    assert_eq!(
        functions,
        BTreeSet::from(["timer_in_closure".into(), "timer_in_async".into()])
    );
}

#[test]
fn unsupported_indirection_is_not_inferred() {
    let actual = fixture_counts(
        r#"
        use tokio::time::*;
        async fn glob_import() { loop { sleep(d).await; } }
        async fn private_wait() { tokio::time::sleep(d).await; }
        async fn private_wrapper() { loop { private_wait().await; } }
        async fn custom_clock(clock: Clock) { loop { clock.sleep(d).await; } }
        fn thread_poll() { loop { std::thread::sleep(d); } }
        async fn closure_return() {
            let make = || other_factory(d);
            let mut timer = make();
            loop { timer.tick().await; }
        }
    "#,
    );
    assert!(actual.is_empty());
}

#[test]
fn constructor_tick_pairing_stays_within_its_function() {
    let actual = fixture_counts(
        r#"
        fn inline_factory() {
            let every = |d| { let t = tokio::time::interval(d); t };
            let (mut a, mut b) = (every(d), every(d));
            tokio::join!(async { loop { a.tick().await; } }, async { loop { b.tick().await; } });
        }
        fn creation_only() { tokio::time::interval(d); }
        fn unknown_tick_only() { loop { unknown.tick(); } }
        fn known_other(other: Other) {
            tokio::time::interval(d);
            loop { other.tick(); }
        }
        fn callback_is_not_a_loop() {
            tokio::time::interval(d);
            loop { let future = async { unknown.tick(); }; }
        }
    "#,
    );
    let functions: BTreeSet<_> = actual.into_keys().map(|(_, _, f)| f).collect();
    assert_eq!(functions, BTreeSet::from(["inline_factory".into()]));
}

#[test]
fn ttl_maps_follow_aliases_value_structs_and_locks_without_counting_connections() {
    let fixture = r#"
        use std::collections::{HashMap as Map, BTreeMap};
        use std::sync::{Mutex as Lock, Arc};
        use std::time::Instant as Stamp;
        type Entries = Map<String, (Stamp, String)>;
        type Lag = (String, String, u64);
        type Timed = (u64, String);
        struct Cached { at: i64, body: String }
        struct Connection { last: Lock<Stamp>, rtt: Option<(i64, i64)> }
        struct Caches {
            direct: Lock<Entries>,
            named: tokio::sync::RwLock<BTreeMap<String, Arc<Cached>>>,
            whole: Lock<Option<(Stamp, std::collections::HashMap<String, String>)>>,
            connections: Lock<Map<String, Arc<Connection>>>,
            ordinary: Lock<Map<String, String>>,
            lagging: Lock<Map<String, Lag>>,
            millis: Lock<Map<String, Timed>>,
            #[cfg(test)] test: Lock<Entries>,
        }
        static TTL: Lock<Map<String, (Stamp, bool)>> = todo!();
        fn cached() { static LOCAL: Lock<Map<String, (Stamp, bool)>> = todo!(); }
        enum Slot { Running, Done(Stamp, String) }
        type Slots = Lock<Map<String, Slot>>;
        fn read(at: i64, stamp: Stamp, entry: Timed) {
            if now() - at < TTL_MS { }
            if now() - entry.0 < TTL_MS { }
            stamp.elapsed();
        }
        #[cfg(test)] static TEST: Lock<Map<String, (Stamp, bool)>> = todo!();
    "#;
    let result = scan_sources(&[Source {
        rel: "fixture/src/lib.rs".into(),
        module: "".into(),
        items: syn::parse_file(fixture).unwrap().items,
    }]);
    let actual: BTreeSet<_> = result
        .into_keys()
        .filter(|(_, c, _)| c == "ttl-cache")
        .map(|(_, _, f)| f)
        .collect();
    assert_eq!(
        actual,
        [
            "Caches::direct",
            "Caches::named",
            "Caches::whole",
            "Caches::millis",
            "TTL",
            "LOCAL",
            "Slots"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[test]
fn r3e_shared_row_deadlines_are_not_ttl_caches_but_raw_lookalikes_still_are() {
    let fixture = r#"
        use steve_runtime::due::RowDeadlines as SharedRows;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        type Rows = Arc<SharedRows>;
        struct Queues { next: SharedRows, deferred: Rows }
        struct RowDeadlines { rows: Mutex<HashMap<String, i64>> }
        fn age(now: i64, at: i64) { let _ = now - at; }
    "#;
    let actual: BTreeSet<_> = scan_sources(&[Source {
        rel: "fixture/src/lib.rs".into(),
        module: "".into(),
        items: syn::parse_file(fixture).unwrap().items,
    }])
    .into_keys()
    .filter(|(_, category, _)| category == "ttl-cache")
    .map(|(_, _, field)| field)
    .collect();
    assert_eq!(actual, BTreeSet::from(["RowDeadlines::rows".to_owned()]));
}
