//! Deriving a node definition from a `.wxsl` file.
//!
//! A shader function file *is* a node
//! ([ADR 0020](../../../docs/adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md)):
//! [`node_from_source`] parses one and answers with the
//! [`NodeDefinition`] the graph, the editor and codegen need. Nothing about
//! the interface is restated in Rust, so a signature and its node cannot
//! drift apart.
//!
//! # What comes from where
//!
//! | In the file | Becomes |
//! |---|---|
//! | the module path (from the file's place in the tree) | the node id and category — `package::math::safe_normalize` is `math.safe_normalize` |
//! | `fn safe_normalize<T: vec2f \| vec3f \| vec4f>(…)` | one [`GenericParam`] per type parameter, its bound list *is* [`GenericParam::allowed`] |
//! | each parameter | one input [`Socket`], in call order |
//! | the return type | one output socket named `out`, or one per field when it is a struct declared in the same file |
//! | `@macro const WXSL_FBM_OCTAVES: i32 = 5;` | a [`MacroDef`] with that default |
//! | the first line of the leading comment block | the label |
//! | the paragraph after it | the documentation |
//! | `// … @default 2.0` after a parameter | that socket's doc and default |
//!
//! # Why the metadata is in comments
//!
//! Real attributes (`@label`, `@default`) would touch the lexer, the grammar
//! and the emitter, for metadata the compiler has no use for — it would be
//! carried through every pass only to be ignored. A comment convention keeps
//! node metadata out of the language it is not part of, and costs nothing at
//! compile time.
//!
//! # The rules a node source must follow
//!
//! * exactly one `fn`, and it is not an entry point;
//! * every type in its signature is one the graph can carry
//!   ([`ValueType`], including the texture and sampler types) or one of
//!   its own type parameters;
//! * every type parameter is bounded, because an unbounded one has no
//!   allowed set to offer;
//! * the leading comment block has a label line and a doc paragraph.
//!
//! Everything else is a diagnostic, pointing at the line responsible.

use wxsl_core::macros::{MacroDef, MacroValue};
use wxsl_core::node::{
    FunctionReturn, GenericParam, NodeDefinition, Socket, Value, ValueType, WxslFunction,
};

use crate::ast::{Declaration, Expr, Function, LiteralKind, Module, StructDecl, TypeExpr, UnaryOp};
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::span::Span;

/// Derive the node definition a `.wxsl` file describes.
///
/// `module_path` is the WXSL path the file is imported as
/// (`package::math::safe_normalize`); it is where the node's id, category
/// and the module half of its [`WxslFunction`] come from, because the file's
/// place in the tree is the only thing that names it.
///
/// Errors carry spans into `source`, so a caller with the text can render
/// them the way any other compile failure is rendered.
pub fn node_from_source(source: &str, module_path: &str) -> Result<NodeDefinition, Diagnostics> {
    let module = crate::parse(source)?;
    Derivation {
        source,
        module_path,
    }
    .run(&module)
}

/// One derivation in progress: the text and the path it is known by.
struct Derivation<'a> {
    source: &'a str,
    module_path: &'a str,
}

impl Derivation<'_> {
    fn run(&self, module: &Module) -> Result<NodeDefinition, Diagnostics> {
        let function = self.the_one_function(module)?;
        let id = self.node_id()?;
        let (label, doc) = self.label_and_doc()?;

        let generics = self.generic_params(function)?;
        let params = self.input_sockets(function, &generics)?;
        let ret = self.return_shape(module, function, &generics)?;

        let call = WxslFunction {
            module: wxsl_core::wxsl::ModulePath::new(self.module_path).ok_or_else(|| {
                self.at_start(format!("`{}` is not a module path", self.module_path))
            })?,
            name: wxsl_core::wxsl::WxslIdent::new(&function.name.node).ok_or_else(|| {
                self.error(
                    format!("`{}` is not a usable function name", function.name.node),
                    function.name.span,
                )
            })?,
            params,
            ret,
        };

        let mut builder = NodeDefinition::builder(id, label).doc(doc);
        for param in generics {
            builder = builder.generic_param(param);
        }
        for declaration in self.macro_defs(module)? {
            builder = builder.macro_var(declaration);
        }
        Ok(builder.call(call))
    }

    // -----------------------------------------------------------------
    // The function
    // -----------------------------------------------------------------

    /// The single function this file declares.
    ///
    /// One `fn` per file is the authoring rule the whole library already
    /// follows, and it is what makes a file nameable as a node: two
    /// functions would leave nothing to say which of them the module path
    /// refers to.
    fn the_one_function<'m>(&self, module: &'m Module) -> Result<&'m Function, Diagnostics> {
        let mut functions =
            module
                .declarations
                .iter()
                .filter_map(|declaration| match declaration {
                    Declaration::Function(function) => Some(function),
                    _ => None,
                });
        let Some(first) = functions.next() else {
            return Err(self.at_start("a node source must declare a function"));
        };
        if let Some(second) = functions.next() {
            return Err(Diagnostic::error(
                "a node source must declare exactly one function",
                second.span,
            )
            .in_module(self.module_path)
            .with_label(first.span, "the first one is here")
            .into());
        }
        if let Some(stage) = first.stage() {
            return Err(self.error(
                format!("a `@{}` entry point is not a node", stage.attribute()),
                first.span,
            ));
        }
        Ok(first)
    }

    // -----------------------------------------------------------------
    // Identity
    // -----------------------------------------------------------------

    /// The registry id: the module path without its package component, dotted.
    fn node_id(&self) -> Result<String, Diagnostics> {
        let components: Vec<&str> = self.module_path.split("::").collect();
        let tail = match components.split_first() {
            Some((first, rest)) if *first == "package" && !rest.is_empty() => rest,
            _ => &components[..],
        };
        if tail.len() < 2 {
            return Err(self.at_start(format!(
                "`{}` has no category component, so it cannot name a node",
                self.module_path
            )));
        }
        Ok(tail.join("."))
    }

    // -----------------------------------------------------------------
    // Sockets
    // -----------------------------------------------------------------

    fn generic_params(&self, function: &Function) -> Result<Vec<GenericParam>, Diagnostics> {
        let mut params = Vec::new();
        for generic in &function.generics {
            if generic.constraints.is_empty() {
                return Err(self.error(
                    format!(
                        "`{}` is unbounded, so a node has no set of types to offer for it",
                        generic.name.node
                    ),
                    generic.span,
                ));
            }
            let mut allowed = Vec::new();
            for constraint in &generic.constraints {
                allowed.push(self.value_type(constraint)?);
            }
            params.push(GenericParam::new(&generic.name.node, allowed));
        }
        Ok(params)
    }

    fn input_sockets(
        &self,
        function: &Function,
        generics: &[GenericParam],
    ) -> Result<Vec<Socket>, Diagnostics> {
        let body_start = Span::at(function.body.span.start);
        let mut sockets = Vec::new();
        for (index, param) in function.params.iter().enumerate() {
            let until = function
                .params
                .get(index + 1)
                .map_or(body_start, |next| next.span);
            let annotation = self.trailing_comment(param.span, until);
            sockets.push(self.socket(
                &param.name.node,
                &param.ty,
                generics,
                annotation.as_deref(),
            )?);
        }
        Ok(sockets)
    }

    fn return_shape(
        &self,
        module: &Module,
        function: &Function,
        generics: &[GenericParam],
    ) -> Result<FunctionReturn, Diagnostics> {
        let Some(ty) = &function.return_type else {
            return Err(self.error(
                "a node source's function must return something",
                function.name.span,
            ));
        };
        // A return type naming a struct declared in this same file is the
        // multi-output case: one socket per field, and the caller binds the
        // call once however many of them are read.
        if let Some(item) = struct_named(module, &ty.name.node) {
            let fields = self.struct_fields(item, generics)?;
            return Ok(FunctionReturn::Struct {
                name: wxsl_core::wxsl::WxslIdent::new(&item.name.node).ok_or_else(|| {
                    self.error(
                        format!("`{}` is not a usable struct name", item.name.node),
                        item.name.span,
                    )
                })?,
                fields,
            });
        }
        Ok(FunctionReturn::Value(
            self.socket("out", ty, generics, None)?,
        ))
    }

    fn struct_fields(
        &self,
        item: &StructDecl,
        generics: &[GenericParam],
    ) -> Result<Vec<Socket>, Diagnostics> {
        let end = Span::at(item.span.end);
        let mut fields = Vec::new();
        for (index, member) in item.members.iter().enumerate() {
            let until = item.members.get(index + 1).map_or(end, |next| next.span);
            let annotation = self.trailing_comment(member.span, until);
            fields.push(self.socket(
                &member.name.node,
                &member.ty,
                generics,
                annotation.as_deref(),
            )?);
        }
        if fields.is_empty() {
            return Err(self.error(
                format!("`{}` has no fields to expose as outputs", item.name.node),
                item.span,
            ));
        }
        Ok(fields)
    }

    /// One socket: its name, its type (concrete or a type parameter), and
    /// whatever its trailing comment said.
    fn socket(
        &self,
        name: &str,
        ty: &TypeExpr,
        generics: &[GenericParam],
        annotation: Option<&str>,
    ) -> Result<Socket, Diagnostics> {
        let generic = generics
            .iter()
            .find(|param| ty.is_named(param.name.as_str()));
        // A generic socket's `ty` is a placeholder codegen never reads; the
        // resolved type comes from the node instance. `F32` is what the
        // hand-written definitions used for it.
        let mut socket = match generic {
            Some(param) => Socket::new(name, ValueType::F32).generic(param.name.as_str()),
            None => Socket::new(name, self.value_type(ty)?),
        };

        let (doc, default) = self.split_annotation(annotation, ty.span)?;
        if !doc.is_empty() {
            socket = socket.with_doc(doc);
        }
        if let Some(text) = default {
            socket = self.apply_default(socket, generic, &text, ty.span)?;
        }
        Ok(socket)
    }

    /// Split a trailing comment into documentation and a `@default`.
    ///
    /// Prose first, annotation last: `// Frequency multiplier per octave.
    /// @default 2.0`. An `@` followed by anything else is a typo rather
    /// than prose, and is reported as one — silently ignoring `@defualt`
    /// would leave a socket mandatory for no visible reason.
    fn split_annotation(
        &self,
        annotation: Option<&str>,
        span: Span,
    ) -> Result<(String, Option<String>), Diagnostics> {
        let Some(text) = annotation else {
            return Ok((String::new(), None));
        };
        let Some(at) = text.find('@') else {
            return Ok((text.trim().to_string(), None));
        };
        let (doc, rest) = text.split_at(at);
        let Some(value) = rest.strip_prefix("@default") else {
            let key: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '@')
                .collect();
            return Err(self.error(
                format!("`{key}` is not a socket annotation; the only one is `@default`"),
                span,
            ));
        };
        Ok((doc.trim().to_string(), Some(value.trim().to_string())))
    }

    /// Apply `@default <text>` to `socket`.
    ///
    /// One number spreads over the socket's type, which is the only form a
    /// generic socket can have — "half, whatever width this turns out to
    /// be" is well defined at every resolution where a fixed value would be
    /// the wrong type at all but one. Several numbers are that type's
    /// components, in order.
    fn apply_default(
        &self,
        socket: Socket,
        generic: Option<&GenericParam>,
        text: &str,
        span: Span,
    ) -> Result<Socket, Diagnostics> {
        let ty = socket.ty;
        if generic.is_none() && ty.is_resource() {
            return Err(self.error(
                format!(
                    "a `{}` parameter is a binding, not a value, so it cannot have a default",
                    ty.wxsl_type()
                ),
                span,
            ));
        }
        match text {
            "true" | "false" => {
                if generic.is_some() || ty != ValueType::Bool {
                    return Err(
                        self.error(format!("`@default {text}` needs a `bool` socket"), span)
                    );
                }
                return Ok(socket.with_default(Value::Bool(text == "true")));
            }
            _ => {}
        }

        let mut numbers = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(self.error("`@default` has an empty component", span));
            }
            let Ok(number) = part.parse::<f32>() else {
                return Err(self.error(format!("`{part}` is not a number"), span));
            };
            numbers.push(number);
        }

        if let [only] = numbers[..] {
            if generic.is_some() {
                return Ok(socket.with_splat_default(only));
            }
            return Ok(match ty {
                ValueType::I32 => socket.with_default(Value::I32(only as i32)),
                ValueType::U32 => socket.with_default(Value::U32(only.max(0.0) as u32)),
                ValueType::Bool => {
                    return Err(self.error("a `bool` socket defaults to `true` or `false`", span))
                }
                _ => socket.with_splat_default(only),
            });
        }

        if generic.is_some() {
            return Err(self.error(
                "a socket whose type a node instance decides can only default to one number, \
                 spread over whatever that type turns out to be",
                span,
            ));
        }
        let value = match (ty, &numbers[..]) {
            (ValueType::Vec2, [x, y]) => Value::Vec2([*x, *y]),
            (ValueType::Vec3, [x, y, z]) => Value::Vec3([*x, *y, *z]),
            (ValueType::Vec4, [x, y, z, w]) => Value::Vec4([*x, *y, *z, *w]),
            (ValueType::Mat3, cells) if cells.len() == 9 => {
                Value::Mat3(cells.try_into().expect("nine cells"))
            }
            (ValueType::Mat4, cells) if cells.len() == 16 => {
                Value::Mat4(cells.try_into().expect("sixteen cells"))
            }
            _ => {
                return Err(self.error(
                    format!(
                        "`@default` gives {} components, but `{}` takes a different number",
                        numbers.len(),
                        ty.wxsl_type()
                    ),
                    span,
                ))
            }
        };
        Ok(socket.with_default(value))
    }

    /// The graph type a WXSL type spells, both in its short form (`vec3f`)
    /// and its long one (`vec3<f32>`).
    fn value_type(&self, ty: &TypeExpr) -> Result<ValueType, Diagnostics> {
        let spelling = ty.to_string();
        // `ValueType::wxsl_type` gives the short spelling the library writes;
        // the templated one is the same type written the long way, which a
        // hand-written file is entitled to use.
        let long_form = match spelling.as_str() {
            "vec2<f32>" => Some(ValueType::Vec2),
            "vec3<f32>" => Some(ValueType::Vec3),
            "vec4<f32>" => Some(ValueType::Vec4),
            "mat3x3<f32>" => Some(ValueType::Mat3),
            "mat4x4<f32>" => Some(ValueType::Mat4),
            _ => None,
        };
        let found = ValueType::ALL
            .iter()
            // The resource types are not in `ALL` — nothing offers a
            // texture where a value is meant — but a signature is entitled
            // to name one: `sample.texture_2d` is an ordinary function node
            // whose first two parameters are a texture and a sampler
            // ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
            .chain(ValueType::RESOURCES)
            .copied()
            .find(|candidate| candidate.wxsl_type() == spelling)
            .or(long_form);
        found.ok_or_else(|| {
            self.error(
                format!("`{spelling}` is not a type a node socket can carry"),
                ty.span,
            )
        })
    }

    // -----------------------------------------------------------------
    // Macros
    // -----------------------------------------------------------------

    fn macro_defs(&self, module: &Module) -> Result<Vec<MacroDef>, Diagnostics> {
        let mut defs = Vec::new();
        for declaration in module.macros() {
            let Some(init) = &declaration.init else {
                return Err(self.error(
                    format!("`{}` has no default value", declaration.name.node),
                    declaration.span,
                ));
            };
            let value = self.macro_value(init)?;
            // Nothing else shares an `@macro const`'s line, so the bound is
            // just the end of the file; `trailing_comment` stops at the
            // newline anyway.
            let doc = self
                .trailing_comment(declaration.span, self.end_of_source())
                .unwrap_or_default();
            defs.push(MacroDef::new(
                &declaration.name.node,
                value,
                doc.trim().to_string(),
            ));
        }
        Ok(defs)
    }

    /// A macro's default, from the initializer it is declared with.
    ///
    /// `@macro const` is where a graph's editable macro set comes from, and
    /// the kind follows the literal: `bool` is a feature flag, an integer an
    /// int, a float a float — the same three kinds
    /// [`MacroValue`] has.
    fn macro_value(&self, init: &Expr) -> Result<MacroValue, Diagnostics> {
        if let Expr::Unary(unary) = init {
            if unary.op == UnaryOp::Negate {
                return match self.macro_value(&unary.operand)? {
                    MacroValue::Int(value) => Ok(MacroValue::Int(-value)),
                    MacroValue::Float(value) => Ok(MacroValue::Float(-value)),
                    MacroValue::Flag(_) => Err(self.error("a flag cannot be negated", unary.span)),
                };
            }
        }
        let Expr::Literal(literal) = init else {
            return Err(self.error(
                "a macro's default has to be a literal, since it is what the editor edits",
                init.span(),
            ));
        };
        let text = literal
            .text
            .trim_end_matches(['i', 'u', 'f', 'h'])
            .to_string();
        let parsed = match literal.kind {
            LiteralKind::Bool => literal.text.parse::<bool>().ok().map(MacroValue::Flag),
            LiteralKind::Int => text.parse::<i32>().ok().map(MacroValue::Int),
            LiteralKind::Float => text.parse::<f32>().ok().map(MacroValue::Float),
        };
        parsed.ok_or_else(|| {
            self.error(
                format!("`{}` is not a value a macro can take", literal.text),
                literal.span,
            )
        })
    }

    // -----------------------------------------------------------------
    // Comments
    // -----------------------------------------------------------------

    /// The label line and the documentation paragraph, from the comment
    /// block the file opens with.
    ///
    /// Split at the first blank comment line: the first line names the node
    /// and the paragraph after it describes it. Whatever follows is for
    /// somebody reading the source — how the body works, which paper the
    /// technique is from — and stays out of the editor.
    fn label_and_doc(&self) -> Result<(String, String), Diagnostics> {
        let mut lines = Vec::new();
        for line in self.source.lines() {
            let line = line.trim();
            if lines.is_empty() && line.is_empty() {
                continue;
            }
            match line.strip_prefix("//") {
                Some(text) => lines.push(text.trim()),
                None => break,
            }
        }
        let Some((label, rest)) = lines.split_first() else {
            return Err(self.at_start(
                "a node source opens with a comment block: a label line, then a paragraph \
                 describing it",
            ));
        };
        if label.is_empty() {
            return Err(self.at_start("the first comment line is the node's label"));
        }
        let doc = rest
            .iter()
            .skip_while(|line| line.is_empty())
            .take_while(|line| !line.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        if doc.is_empty() {
            return Err(self.at_start(format!(
                "`{label}` has no documentation paragraph after its label line"
            )));
        }
        Ok(((*label).to_string(), doc))
    }

    /// The empty span at the end of the file, as an upper bound that never
    /// clips anything.
    fn end_of_source(&self) -> Span {
        Span::at(u32::try_from(self.source.len()).unwrap_or(u32::MAX))
    }

    /// The `//` comment that follows `span` on the same line, if any.
    ///
    /// `until` bounds the search so a comment after the *next* parameter on
    /// a shared line is not read as this one's — which is why a node's
    /// parameters are written one per line, each with its own annotation.
    fn trailing_comment(&self, span: Span, until: Span) -> Option<String> {
        let from = span.end as usize;
        let bound = (until.start as usize).max(from).min(self.source.len());
        let region = self.source.get(from..bound)?;
        let region = match region.find('\n') {
            Some(newline) => &region[..newline],
            None => region,
        };
        let comment = region.find("//")?;
        Some(region[comment + 2..].trim().to_string())
    }

    // -----------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------

    fn error(&self, message: impl Into<String>, span: Span) -> Diagnostics {
        Diagnostic::error(message, span)
            .in_module(self.module_path)
            .into()
    }

    fn at_start(&self, message: impl Into<String>) -> Diagnostics {
        self.error(message, Span::at(0))
    }
}

/// The struct named `name` declared in `module`, if there is one.
fn struct_named<'m>(module: &'m Module, name: &str) -> Option<&'m StructDecl> {
    module
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Struct(item) if item.name.node == name => Some(item),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::node::NodeBody;

    fn derive(source: &str) -> NodeDefinition {
        node_from_source(source, "package::demo::thing").unwrap_or_else(|errors| {
            panic!("{}", errors.render(&|_| Some(source.to_string())));
        })
    }

    fn failure(source: &str) -> String {
        let errors =
            node_from_source(source, "package::demo::thing").expect_err("should not derive");
        errors
            .iter()
            .map(|item| item.message.clone())
            .collect::<Vec<_>>()
            .join("; ")
    }

    #[test]
    fn a_plain_function_becomes_a_node() {
        let def = derive(
            "// Luminance\n\
             //\n\
             // Relative luminance of a linear colour.\n\
             fn luminance(\n\
                 color: vec3f, // The colour. @default 1.0\n\
             ) -> f32 {\n\
                 return dot(color, vec3f(0.2126, 0.7152, 0.0722));\n\
             }\n",
        );
        assert_eq!(def.id, "demo.thing");
        assert_eq!(def.category, "demo");
        assert_eq!(def.label, "Luminance");
        assert_eq!(def.doc, "Relative luminance of a linear colour.");
        assert_eq!(def.inputs.len(), 1);
        assert_eq!(def.inputs[0].name.as_str(), "color");
        assert_eq!(def.inputs[0].ty, ValueType::Vec3);
        assert_eq!(def.inputs[0].doc, "The colour.");
        assert_eq!(def.inputs[0].default, Some(Value::Vec3([1.0; 3])));
        assert_eq!(def.outputs.len(), 1);
        assert_eq!(def.outputs[0].name.as_str(), "out");
        assert_eq!(def.outputs[0].ty, ValueType::F32);

        let NodeBody::Call(call) = &def.body else {
            panic!("a function file is a call");
        };
        assert_eq!(call.module.as_str(), "package::demo::thing");
        assert_eq!(call.name.as_str(), "luminance");
    }

    #[test]
    fn a_template_bound_list_is_the_allowed_set() {
        let def = derive(
            "// Safe normalize\n\
             //\n\
             // Normalize, returning zero for a zero-length input.\n\
             fn safe_normalize<T: vec2f | vec3f | vec4f>(\n\
                 v: T, // @default 0.0\n\
             ) -> T {\n\
                 return v;\n\
             }\n",
        );
        assert_eq!(def.generics.len(), 1);
        assert_eq!(def.generics[0].name.as_str(), "T");
        assert_eq!(
            def.generics[0].allowed,
            vec![ValueType::Vec2, ValueType::Vec3, ValueType::Vec4]
        );
        // A generic socket carries the parameter, has no fixed default, and
        // spreads its scalar over whatever the instance resolves to.
        let input = &def.inputs[0];
        assert_eq!(input.generic.as_ref().map(|p| p.as_str()), Some("T"));
        assert_eq!(input.default, None);
        assert_eq!(input.splat_default, Some(0.0));
        assert_eq!(
            def.outputs[0].generic.as_ref().map(|p| p.as_str()),
            Some("T")
        );
    }

    #[test]
    fn a_struct_return_is_one_output_per_field() {
        let def = derive(
            "// Split\n\
             //\n\
             // Two halves at once.\n\
             struct Halves {\n\
                 diffuse: vec3f,\n\
                 specular: vec3f, // The specular half.\n\
             }\n\
             fn split(\n\
                 x: f32, // @default 0.0\n\
             ) -> Halves {\n\
                 var out: Halves;\n\
                 return out;\n\
             }\n",
        );
        assert_eq!(
            def.outputs
                .iter()
                .map(|socket| socket.name.as_str())
                .collect::<Vec<_>>(),
            ["diffuse", "specular"]
        );
        assert_eq!(def.outputs[1].doc, "The specular half.");
        let NodeBody::Call(call) = &def.body else {
            panic!("a call");
        };
        assert!(matches!(call.ret, FunctionReturn::Struct { .. }));
        // The struct type is imported alongside the function.
        assert_eq!(call.imports().len(), 2);
    }

    #[test]
    fn macro_constants_become_the_nodes_macro_set() {
        let def = derive(
            "// Fractal noise\n\
             //\n\
             // Octaves of noise.\n\
             @macro const WXSL_FBM_OCTAVES: i32 = 5; // How many octaves to sum.\n\
             @macro const wxsl_fbm_ridged: bool = false; // Fold each octave into a ridge.\n\
             fn fbm3(\n\
                 p: vec3f, // @default 0.0\n\
             ) -> f32 {\n\
                 return f32(WXSL_FBM_OCTAVES);\n\
             }\n",
        );
        assert_eq!(def.macros.len(), 2);
        assert_eq!(def.macros[0].name.as_str(), "WXSL_FBM_OCTAVES");
        assert_eq!(def.macros[0].default, MacroValue::Int(5));
        assert_eq!(def.macros[0].doc, "How many octaves to sum.");
        assert_eq!(def.macros[1].default, MacroValue::Flag(false));
    }

    #[test]
    fn several_components_give_an_exact_default() {
        let def = derive(
            "// HSV to RGB\n\
             //\n\
             // Hue, saturation, value to RGB.\n\
             fn hsv_to_rgb(\n\
                 hsv: vec3f, // @default 0.0, 1.0, 1.0\n\
             ) -> vec3f {\n\
                 return hsv;\n\
             }\n",
        );
        assert_eq!(def.inputs[0].default, Some(Value::Vec3([0.0, 1.0, 1.0])));
    }

    #[test]
    fn a_parameter_with_no_annotation_is_mandatory() {
        let def = derive(
            "// Thing\n\
             //\n\
             // Does a thing.\n\
             fn thing(\n\
                 x: f32,\n\
             ) -> f32 {\n\
                 return x;\n\
             }\n",
        );
        assert!(def.inputs[0].is_required());
    }

    #[test]
    fn the_paragraphs_after_the_doc_stay_in_the_file() {
        let def = derive(
            "// Thing\n\
             //\n\
             // What it is for.\n\
             //\n\
             // How the body works, which nobody editing a graph needs.\n\
             fn thing(\n\
                 x: f32, // @default 0.0\n\
             ) -> f32 {\n\
                 return x;\n\
             }\n",
        );
        assert_eq!(def.doc, "What it is for.");
    }

    #[test]
    fn two_functions_in_one_file_are_rejected() {
        let message = failure(
            "// Thing\n\
             //\n\
             // Does a thing.\n\
             fn helper() -> f32 { return 1.0; }\n\
             fn thing() -> f32 { return helper(); }\n",
        );
        assert!(
            message.contains("exactly one function"),
            "unexpected: {message}"
        );
    }

    #[test]
    fn an_entry_point_is_not_a_node() {
        let message = failure(
            "// Thing\n\
             //\n\
             // Does a thing.\n\
             @fragment\n\
             fn thing() -> @location(0) vec4f { return vec4f(1.0); }\n",
        );
        assert!(message.contains("entry point"), "unexpected: {message}");
    }

    #[test]
    fn an_unbounded_type_parameter_is_rejected() {
        let message = failure(
            "// Thing\n\
             //\n\
             // Does a thing.\n\
             fn thing<T>(\n\
                 x: T,\n\
             ) -> T {\n\
                 return x;\n\
             }\n",
        );
        assert!(message.contains("unbounded"), "unexpected: {message}");
    }

    #[test]
    fn a_type_no_socket_can_carry_is_rejected() {
        let message = failure(
            "// Thing\n\
             //\n\
             // Does a thing.\n\
             fn thing(\n\
                 x: mat2x3f,\n\
             ) -> f32 {\n\
                 return 0.0;\n\
             }\n",
        );
        assert!(message.contains("mat2x3f"), "unexpected: {message}");
    }

    #[test]
    fn a_misspelled_annotation_is_reported_rather_than_ignored() {
        let message = failure(
            "// Thing\n\
             //\n\
             // Does a thing.\n\
             fn thing(\n\
                 x: f32, // @defualt 1.0\n\
             ) -> f32 {\n\
                 return x;\n\
             }\n",
        );
        assert!(message.contains("@default"), "unexpected: {message}");
    }

    #[test]
    fn a_missing_label_block_is_reported() {
        let message = failure("fn thing() -> f32 { return 1.0; }\n");
        assert!(message.contains("label"), "unexpected: {message}");
    }
}
