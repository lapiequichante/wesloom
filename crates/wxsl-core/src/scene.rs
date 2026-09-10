//! The scene document: what exists, never how it is drawn.
//!
//! A scene is meshes, materials and instances — pure serializable data with
//! no `wgpu` anywhere near it, which is why it lives here rather than in
//! `wxsl-render`. It says a cube exists at this transform with that
//! material; it says nothing about passes, targets or order, because the
//! *pipeline* decides those and the pipeline belongs to the renderer
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//!
//! Translating a scene into draws is the application's job, not the
//! renderer's — `wxsl-render` keeps taking a draw list, because batching,
//! culling and sorting belong above it. The `wxsl` facade's `scene` module
//! is that translation for the common case.
//!
//! # Tags, and who introspects whom
//!
//! An instance carries [`Tags`] it was authored with (`opaque`,
//! `transparent`, `outlined`), and a geometry pass draws a [`TagExpr`]
//! (`opaque`, `opaque && !outlined`). The material says what it *is*, the
//! pass says what it *draws*, and neither has to know the other exists.

use core::fmt;
use core::str::FromStr;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::graph::Graph;
use crate::macros::MacroSet;

/// A whole scene: the meshes it uses, the materials on them, and where each
/// instance sits.
///
/// Indices rather than names on the wire: an instance points at a mesh and a
/// material by position, which is one lookup and cannot be misspelled.
/// [`Scene::validate`] is what catches an index that points nowhere.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Scene {
    /// Name of the scene, for a title bar or a file listing.
    #[cfg_attr(feature = "serde", serde(default))]
    pub name: String,
    /// The geometry the instances draw.
    #[cfg_attr(feature = "serde", serde(default))]
    pub meshes: Vec<MeshEntry>,
    /// The materials the instances wear.
    #[cfg_attr(feature = "serde", serde(default))]
    pub materials: Vec<MaterialEntry>,
    /// One entry per thing to draw.
    #[cfg_attr(feature = "serde", serde(default))]
    pub instances: Vec<Instance>,
}

impl Scene {
    /// An empty scene called `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Scene {
            name: name.into(),
            ..Scene::default()
        }
    }

    /// Add a mesh, returning its index.
    pub fn add_mesh(&mut self, entry: MeshEntry) -> usize {
        self.meshes.push(entry);
        self.meshes.len() - 1
    }

    /// Add a material, returning its index.
    pub fn add_material(&mut self, entry: MaterialEntry) -> usize {
        self.materials.push(entry);
        self.materials.len() - 1
    }

    /// Add an instance, returning its index.
    pub fn add_instance(&mut self, instance: Instance) -> usize {
        self.instances.push(instance);
        self.instances.len() - 1
    }

    /// The tags an instance draws under: its own if it overrides them,
    /// otherwise its material's.
    ///
    /// The override is for the odd case — one crate in a pile that should
    /// also be outlined — and not for re-tagging a whole material, which is
    /// an edit to the material.
    pub fn instance_tags<'a>(&'a self, instance: &'a Instance) -> &'a Tags {
        match instance.tags.as_ref() {
            Some(tags) => tags,
            None => self
                .materials
                .get(instance.material)
                .map(|material| &material.tags)
                .unwrap_or(Tags::EMPTY),
        }
    }

    /// Every index an instance names actually exists.
    ///
    /// Returned as a list because a scene loaded from a file is usually
    /// wrong in more than one place, and fixing them one error per load is
    /// the same misery as fixing a graph one error at a time.
    pub fn validate(&self) -> Vec<SceneError> {
        let mut errors = Vec::new();
        for (index, instance) in self.instances.iter().enumerate() {
            if instance.mesh >= self.meshes.len() {
                errors.push(SceneError::NoSuchMesh {
                    instance: index,
                    mesh: instance.mesh,
                });
            }
            if instance.material >= self.materials.len() {
                errors.push(SceneError::NoSuchMaterial {
                    instance: index,
                    material: instance.material,
                });
            }
        }
        errors
    }
}

/// What is wrong with a scene document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneError {
    /// An instance names a mesh index the scene does not have.
    NoSuchMesh {
        /// Index of the offending instance.
        instance: usize,
        /// The mesh index it named.
        mesh: usize,
    },
    /// An instance names a material index the scene does not have.
    NoSuchMaterial {
        /// Index of the offending instance.
        instance: usize,
        /// The material index it named.
        material: usize,
    },
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SceneError::NoSuchMesh { instance, mesh } => {
                write!(
                    f,
                    "instance {instance} names mesh {mesh}, which does not exist"
                )
            }
            SceneError::NoSuchMaterial { instance, material } => write!(
                f,
                "instance {instance} names material {material}, which does not exist"
            ),
        }
    }
}

impl std::error::Error for SceneError {}

/// One mesh in a scene, by name and by where its geometry comes from.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MeshEntry {
    /// Name, for the editor and for diagnostics.
    #[cfg_attr(feature = "serde", serde(default))]
    pub name: String,
    /// Where the geometry comes from.
    pub source: MeshSource,
}

impl MeshEntry {
    /// A named mesh from `source`.
    pub fn new(name: impl Into<String>, source: MeshSource) -> Self {
        MeshEntry {
            name: name.into(),
            source,
        }
    }
}

/// Where a mesh's geometry comes from.
///
/// A generated primitive or a file on disk. The renderer resolves both; the
/// document only records which, so a scene stays a few kilobytes of text
/// rather than a vertex dump.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
pub enum MeshSource {
    /// A cube of the given edge length, centred on the origin.
    Cube {
        /// Edge length.
        size: f32,
    },
    /// A UV sphere.
    Sphere {
        /// Radius.
        radius: f32,
    },
    /// A subdivided quad in the xz plane, facing up.
    Plane {
        /// Edge length.
        size: f32,
    },
    /// A torus around the y axis.
    Torus {
        /// Radius of the main ring.
        radius: f32,
        /// Radius of the tube.
        tube_radius: f32,
    },
    /// Geometry loaded from a file, currently glTF/GLB.
    ///
    /// Resolving this needs `wxsl-render`'s `gltf` feature; without it the
    /// scene still parses and the mesh is reported as unavailable rather
    /// than silently drawn as nothing.
    File {
        /// Path to the file, relative to the scene document.
        path: String,
        /// Which primitive of the file, in depth-first order. `None` merges
        /// every primitive into one mesh.
        #[cfg_attr(feature = "serde", serde(default))]
        primitive: Option<usize>,
    },
}

/// One material in a scene: the graph, the macro values it was authored
/// with, and the tags it draws under.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MaterialEntry {
    /// Name, for the editor and for diagnostics.
    #[cfg_attr(feature = "serde", serde(default))]
    pub name: String,
    /// The surface graph.
    pub graph: Graph,
    /// Macro values pinned on top of the graph's own.
    #[cfg_attr(feature = "serde", serde(default))]
    pub macros: MacroSet,
    /// What this material *is*, for a pass to select on.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tags: Tags,
}

impl MaterialEntry {
    /// A named material from `graph`, tagged `opaque`.
    pub fn new(name: impl Into<String>, graph: Graph) -> Self {
        MaterialEntry {
            name: name.into(),
            graph,
            macros: MacroSet::new(),
            tags: Tags::from_iter([TAG_OPAQUE]),
        }
    }

    /// Replace the tags.
    pub fn with_tags(mut self, tags: Tags) -> Self {
        self.tags = tags;
        self
    }

    /// Replace the macro values.
    pub fn with_macros(mut self, macros: MacroSet) -> Self {
        self.macros = macros;
        self
    }
}

/// One thing to draw: geometry, a material, and where it is.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Instance {
    /// Name, for the editor and for diagnostics.
    #[cfg_attr(feature = "serde", serde(default))]
    pub name: String,
    /// Index into [`Scene::meshes`].
    pub mesh: usize,
    /// Index into [`Scene::materials`].
    pub material: usize,
    /// Object-to-world matrix, column-major — the same order `glam`'s
    /// `Mat4::to_cols_array` produces, so the renderer's conversion is a
    /// `from_cols_array` and nothing else.
    #[cfg_attr(feature = "serde", serde(default = "identity_transform"))]
    pub transform: [f32; 16],
    /// Tags for this instance alone, overriding the material's.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub tags: Option<Tags>,
}

fn identity_transform() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]
}

impl Instance {
    /// An instance of `mesh` with `material`, at the origin.
    pub fn new(mesh: usize, material: usize) -> Self {
        Instance {
            name: String::new(),
            mesh,
            material,
            transform: identity_transform(),
            tags: None,
        }
    }

    /// Name it.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Place it, with a column-major object-to-world matrix.
    pub fn with_transform(mut self, transform: [f32; 16]) -> Self {
        self.transform = transform;
        self
    }

    /// Override the material's tags for this instance.
    pub fn with_tags(mut self, tags: Tags) -> Self {
        self.tags = Some(tags);
        self
    }
}

/// The tag every material gets unless it says otherwise.
pub const TAG_OPAQUE: &str = "opaque";
/// The conventional tag for a material that blends.
pub const TAG_TRANSPARENT: &str = "transparent";

/// A set of tags, sorted and unique.
///
/// Strings rather than a bitset: the set of tags is open — an application
/// invents `outlined` or `underwater` without asking anyone — and a scene
/// that round-trips through a file cannot carry a bit whose meaning lived in
/// someone else's enum. Sets are small enough that a linear scan wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(transparent))]
pub struct Tags(Vec<String>);

impl Tags {
    /// The empty set, as a reference.
    ///
    /// The answer for an instance whose material index points nowhere, and
    /// the default for a draw nobody tagged.
    pub const EMPTY: &'static Tags = &Tags(Vec::new());

    /// No tags.
    pub fn new() -> Self {
        Tags::default()
    }

    /// Whether `tag` is in the set.
    pub fn contains(&self, tag: &str) -> bool {
        self.0.iter().any(|held| held == tag)
    }

    /// Add `tag`, keeping the set sorted and unique.
    pub fn insert(&mut self, tag: impl Into<String>) {
        let tag = tag.into();
        if let Err(position) = self.0.binary_search(&tag) {
            self.0.insert(position, tag);
        }
    }

    /// Remove `tag`, reporting whether it was there.
    pub fn remove(&mut self, tag: &str) -> bool {
        match self.0.iter().position(|held| held == tag) {
            Some(position) => {
                self.0.remove(position);
                true
            }
            None => false,
        }
    }

    /// The tags, in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    /// How many tags.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<T: Into<String>> FromIterator<T> for Tags {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut tags = Tags::new();
        for tag in iter {
            tags.insert(tag);
        }
        tags
    }
}

impl fmt::Display for Tags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join(" "))
    }
}

/// What a pass draws, as a predicate over an instance's [`Tags`].
///
/// The grammar is the obvious one — `!`, `&&`, `||`, parentheses, and `*`
/// for "everything" — so a pass list written in Rust and a pass node in an
/// editor spell the same thing the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "String", into = "String"))]
#[derive(Default)]
pub enum TagExpr {
    /// Matches every instance.
    #[default]
    Always,
    /// Matches nothing — useful as the identity of an `Or` fold.
    Never,
    /// Matches an instance carrying this tag.
    Tag(String),
    /// Matches when the inner expression does not.
    Not(Box<TagExpr>),
    /// Matches when both do.
    And(Box<TagExpr>, Box<TagExpr>),
    /// Matches when either does.
    Or(Box<TagExpr>, Box<TagExpr>),
}

impl TagExpr {
    /// An expression matching instances tagged `tag`.
    pub fn tag(tag: impl Into<String>) -> Self {
        TagExpr::Tag(tag.into())
    }

    /// Both, as one expression.
    pub fn and(self, other: TagExpr) -> Self {
        TagExpr::And(Box::new(self), Box::new(other))
    }

    /// Either, as one expression.
    pub fn or(self, other: TagExpr) -> Self {
        TagExpr::Or(Box::new(self), Box::new(other))
    }

    /// The negation.
    ///
    /// Not `std::ops::Not`: `!expr` reads as a boolean negation of a value
    /// the caller already has, and this builds a *predicate* out of one.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        TagExpr::Not(Box::new(self))
    }

    /// Whether an instance with `tags` is drawn by a pass asking for this.
    pub fn matches(&self, tags: &Tags) -> bool {
        match self {
            TagExpr::Always => true,
            TagExpr::Never => false,
            TagExpr::Tag(tag) => tags.contains(tag),
            TagExpr::Not(inner) => !inner.matches(tags),
            TagExpr::And(left, right) => left.matches(tags) && right.matches(tags),
            TagExpr::Or(left, right) => left.matches(tags) || right.matches(tags),
        }
    }

    /// Parse the surface syntax: `opaque && !outlined`, `*`, `a || (b && c)`.
    pub fn parse(text: &str) -> Result<Self, TagExprError> {
        let tokens = tokenize(text)?;
        let mut parser = Parser {
            tokens: &tokens,
            position: 0,
        };
        let expr = parser.expression()?;
        if parser.position != tokens.len() {
            return Err(TagExprError::Trailing);
        }
        Ok(expr)
    }
}

impl FromStr for TagExpr {
    type Err = TagExprError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        TagExpr::parse(text)
    }
}

impl TryFrom<String> for TagExpr {
    type Error = TagExprError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        TagExpr::parse(&text)
    }
}

impl From<TagExpr> for String {
    fn from(expr: TagExpr) -> String {
        expr.to_string()
    }
}

impl fmt::Display for TagExpr {
    /// Round-trips through [`TagExpr::parse`], parenthesized wherever
    /// precedence would otherwise change the meaning.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagExpr::Always => f.write_str("*"),
            TagExpr::Never => f.write_str("!*"),
            TagExpr::Tag(tag) => f.write_str(tag),
            TagExpr::Not(inner) => match inner.as_ref() {
                TagExpr::And(..) | TagExpr::Or(..) => write!(f, "!({inner})"),
                _ => write!(f, "!{inner}"),
            },
            TagExpr::And(left, right) => {
                let side = |expr: &TagExpr, f: &mut fmt::Formatter<'_>| match expr {
                    TagExpr::Or(..) => write!(f, "({expr})"),
                    _ => write!(f, "{expr}"),
                };
                side(left, f)?;
                f.write_str(" && ")?;
                side(right, f)
            }
            TagExpr::Or(left, right) => write!(f, "{left} || {right}"),
        }
    }
}

/// Why a tag expression would not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagExprError {
    /// A character that is not a tag character, an operator or a bracket.
    UnexpectedCharacter(char),
    /// A `&` or `|` not doubled — `&&` and `||`, as in the shading language.
    SingleOperator(char),
    /// An operand was expected and the expression ended, or an operator
    /// followed an operator.
    ExpectedOperand,
    /// An opened bracket was never closed.
    UnclosedBracket,
    /// The expression parsed, but there was more text after it.
    Trailing,
}

impl fmt::Display for TagExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagExprError::UnexpectedCharacter(character) => {
                write!(f, "`{character}` is not valid in a tag expression")
            }
            TagExprError::SingleOperator(character) => {
                write!(f, "write `{character}{character}`, not `{character}`")
            }
            TagExprError::ExpectedOperand => f.write_str("expected a tag here"),
            TagExprError::UnclosedBracket => f.write_str("unclosed `(`"),
            TagExprError::Trailing => f.write_str("unexpected text after the expression"),
        }
    }
}

impl std::error::Error for TagExprError {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Tag(String),
    Star,
    Not,
    And,
    Or,
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Token>, TagExprError> {
    let bytes: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let character = bytes[index];
        match character {
            c if c.is_whitespace() => index += 1,
            '*' => {
                tokens.push(Token::Star);
                index += 1;
            }
            '!' => {
                tokens.push(Token::Not);
                index += 1;
            }
            '(' => {
                tokens.push(Token::Open);
                index += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                index += 1;
            }
            '&' | '|' => {
                if bytes.get(index + 1) != Some(&character) {
                    return Err(TagExprError::SingleOperator(character));
                }
                tokens.push(if character == '&' {
                    Token::And
                } else {
                    Token::Or
                });
                index += 2;
            }
            c if is_tag_char(c) => {
                let start = index;
                while index < bytes.len() && is_tag_char(bytes[index]) {
                    index += 1;
                }
                tokens.push(Token::Tag(bytes[start..index].iter().collect()));
            }
            other => return Err(TagExprError::UnexpectedCharacter(other)),
        }
    }
    Ok(tokens)
}

/// What a tag may be spelled with: an identifier, plus `.` and `-` so
/// `crate.opaque` and `two-sided` are one tag rather than three tokens.
fn is_tag_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '.' | '-')
}

struct Parser<'a> {
    tokens: &'a [Token],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.position += 1;
            return true;
        }
        false
    }

    /// `or := and ('||' and)*`
    fn expression(&mut self) -> Result<TagExpr, TagExprError> {
        let mut left = self.conjunction()?;
        while self.eat(&Token::Or) {
            left = left.or(self.conjunction()?);
        }
        Ok(left)
    }

    /// `and := unary ('&&' unary)*`
    fn conjunction(&mut self) -> Result<TagExpr, TagExprError> {
        let mut left = self.unary()?;
        while self.eat(&Token::And) {
            left = left.and(self.unary()?);
        }
        Ok(left)
    }

    /// `unary := '!' unary | atom`
    fn unary(&mut self) -> Result<TagExpr, TagExprError> {
        if self.eat(&Token::Not) {
            return Ok(self.unary()?.not());
        }
        self.atom()
    }

    /// `atom := tag | '*' | '(' expression ')'`
    fn atom(&mut self) -> Result<TagExpr, TagExprError> {
        match self.peek().cloned() {
            Some(Token::Tag(tag)) => {
                self.position += 1;
                Ok(TagExpr::Tag(tag))
            }
            Some(Token::Star) => {
                self.position += 1;
                Ok(TagExpr::Always)
            }
            Some(Token::Open) => {
                self.position += 1;
                let inner = self.expression()?;
                if !self.eat(&Token::Close) {
                    return Err(TagExprError::UnclosedBracket);
                }
                Ok(inner)
            }
            _ => Err(TagExprError::ExpectedOperand),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Tags {
        Tags::from_iter(list.iter().copied())
    }

    #[test]
    fn tags_are_sorted_and_unique() {
        let mut set = tags(&["outlined", "opaque", "opaque"]);
        assert_eq!(set.len(), 2);
        assert_eq!(set.to_string(), "opaque outlined");
        assert!(set.contains("opaque"));
        assert!(set.remove("opaque"));
        assert!(!set.remove("opaque"));
        assert_eq!(set.to_string(), "outlined");
    }

    #[test]
    fn a_tag_expression_selects_what_a_pass_draws() {
        let expr = TagExpr::parse("opaque && !outlined").expect("parses");
        assert!(expr.matches(&tags(&["opaque"])));
        assert!(!expr.matches(&tags(&["opaque", "outlined"])));
        assert!(!expr.matches(&tags(&["transparent"])));

        let everything = TagExpr::parse("*").expect("parses");
        assert!(everything.matches(&Tags::new()));
    }

    #[test]
    fn precedence_is_the_usual_one_and_brackets_override_it() {
        // `a || b && c` is `a || (b && c)`, so `a` alone matches.
        let expr = TagExpr::parse("a || b && c").expect("parses");
        assert!(expr.matches(&tags(&["a"])));
        assert!(!expr.matches(&tags(&["b"])));
        assert!(expr.matches(&tags(&["b", "c"])));

        let bracketed = TagExpr::parse("(a || b) && c").expect("parses");
        assert!(!bracketed.matches(&tags(&["a"])));
        assert!(bracketed.matches(&tags(&["a", "c"])));
    }

    #[test]
    fn every_expression_round_trips_through_its_own_display() {
        for text in [
            "*",
            "opaque",
            "!opaque",
            "opaque && outlined",
            "a || b && c",
            "(a || b) && c",
            "!(a || b)",
            "two-sided && crate.opaque",
        ] {
            let expr = TagExpr::parse(text).expect(text);
            let printed = expr.to_string();
            let reparsed = TagExpr::parse(&printed).expect(&printed);
            assert_eq!(expr, reparsed, "`{text}` printed as `{printed}`");
        }
    }

    #[test]
    fn malformed_expressions_say_what_is_wrong() {
        assert_eq!(
            TagExpr::parse("a & b"),
            Err(TagExprError::SingleOperator('&'))
        );
        assert_eq!(TagExpr::parse("a &&"), Err(TagExprError::ExpectedOperand));
        assert_eq!(TagExpr::parse("(a"), Err(TagExprError::UnclosedBracket));
        assert_eq!(TagExpr::parse("a b"), Err(TagExprError::Trailing));
        assert_eq!(
            TagExpr::parse("a # b"),
            Err(TagExprError::UnexpectedCharacter('#'))
        );
    }

    #[test]
    fn an_instance_falls_back_to_its_materials_tags() {
        let mut scene = Scene::new("two boxes");
        let mesh = scene.add_mesh(MeshEntry::new("cube", MeshSource::Cube { size: 1.0 }));
        let material = scene.add_material(
            MaterialEntry::new("paint", Graph::new("paint")).with_tags(tags(&["opaque"])),
        );
        let plain = Instance::new(mesh, material);
        let outlined = Instance::new(mesh, material).with_tags(tags(&["opaque", "outlined"]));

        assert_eq!(scene.instance_tags(&plain), &tags(&["opaque"]));
        assert_eq!(
            scene.instance_tags(&outlined),
            &tags(&["opaque", "outlined"])
        );
        assert!(scene.validate().is_empty());
    }

    #[test]
    fn a_dangling_index_is_reported_rather_than_panicking() {
        let mut scene = Scene::new("broken");
        scene.add_instance(Instance::new(3, 7));
        assert_eq!(
            scene.validate(),
            vec![
                SceneError::NoSuchMesh {
                    instance: 0,
                    mesh: 3
                },
                SceneError::NoSuchMaterial {
                    instance: 0,
                    material: 7
                },
            ]
        );
        // And the tag lookup still answers, rather than indexing out of
        // bounds on a scene someone is halfway through editing.
        assert!(scene.instance_tags(&scene.instances[0]).is_empty());
    }
}
