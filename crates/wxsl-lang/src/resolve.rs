//! Import resolution: many modules in, one flat module out.
//!
//! Each imported module's declarations are renamed to a mangled form that
//! encodes where they came from, references to them are rewritten, and
//! everything is concatenated with dependencies first. What is left has no
//! imports, so it can be emitted as WGSL.
//!
//! # The part that is not mechanical
//!
//! Rewriting references needs to respect **shadowing**. In
//!
//! ```wxsl
//! import package::wxsl::bindings::camera;
//!
//! fn f(camera: Camera) -> vec3f {
//!     return camera.position;
//! }
//! ```
//!
//! the `camera` in the body is the parameter, not the import, and rewriting
//! it to the mangled global would silently change what the shader computes.
//! So the rewriter carries a scope stack: parameters and `let`/`var`/`const`
//! locals shadow module-scope names for the rest of their block.
//!
//! # What is not renamed
//!
//! Member accesses (`v.x`, `surface.roughness`) are field names, not
//! references to declarations. Attribute arguments are left alone too:
//! `@builtin(position)` names a WGSL builtin, and by the time resolution
//! runs, `@if` and `@macro` are already gone
//! ([`crate::cond`] runs first).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::cond::{self, Bindings};
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::parse::parse;

/// The module sources available to a compilation.
///
/// The application fills this — the compiler reads nothing from disk, which
/// is the same arrangement `wxsl-render`'s `ShaderLibrary` already had
/// ([ADR 0009](../../../docs/adr/0009-the-application-supplies-the-shader-library.md)).
#[derive(Clone, Debug, Default)]
pub struct Modules {
    sources: BTreeMap<String, String>,
}

impl Modules {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace one module's source. A later insert wins, which is how
    /// an application substitutes a library module.
    pub fn insert(&mut self, path: impl Into<String>, source: impl Into<String>) {
        self.sources.insert(path.into(), source.into());
    }

    /// Add several.
    pub fn insert_all<P: Into<String>, S: Into<String>>(
        &mut self,
        entries: impl IntoIterator<Item = (P, S)>,
    ) {
        for (path, source) in entries {
            self.insert(path, source);
        }
    }

    /// One module's source.
    pub fn get(&self, path: &str) -> Option<&str> {
        self.sources.get(path).map(String::as_str)
    }

    /// Whether `path` is present.
    pub fn contains(&self, path: &str) -> bool {
        self.sources.contains_key(path)
    }

    /// How many modules.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether nothing is present.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Every module path, in order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.sources.keys().map(String::as_str)
    }
}

/// The name an imported declaration takes in the flattened output.
///
/// `package::math::remap` + `remap` becomes `package_math_remap_remap`.
/// Verbose on purpose: the mangled name appears in shader diagnostics and in
/// captured GPU frames, and being able to read the origin off it is worth
/// more than brevity.
pub fn mangle(module_path: &str, name: &str) -> String {
    format!("{}_{name}", module_path.replace("::", "_"))
}

/// Resolve `root` and everything it imports into one module.
///
/// `bindings` are the macro values to apply. Conditional translation runs on
/// each module as it is parsed, *before* any renaming: an `@if` names macros
/// in the module's own vocabulary, and a dropped branch should never have
/// its references resolved at all. Keeping that ordering here rather than in
/// the caller is deliberate — it is an invariant of the pipeline, not a
/// choice.
pub fn resolve(modules: &Modules, root: &str, bindings: &Bindings) -> Result<Module, Diagnostics> {
    let mut resolver = Resolver {
        modules,
        bindings,
        parsed: HashMap::new(),
        order: Vec::new(),
        visiting: Vec::new(),
        diagnostics: Diagnostics::new(),
    };

    resolver.load(root, None);
    if resolver.diagnostics.has_errors() {
        return Err(resolver.diagnostics);
    }

    // Dependencies first, root last: WGSL does not require declaration
    // before use, but emitting in dependency order keeps the output
    // readable and matches what a person would write.
    let mut out = Module::default();
    let mut seen_directives = BTreeSet::new();

    for path in resolver.order.clone() {
        let module = resolver.parsed.get(&path).expect("loaded").clone();
        let is_root = path == root;
        let renamed = resolver.rewrite(&module, &path, is_root);

        for directive in renamed.directives {
            // `enable f16;` from two modules is one directive in the output.
            let key = format!("{:?}{:?}", directive.kind, directive.names);
            if seen_directives.insert(key) {
                out.directives.push(directive);
            }
        }
        out.declarations.extend(renamed.declarations);
    }

    if resolver.diagnostics.has_errors() {
        return Err(resolver.diagnostics);
    }

    // Everything the root does not reach is dead weight in the output and,
    // worse, can fail to compile for reasons the author never sees. Drop it.
    let roots: Vec<String> = resolver
        .parsed
        .get(root)
        .expect("loaded")
        .declarations
        .iter()
        .filter_map(|declaration| declaration.name().map(str::to_string))
        .collect();
    retain_reachable(&mut out, &roots);
    Ok(out)
}

struct Resolver<'a> {
    modules: &'a Modules,
    bindings: &'a Bindings,
    parsed: HashMap<String, Module>,
    /// Post-order: a module appears after everything it imports.
    order: Vec<String>,
    /// The current import chain, for cycle reporting.
    visiting: Vec<String>,
    diagnostics: Diagnostics,
}

impl Resolver<'_> {
    fn load(&mut self, path: &str, imported_from: Option<(&str, crate::span::Span)>) {
        if self.parsed.contains_key(path) {
            return;
        }
        if self.visiting.iter().any(|seen| seen == path) {
            let chain = self
                .visiting
                .iter()
                .map(String::as_str)
                .chain([path])
                .collect::<Vec<_>>()
                .join(" -> ");
            let mut diagnostic = Diagnostic::error(
                format!("import cycle: {chain}"),
                imported_from.map(|(_, span)| span).unwrap_or_default(),
            );
            if let Some((from, _)) = imported_from {
                diagnostic = diagnostic.in_module(from);
            }
            self.diagnostics.push(diagnostic);
            return;
        }

        let Some(source) = self.modules.get(path) else {
            let mut diagnostic = Diagnostic::error(
                format!("no module `{path}`"),
                imported_from.map(|(_, span)| span).unwrap_or_default(),
            )
            .with_note("the application supplies the module set; this path is not in it");
            if let Some((from, _)) = imported_from {
                diagnostic = diagnostic.in_module(from);
            }
            self.diagnostics.push(diagnostic);
            return;
        };

        let mut module = match parse(source) {
            Ok(module) => module,
            Err(diagnostics) => {
                for diagnostic in diagnostics.into_vec() {
                    self.diagnostics.push(diagnostic.or_module(path));
                }
                return;
            }
        };

        if let Err(diagnostics) = cond::apply(&mut module, self.bindings) {
            for diagnostic in diagnostics.into_vec() {
                self.diagnostics.push(diagnostic.or_module(path));
            }
            return;
        }

        self.visiting.push(path.to_string());
        for import in &module.imports {
            let target = import.path.to_string();
            self.load(&target, Some((path, import.span)));
        }
        self.visiting.pop();

        self.parsed.insert(path.to_string(), module);
        self.order.push(path.to_string());
    }

    /// Rename `module`'s declarations and rewrite its references.
    fn rewrite(&mut self, module: &Module, path: &str, is_root: bool) -> Module {
        // local name -> final name
        let mut names: HashMap<String, String> = HashMap::new();

        for declaration in &module.declarations {
            if let Some(name) = declaration.name() {
                let final_name = if is_root {
                    // Root declarations keep their names: entry points are
                    // looked up by name by the renderer.
                    name.to_string()
                } else {
                    mangle(path, name)
                };
                names.insert(name.to_string(), final_name);
            }
        }

        for import in &module.imports {
            let target = import.path.to_string();
            for item in &import.items {
                if !self.parsed.contains_key(&target) {
                    // Already reported by `load`.
                    continue;
                }
                let source_module = &self.parsed[&target];
                if source_module.declaration(&item.name.node).is_none() {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("`{target}` has no `{}`", item.name.node),
                            item.name.span,
                        )
                        .in_module(path),
                    );
                    continue;
                }
                names.insert(item.local.clone(), mangle(&target, &item.name.node));
            }
        }

        let mut rewriter = Rewriter {
            names: &names,
            scopes: Vec::new(),
        };
        let mut out = module.clone();
        out.imports.clear();
        for declaration in &mut out.declarations {
            rewriter.declaration(declaration);
        }
        out
    }
}

/// Rewrites references, respecting shadowing.
struct Rewriter<'a> {
    names: &'a HashMap<String, String>,
    scopes: Vec<HashSet<String>>,
}

impl Rewriter<'_> {
    fn shadowed(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope| scope.contains(name))
    }

    fn bind(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }
    }

    fn rename(&self, name: &mut String) {
        if self.shadowed(name) {
            return;
        }
        if let Some(final_name) = self.names.get(name.as_str()) {
            *name = final_name.clone();
        }
    }

    fn declaration(&mut self, declaration: &mut Declaration) {
        match declaration {
            Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
                // The declared name itself is renamed from the table, but a
                // reference inside the initializer must not be shadowed by
                // it, so rename the name last.
                if let Some(ty) = &mut value.ty {
                    self.type_expr(ty);
                }
                for arg in &mut value.address_space {
                    self.expr(&mut arg.expr);
                }
                if let Some(init) = &mut value.init {
                    self.expr(init);
                }
                self.rename(&mut value.name.node);
            }
            Declaration::Alias(alias) => {
                self.type_expr(&mut alias.ty);
                self.rename(&mut alias.name.node);
            }
            Declaration::Struct(item) => {
                for member in &mut item.members {
                    self.type_expr(&mut member.ty);
                }
                self.rename(&mut item.name.node);
            }
            Declaration::Function(function) => {
                self.scopes.push(HashSet::new());
                for param in &mut function.params {
                    self.type_expr(&mut param.ty);
                }
                // Parameters shadow module scope inside the body.
                for param in &function.params {
                    let name = param.name.node.clone();
                    self.bind(&name);
                }
                if let Some(ty) = &mut function.return_type {
                    self.type_expr(ty);
                }
                self.block(&mut function.body);
                self.scopes.pop();
                self.rename(&mut function.name.node);
            }
            Declaration::ConstAssert(assert) => self.expr(&mut assert.expr),
        }
    }

    fn block(&mut self, block: &mut Block) {
        self.scopes.push(HashSet::new());
        for statement in &mut block.statements {
            self.statement(statement);
        }
        self.scopes.pop();
    }

    fn statement(&mut self, statement: &mut Statement) {
        match statement {
            Statement::Local(local) => {
                if let Some(ty) = &mut local.ty {
                    self.type_expr(ty);
                }
                for arg in &mut local.address_space {
                    self.expr(&mut arg.expr);
                }
                if let Some(init) = &mut local.init {
                    self.expr(init);
                }
                // Bound *after* the initializer: `let x = x;` refers to the
                // outer `x` on the right.
                let name = local.name.node.clone();
                self.bind(&name);
            }
            Statement::Assign(assign) => {
                if let Some(target) = &mut assign.target {
                    self.expr(target);
                }
                self.expr(&mut assign.value);
            }
            Statement::Step(step) => self.expr(&mut step.target),
            Statement::Block(block) => self.block(block),
            Statement::If(item) => {
                for (condition, body) in &mut item.arms {
                    self.expr(condition);
                    self.block(body);
                }
                if let Some(body) = &mut item.otherwise {
                    self.block(body);
                }
            }
            Statement::Switch(item) => {
                self.expr(&mut item.selector);
                for clause in &mut item.clauses {
                    for selector in &mut clause.selectors {
                        if let CaseSelector::Value(expr) = selector {
                            self.expr(expr);
                        }
                    }
                    self.block(&mut clause.body);
                }
            }
            Statement::Loop(item) => {
                self.block(&mut item.body);
                if let Some(continuing) = &mut item.continuing {
                    self.block(&mut continuing.body);
                    if let Some(condition) = &mut continuing.break_if {
                        self.expr(condition);
                    }
                }
            }
            Statement::For(item) => {
                // The init's binding is visible in the condition, the update
                // and the body, so they share one scope.
                self.scopes.push(HashSet::new());
                if let Some(init) = &mut item.init {
                    self.statement(init);
                }
                if let Some(condition) = &mut item.condition {
                    self.expr(condition);
                }
                if let Some(update) = &mut item.update {
                    self.statement(update);
                }
                self.block(&mut item.body);
                self.scopes.pop();
            }
            Statement::While(item) => {
                self.expr(&mut item.condition);
                self.block(&mut item.body);
            }
            Statement::Return(item) => {
                if let Some(value) = &mut item.value {
                    self.expr(value);
                }
            }
            Statement::Call(item) => self.expr(&mut item.call),
            Statement::ConstAssert(item) => self.expr(&mut item.expr),
            Statement::Break(_) | Statement::Continue(_) | Statement::Discard(_) => {}
        }
    }

    fn type_expr(&mut self, ty: &mut TypeExpr) {
        self.rename(&mut ty.name.node);
        for arg in &mut ty.template_args {
            self.expr(&mut arg.expr);
        }
    }

    fn expr(&mut self, expr: &mut Expr) {
        match expr {
            Expr::Literal(_) => {}
            Expr::Name(name) => {
                self.rename(&mut name.name.node);
                for arg in &mut name.template_args {
                    self.expr(&mut arg.expr);
                }
            }
            Expr::Unary(unary) => self.expr(&mut unary.operand),
            Expr::Binary(binary) => {
                self.expr(&mut binary.left);
                self.expr(&mut binary.right);
            }
            Expr::Call(call) => {
                self.rename(&mut call.callee.name.node);
                for arg in &mut call.callee.template_args {
                    self.expr(&mut arg.expr);
                }
                for arg in &mut call.args {
                    self.expr(arg);
                }
            }
            Expr::Index(index) => {
                self.expr(&mut index.base);
                self.expr(&mut index.index);
            }
            // The member name is a field, not a reference to a declaration.
            Expr::Member(member) => self.expr(&mut member.base),
            Expr::Paren(paren) => self.expr(&mut paren.inner),
        }
    }
}

/// Drop declarations nothing in `roots` reaches, transitively.
fn retain_reachable(module: &mut Module, roots: &[String]) {
    let mut wanted: HashSet<String> = roots.iter().cloned().collect();
    // Repeat until nothing new is pulled in: a kept declaration's references
    // become wanted too.
    loop {
        let mut added = false;
        for declaration in &module.declarations {
            let Some(name) = declaration.name() else {
                continue;
            };
            if !wanted.contains(name) {
                continue;
            }
            let mut referenced = Vec::new();
            collect_references(declaration, &mut referenced);
            for reference in referenced {
                if wanted.insert(reference) {
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    module
        .declarations
        .retain(|declaration| match declaration.name() {
            // A `const_assert` has no name and is always kept: it is an
            // assertion about the module, not a definition anything refers to.
            None => true,
            Some(name) => wanted.contains(name),
        });
}

/// Every name a declaration mentions.
fn collect_references(declaration: &Declaration, out: &mut Vec<String>) {
    let mut walker = ReferenceWalker { out };
    walker.declaration(declaration);
}

struct ReferenceWalker<'a> {
    out: &'a mut Vec<String>,
}

impl ReferenceWalker<'_> {
    fn declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
                if let Some(ty) = &value.ty {
                    self.type_expr(ty);
                }
                if let Some(init) = &value.init {
                    self.expr(init);
                }
            }
            Declaration::Alias(alias) => self.type_expr(&alias.ty),
            Declaration::Struct(item) => {
                for member in &item.members {
                    self.type_expr(&member.ty);
                }
            }
            Declaration::Function(function) => {
                for param in &function.params {
                    self.type_expr(&param.ty);
                }
                if let Some(ty) = &function.return_type {
                    self.type_expr(ty);
                }
                self.block(&function.body);
            }
            Declaration::ConstAssert(assert) => self.expr(&assert.expr),
        }
    }

    fn block(&mut self, block: &Block) {
        for statement in &block.statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Local(local) => {
                if let Some(ty) = &local.ty {
                    self.type_expr(ty);
                }
                if let Some(init) = &local.init {
                    self.expr(init);
                }
            }
            Statement::Assign(assign) => {
                if let Some(target) = &assign.target {
                    self.expr(target);
                }
                self.expr(&assign.value);
            }
            Statement::Step(step) => self.expr(&step.target),
            Statement::Block(block) => self.block(block),
            Statement::If(item) => {
                for (condition, body) in &item.arms {
                    self.expr(condition);
                    self.block(body);
                }
                if let Some(body) = &item.otherwise {
                    self.block(body);
                }
            }
            Statement::Switch(item) => {
                self.expr(&item.selector);
                for clause in &item.clauses {
                    for selector in &clause.selectors {
                        if let CaseSelector::Value(expr) = selector {
                            self.expr(expr);
                        }
                    }
                    self.block(&clause.body);
                }
            }
            Statement::Loop(item) => {
                self.block(&item.body);
                if let Some(continuing) = &item.continuing {
                    self.block(&continuing.body);
                    if let Some(condition) = &continuing.break_if {
                        self.expr(condition);
                    }
                }
            }
            Statement::For(item) => {
                if let Some(init) = &item.init {
                    self.statement(init);
                }
                if let Some(condition) = &item.condition {
                    self.expr(condition);
                }
                if let Some(update) = &item.update {
                    self.statement(update);
                }
                self.block(&item.body);
            }
            Statement::While(item) => {
                self.expr(&item.condition);
                self.block(&item.body);
            }
            Statement::Return(item) => {
                if let Some(value) = &item.value {
                    self.expr(value);
                }
            }
            Statement::Call(item) => self.expr(&item.call),
            Statement::ConstAssert(item) => self.expr(&item.expr),
            Statement::Break(_) | Statement::Continue(_) | Statement::Discard(_) => {}
        }
    }

    fn type_expr(&mut self, ty: &TypeExpr) {
        self.out.push(ty.name.node.clone());
        for arg in &ty.template_args {
            self.expr(&arg.expr);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal(_) => {}
            Expr::Name(name) => {
                self.out.push(name.name.node.clone());
                for arg in &name.template_args {
                    self.expr(&arg.expr);
                }
            }
            Expr::Unary(unary) => self.expr(&unary.operand),
            Expr::Binary(binary) => {
                self.expr(&binary.left);
                self.expr(&binary.right);
            }
            Expr::Call(call) => {
                self.out.push(call.callee.name.node.clone());
                for arg in &call.callee.template_args {
                    self.expr(&arg.expr);
                }
                for arg in &call.args {
                    self.expr(arg);
                }
            }
            Expr::Index(index) => {
                self.expr(&index.base);
                self.expr(&index.index);
            }
            Expr::Member(member) => self.expr(&member.base),
            Expr::Paren(paren) => self.expr(&paren.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::emit;

    fn resolve_for_test(modules: &Modules, root: &str) -> Result<Module, Diagnostics> {
        resolve(modules, root, &Bindings::new())
    }

    fn modules(entries: &[(&str, &str)]) -> Modules {
        let mut modules = Modules::new();
        modules.insert_all(entries.iter().copied());
        modules
    }

    fn flatten(entries: &[(&str, &str)], root: &str) -> String {
        let module = match resolve(&modules(entries), root, &Bindings::new()) {
            Ok(module) => module,
            Err(diagnostics) => panic!(
                "{}",
                diagnostics.render(&|path| modules(entries).get(path).map(str::to_string))
            ),
        };
        emit(&module)
    }

    #[test]
    fn an_import_is_inlined_and_mangled() {
        let out = flatten(
            &[
                (
                    "package::math::double",
                    "fn double(x: f32) -> f32 { return x * 2.0; }\n",
                ),
                (
                    "package::main",
                    "import package::math::double::double;\n\n@fragment\nfn fs() -> @location(0) vec4f {\n    return vec4f(double(0.5));\n}\n",
                ),
            ],
            "package::main",
        );
        // The dependency is renamed by origin, the call site follows it, and
        // the root's own entry point keeps its name.
        assert!(
            out.contains("fn package_math_double_double(x: f32)"),
            "{out}"
        );
        assert!(out.contains("package_math_double_double(0.5)"), "{out}");
        assert!(out.contains("fn fs()"), "{out}");
        assert!(!out.contains("import"), "{out}");
    }

    #[test]
    fn a_local_binding_shadows_an_import() {
        // The bug this guards: rewriting the parameter's uses to the mangled
        // global would change what the shader computes, silently.
        let out = flatten(
            &[
                (
                    "package::bind",
                    "struct Camera { position: vec3f }\nvar<uniform> camera: Camera;\n",
                ),
                (
                    "package::main",
                    "import package::bind::{camera, Camera};\n\n\
                     fn from_global() -> vec3f { return camera.position; }\n\n\
                     fn from_param(camera: Camera) -> vec3f { return camera.position; }\n\n\
                     fn from_local() -> vec3f {\n    let camera = Camera(vec3f(1.0));\n    return camera.position;\n}\n",
                ),
            ],
            "package::main",
        );
        assert!(
            out.contains("fn from_global() -> vec3f {\n    return package_bind_camera.position;"),
            "the global reference should be mangled:\n{out}"
        );
        assert!(
            out.contains("fn from_param(camera: package_bind_Camera) -> vec3f {\n    return camera.position;"),
            "the parameter must not be rewritten:\n{out}"
        );
        assert!(
            out.contains(
                "let camera = package_bind_Camera(vec3f(1.0));\n    return camera.position;"
            ),
            "the local must not be rewritten:\n{out}"
        );
    }

    #[test]
    fn shadowing_ends_with_its_scope() {
        let out = flatten(
            &[
                ("package::bind", "var<uniform> value: f32;\n"),
                (
                    "package::main",
                    "import package::bind::value;\n\n\
                     fn f() -> f32 {\n    \
                     if true {\n        let value = 1.0;\n        _ = value;\n    }\n    \
                     return value;\n}\n",
                ),
            ],
            "package::main",
        );
        assert!(out.contains("let value = 1.0;"), "{out}");
        assert!(out.contains("_ = value;"), "{out}");
        // Outside the `if`, `value` is the import again.
        assert!(out.contains("return package_bind_value;"), "{out}");
    }

    #[test]
    fn transitive_imports_come_out_in_dependency_order() {
        let out = flatten(
            &[
                ("package::a", "fn a() -> f32 { return 1.0; }\n"),
                (
                    "package::b",
                    "import package::a::a;\nfn b() -> f32 { return a() * 2.0; }\n",
                ),
                (
                    "package::main",
                    "import package::b::b;\nfn main_fn() -> f32 { return b(); }\n",
                ),
            ],
            "package::main",
        );
        let a = out.find("fn package_a_a").expect("a is present");
        let b = out.find("fn package_b_b").expect("b is present");
        let main = out.find("fn main_fn").expect("main is present");
        assert!(a < b && b < main, "not in dependency order:\n{out}");
        assert!(out.contains("return package_a_a() * 2.0;"), "{out}");
    }

    #[test]
    fn an_import_used_by_nobody_is_dropped() {
        let out = flatten(
            &[
                (
                    "package::lib",
                    "fn used() -> f32 { return 1.0; }\nfn unused() -> f32 { return 2.0; }\n",
                ),
                (
                    "package::main",
                    "import package::lib::{used, unused};\nfn main_fn() -> f32 { return used(); }\n",
                ),
            ],
            "package::main",
        );
        assert!(out.contains("package_lib_used"), "{out}");
        assert!(
            !out.contains("package_lib_unused"),
            "dead code should be stripped:\n{out}"
        );
    }

    #[test]
    fn a_struct_a_function_returns_is_kept() {
        // Reachability has to follow types, not just calls.
        let out = flatten(
            &[
                ("package::lib", "struct Sample { value: f32 }\nstruct Ignored { value: f32 }\n"),
                (
                    "package::main",
                    "import package::lib::{Sample, Ignored};\nfn f() -> Sample { return Sample(1.0); }\n",
                ),
            ],
            "package::main",
        );
        assert!(out.contains("struct package_lib_Sample"), "{out}");
        assert!(!out.contains("package_lib_Ignored"), "{out}");
    }

    #[test]
    fn renaming_on_import_works() {
        let out = flatten(
            &[
                ("package::lib", "fn original() -> f32 { return 1.0; }\n"),
                (
                    "package::main",
                    "import package::lib::original as renamed;\nfn f() -> f32 { return renamed(); }\n",
                ),
            ],
            "package::main",
        );
        assert!(out.contains("package_lib_original()"), "{out}");
    }

    #[test]
    fn a_missing_module_is_reported_with_the_importer() {
        let error = resolve_for_test(
            &modules(&[(
                "package::main",
                "import package::gone::thing;\nfn f() { }\n",
            )]),
            "package::main",
        )
        .expect_err("missing module");
        let rendered = error.to_string();
        assert!(rendered.contains("no module `package::gone`"), "{rendered}");
    }

    #[test]
    fn a_missing_item_is_reported() {
        let error = resolve_for_test(
            &modules(&[
                ("package::lib", "fn present() { }\n"),
                (
                    "package::main",
                    "import package::lib::absent;\nfn f() { }\n",
                ),
            ]),
            "package::main",
        )
        .expect_err("missing item");
        assert!(error.to_string().contains("has no `absent`"), "{error}");
    }

    #[test]
    fn an_import_cycle_is_reported_rather_than_hanging() {
        let error = resolve_for_test(
            &modules(&[
                ("package::a", "import package::b::b;\nfn a() { }\n"),
                ("package::b", "import package::a::a;\nfn b() { }\n"),
            ]),
            "package::a",
        )
        .expect_err("cycle");
        assert!(error.to_string().contains("import cycle"), "{error}");
    }

    #[test]
    fn a_parse_error_in_a_dependency_names_that_module() {
        let error = resolve_for_test(
            &modules(&[
                ("package::lib", "fn broken( { }\n"),
                (
                    "package::main",
                    "import package::lib::broken;\nfn f() { }\n",
                ),
            ]),
            "package::main",
        )
        .expect_err("bad dependency");
        let rendered = error.render(&|path| {
            modules(&[("package::lib", "fn broken( { }\n")])
                .get(path)
                .map(str::to_string)
        });
        assert!(rendered.contains("package::lib"), "{rendered}");
    }

    #[test]
    fn mangling_encodes_the_origin() {
        assert_eq!(
            mangle("package::lighting::pbr_direct", "pbr_direct"),
            "package_lighting_pbr_direct_pbr_direct"
        );
    }
}
