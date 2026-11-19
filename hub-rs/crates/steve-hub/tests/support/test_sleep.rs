//! Test-side time debt: a file inventory that can only shrink. The wait implementation is exempt.
use super::ratchet;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use syn::{
    parse::{ParseStream, Parser},
    visit::{self, Visit},
};
const HELP: &str = "use steve_runtime::testkit for condition waiting, or virtual time for timing assertions; shorten with STEVE_RUNTIME_BLESS=1";
const LIST: &str = "tests/test_sleep.txt";
const HEADER: &str = "# Existing test sleep calls; only shrinks.\n# file under hub-rs/crates<TAB>count\n# Use steve-runtime testkit eventual conditions or virtual time.\n# STEVE_RUNTIME_BLESS=1 rewrites after removals; the empty inventory and guard remain.\n";

// Rust tokenization discards comments and treats strings as one token. Macro bodies still count,
// including select!/assert! arguments, without mistaking a quoted counterexample for a sleep.
// A method call `.sleep(` is the injected `Clock::sleep` (virtual in tests, the rule product code
// follows too), so only free-function sleeps (std::thread, tokio::time, `use … sleep`) count.
fn tokens(input: ParseStream<'_>) -> syn::Result<usize> {
    let mut n = 0;
    let mut after_dot = false;
    while !input.is_empty() {
        if input.peek(syn::Ident) {
            let name: syn::Ident = input.parse()?;
            if name == "sleep" && input.peek(syn::token::Paren) && !after_dot {
                n += 1;
            }
            after_dot = false;
        } else if input.peek(syn::token::Brace) {
            let inner;
            syn::braced!(inner in input);
            n += tokens(&inner)?;
        } else if input.peek(syn::token::Paren) {
            let inner;
            syn::parenthesized!(inner in input);
            n += tokens(&inner)?;
        } else if input.peek(syn::token::Bracket) {
            let inner;
            syn::bracketed!(inner in input);
            n += tokens(&inner)?;
        } else {
            after_dot = input.peek(syn::Token![.]);
            input.step(|c| {
                c.token_tree()
                    .map(|(_, next)| ((), next))
                    .ok_or_else(|| c.error("token"))
            })?;
            continue;
        }
        after_dot = false;
    }
    Ok(n)
}
fn attrs_test(attrs: &[syn::Attribute]) -> bool {
    super::test_attrs(attrs)
        || attrs
            .iter()
            .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}
struct Scan {
    test: bool,
    count: usize,
    dir: PathBuf,
    path_dir: PathBuf,
    external: BTreeSet<PathBuf>,
}
impl<'ast> Visit<'ast> for Scan {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let old = self.test;
        self.test |= attrs_test(super::attrs(item));
        visit::visit_item(self, item);
        self.test = old;
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let old = self.test;
        self.test |= attrs_test(&item.attrs);
        visit::visit_impl_item_fn(self, item);
        self.test = old;
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let old = self.test;
        self.test |= item.ident == "tests" || attrs_test(&item.attrs);
        if let Some((_, items)) = &item.content {
            let dir = self.dir.clone();
            let path_dir = self.path_dir.clone();
            self.dir = dir.join(item.ident.to_string());
            self.path_dir = self.dir.clone();
            for item in items {
                self.visit_item(item);
            }
            self.dir = dir;
            self.path_dir = path_dir;
        } else if self.test {
            let explicit = item.attrs.iter().find_map(|a| match &a.meta {
                syn::Meta::NameValue(n) if n.path.is_ident("path") => match &n.value {
                    syn::Expr::Lit(l) => match &l.lit {
                        syn::Lit::Str(s) => Some(s.value()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            });
            let child = if let Some(p) = explicit {
                Some(self.path_dir.join(p))
            } else {
                [
                    self.dir.join(format!("{}.rs", item.ident)),
                    self.dir.join(item.ident.to_string()).join("mod.rs"),
                ]
                .into_iter()
                .find(|p| p.exists())
            };
            if let Some(p) = child.filter(|p| p.exists()) {
                self.external.insert(p.canonicalize().unwrap());
            }
        }
        self.test = old;
    }
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if self.test
            && matches!(&*call.func,syn::Expr::Path(p) if p.path.segments.last().is_some_and(|s|s.ident=="sleep"))
        {
            self.count += 1;
        }
        visit::visit_expr_call(self, call);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if self.test {
            self.count += tokens.parse2(m.tokens.clone()).expect("macro tokens");
        }
    }
}
struct File {
    rel: String,
    inline: usize,
    all: usize,
    all_mods: BTreeSet<PathBuf>,
}
fn scan(file: &Path, ast: &syn::File, test: bool) -> Scan {
    let dir = if ["lib.rs", "main.rs", "mod.rs"]
        .contains(&file.file_name().unwrap().to_str().unwrap())
        || file
            .parent()
            .unwrap()
            .file_name()
            .is_some_and(|s| s == "bin")
    {
        file.parent().unwrap().to_path_buf()
    } else {
        file.with_extension("")
    };
    let mut scan = Scan {
        test,
        count: 0,
        dir,
        path_dir: file.parent().unwrap().to_path_buf(),
        external: BTreeSet::new(),
    };
    scan.visit_file(ast);
    scan
}
fn found() -> ratchet::Counts {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../../..").canonicalize().unwrap();
    let crates = root.join("hub-rs/crates");
    let output = std::process::Command::new("git")
        .current_dir(&root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ])
        .output()
        .expect("git files");
    assert!(output.status.success());
    let mut files = BTreeMap::new();
    let mut tested = BTreeSet::new();
    for rel in String::from_utf8(output.stdout).unwrap().lines() {
        if !rel.starts_with("hub-rs/crates/")
            || rel
                .split('/')
                .any(|p| p == "target" || p.starts_with(".tmp"))
        {
            continue;
        }
        let file = root.join(rel).canonicalize().unwrap();
        let raw = std::fs::read_to_string(&file).unwrap();
        let whole = file.components().any(|c| c.as_os_str() == "tests")
            || file.file_stem().is_some_and(|s| s == "tests");
        let (inline, all, inline_mods, all_mods) = match syn::parse_file(&raw) {
            Ok(ast) => {
                if whole || attrs_test(&ast.attrs) {
                    tested.insert(file.clone());
                }
                let a = scan(&file, &ast, false);
                let b = scan(&file, &ast, true);
                (a.count, b.count, a.external, b.external)
            }
            // Compiler/protocol fixtures may be Rust fragments rather than files of items.
            Err(_) => {
                if whole {
                    tested.insert(file.clone());
                }
                (
                    0,
                    tokens
                        .parse_str(&raw)
                        .unwrap_or_else(|e| panic!("{}: {e}", file.display())),
                    BTreeSet::new(),
                    BTreeSet::new(),
                )
            }
        };
        tested.extend(inline_mods.iter().cloned());
        files.insert(
            file.clone(),
            File {
                rel: file
                    .strip_prefix(&crates)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                inline,
                all,
                all_mods,
            },
        );
    }
    loop {
        let before = tested.len();
        for (path, file) in &files {
            if tested.contains(path) {
                tested.extend(file.all_mods.iter().cloned());
            }
        }
        if tested.len() == before {
            break;
        }
    }
    let mut out = ratchet::Counts::new();
    for (path, file) in files {
        if file.rel == "steve-runtime/src/testkit.rs" {
            continue;
        }
        let count = if tested.contains(&path) {
            file.all
        } else {
            file.inline
        };
        if count > 0 {
            out.insert(vec![file.rel], count);
        }
    }
    out
}
#[test]
fn test_sleep_inventory_only_shrinks() {
    ratchet::check(
        found(),
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST),
        1,
        HEADER,
        HELP,
    );
}
#[test]
fn source_modules_and_integration_files_have_distinct_scopes() {
    let ast = syn::parse_file(
        r#"
        fn production() { tokio::time::sleep(delay); }
        #[cfg(test)]
        mod checks { fn test_helper() { std::thread::sleep(delay); } }
        "#,
    )
    .unwrap();
    assert_eq!(scan(Path::new("lib.rs"), &ast, false).count, 1);
    assert_eq!(scan(Path::new("integration.rs"), &ast, true).count, 2);
}

#[test]
fn added_sleep_fails_but_comments_and_quoted_examples_do_not_count() {
    let source = r#"// sleep(d)
        let example="std::thread::sleep(d)";
        tokio::select! { _=tokio::time::sleep(d)=>{}, _=f()=>{std::thread::sleep(d);} }
    "#;
    assert_eq!(tokens.parse_str(source).unwrap(), 2);
    // the injected clock's method is virtual time, not a wall-clock wait
    assert_eq!(
        tokens.parse_str("tokio::join!(clock.sleep(d), x)").unwrap(),
        0
    );
    let old = ratchet::Counts::from([(vec!["fixture.rs".into()], 1)]);
    let added = ratchet::Counts::from([(vec!["fixture.rs".into()], 2)]);
    assert!(
        !ratchet::differences(&added, &old, HELP).is_empty(),
        "an added call must fail"
    );
    assert!(
        !ratchet::differences(&ratchet::Counts::new(), &old, HELP).is_empty(),
        "removal requires shortening the inventory"
    );
    assert!(ratchet::differences(&old, &old, HELP).is_empty());
}
