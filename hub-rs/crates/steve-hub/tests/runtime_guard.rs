//! Runtime ratchet: move timers and wall-time policy into steve-runtime. This is a
//! syntax guard, not a Rust type checker: review `.earliest()` receivers in the inventory.
//! Scan module trees, not tests/examples or disconnected source files.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::parse::{ParseStream, Parser, discouraged::Speculative};
use syn::visit::{self, Visit};
use syn::{Expr, Item, Meta, Token};

type Counts = BTreeMap<(String, String, String), usize>;
const LIST: &str = "tests/runtime_guard.txt";
const HELP: &str = "use steve-runtime due_loop / every / clock / tz, or explain in the task card before rewriting with STEVE_RUNTIME_BLESS=1";

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
    globs: Vec<String>,
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
            syn::UseTree::Glob(_) => self.globs.push(prefix.into()),
        }
    }
    fn items(items: &[Item]) -> Self {
        let mut out = Self::default();
        for i in items.iter().filter(|i| production(i)) {
            if let Item::Use(u) = i {
                out.add("", &u.tree);
            } else {
                let name = match i {
                    Item::Struct(i) => Some(&i.ident),
                    Item::Enum(i) => Some(&i.ident),
                    Item::Fn(i) => Some(&i.sig.ident),
                    Item::Type(i) => Some(&i.ident),
                    _ => None,
                };
                if let Some(n) = name {
                    out.names.insert(ident(n), ident(n));
                }
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
// Installer declarations live in a macro DSL but still declare ordinary modules.
fn macro_modules(input: ParseStream<'_>, out: &mut Vec<Item>) -> syn::Result<()> {
    while !input.is_empty() {
        let fork = input.fork();
        if let Ok(module) = fork.parse::<syn::ItemMod>() {
            input.advance_to(&fork);
            out.push(Item::Mod(module));
        } else if input.peek(syn::token::Brace) {
            let inner;
            syn::braced!(inner in input);
            macro_modules(&inner, out)?;
        } else if input.peek(syn::token::Paren) {
            let inner;
            syn::parenthesized!(inner in input);
            macro_modules(&inner, out)?;
        } else if input.peek(syn::token::Bracket) {
            let inner;
            syn::bracketed!(inner in input);
            macro_modules(&inner, out)?;
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
            if !m.mac.path.is_ident("macro_rules") {
                (|input: ParseStream<'_>| macro_modules(input, &mut expanded))
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

struct Guard<'a> {
    modules: &'a BTreeMap<String, Imports>,
    module: String,
    imports: Vec<Imports>,
    bindings: Vec<BTreeMap<String, String>>,
    fields: &'a BTreeMap<String, BTreeMap<String, String>>,
    owner: String,
    function: String,
    loops: usize,
    counts: BTreeMap<(String, String), usize>,
    resolved: RefCell<BTreeMap<(String, String), String>>,
    resolving: RefCell<BTreeSet<(String, String)>>,
}
impl Guard<'_> {
    fn resolve_in(&self, raw: &str, module: &str, depth: usize) -> String {
        let key = (raw.to_owned(), module.to_owned());
        if let Some(resolved) = self.resolved.borrow().get(&key) {
            return resolved.clone();
        }
        if !self.resolving.borrow_mut().insert(key.clone()) {
            return raw.into();
        }
        let result = self.resolve_path(raw, module, depth);
        self.resolving.borrow_mut().remove(&key);
        self.resolved.borrow_mut().insert(key, result.clone());
        result
    }
    fn resolve_path(&self, raw: &str, module: &str, depth: usize) -> String {
        assert!(
            depth <= 128,
            "runtime guard import expansion too deep: {module} {raw}"
        );
        let (head, rest) = raw.split_once("::").unwrap_or((raw, ""));
        if head == "chrono" {
            return raw.replacen("chrono::offset::Local", "chrono::Local", 1);
        }
        if head == "tokio" {
            return raw.into();
        }
        let absolute = |scope: &str, rest: &str| {
            let full = join(scope, rest);
            if self.modules.contains_key(&full) {
                return format!("@::{full}");
            }
            if !full.contains("::")
                && self
                    .modules
                    .get("")
                    .is_some_and(|e| e.names.contains_key(&full))
            {
                return self.resolve_in(&full, "", depth + 1);
            }
            if let Some((scope, name)) = full.rsplit_once("::") {
                if self.modules.contains_key(scope) {
                    return self.resolve_in(name, scope, depth + 1);
                }
            }
            format!("@::{full}")
        };
        if head == "crate" || head == "@" {
            return absolute("", rest);
        }
        if head == "self" {
            return absolute(module, rest);
        }
        if head == "super" {
            let parent = module.rsplit_once("::").map_or("", |(p, _)| p);
            return if rest.starts_with("super::") {
                self.resolve_in(rest, parent, depth + 1)
            } else {
                absolute(parent, rest)
            };
        }
        // Modules and functions occupy different namespaces: `pub use ingest::ingest`
        // must resolve the module prefix before the same-named function alias.
        let child = join(module, head);
        if !rest.is_empty() && self.modules.contains_key(&child) {
            return self.resolve_in(rest, &child, depth + 1);
        }
        let envs = self
            .imports
            .iter()
            .rev()
            .filter(|_| module == self.module)
            .chain(self.modules.get(module));
        for env in envs {
            if let Some(target) = env.names.get(head) {
                // A macro/value import can share an external crate's name
                // (`use anyhow::anyhow`). Its namespace-qualified path is already
                // rooted; expanding that prefix again would grow it forever.
                if target == head || target.starts_with(&format!("{head}::")) {
                    return raw.into();
                }
                let resolved = self.resolve_in(target, module, depth + 1);
                return if rest.is_empty() {
                    resolved
                } else {
                    self.resolve_in(&join(&resolved, rest), module, depth + 1)
                };
            }
            for glob in &env.globs {
                let base = self.resolve_in(glob, module, depth + 1);
                let imported = match base.as_str() {
                    "tokio::time" => [
                        "sleep",
                        "sleep_until",
                        "interval",
                        "interval_at",
                        "Interval",
                    ]
                    .contains(&head),
                    "tokio" => head == "time",
                    "chrono" => ["Local", "LocalResult"].contains(&head),
                    _ => false,
                };
                if imported {
                    return join(&base, raw);
                }
                if let Some(scope) = base.strip_prefix("@::") {
                    if scope != module {
                        let resolved = self.resolve_in(raw, scope, depth + 1);
                        if resolved != raw {
                            return resolved;
                        }
                    }
                }
            }
        }
        if raw.contains("::") {
            let candidate = absolute(module, raw);
            if !candidate.starts_with("@::") {
                return candidate;
            }
        }
        raw.into()
    }
    fn resolve(&self, raw: &str) -> String {
        self.resolve_in(raw, &self.module, 0)
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
                    callable.strip_prefix("fn:").unwrap_or_default().to_owned()
                }
            }
            Expr::Closure(c) => format!("fn:{}", self.value(&c.body)),
            Expr::Block(b) => self.block_value(&b.block),
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
                self.fields
                    .get(&join(&self.module, &owner))
                    .and_then(|fields| fields.get(&name))
                    .map(|t| self.resolve(t))
                    .unwrap_or_default()
            }
            _ => String::new(),
        }
    }
    // Only infer local return values, enough for interval-building closures. No code runs.
    fn block_value(&self, block: &syn::Block) -> String {
        let mut locals = BTreeMap::new();
        let lookup = |expr: &Expr, locals: &BTreeMap<String, String>| {
            if let Expr::Path(p) = expr {
                if let Some(value) = locals.get(&path(&p.path)) {
                    return value.clone();
                }
            }
            self.value(expr)
        };
        for stmt in &block.stmts {
            match stmt {
                syn::Stmt::Local(l) if !test_attrs(&l.attrs) => {
                    if let (syn::Pat::Ident(p), Some(init)) = (&l.pat, &l.init) {
                        locals.insert(ident(&p.ident), lookup(&init.expr, &locals));
                    }
                }
                syn::Stmt::Expr(Expr::Return(r), _) => {
                    return r
                        .expr
                        .as_ref()
                        .map(|e| lookup(e, &locals))
                        .unwrap_or_default();
                }
                syn::Stmt::Expr(expr, None) => return lookup(expr, &locals),
                _ => {}
            }
        }
        String::new()
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
        let old = (self.function.clone(), self.loops);
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
        (self.function, self.loops) = old;
    }
}
impl<'ast> Visit<'ast> for Guard<'_> {
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
        let changed = !env.names.is_empty() || !env.globs.is_empty();
        self.imports.push(env);
        if changed {
            self.resolved.borrow_mut().clear();
        }
        self.bindings.push(BTreeMap::new());
        visit::visit_block(self, b);
        self.bindings.pop();
        self.imports.pop();
        if changed {
            self.resolved.borrow_mut().clear();
        }
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
        self.bindings.push(BTreeMap::new());
        for pat in &e.inputs {
            self.bind(pat, String::new());
        }
        visit::visit_expr_closure(self, e);
        self.bindings.pop();
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
        if self.loops > 0
            && [
                "tokio::time::sleep",
                "tokio::time::sleep_until",
                "tokio::time::Interval::tick",
            ]
            .contains(&self.value(&c.func).as_str())
        {
            self.hit("timer-loop");
        }
        visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if m.method == "earliest" && m.args.is_empty() {
            self.hit("earliest");
        }
        if self.loops > 0
            && m.method == "tick"
            && self.value(&m.receiver) == "tokio::time::Interval"
        {
            self.hit("timer-loop");
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
        fn expressions(input: ParseStream<'_>, guard: &mut Guard<'_>) -> syn::Result<()> {
            while !input.is_empty() {
                let fork = input.fork();
                if let Ok(expr) = fork.parse::<Expr>() {
                    input.advance_to(&fork);
                    guard.visit_expr(&expr);
                } else if input.peek(syn::token::Brace) {
                    let inner;
                    syn::braced!(inner in input);
                    expressions(&inner, guard)?;
                } else if input.peek(syn::token::Paren) {
                    let inner;
                    syn::parenthesized!(inner in input);
                    expressions(&inner, guard)?;
                } else if input.peek(syn::token::Bracket) {
                    let inner;
                    syn::bracketed!(inner in input);
                    expressions(&inner, guard)?;
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
        (|input: ParseStream<'_>| expressions(input, self))
            .parse2(m.tokens.clone())
            .expect("macro expressions");
    }
}

fn scan_sources(sources: &[Source]) -> Counts {
    let envs: BTreeMap<_, _> = sources
        .iter()
        .map(|s| (s.module.clone(), Imports::items(&s.items)))
        .collect();
    let mut fields = BTreeMap::new();
    for source in sources {
        for item in &source.items {
            if let Item::Struct(s) = item {
                fields.insert(
                    join(&source.module, &ident(&s.ident)),
                    s.fields
                        .iter()
                        .filter_map(|f| {
                            if test_attrs(&f.attrs) {
                                return None;
                            }
                            match &f.ty {
                                syn::Type::Path(t) => {
                                    Some((ident(f.ident.as_ref()?), path(&t.path)))
                                }
                                _ => None,
                            }
                        })
                        .collect(),
                );
            }
        }
    }
    let mut out = Counts::new();
    for source in sources {
        let mut g = Guard {
            modules: &envs,
            module: source.module.clone(),
            imports: vec![],
            bindings: vec![BTreeMap::new()],
            fields: &fields,
            owner: String::new(),
            function: "<module>".into(),
            loops: 0,
            counts: BTreeMap::new(),
            resolved: RefCell::default(),
            resolving: RefCell::default(),
        };
        for item in &source.items {
            g.visit_item(item);
        }
        for ((category, function), count) in g.counts {
            let location = join(&source.module, &function);
            let key = (source.rel.clone(), category.clone(), location);
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
fn differences(now: &Counts, listed: &Counts) -> Vec<String> {
    let keys: BTreeSet<_> = now.keys().chain(listed.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let actual = now.get(key).copied().unwrap_or(0);
            let old = listed.get(key).copied().unwrap_or(0);
            (actual != old).then(|| {
                format!(
                    "{} {} {}: found {actual}, listed {old}; {}",
                    key.0,
                    key.1,
                    key.2,
                    if actual < old {
                        "shorten the list"
                    } else {
                        HELP
                    }
                )
            })
        })
        .collect()
}
#[test]
fn runtime_policy_only_shrinks() {
    let now = found();
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(LIST);
    if std::env::var("STEVE_RUNTIME_BLESS").as_deref() == Ok("1") {
        let mut text = String::from(
            "# Existing runtime policy outside steve-runtime; only shrinks.\n# Move timers to due_loop / every / clock and wall-time policy to tz.\n# Syntax scan of earliest() calls; timer loops count once per function.\n# file under hub-rs/crates<TAB>category<TAB>function (module/impl qualified)<TAB>count\n# Rewrite after explaining in the task card: STEVE_RUNTIME_BLESS=1 cargo test -p steve-hub --test runtime_guard\n",
        );
        for ((file, category, function), count) in now {
            text.push_str(&format!("{file}\t{category}\t{function}\t{count}\n"));
        }
        std::fs::write(file, text).unwrap();
        return;
    }
    let mut listed = Counts::new();
    for line in std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let parts: Vec<_> = line.split('\t').collect();
        assert_eq!(parts.len(), 4, "bad runtime baseline row: {line}");
        let count: usize = parts[3].parse().expect("runtime count");
        assert!(
            count > 0
                && listed
                    .insert((parts[0].into(), parts[1].into(), parts[2].into()), count)
                    .is_none(),
            "duplicate or empty row: {line}"
        );
    }
    let problems = differences(&now, &listed);
    assert!(
        problems.is_empty(),
        "runtime guard:\n{}",
        problems.join("\n")
    );
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
        async fn block_import() { use tokio::time::*; loop { sleep(d).await; } Here::now(); }
        fn zone() { dates::Local::today(); convert(&Here); Here.from_local_datetime(&d).earliest(); }
        fn normal(other: Other) { loop { other.tick(); } let s = "chrono::Local::now()"; }
        async fn once() { nap(d).await; timers::interval(d); }
        #[cfg(test)] fn unit() { Here::now(); }
        #[cfg(all(unix, feature = "fake"))] fn fake() { Here::now(); }
        #[cfg(feature = "testkit")] fn kit() { Here::now(); }
        #[cfg(any(test, unix))] fn production_too() { Here::now(); }
        macro_rules! unused { () => { fn generated() { Here::now(); } } }
    "#;
    let parsed = syn::parse_file(fixture).unwrap();
    let result = scan_sources(&[Source {
        rel: "fixture/src/lib.rs".into(),
        module: "".into(),
        items: parsed.items,
    }]);
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
fn module_tree_follows_path_attributes_and_reexports_without_test_sources() {
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
            pub use shared::*;
            mod r#type;
            mod same;
            pub use same::same;
            fn call_same() { same::same(); }
            mod timers { pub use tokio::time::{sleep, Interval}; }
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
            use super::timers::{sleep as nap, Interval as Ticker};
            use crate::*;
            async fn f(t: &mut Ticker) { loop { t.tick().await; nap(d).await; } }
            async fn g() { use crate::timers::*; loop { sleep(d).await; } }
        "#,
        ),
        (
            "inline/renamed.rs",
            r#"
            use chrono::*;
            fn zone() { Local::now(); }
        "#,
        ),
        (
            "generated.rs",
            r#"install! { install(a, b) [input] => [output] {
            let every = |d| { let t = tokio::time::interval(d); t };
            let (mut refresh, mut health) = (every(d), every(d));
            tokio::join!(async { loop { refresh.tick().await; } }, async { loop { health.tick().await; } });
        }}"#,
        ),
        ("type.rs", "fn clean() {}"),
        ("same.rs", "pub fn same() {}"),
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
    assert_eq!(sources.len(), 8);
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
