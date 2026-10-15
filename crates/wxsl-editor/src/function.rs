//! Functions written in the editor (plan3 M11): "write a function, get a
//! node".
//!
//! A buffer holds one `.wxsl` node source, by the same authoring rules as
//! a file under `wxsl-stdlib/shaders/` (ADR 0020) — one `fn`, a label line
//! and a documentation paragraph, one parameter per line with its
//! `@default`. Saving it runs the derivation the build script runs,
//! [`wxsl_lang::node_from_source`], at runtime: the module is mounted at
//! `package::user::<fn name>`, so the node is `user.<fn name>` and the
//! palette shows it under `user` in the same session. Nothing is written
//! to disk; the buffers live as long as the editor.
//!
//! [`derive()`] is the device-free half — parse, derive, resolve against the
//! library — and answers with the node or with the compiler's diagnostics
//! placed on their own lines. [`validate()`] is the device half: the WGSL
//! the module compiles to must validate, or the save is refused.

use wxsl_core::node::NodeDefinition;
use wxsl_lang::ast::Declaration;
use wxsl_lang::{Diagnostics, Span};
use wxsl_render::library::ShaderLibrary;

use crate::buffer::TextBuffer;
use crate::highlight::{highlight, Run};
use crate::ui::LineMark;

/// The package every function written in the editor is mounted under.
pub const PACKAGE: &str = "user";

/// What a new buffer starts as: a complete, savable node source, so the
/// first save already shows the whole loop.
pub const TEMPLATE: &str = "\
// Tint
//
// Multiply a colour by a tint, then lift its blacks. Written in the
// editor: save it (Ctrl+S) and it is a node in the palette, under `user`.

fn tint(
    color: vec3f, // The colour to tint. @default 1.0, 1.0, 1.0
    tint: vec3f,  // What to multiply it by. @default 1.0, 0.55, 0.3
    lift: f32,    // Added to every channel afterwards. @default 0.05
) -> vec3f {
    return color * tint + vec3f(lift);
}
";

/// One function buffer: its text, what the last save said about it, and
/// the highlighting of its current text.
#[derive(Clone, Debug)]
pub struct FunctionSource {
    /// The text being edited.
    pub buffer: TextBuffer,
    /// The last save's diagnostics, on their lines; empty when it took.
    pub marks: Vec<LineMark>,
    /// The node id the last successful save registered.
    pub node: Option<String>,
    /// The text's revision at the last save, for "unsaved changes".
    saved: Option<u64>,
    highlight: (u64, Vec<Run>),
}

impl FunctionSource {
    /// A buffer over `text`.
    pub fn new(text: &str) -> Self {
        let buffer = TextBuffer::new(text);
        let runs = highlight(buffer.text());
        FunctionSource {
            highlight: (buffer.revision(), runs),
            buffer,
            marks: Vec::new(),
            node: None,
            saved: None,
        }
    }

    /// The highlighting of the current text, recomputed only when it
    /// changed.
    pub fn runs(&mut self) -> &[Run] {
        if self.highlight.0 != self.buffer.revision() {
            self.highlight = (self.buffer.revision(), highlight(self.buffer.text()));
        }
        &self.highlight.1
    }

    /// What the buffer is called in its list: the function it declares,
    /// once it has been saved as one.
    pub fn name(&self) -> String {
        match &self.node {
            Some(node) => node.clone(),
            None => function_name(self.buffer.text()).unwrap_or_else(|| "untitled".to_string()),
        }
    }

    /// Whether the text changed since it last saved cleanly.
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.buffer.revision())
    }

    /// Record a save of the current text: the node it became, or why not.
    pub fn record(&mut self, result: Result<String, Vec<LineMark>>) {
        match result {
            Ok(node) => {
                self.node = Some(node);
                self.marks.clear();
                self.saved = Some(self.buffer.revision());
            }
            Err(marks) => self.marks = marks,
        }
    }
}

/// Every function buffer the editor holds, and which one is showing.
#[derive(Clone, Debug)]
pub struct Functions {
    /// The buffers, in the order they were opened.
    pub buffers: Vec<FunctionSource>,
    /// Which one the panel shows.
    pub current: usize,
}

impl Default for Functions {
    fn default() -> Self {
        Functions {
            buffers: vec![FunctionSource::new(TEMPLATE)],
            current: 0,
        }
    }
}

impl Functions {
    /// The buffer the panel shows.
    pub fn current(&self) -> &FunctionSource {
        &self.buffers[self.current]
    }

    /// The buffer the panel shows, to edit.
    pub fn current_mut(&mut self) -> &mut FunctionSource {
        &mut self.buffers[self.current]
    }

    /// Open a new buffer on the template and show it.
    pub fn open_new(&mut self) {
        self.buffers.push(FunctionSource::new(TEMPLATE));
        self.current = self.buffers.len() - 1;
    }
}

/// The name of the first function `source` declares, if it parses that
/// far.
pub fn function_name(source: &str) -> Option<String> {
    wxsl_lang::parse(source)
        .ok()?
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Function(function) => Some(function.name.node.clone()),
            _ => None,
        })
}

/// The module a function named `name` is mounted at.
pub fn module_path(name: &str) -> String {
    format!("package::{PACKAGE}::{name}")
}

/// Derive the node `source` describes, mounted at
/// `package::user::<fn name>` and resolved against `library` — or the
/// diagnostics that stop it, each on the line it is about.
///
/// A syntax error is the parser's span; a broken authoring rule (a
/// missing `@default`, a second `fn`) is the derivation's; an import that
/// does not resolve is the resolver's. A diagnostic about another module
/// is pinned to the first line, with that module named.
pub fn derive(
    source: &str,
    library: &ShaderLibrary,
) -> Result<(String, NodeDefinition), Vec<LineMark>> {
    let parsed = wxsl_lang::parse(source).map_err(|errors| marks(source, None, &errors))?;
    let name = parsed
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Function(function) => Some(function.name.node.clone()),
            _ => None,
        })
        // The derivation says why with the right words.
        .unwrap_or_else(|| "function".to_string());
    let path = module_path(&name);
    let definition = wxsl_lang::node_from_source(source, &path)
        .map_err(|errors| marks(source, Some(&path), &errors))?;

    let mut modules = wxsl_lang::Modules::new();
    for (module, text) in library.iter() {
        modules.insert(module, text);
    }
    modules.insert(&path, source);
    wxsl_lang::compile_to_wxsl(&modules, &path, &wxsl_lang::Bindings::new())
        .map_err(|errors| marks(source, Some(&path), &errors))?;
    Ok((path, definition))
}

/// The device half of a save: compile the module on its own to WGSL and
/// have `device` validate it. The WXSL compiler does not type-check a
/// body, so without this a function returning the wrong type saves
/// cleanly and only fails once a graph uses it. A templated function has
/// no WGSL until a node instantiates it, so it is checked then.
pub fn validate(
    device: &wgpu::Device,
    path: &str,
    source: &str,
    library: &ShaderLibrary,
) -> Result<(), Vec<LineMark>> {
    let mark = |message: String| {
        vec![LineMark {
            line: 0,
            column: 0,
            message,
        }]
    };
    let wgsl = wxsl_render::variants::compile(
        library,
        &[(path, std::borrow::Cow::Borrowed(source))],
        path,
        &Default::default(),
    )
    .map_err(|error| mark(error.to_string()))?;
    wxsl_render::variants::shader_module(device, path, &wgsl)
        .map(drop)
        .map_err(|error| mark(error.to_string()))
}

/// Place `errors` on `source`'s lines. A diagnostic that names another
/// module than `module` goes on the first line, saying which.
fn marks(source: &str, module: Option<&str>, errors: &Diagnostics) -> Vec<LineMark> {
    let mut out: Vec<LineMark> = errors
        .iter()
        .map(|diagnostic| {
            let own = diagnostic.module.is_none() || diagnostic.module.as_deref() == module;
            if own {
                let (line, column) = place(source, diagnostic.span);
                LineMark {
                    line,
                    column,
                    message: diagnostic.message.clone(),
                }
            } else {
                LineMark {
                    line: 0,
                    column: 0,
                    message: format!(
                        "{}: {}",
                        diagnostic.module.as_deref().unwrap_or("?"),
                        diagnostic.message
                    ),
                }
            }
        })
        .collect();
    out.sort_by_key(|mark| (mark.line, mark.column));
    out
}

/// A span's 0-based line and character column in `source`.
fn place(source: &str, span: Span) -> (usize, usize) {
    let start = (span.start as usize).min(source.len());
    let span = Span::at(start as u32);
    let (line, column) = span.line_column(source);
    (line - 1, column - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_derives_a_user_node() {
        let (path, definition) =
            derive(TEMPLATE, &ShaderLibrary::new()).expect("the template is a node");
        assert_eq!(path, "package::user::tint");
        assert_eq!(definition.id, "user.tint");
        assert_eq!(definition.label, "Tint");
        assert_eq!(definition.inputs.len(), 3);
    }

    #[test]
    fn a_syntax_error_is_reported_on_its_own_line() {
        let broken = TEMPLATE.replace(
            "    return color * tint + vec3f(lift);",
            "    return color * tint +;",
        );
        let marks = derive(&broken, &ShaderLibrary::new()).expect_err("it does not parse");
        let line = broken
            .lines()
            .position(|line| line.contains("tint +;"))
            .expect("the broken line");
        assert_eq!(marks[0].line, line, "{marks:?}");
        assert!(marks[0].column > 0);
    }

    #[test]
    fn a_broken_authoring_rule_points_at_where_it_broke() {
        // One `fn` per node source: the second one is the line at fault.
        let twice = format!("{TEMPLATE}\nfn helper() -> f32 {{\n    return 1.0;\n}}\n");
        let marks = derive(&twice, &ShaderLibrary::new()).expect_err("two functions");
        let line = twice
            .lines()
            .position(|line| line.starts_with("fn helper"))
            .expect("the second function");
        assert_eq!(marks[0].line, line, "{marks:?}");
    }

    #[test]
    fn an_unresolved_import_is_an_error_before_the_preview_sees_it() {
        let importing = format!("import package::nowhere::thing;\n{TEMPLATE}");
        let marks = derive(&importing, &ShaderLibrary::new()).expect_err("no such module");
        assert_eq!(marks[0].line, 0, "{marks:?}");
    }
}
