//! The node palette: finding one of two hundred nodes and adding it.
//!
//! Built entirely from what the registry already exposes —
//! [`NodeRegistry::categories`] for the grouping and each definition's
//! `label`, `id` and `doc` for the rows — so a node library the editor has
//! never heard of appears in it with no editor change (ADR 0004).
//!
//! Matching is a scored substring search rather than a fuzzy matcher: with
//! ids like `math.add.vec3f` the useful query is a prefix or a fragment
//! ("add", "vec3", "pbr"), and a scored search puts an exact label first,
//! which is the behaviour a user who knows the name expects.

use wxsl_core::node::NodeRegistry;

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Registry id of the definition.
    pub id: String,
    /// Its label.
    pub label: String,
    /// Its category.
    pub category: String,
    /// How well it matched: higher is better.
    pub score: i32,
}

/// Score `query` against one definition, or `None` if it does not match.
///
/// The ranking, best first: an exact label or id, a label that starts with
/// the query, an id that starts with it, a label that contains it, an id that
/// contains it, the documentation. Shorter matches win ties, because a query
/// is more likely to have meant `add` than `add_weighted`.
fn score(query: &str, id: &str, label: &str, doc: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_ascii_lowercase();
    let id_lower = id.to_ascii_lowercase();
    let label_lower = label.to_ascii_lowercase();

    let mut score = if label_lower == query || id_lower == query {
        1000
    } else if label_lower.starts_with(&query) {
        800
    } else if id_lower.starts_with(&query) {
        700
    } else if label_lower.contains(&query) {
        500
    } else if id_lower.contains(&query) {
        400
    } else if doc.to_ascii_lowercase().contains(&query) {
        100
    } else {
        // Every whitespace-separated word of the query has to appear
        // somewhere, so "noise vec3" finds the 3D noise node.
        let haystack = format!("{id_lower} {label_lower}");
        if query.split_whitespace().all(|word| haystack.contains(word)) {
            200
        } else {
            return None;
        }
    };
    // Prefer the shorter of two otherwise equal matches.
    score -= (id.len() as i32).min(99);
    Some(score)
}

/// The palette's state: what has been typed, and what is highlighted.
#[derive(Clone, Debug, Default)]
pub struct NodePicker {
    /// The search box's contents.
    pub query: String,
    /// Which category is being shown, or `None` for all of them.
    pub category: Option<String>,
    /// Which result the keyboard is on.
    pub highlighted: usize,
    /// Where a node added from here should go, in graph space. Set when the
    /// palette is opened from the canvas's context menu.
    pub target: Option<glam::Vec2>,
    /// Whether the palette is showing as a popover over the canvas.
    pub open: bool,
}

impl NodePicker {
    /// A closed palette with nothing typed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the palette, to add a node at `target` in graph space.
    pub fn open_at(&mut self, target: glam::Vec2) {
        self.open = true;
        self.target = Some(target);
        self.query.clear();
        self.highlighted = 0;
    }

    /// Close the palette.
    pub fn close(&mut self) {
        self.open = false;
        self.target = None;
    }

    /// The matching definitions, best first.
    ///
    /// Capped, because a hundred rows nobody will scroll to costs a hundred
    /// rows of glyphs; the search is how you reach the rest.
    pub fn matches(&self, registry: &NodeRegistry, limit: usize) -> Vec<Match> {
        let mut matches: Vec<Match> = registry
            .iter()
            .filter(|definition| {
                self.category
                    .as_ref()
                    .is_none_or(|category| definition.category == *category)
            })
            .filter_map(|definition| {
                let score = score(
                    self.query.trim(),
                    &definition.id,
                    &definition.label,
                    &definition.doc,
                )?;
                Some(Match {
                    id: definition.id.clone(),
                    label: definition.label.clone(),
                    category: definition.category.clone(),
                    score,
                })
            })
            .collect();
        // Score first, then id, so the order is stable between frames — an
        // unstable order in a list you are arrowing through is unusable.
        matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        matches.truncate(limit);
        matches
    }

    /// Move the highlight, clamped to the result count.
    pub fn move_highlight(&mut self, delta: i32, count: usize) {
        if count == 0 {
            self.highlighted = 0;
            return;
        }
        let next = self.highlighted as i32 + delta;
        self.highlighted = next.clamp(0, count as i32 - 1) as usize;
    }

    /// The highlighted result, if there is one.
    pub fn highlighted<'a>(&self, matches: &'a [Match]) -> Option<&'a Match> {
        matches.get(self.highlighted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::node::{NodeDefinition, Socket, ValueType};

    fn registry() -> NodeRegistry {
        let node = |id: &str, label: &str, doc: &str| {
            NodeDefinition::builder(id, label)
                .doc(doc)
                .output(Socket::new("out", ValueType::F32))
                .expr("0.0")
        };
        let mut registry = NodeRegistry::new();
        registry.register_all([
            node("math.add.f32", "Add", "Sum of two values."),
            node("math.add.vec3f", "Add", "Sum of two values."),
            node("math.add_weighted.f32", "Add weighted", "Weighted sum."),
            node("color.hsv_to_rgb", "HSV to RGB", "Convert a colour."),
            node("generative.value_noise3", "Value noise 3D", "Smooth noise."),
            node("lighting.pbr_direct", "PBR direct", "Direct lighting."),
        ]);
        registry
    }

    #[test]
    fn an_empty_query_lists_everything() {
        let picker = NodePicker::new();
        let matches = picker.matches(&registry(), 100);
        assert_eq!(matches.len(), 6);
    }

    #[test]
    fn the_limit_is_respected() {
        let picker = NodePicker::new();
        assert_eq!(picker.matches(&registry(), 3).len(), 3);
    }

    #[test]
    fn an_exact_label_outranks_a_longer_one_containing_it() {
        let mut picker = NodePicker::new();
        picker.query = "add".to_string();
        let matches = picker.matches(&registry(), 100);
        assert!(matches.len() >= 3);
        // "Add" before "Add weighted", whichever order the registry is in.
        assert_eq!(matches[0].label, "Add");
        let weighted = matches
            .iter()
            .position(|found| found.id.contains("add_weighted"))
            .expect("weighted matched too");
        assert!(weighted > 0, "{matches:?}");
    }

    #[test]
    fn a_query_can_match_the_id_the_label_or_the_docs() {
        let mut picker = NodePicker::new();
        picker.query = "vec3".to_string();
        let by_id = picker.matches(&registry(), 100);
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].id, "math.add.vec3f");

        picker.query = "colour".to_string();
        let by_doc = picker.matches(&registry(), 100);
        assert_eq!(by_doc.len(), 1, "the documentation matched");
        assert_eq!(by_doc[0].id, "color.hsv_to_rgb");

        picker.query = "nothing here".to_string();
        assert!(picker.matches(&registry(), 100).is_empty());
    }

    #[test]
    fn several_words_all_have_to_appear() {
        let mut picker = NodePicker::new();
        picker.query = "noise 3".to_string();
        let matches = picker.matches(&registry(), 100);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "generative.value_noise3");

        picker.query = "noise pbr".to_string();
        assert!(picker.matches(&registry(), 100).is_empty());
    }

    #[test]
    fn a_category_filter_narrows_the_list() {
        let mut picker = NodePicker::new();
        picker.category = Some("math".to_string());
        let matches = picker.matches(&registry(), 100);
        assert_eq!(matches.len(), 3);
        assert!(matches.iter().all(|found| found.category == "math"));
    }

    #[test]
    fn the_order_is_stable_between_identical_searches() {
        // An unstable order in a list being arrowed through is unusable.
        let picker = NodePicker::new();
        let registry = registry();
        let first = picker.matches(&registry, 100);
        let second = picker.matches(&registry, 100);
        assert_eq!(first, second);
    }

    #[test]
    fn the_highlight_stays_inside_the_results() {
        let mut picker = NodePicker::new();
        picker.move_highlight(5, 3);
        assert_eq!(picker.highlighted, 2);
        picker.move_highlight(-10, 3);
        assert_eq!(picker.highlighted, 0);
        // And an empty result list has nothing to highlight.
        picker.move_highlight(3, 0);
        assert_eq!(picker.highlighted, 0);
        assert!(picker.highlighted(&[]).is_none());
    }

    #[test]
    fn opening_the_palette_resets_the_search_and_remembers_where() {
        let mut picker = NodePicker::new();
        picker.query = "stale".to_string();
        picker.highlighted = 4;
        picker.open_at(glam::Vec2::new(120.0, 40.0));
        assert!(picker.open);
        assert!(picker.query.is_empty());
        assert_eq!(picker.highlighted, 0);
        assert_eq!(picker.target, Some(glam::Vec2::new(120.0, 40.0)));
        picker.close();
        assert!(!picker.open);
        assert_eq!(picker.target, None);
    }
}
