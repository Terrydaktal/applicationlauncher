//! Canonical pre-observer Rust boundary shared by every shipped artifact.
use quote::ToTokens;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use syn::{
    Expr, Item, Meta, Stmt,
    visit_mut::{self, VisitMut},
};

fn optional_cfg(meta: &Meta) -> Option<bool> {
    match meta {
        Meta::Path(path) if path.is_ident("test") => Some(false),
        Meta::NameValue(value) if value.path.is_ident("feature") => {
            let Expr::Lit(value) = &value.value else {
                return None;
            };
            let syn::Lit::Str(value) = &value.lit else {
                return None;
            };
            match value.value().as_str() {
                "observability" | "update-timings" | "diagnostic-build" => Some(false),
                "release-minimal" => Some(true),
                _ => None,
            }
        }
        Meta::List(list) => {
            let values = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .ok()?;
            let values = values
                .iter()
                .map(optional_cfg)
                .collect::<Option<Vec<_>>>()?;
            if list.path.is_ident("not") && values.len() == 1 {
                Some(!values[0])
            } else if list.path.is_ident("all") {
                Some(values.iter().all(|value| *value))
            } else if list.path.is_ident("any") {
                Some(values.iter().any(|value| *value))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn retain_attributes(attributes: &mut Vec<syn::Attribute>) -> bool {
    let mut keep = true;
    attributes.retain(|attribute| {
        if attribute.path().is_ident("test") {
            keep = false;
            return false;
        }
        if attribute.path().is_ident("doc") {
            return false;
        }
        if attribute.path().is_ident("cfg") {
            if let Ok(meta) = attribute.parse_args::<Meta>() {
                if let Some(value) = optional_cfg(&meta) {
                    keep &= value;
                    return false;
                }
            }
        }
        true
    });
    keep
}

fn item_attributes(item: &mut Item) -> Option<&mut Vec<syn::Attribute>> {
    match item {
        Item::Const(value) => Some(&mut value.attrs),
        Item::Enum(value) => Some(&mut value.attrs),
        Item::ExternCrate(value) => Some(&mut value.attrs),
        Item::Fn(value) => Some(&mut value.attrs),
        Item::ForeignMod(value) => Some(&mut value.attrs),
        Item::Impl(value) => Some(&mut value.attrs),
        Item::Macro(value) => Some(&mut value.attrs),
        Item::Mod(value) => Some(&mut value.attrs),
        Item::Static(value) => Some(&mut value.attrs),
        Item::Struct(value) => Some(&mut value.attrs),
        Item::Trait(value) => Some(&mut value.attrs),
        Item::Type(value) => Some(&mut value.attrs),
        Item::Union(value) => Some(&mut value.attrs),
        Item::Use(value) => Some(&mut value.attrs),
        _ => None,
    }
}

fn observer(name: &str) -> bool {
    matches!(name, "observe" | "diagnostic_log" | "diagnostic_line")
}

fn product_item(item: &mut Item) -> bool {
    if let Item::Macro(value) = item {
        if value
            .ident
            .as_ref()
            .is_some_and(|name| observer(&name.to_string()) || name == "spawn_worker")
        {
            return false;
        }
    }
    item_attributes(item).is_none_or(retain_attributes)
}

struct Product;
impl VisitMut for Product {
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        retain_attributes(&mut file.attrs);
        file.items.retain_mut(product_item);
        visit_mut::visit_file_mut(self, file);
    }

    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if let Some((_, items)) = &mut module.content {
            items.retain_mut(product_item);
        }
        visit_mut::visit_item_mod_mut(self, module);
    }

    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        block.stmts.retain_mut(|statement| match statement {
            Stmt::Local(value) => retain_attributes(&mut value.attrs),
            Stmt::Item(value) => product_item(value),
            Stmt::Macro(value) => retain_attributes(&mut value.attrs),
            Stmt::Expr(Expr::If(value), _) => retain_attributes(&mut value.attrs),
            Stmt::Expr(Expr::Block(value), _) => retain_attributes(&mut value.attrs),
            _ => true,
        });
        for statement in &mut block.stmts {
            if let Stmt::Macro(value) = statement {
                let name = value.mac.path.segments.last().unwrap().ident.to_string();
                if observer(&name) {
                    *statement = syn::parse_quote!({});
                } else if name == "spawn_worker" {
                    let expression = erase_spawn(&value.mac);
                    *statement = Stmt::Expr(expression, Some(Default::default()));
                }
            }
        }
        visit_mut::visit_block_mut(self, block);
    }

    fn visit_expr_mut(&mut self, expression: &mut Expr) {
        if let Expr::Macro(value) = expression {
            let name = value.mac.path.segments.last().unwrap().ident.to_string();
            if observer(&name) {
                *expression = syn::parse_quote!({});
            } else if name == "spawn_worker" {
                *expression = erase_spawn(&value.mac);
            }
        }
        visit_mut::visit_expr_mut(self, expression);
    }
}

fn erase_spawn(mac: &syn::Macro) -> Expr {
    let arguments = mac
        .parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        .expect("spawn_worker must have a literal label and one closure");
    assert_eq!(arguments.len(), 2);
    let Expr::Closure(closure) = &arguments[1] else {
        panic!("spawn_worker requires a closure")
    };
    let body = &closure.body;
    syn::parse_quote!(::std::thread::spawn(move || #body))
}

fn files(directory: &Path, result: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && path.file_name().is_some_and(|name| name != "__pycache__") {
            files(&path, result);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            result.push(path);
        }
    }
}

pub fn emit(root: &Path, output: &Path, features: &[String], source_inputs: &[serde_json::Value]) {
    let mut paths = Vec::new();
    files(&root.join("src"), &mut paths);
    paths.sort();
    let mut semantic = Sha256::new();
    let mut source = Sha256::new();
    let mut inventory = Vec::new();
    for path in paths {
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let basename = path.file_name().unwrap().to_str().unwrap();
        if basename.contains("tests")
            || matches!(
                basename,
                "observability.rs" | "diagnostic_capture.rs" | "replay.rs" | "timing.rs"
            )
        {
            continue;
        }
        let content = fs::read_to_string(&path).unwrap();
        let mut tree = syn::parse_file(&content).unwrap_or_else(|error| panic!("{name}: {error}"));
        Product.visit_file_mut(&mut tree);
        let canonical = tree.into_token_stream().to_string();
        source.update(name.as_bytes());
        source.update(content.as_bytes());
        semantic.update(name.as_bytes());
        semantic.update(canonical.as_bytes());
        inventory.push(serde_json::json!({"path":name, "source_sha256":format!("{:x}",Sha256::digest(content.as_bytes())),
            "product_sha256":format!("{:x}",Sha256::digest(canonical.as_bytes()))}));
    }
    let profile = if features.iter().any(|feature| feature == "release-minimal") {
        "release-minimal"
    } else if features.iter().any(|feature| feature == "diagnostic-build") {
        "diagnostic-build"
    } else if features.iter().any(|feature| feature == "observability") {
        "release-observable"
    } else {
        "unverified-product-core"
    };
    let manifest = serde_json::json!({"schema_version":1,"artifact_profile":profile,"features":features,
        "actual_lock_sha256":format!("{:x}", Sha256::digest(fs::read(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("Cargo.lock")).unwrap())),
        "source_sha256":format!("{:x}",source.finalize()),"product_semantic_sha256":format!("{:x}",semantic.finalize()),
        "canonicalization":"syn-2 token stream; test and observer cfg removed; observer macros erased; product spawn retained; no algorithm rewriting",
        "rust_target":std::env::var("TARGET").unwrap(),"optimization_profile":std::env::var("PROFILE").unwrap(),
        "files":inventory,"source_inputs":source_inputs});
    fs::write(
        output.join("artifact-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}
