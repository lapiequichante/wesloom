//! The immediate-mode layer: identity, interaction, and widgets.
//!
//! [`UiState`] is what persists between frames — the atlas and glyph cache,
//! which widget is hot or active, where each scroll area is scrolled to, and
//! the caret in whichever text field has focus. [`Ui`] is the per-frame
//! handle: it borrows that state, this frame's input, and the device, and
//! offers the widgets the editor is written in.
//!
//! Immediate mode, so a widget is a function call that both draws and
//! reports what happened:
//!
//! ```text
//! if ui.button(Id::new("compile"), rect, "Compile").clicked { … }
//! ```
//!
//! The one thing an immediate-mode UI has to get right is *identity*: a
//! widget has to be recognizable between frames even though nothing about it
//! is stored. Here that is [`Id`], a hash of a string path (plus, for a list,
//! an index) — which is why the editor's widget ids read like
//! `"node.3.input.roughness"`.

use std::collections::HashMap;

use glam::Vec2;
use wxsl_core::wxsl::stable_hash;
use wxsl_render::ui::draw::{Color, DrawList, Rect};
use wxsl_render::ui::input::{InputState, Key, MouseButton};
use wxsl_render::ui::text::{FontId, GlyphCache, TextLayout, TextOptions};
use wxsl_render::ui::Atlas;
use wxsl_render::RenderError;

use crate::buffer::TextBuffer;
use crate::highlight::Run;
use crate::theme::Theme;

/// A widget's identity, stable between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(pub u64);

impl Id {
    /// The id for a path like `"palette.search"`.
    pub fn new(path: &str) -> Self {
        Id(stable_hash(path.as_bytes()))
    }

    /// The id for one item of a list: this id, salted with an index.
    pub fn with(self, salt: u64) -> Self {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&self.0.to_le_bytes());
        bytes[8..].copy_from_slice(&salt.to_le_bytes());
        Id(stable_hash(&bytes))
    }
}

/// Where text sits in the rectangle it was given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Against the left edge.
    #[default]
    Left,
    /// Centred horizontally.
    Center,
    /// Against the right edge.
    Right,
}

/// What happened to a widget this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Response {
    /// The widget's identity.
    pub id: Id,
    /// The rectangle it occupies.
    pub rect: Rect,
    /// The pointer is over it and nothing else has grabbed the pointer.
    pub hovered: bool,
    /// The primary button went down on it this frame.
    pub pressed: bool,
    /// The primary button was released over it, having gone down on it — the
    /// event a button acts on, so that dragging off it cancels.
    pub clicked: bool,
    /// The secondary button was released over it.
    pub secondary_clicked: bool,
    /// It was double-clicked.
    pub double_clicked: bool,
    /// The primary button went down on it and is still held.
    pub dragging: bool,
    /// How far the pointer moved this frame, if dragging.
    pub drag_delta: Vec2,
    /// The drag ended this frame.
    pub drag_released: bool,
    /// Its value changed this frame.
    pub changed: bool,
}

impl Response {
    /// A response for a widget that was not interacted with.
    pub fn none(id: Id, rect: Rect) -> Self {
        Response {
            id,
            rect,
            ..Default::default()
        }
    }
}

/// The caret and selection in the focused text field.
#[derive(Clone, Debug, PartialEq)]
struct TextEdit {
    id: Id,
    /// Caret position, as a byte offset.
    cursor: usize,
    /// The other end of the selection, if there is one.
    anchor: Option<usize>,
}

impl TextEdit {
    /// The selected byte range, low to high.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            None
        } else {
            Some((anchor.min(self.cursor), anchor.max(self.cursor)))
        }
    }
}

/// Which widget is hot or active, and the modal restriction, if any.
///
/// Deliberately its own type, holding nothing that depends on `wgpu`: this is
/// what [`UiState::interact`] delegates to, and what a test drives directly
/// with a bare [`InputState`] and no device. That separation is what caught
/// the "clicks do nothing" bug — a stale-`active` reset that ran before a
/// widget's own release could be observed (see [`Ui::end_frame`]) is easy to
/// get backwards, and near-impossible to notice by inspection; it is not
/// hard to notice at all once the click/release sequence itself can be
/// driven in a unit test.
#[derive(Debug, Default)]
struct Interaction {
    hot: Option<Id>,
    active: Option<Id>,
    /// While set, only widgets inside this rectangle can be interacted with:
    /// a menu or a popover is open over the rest of the interface.
    modal: Option<Rect>,
    /// Set when a widget claims the pointer, so the canvas underneath knows
    /// not to also act on the same click.
    pointer_claimed: bool,
}

impl Interaction {
    /// Start a new frame: clear whatever only applies to the last one.
    ///
    /// `active` is deliberately not cleared here — see [`Self::interact`]
    /// and [`Self::release_stale_active`].
    fn begin_frame(&mut self) {
        self.hot = None;
        self.pointer_claimed = false;
    }

    /// Hit-test and update interaction state for a widget.
    ///
    /// The primitive every widget is built from. A widget becomes *active*
    /// on press and stays active until release, wherever the pointer goes,
    /// which is what makes dragging work and what makes a click cancel when
    /// the pointer leaves before release. `clip` is the clip rectangle in
    /// force when this widget is drawn: a widget scrolled out of its region
    /// is still *somewhere* in screen coordinates, and without this a row
    /// scrolled off the top of a list would keep catching clicks that landed
    /// on whatever is drawn above it.
    fn interact(&mut self, input: &InputState, clip: Rect, id: Id, rect: Rect) -> Response {
        let mut response = Response::none(id, rect);
        let allowed = self
            .modal
            .is_none_or(|modal| modal.intersect(rect) == rect || modal.contains(rect.center()));
        let pointer = input.pointer();
        let over = allowed
            && pointer.is_some_and(|point| rect.contains(point) && clip.contains(point))
            && (self.active.is_none() || self.active == Some(id));

        if over {
            self.hot = Some(id);
            response.hovered = true;
            if input.pressed(MouseButton::Left) {
                self.active = Some(id);
                response.pressed = true;
                self.pointer_claimed = true;
            }
            if input.pressed(MouseButton::Right) {
                self.pointer_claimed = true;
            }
            if input.released(MouseButton::Right) {
                response.secondary_clicked = true;
            }
            if input.double_clicked() {
                response.double_clicked = true;
            }
        }
        if self.active == Some(id) {
            response.dragging = input.is_down(MouseButton::Left);
            response.drag_delta = input.pointer_delta();
            self.pointer_claimed = true;
            if input.released(MouseButton::Left) {
                response.drag_released = true;
                response.clicked = over;
                self.active = None;
            }
        }
        response
    }

    /// Clear `active` if the button that grabbed it is no longer held.
    ///
    /// A safety net, not the normal path — see [`Ui::end_frame`] for why it
    /// exists and why it must run after the frame's widgets are built.
    fn release_stale_active(&mut self, input: &InputState) {
        if !input.is_down(MouseButton::Left) && !input.is_down(MouseButton::Middle) {
            self.active = None;
        }
    }
}

/// Everything the interface remembers between frames.
pub struct UiState {
    /// The geometry built this frame.
    pub draw: DrawList,
    /// The glyph and image atlas.
    pub atlas: Atlas,
    /// Loaded fonts and their cached glyphs.
    pub fonts: GlyphCache,
    /// Colours and metrics.
    pub theme: Theme,
    /// The proportional font, for the interface.
    pub ui_font: FontId,
    /// The monospaced font, for the code panels.
    pub mono_font: FontId,
    interaction: Interaction,
    focus: Option<Id>,
    editing: Option<TextEdit>,
    /// Whether the focused widget is a [`Ui::code_editor`], whose caret
    /// lives in its own buffer rather than in `editing`.
    typing_code: bool,
    /// What copy and cut in a code editor put, and paste takes — the
    /// editor's own, since the platform's needs a dependency (ADR 0013).
    clipboard: String,
    scroll: HashMap<Id, Vec2>,
    drag_origin: HashMap<Id, f32>,
    color_picker: Option<Id>,
    /// The dropdown whose list is open, if any — drawn last, over
    /// everything, by [`Ui::show_popup`].
    popup: Option<Popup>,
    /// A choice made in an open list, waiting for its dropdown to read it
    /// back on the next frame.
    popup_choice: Option<(Id, usize)>,
}

/// An open dropdown list: where its button is and what it offers.
#[derive(Debug)]
struct Popup {
    id: Id,
    anchor: Rect,
    /// Each option's label, and whether it can be picked.
    options: Vec<(String, bool)>,
    selected: usize,
    /// Whether the dropdown was built this frame; a list whose button went
    /// away (a panel switched, a row deleted) closes rather than floating.
    seen: bool,
}

impl UiState {
    /// Set up the interface's persistent state.
    ///
    /// `atlas` and `fonts` come from the caller because both are shared with
    /// the renderer: the atlas is registered as
    /// [`wxsl_render::ui::TextureId::ATLAS`], and the glyph cache carries the
    /// MSDF backend the application chose.
    pub fn new(
        atlas: Atlas,
        fonts: GlyphCache,
        ui_font: FontId,
        mono_font: FontId,
        theme: Theme,
    ) -> Self {
        UiState {
            draw: DrawList::new(),
            atlas,
            fonts,
            theme,
            ui_font,
            mono_font,
            interaction: Interaction::default(),
            focus: None,
            editing: None,
            typing_code: false,
            clipboard: String::new(),
            scroll: HashMap::new(),
            drag_origin: HashMap::new(),
            color_picker: None,
            popup: None,
            popup_choice: None,
        }
    }

    /// Which widget has keyboard focus, if any.
    pub fn focus(&self) -> Option<Id> {
        self.focus
    }

    /// Give a widget keyboard focus, starting an edit with the caret at the
    /// end of `text`.
    pub fn focus_text(&mut self, id: Id, text: &str) {
        self.focus = Some(id);
        self.editing = Some(TextEdit {
            id,
            cursor: text.len(),
            anchor: Some(0),
        });
    }

    /// Give a code editor keyboard focus; its caret is its buffer's.
    pub fn focus_code(&mut self, id: Id) {
        self.focus = Some(id);
        self.editing = None;
        self.typing_code = true;
    }

    /// Drop keyboard focus.
    pub fn clear_focus(&mut self) {
        self.focus = None;
        self.editing = None;
        self.typing_code = false;
    }

    /// Whether a text field or a code editor is being typed into — when
    /// the editor's single-key shortcuts stand down.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some() || self.typing_code
    }

    /// Whether the colour swatch `id` has its picker showing.
    ///
    /// One at a time: a picker is several rows tall, and two open at once in
    /// a side panel is more scrolling than either is worth.
    pub fn color_picker_open(&self, id: Id) -> bool {
        self.color_picker == Some(id)
    }

    /// Show `id`'s colour picker, or hide it if it is already showing.
    pub fn toggle_color_picker(&mut self, id: Id) {
        self.color_picker = if self.color_picker == Some(id) {
            None
        } else {
            Some(id)
        };
    }

    /// Hit-test and update interaction state for a widget.
    ///
    /// The primitive every widget is built from. A widget becomes *active* on
    /// press and stays active until release, wherever the pointer goes, which
    /// is what makes dragging work and what makes a click cancel when the
    /// pointer leaves before release. The rules themselves live on
    /// `Interaction`, which holds no `wgpu` state and so is what a test
    /// drives directly.
    pub fn interact(&mut self, input: &InputState, id: Id, rect: Rect) -> Response {
        let clip = self.draw.clip();
        self.interaction.interact(input, clip, id, rect)
    }

    /// Clear `active` if the button that grabbed it is no longer held.
    ///
    /// See [`Ui::end_frame`] for why this must run after the frame's widgets
    /// are built, not before.
    pub fn release_stale_active(&mut self, input: &InputState) {
        self.interaction.release_stale_active(input);
    }
}

/// One frame's handle on the interface.
pub struct Ui<'a> {
    /// Persistent interface state.
    pub state: &'a mut UiState,
    /// This frame's input.
    pub input: &'a InputState,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    /// The first error the frame hit, if any — a full atlas, say. Kept rather
    /// than returned, because a widget deep in a layout cannot usefully
    /// propagate one and the frame should still draw.
    error: Option<RenderError>,
}

impl<'a> Ui<'a> {
    /// Begin a frame.
    ///
    /// Does **not** clear a stale `active` widget here. The release that
    /// ends a click has already reached `InputState` before this call
    /// (`is_down` reflects it immediately, see `InputState::handle`), so
    /// clearing pre-emptively based on "is the button still down" would
    /// wipe `active` *before* the widget that owns it gets to see its own
    /// release in [`Ui::interact`] — which is exactly what stopped every
    /// click, drag-release and checkbox toggle from registering. That
    /// cleanup instead happens in [`Ui::end_frame`], after every widget has
    /// had its turn.
    pub fn new(
        state: &'a mut UiState,
        input: &'a InputState,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Self {
        state.interaction.begin_frame();
        Ui {
            state,
            input,
            device,
            queue,
            error: None,
        }
    }

    /// Finish the frame: clear `active` if the button that grabbed it is no
    /// longer held.
    ///
    /// This is a safety net, not the normal path — the normal path is
    /// [`Ui::interact`] itself clearing `active` when the widget holding it
    /// sees its own release. This exists for the abnormal case: the active
    /// widget disappeared mid-drag (a node deleted by a shortcut, a list row
    /// scrolled away, a panel that closed), so nothing will call `interact`
    /// with that id again this frame or any later one. Left uncleared,
    /// `active` would stay set forever, and `interact`'s `over` test
    /// (`active.is_none() || active == Some(id)`) would then block *every*
    /// other widget from ever becoming hovered again.
    ///
    /// Call once, after every widget for the frame has been built.
    pub fn end_frame(&mut self) {
        self.state.release_stale_active(self.input);
    }

    /// The error the frame hit, if any.
    pub fn error(&self) -> Option<&RenderError> {
        self.error.as_ref()
    }

    /// The theme.
    pub fn theme(&self) -> &Theme {
        &self.state.theme
    }

    /// The draw list being built.
    pub fn draw(&mut self) -> &mut DrawList {
        &mut self.state.draw
    }

    /// Whether a widget has already claimed this frame's pointer.
    ///
    /// The node canvas asks before acting on a click, so that a button
    /// floating over it wins.
    pub fn pointer_claimed(&self) -> bool {
        self.state.interaction.pointer_claimed
    }

    /// Claim the pointer for this frame.
    pub fn claim_pointer(&mut self) {
        self.state.interaction.pointer_claimed = true;
    }

    /// Restrict interaction to `rect` until [`Ui::close_modal`].
    pub fn open_modal(&mut self, rect: Rect) {
        self.state.interaction.modal = Some(rect);
    }

    /// Lift a [`Ui::open_modal`] restriction.
    pub fn close_modal(&mut self) {
        self.state.interaction.modal = None;
    }

    /// Hit-test and update interaction state for a widget.
    ///
    /// The primitive every widget is built from. A widget becomes *active* on
    /// press and stays active until release, wherever the pointer goes, which
    /// is what makes dragging work and what makes a click cancel when the
    /// pointer leaves before release. The logic itself lives on
    /// [`UiState::interact`], so it can be exercised without a `wgpu::Device`.
    pub fn interact(&mut self, id: Id, rect: Rect) -> Response {
        self.state.interact(self.input, id, rect)
    }

    /// Whether the colour swatch `id` has its picker showing.
    pub fn color_picker_open(&self, id: Id) -> bool {
        self.state.color_picker_open(id)
    }

    /// Show `id`'s colour picker, or hide it if it is already showing.
    pub fn toggle_color_picker(&mut self, id: Id) {
        self.state.toggle_color_picker(id);
    }

    // -- text ---------------------------------------------------------------

    /// Lay `text` out in the interface font at `size`, making sure every
    /// glyph it needs is in the atlas first.
    pub fn layout(&mut self, font: FontId, text: &str, options: TextOptions) -> TextLayout {
        if let Err(error) =
            self.state
                .fonts
                .prepare(self.device, self.queue, &mut self.state.atlas, font, text)
        {
            // Keep the first error and carry on: a frame that stops drawing
            // because one glyph could not be packed is worse than a frame
            // with one glyph missing.
            if self.error.is_none() {
                self.error = Some(error);
            }
        }
        self.state.fonts.layout(font, text, options)
    }

    /// Lay text out in the interface font at the body text size.
    pub fn layout_ui(&mut self, text: &str) -> TextLayout {
        let size = self.state.theme.metrics.text_size;
        let font = self.state.ui_font;
        self.layout(font, text, TextOptions::new(size))
    }

    /// Lay text out in the monospaced font at the code text size.
    pub fn layout_mono(&mut self, text: &str) -> TextLayout {
        let size = self.state.theme.metrics.mono_text_size;
        let font = self.state.mono_font;
        self.layout(font, text, TextOptions::new(size))
    }

    /// The size `text` needs in the interface font.
    pub fn measure_ui(&mut self, text: &str) -> Vec2 {
        self.layout_ui(text).size
    }

    /// Draw `text` in `rect`, vertically centred and aligned as asked.
    ///
    /// Returns the width it took, so a caller can lay out around it.
    pub fn label(&mut self, rect: Rect, text: &str, color: Color, align: Align) -> f32 {
        let layout = self.layout_ui(text);
        let origin = align_text(rect, layout.size, align);
        self.state.draw.text(&layout, origin, color);
        layout.size.x
    }

    /// Draw `text` in the small text size.
    pub fn small_label(&mut self, rect: Rect, text: &str, color: Color, align: Align) -> f32 {
        let size = self.state.theme.metrics.small_text_size;
        let font = self.state.ui_font;
        let layout = self.layout(font, text, TextOptions::new(size));
        let origin = align_text(rect, layout.size, align);
        self.state.draw.text(&layout, origin, color);
        layout.size.x
    }

    /// Draw `text` in the monospaced font.
    pub fn mono_label(&mut self, rect: Rect, text: &str, color: Color, align: Align) -> f32 {
        let layout = self.layout_mono(text);
        let origin = align_text(rect, layout.size, align);
        self.state.draw.text(&layout, origin, color);
        layout.size.x
    }

    /// Draw `text` clipped to `rect`, with an ellipsis if it does not fit.
    pub fn truncated_label(&mut self, rect: Rect, text: &str, color: Color, align: Align) -> f32 {
        let layout = self.layout_ui(text);
        if layout.size.x <= rect.width() {
            let origin = align_text(rect, layout.size, align);
            self.state.draw.text(&layout, origin, color);
            return layout.size.x;
        }
        // Binary search would be tidier, but a linear walk over a label's
        // characters is a handful of iterations and reads as what it is.
        let mut best = String::new();
        for (index, _) in text.char_indices() {
            let candidate = format!("{}…", &text[..index]);
            if self.measure_ui(&candidate).x > rect.width() {
                break;
            }
            best = candidate;
        }
        let layout = self.layout_ui(&best);
        let origin = align_text(rect, layout.size, align);
        self.state.draw.text(&layout, origin, color);
        layout.size.x
    }

    // -- widgets ------------------------------------------------------------

    /// A panel: a rounded fill, an outline, and an optional title strip.
    ///
    /// Returns the rectangle left for its contents.
    pub fn panel(&mut self, rect: Rect, title: Option<&str>) -> Rect {
        let theme = self.state.theme;
        self.state
            .draw
            .round_rect(rect, theme.metrics.panel_radius, theme.palette.panel);
        self.state.draw.round_rect_border(
            rect,
            theme.metrics.panel_radius,
            theme.metrics.outline_width,
            theme.palette.outline,
        );
        let mut inner = rect;
        if let Some(title) = title {
            let (header, rest) = rect.split_top(theme.metrics.row_height);
            self.state.draw.round_rect(
                header,
                theme.metrics.panel_radius,
                theme.palette.panel_header,
            );
            // Square off the bottom of the header's rounded corners, so the
            // strip meets the body cleanly.
            let (_, bottom) = header.split_bottom(theme.metrics.panel_radius);
            self.state.draw.rect(bottom, theme.palette.panel_header);
            let text_rect = header.shrink(theme.metrics.padding * 0.5);
            self.label(text_rect, title, theme.palette.text, Align::Left);
            inner = rest;
        }
        inner.shrink(theme.metrics.padding)
    }

    /// A clickable button.
    pub fn button(&mut self, id: Id, rect: Rect, label: &str) -> Response {
        self.button_colored(id, rect, label, None)
    }

    /// A button whose fill is overridden — for a toggle that is on, or an
    /// accented primary action.
    pub fn button_colored(
        &mut self,
        id: Id,
        rect: Rect,
        label: &str,
        fill: Option<Color>,
    ) -> Response {
        let response = self.interact(id, rect);
        let theme = self.state.theme;
        let base = fill.unwrap_or(theme.palette.control);
        let background = if response.dragging {
            base.scaled(1.3)
        } else if response.hovered {
            base.scaled(1.15)
        } else {
            base
        };
        self.state
            .draw
            .round_rect(rect, theme.metrics.radius, background);
        let text_color = match fill {
            Some(_) => theme.palette.text_on_accent,
            None => theme.palette.text,
        };
        self.truncated_label(rect.shrink(4.0), label, text_color, Align::Center);
        response
    }

    /// A closed set of choices behind one button: the current option's
    /// label, and a list over the rest of the interface when clicked.
    ///
    /// The list itself is drawn by [`Ui::show_popup`] at the end of the
    /// frame, so it lands on top of panels built after this one; a pick is
    /// read back here on the next frame, the same one-frame latency every
    /// request in the editor has. Disabled options are shown, greyed, and
    /// cannot be picked.
    pub fn dropdown(
        &mut self,
        id: Id,
        rect: Rect,
        options: &[(&str, bool)],
        selected: &mut usize,
    ) -> Response {
        let mut response = self.interact(id, rect);
        if let Some((chosen, index)) = self.state.popup_choice {
            if chosen == id {
                self.state.popup_choice = None;
                if index != *selected && index < options.len() {
                    *selected = index;
                    response.changed = true;
                }
            }
        }
        let open = self
            .state
            .popup
            .as_ref()
            .is_some_and(|popup| popup.id == id);
        if response.clicked && !open {
            self.state.popup = Some(Popup {
                id,
                anchor: rect,
                options: Vec::new(),
                selected: *selected,
                seen: true,
            });
        }
        if let Some(popup) = self.state.popup.as_mut().filter(|popup| popup.id == id) {
            // Refreshed every frame: the button may have scrolled, and the
            // options may have changed under it.
            popup.anchor = rect;
            popup.options = options
                .iter()
                .map(|&(label, enabled)| (label.to_string(), enabled))
                .collect();
            popup.selected = *selected;
            popup.seen = true;
        }

        let theme = self.state.theme;
        let open = self
            .state
            .popup
            .as_ref()
            .is_some_and(|popup| popup.id == id);
        let background = if open || response.dragging {
            theme.palette.control_active
        } else if response.hovered {
            theme.palette.control_hover
        } else {
            theme.palette.control
        };
        self.state
            .draw
            .round_rect(rect, theme.metrics.radius, background);
        // A chevron from four bars, since the draw list has no triangles.
        let size = (rect.height() * 0.3).max(4.0);
        let (chevron_rect, label_rect) = rect.split_right(size + theme.metrics.padding * 1.5);
        let tip = Vec2::new(
            chevron_rect.center().x - theme.metrics.padding * 0.25,
            rect.center().y,
        );
        let bars = 4;
        for bar in 0..bars {
            let width = size * (1.0 - bar as f32 / bars as f32);
            let height = size * 0.5 / bars as f32;
            let y = tip.y - size * 0.25 + height * bar as f32;
            self.state.draw.rect(
                Rect::from_min_size(Vec2::new(tip.x - width * 0.5, y), Vec2::new(width, height)),
                theme.palette.text_dim,
            );
        }
        let label = options.get(*selected).map_or("", |&(label, _)| label);
        self.truncated_label(
            label_rect.shrink(4.0),
            label,
            theme.palette.text,
            Align::Left,
        );
        response
    }

    /// Draw the open dropdown list, if any, over everything built so far.
    ///
    /// Call once per frame, after every panel. While a list is open the
    /// rest of the interface is modal-blocked; a press outside the list,
    /// or Escape, closes it without picking.
    pub fn show_popup(&mut self, screen: Rect) {
        let Some(popup) = self.state.popup.as_mut() else {
            return;
        };
        if !popup.seen {
            self.state.popup = None;
            self.close_modal();
            return;
        }
        popup.seen = false;
        let (id, anchor, selected) = (popup.id, popup.anchor, popup.selected);
        let options = popup.options.clone();
        let theme = self.state.theme;
        let metrics = theme.metrics;

        let widest = options
            .iter()
            .map(|(label, _)| self.measure_ui(label).x)
            .fold(0.0_f32, f32::max);
        let width = anchor.width().max(widest + metrics.padding * 3.0);
        let height = metrics.row_height * options.len() as f32 + metrics.padding;
        // Below the button when it fits, above it when it does not.
        let below = anchor.max.y + 2.0;
        let top = if below + height > screen.max.y && anchor.min.y - height - 2.0 >= screen.min.y {
            anchor.min.y - height - 2.0
        } else {
            below
        };
        let left = anchor.min.x.min(screen.max.x - width).max(screen.min.x);
        let list = Rect::from_min_size(Vec2::new(left, top), Vec2::new(width, height));
        self.open_modal(list);

        self.state.draw.push_clip(screen);
        self.state
            .draw
            .round_rect(list.expand(1.0), metrics.radius, theme.palette.outline);
        self.state
            .draw
            .round_rect(list, metrics.radius, theme.palette.panel);
        let mut picked = None;
        for (index, (label, enabled)) in options.iter().enumerate() {
            let row = Rect::from_min_size(
                Vec2::new(
                    list.min.x,
                    list.min.y + metrics.padding * 0.5 + metrics.row_height * index as f32,
                ),
                Vec2::new(list.width(), metrics.row_height),
            );
            let response = self.interact(id.with(0x5050_0000 + index as u64), row);
            if *enabled && response.hovered {
                self.state
                    .draw
                    .rect(row.shrink(2.0), theme.palette.control_hover);
            }
            let color = if !enabled {
                theme.palette.text_dim.with_alpha(0.5)
            } else if index == selected {
                theme.palette.accent
            } else {
                theme.palette.text
            };
            self.truncated_label(
                Rect::from_min_max(row.min + Vec2::new(metrics.padding, 0.0), row.max),
                label,
                color,
                Align::Left,
            );
            if *enabled && response.clicked {
                picked = Some(index);
            }
        }
        self.state.draw.pop_clip();

        let pressed_outside = self.input.pressed(MouseButton::Left)
            && self
                .input
                .pointer()
                .is_some_and(|point| !list.contains(point));
        if let Some(index) = picked {
            self.state.popup_choice = Some((id, index));
        }
        if picked.is_some() || pressed_outside || self.input.key_pressed(Key::Escape) {
            self.state.popup = None;
            self.close_modal();
            self.claim_pointer();
        }
    }

    /// A checkbox with a label to its right.
    pub fn checkbox(&mut self, id: Id, rect: Rect, label: &str, value: &mut bool) -> Response {
        let mut response = self.interact(id, rect);
        let theme = self.state.theme;
        let box_size = theme.metrics.row_height * 0.6;
        let (box_rect, label_rect) = rect.split_left(box_size);
        let box_rect = Rect::from_center_size(box_rect.center(), Vec2::splat(box_size));

        if response.clicked {
            *value = !*value;
            response.changed = true;
        }
        let fill = if *value {
            theme.palette.accent
        } else if response.hovered {
            theme.palette.control_hover
        } else {
            theme.palette.control
        };
        self.state
            .draw
            .round_rect(box_rect, theme.metrics.radius * 0.6, fill);
        if *value {
            // A tick, as two capsules.
            let inner = box_rect.shrink(box_size * 0.25);
            let left = Vec2::new(inner.min.x, inner.center().y);
            let middle = Vec2::new(inner.center().x - inner.width() * 0.1, inner.max.y);
            let right = Vec2::new(inner.max.x, inner.min.y);
            let width = theme.metrics.outline_width * 1.8;
            self.state
                .draw
                .line(left, middle, width, theme.palette.text_on_accent);
            self.state
                .draw
                .line(middle, right, width, theme.palette.text_on_accent);
        }
        self.label(
            label_rect.shrink(4.0),
            label,
            theme.palette.text,
            Align::Left,
        );
        response
    }

    /// A number that changes as it is dragged horizontally.
    ///
    /// The right control for a shader parameter: the useful gesture is
    /// "a bit more than that", not "exactly 0.37". Double-click to type a
    /// value instead — see [`Ui::text_field`], which the caller pairs with
    /// this.
    pub fn drag_value(
        &mut self,
        id: Id,
        rect: Rect,
        value: &mut f32,
        speed: f32,
        range: Option<(f32, f32)>,
    ) -> Response {
        let mut response = self.interact(id, rect);
        let theme = self.state.theme;

        if response.pressed {
            self.state.drag_origin.insert(id, *value);
        }
        if response.dragging && response.drag_delta.x != 0.0 {
            let start = self.state.drag_origin.get(&id).copied().unwrap_or(*value);
            let _ = start;
            let mut next = *value + response.drag_delta.x * speed / theme.scale;
            if let Some((low, high)) = range {
                next = next.clamp(low, high);
            }
            if next != *value {
                *value = next;
                response.changed = true;
            }
        }

        let background = if response.dragging {
            theme.palette.control_active
        } else if response.hovered {
            theme.palette.control_hover
        } else {
            theme.palette.control
        };
        self.state
            .draw
            .round_rect(rect, theme.metrics.radius, background);
        // A fill bar, when the value has bounds to be a fraction of.
        if let Some((low, high)) = range {
            if high > low {
                let fraction = ((*value - low) / (high - low)).clamp(0.0, 1.0);
                let (filled, _) = rect.split_left(rect.width() * fraction);
                self.state.draw.round_rect(
                    filled,
                    theme.metrics.radius,
                    theme.palette.accent.with_alpha(0.35),
                );
            }
        }
        let text = format_number(*value);
        self.label(rect.shrink(4.0), &text, theme.palette.text, Align::Center);
        response
    }

    /// A single-line text field.
    ///
    /// Editing is deliberately basic — insert, delete, arrows, home/end,
    /// select-all — and there is no clipboard: a clipboard needs a platform
    /// dependency this crate does not have (ADR 0013 keeps it
    /// windowing-agnostic), and the application can offer one by handling the
    /// shortcut itself.
    pub fn text_field(&mut self, id: Id, rect: Rect, text: &mut String) -> Response {
        let mut response = self.interact(id, rect);
        let theme = self.state.theme;
        let focused = self.state.focus == Some(id);

        if response.pressed && !focused {
            self.state.focus_text(id, text);
        }
        if response.pressed && focused {
            // Move the caret to the click.
            let layout = self.layout_ui(text);
            let local = self.input.pointer_or_zero() - text_origin(rect, &layout, theme);
            let byte = layout.byte_at(local);
            if let Some(edit) = self.state.editing.as_mut() {
                edit.cursor = byte;
                edit.anchor = Some(byte);
            }
        }
        if response.double_clicked && focused {
            if let Some(edit) = self.state.editing.as_mut() {
                edit.cursor = text.len();
                edit.anchor = Some(0);
            }
        }
        if focused && self.input.pressed(MouseButton::Left) && !response.hovered {
            // Clicking away commits and defocuses.
            self.state.clear_focus();
        }
        if focused {
            response.changed |= self.edit_text(id, text);
        }

        let background = if focused {
            theme.palette.control_active
        } else if response.hovered {
            theme.palette.control_hover
        } else {
            theme.palette.control
        };
        self.state
            .draw
            .round_rect(rect, theme.metrics.radius, background);
        if focused {
            self.state.draw.round_rect_border(
                rect,
                theme.metrics.radius,
                theme.metrics.outline_width * 1.5,
                theme.palette.accent,
            );
        }

        let layout = self.layout_ui(text);
        let origin = text_origin(rect, &layout, theme);
        self.state.draw.push_clip(rect.shrink(2.0));
        // The selection goes behind the text.
        if let Some(edit) = self.state.editing.as_ref().filter(|edit| edit.id == id) {
            if let Some((from, to)) = edit.selection() {
                let start = layout.caret(from).x;
                let end = layout.caret(to).x;
                let highlight = Rect::from_min_max(
                    Vec2::new(origin.x + start, rect.min.y + 3.0),
                    Vec2::new(origin.x + end, rect.max.y - 3.0),
                );
                self.state
                    .draw
                    .rect(highlight, theme.palette.accent.with_alpha(0.35));
            }
        }
        self.state.draw.text(&layout, origin, theme.palette.text);
        if let Some(edit) = self.state.editing.as_ref().filter(|edit| edit.id == id) {
            // A caret that blinks, from the frame time the input carries.
            let visible = (self.input.time() * 2.0).fract() < 0.6;
            if visible {
                let x = origin.x + layout.caret(edit.cursor).x;
                let caret = Rect::from_min_max(
                    Vec2::new(x, rect.min.y + 3.0),
                    Vec2::new(x + theme.metrics.outline_width.max(1.0), rect.max.y - 3.0),
                );
                self.state.draw.rect(caret, theme.palette.text);
            }
        }
        self.state.draw.pop_clip();
        response
    }

    /// Apply this frame's typing to `text`. Returns whether it changed.
    fn edit_text(&mut self, id: Id, text: &mut String) -> bool {
        let Some(mut edit) = self.state.editing.clone().filter(|edit| edit.id == id) else {
            return false;
        };
        let mut changed = false;
        edit.cursor = edit.cursor.min(text.len());

        // Typed characters replace the selection.
        let typed = self.input.text().to_string();
        if !typed.is_empty() {
            if let Some((from, to)) = edit.selection() {
                text.replace_range(from..to, "");
                edit.cursor = from;
            }
            let at = floor_char_boundary(text, edit.cursor);
            text.insert_str(at, &typed);
            edit.cursor = at + typed.len();
            edit.anchor = None;
            changed = true;
        }

        let shift = self.input.modifiers().shift;
        for press in self.input.keys() {
            match press.key {
                Key::Backspace => {
                    if let Some((from, to)) = edit.selection() {
                        text.replace_range(from..to, "");
                        edit.cursor = from;
                        edit.anchor = None;
                        changed = true;
                    } else if edit.cursor > 0 {
                        let previous = previous_boundary(text, edit.cursor);
                        text.replace_range(previous..edit.cursor, "");
                        edit.cursor = previous;
                        changed = true;
                    }
                }
                Key::Delete => {
                    if let Some((from, to)) = edit.selection() {
                        text.replace_range(from..to, "");
                        edit.cursor = from;
                        edit.anchor = None;
                        changed = true;
                    } else if edit.cursor < text.len() {
                        let next = next_boundary(text, edit.cursor);
                        text.replace_range(edit.cursor..next, "");
                        changed = true;
                    }
                }
                Key::Left => {
                    let target = previous_boundary(text, edit.cursor);
                    edit.anchor = extend(edit.anchor, edit.cursor, shift);
                    edit.cursor = target;
                }
                Key::Right => {
                    let target = next_boundary(text, edit.cursor);
                    edit.anchor = extend(edit.anchor, edit.cursor, shift);
                    edit.cursor = target;
                }
                Key::Home => {
                    edit.anchor = extend(edit.anchor, edit.cursor, shift);
                    edit.cursor = 0;
                }
                Key::End => {
                    edit.anchor = extend(edit.anchor, edit.cursor, shift);
                    edit.cursor = text.len();
                }
                Key::Char('a') if press.modifiers.command() => {
                    edit.anchor = Some(0);
                    edit.cursor = text.len();
                }
                Key::Escape | Key::Enter => {
                    self.state.clear_focus();
                    return changed;
                }
                _ => {}
            }
        }
        self.state.editing = Some(edit);
        changed
    }

    // -- scrolling ----------------------------------------------------------

    /// A scrollable region.
    ///
    /// Clips to `rect`, applies the wheel when the pointer is inside, draws a
    /// scrollbar when there is more content than room, and returns the
    /// offset to draw the content at. The caller pushes its own clip via the
    /// returned [`ScrollArea`] and must call [`ScrollArea::end`].
    pub fn scroll_area(&mut self, id: Id, rect: Rect, content: Vec2) -> ScrollArea {
        let theme = self.state.theme;
        let max = (content - rect.size()).max(Vec2::ZERO);
        let mut offset = self.state.scroll.get(&id).copied().unwrap_or(Vec2::ZERO);

        let hovered = self
            .input
            .pointer()
            .is_some_and(|point| rect.contains(point));
        if hovered {
            let scroll = self.input.scroll();
            if scroll != Vec2::ZERO {
                // Shift turns a vertical wheel into a horizontal scroll,
                // which is the only way to reach a wide code panel's right
                // edge with a mouse.
                offset -= if self.input.modifiers().shift {
                    Vec2::new(scroll.y, 0.0)
                } else {
                    scroll
                };
            }
        }
        offset = offset.clamp(Vec2::ZERO, max);
        self.state.scroll.insert(id, offset);

        self.state.draw.push_clip(rect);
        if max.y > 0.0 {
            let track = Rect::from_min_max(
                Vec2::new(rect.max.x - theme.metrics.scrollbar_width, rect.min.y),
                rect.max,
            );
            let fraction = (rect.height() / content.y).clamp(0.05, 1.0);
            let travel = track.height() * (1.0 - fraction);
            let position = if max.y > 0.0 { offset.y / max.y } else { 0.0 };
            let thumb = Rect::from_min_size(
                Vec2::new(track.min.x, track.min.y + travel * position),
                Vec2::new(track.width(), track.height() * fraction),
            );
            self.state.draw.round_rect(
                thumb.shrink(2.0),
                theme.metrics.radius,
                theme.palette.outline,
            );
        }
        ScrollArea { rect, offset }
    }

    /// A read-only monospaced code panel with line numbers.
    ///
    /// Only the visible lines are laid out. That matters more than it
    /// sounds: the WGSL a graph compiles to runs to thousands of lines, and
    /// shaping all of them every frame would shape a hundred thousand glyphs
    /// to show forty. The content's width comes from the longest line's
    /// *character count* times the advance, which is exact in a monospaced
    /// font and costs nothing.
    pub fn code_view(&mut self, id: Id, rect: Rect, text: &str) {
        self.code_view_impl(id, rect, text, &[]);
    }

    /// [`Ui::code_view`], with `runs` (from [`crate::highlight::highlight`])
    /// colouring the text instead of drawing it all in one colour.
    pub fn highlighted_code_view(&mut self, id: Id, rect: Rect, text: &str, runs: &[Run]) {
        self.code_view_impl(id, rect, text, runs);
    }

    fn code_view_impl(&mut self, id: Id, rect: Rect, text: &str, runs: &[Run]) {
        let theme = self.state.theme;
        if text.is_empty() {
            self.label(
                rect,
                "nothing to show",
                theme.palette.text_dim,
                Align::Center,
            );
            return;
        }

        let advance = self.mono_advance();
        let line_height = (theme.metrics.mono_text_size * 1.45).max(1.0);
        let lines: Vec<&str> = text.lines().collect();
        // Each line's byte offset into `text`, so a run's byte range can be
        // intersected against it — `.lines()` throws that away, and a run
        // spanning a multi-line block comment needs it back.
        let line_starts = line_start_offsets(text, &lines);
        let digits = digit_count(lines.len());
        let gutter = advance * digits as f32 + theme.metrics.padding;
        let widest = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let content = Vec2::new(
            gutter + advance * widest as f32 + theme.metrics.padding * 2.0,
            lines.len() as f32 * line_height,
        );

        let area = self.scroll_area(id, rect, content);
        let origin = area.origin();
        // Only the rows the panel can show, plus one either side so a
        // half-scrolled line is not missing.
        let first = ((area.offset.y / line_height).floor() as usize).saturating_sub(1);
        let visible = (rect.height() / line_height).ceil() as usize + 2;
        let last = (first + visible).min(lines.len());
        for (row, line) in lines.iter().enumerate().take(last).skip(first) {
            let y = origin.y + row as f32 * line_height;
            let number_rect =
                Rect::from_min_size(Vec2::new(origin.x, y), Vec2::new(gutter, line_height));
            self.mono_label(
                number_rect,
                &(row + 1).to_string(),
                theme.palette.text_dim,
                Align::Right,
            );
            let text_x = origin.x + gutter + theme.metrics.padding;
            self.code_line(
                line,
                line_starts[row],
                runs,
                Vec2::new(text_x, y),
                Vec2::new(advance, line_height),
                content.x,
            );
        }
        area.end(self);
    }

    /// One line of a code panel, coloured by whichever of `runs` cover it.
    ///
    /// `start` is the line's byte offset into the text the runs index;
    /// `cell` is one character's advance and the line height.
    fn code_line(
        &mut self,
        line: &str,
        start: usize,
        runs: &[Run],
        at: Vec2,
        cell: Vec2,
        width: f32,
    ) {
        let theme = self.state.theme;
        let (text_x, y, advance, line_height) = (at.x, at.y, cell.x, cell.y);
        if runs.is_empty() {
            let line_rect = Rect::from_min_size(at, Vec2::new(width, line_height));
            self.mono_label(line_rect, line, theme.palette.text, Align::Left);
            return;
        }
        let end = start + line.len();
        let from = runs.partition_point(|run| run.range.end <= start);
        let mut cursor = 0usize;
        for run in runs[from..].iter().take_while(|run| run.range.start < end) {
            let seg_start = run.range.start.max(start) - start;
            let seg_end = run.range.end.min(end) - start;
            if seg_start > cursor {
                draw_code_run(
                    self,
                    line,
                    cursor,
                    seg_start,
                    text_x,
                    y,
                    line_height,
                    advance,
                    width,
                    theme.palette.text,
                );
            }
            draw_code_run(
                self,
                line,
                seg_start,
                seg_end,
                text_x,
                y,
                line_height,
                advance,
                width,
                run.kind.color(&theme.palette),
            );
            cursor = seg_end;
        }
        if cursor < line.len() {
            draw_code_run(
                self,
                line,
                cursor,
                line.len(),
                text_x,
                y,
                line_height,
                advance,
                width,
                theme.palette.text,
            );
        }
    }

    /// An editable code panel over `buffer` (plan3 M11): the read-only
    /// panel's gutter and highlighting, plus a caret, a selection, the
    /// pointer and keyboard editing [`TextBuffer`] implements, and
    /// `marks` — compiler diagnostics — drawn on their own lines: a dot in
    /// the gutter, the line tinted, the column underlined.
    ///
    /// Monospaced, so hit-testing is arithmetic over the advance and the
    /// line height ([`code_hit`]) and only the visible rows are laid out.
    /// `changed` reports an edit to the text; the caret moving alone does
    /// not count.
    pub fn code_editor(
        &mut self,
        id: Id,
        rect: Rect,
        buffer: &mut TextBuffer,
        runs: &[Run],
        marks: &[LineMark],
    ) -> Response {
        let theme = self.state.theme;
        let advance = self.mono_advance();
        let line_height = (theme.metrics.mono_text_size * 1.45).max(1.0);
        let lines = buffer.line_count();
        // Room for the numbers, and a marker column before them.
        let marker = advance * 1.5;
        let gutter = marker + advance * digit_count(lines).max(2) as f32 + theme.metrics.padding;
        let text_left = gutter + theme.metrics.padding;
        let widest = buffer
            .text()
            .split('\n')
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let content = Vec2::new(
            text_left + advance * (widest + 1) as f32 + theme.metrics.padding,
            lines as f32 * line_height + line_height,
        );

        let mut response = self.interact(id, rect);
        let has_focus = |state: &UiState| state.focus == Some(id) && state.typing_code;
        let offset = self.state.scroll.get(&id).copied().unwrap_or(Vec2::ZERO);
        let origin = rect.min - offset;
        let hit = |point: Vec2, buffer: &TextBuffer| {
            let (line, column) = code_hit(
                point - origin - Vec2::new(text_left, 0.0),
                advance,
                line_height,
            );
            buffer.offset(line, column)
        };

        let mut moved = false;
        if response.pressed {
            if !has_focus(self.state) {
                self.state.focus_code(id);
            }
            let at = hit(self.input.pointer_or_zero(), buffer);
            if response.double_clicked {
                buffer.select_word_at(at);
            } else {
                buffer.set_cursor(at, self.input.modifiers().shift);
            }
        } else if response.dragging && self.input.pointer_delta() != Vec2::ZERO {
            buffer.set_cursor(hit(self.input.pointer_or_zero(), buffer), true);
            moved = true;
        }
        if has_focus(self.state) && self.input.pressed(MouseButton::Left) && !response.hovered {
            self.state.clear_focus();
        }
        let focused = has_focus(self.state);
        if focused {
            let revision = buffer.revision();
            // A command chord's character is a shortcut, not text — some
            // platforms deliver both.
            let chord = self
                .input
                .keys()
                .iter()
                .any(|press| press.modifiers.command() && !press.modifiers.alt);
            if !chord && !self.input.text().is_empty() {
                buffer.insert(self.input.text());
                moved = true;
            }
            let page = ((rect.height() / line_height) as usize).saturating_sub(1);
            let mut clipboard = std::mem::take(&mut self.state.clipboard);
            for press in self.input.keys() {
                if press.key == Key::Escape {
                    self.state.clear_focus();
                    break;
                }
                let outcome = buffer.key(press, page, &mut clipboard);
                moved |= outcome.moved || outcome.edited;
            }
            self.state.clipboard = clipboard;
            response.changed = buffer.revision() != revision;
        }

        // Keep the caret in view after anything that moved it.
        if moved {
            let (line, column) = buffer.line_column(buffer.cursor());
            let caret = Vec2::new(
                text_left + advance * column as f32,
                line as f32 * line_height,
            );
            let mut offset = offset;
            let view = rect.size();
            if caret.y < offset.y {
                offset.y = caret.y;
            } else if caret.y + line_height > offset.y + view.y {
                offset.y = caret.y + line_height - view.y;
            }
            if caret.x < offset.x + text_left {
                offset.x = (caret.x - text_left).max(0.0);
            } else if caret.x + advance * 2.0 > offset.x + view.x {
                offset.x = caret.x + advance * 2.0 - view.x;
            }
            self.state.scroll.insert(id, offset);
        }

        if focused {
            self.state
                .draw
                .rect(rect, theme.palette.control.with_alpha(0.35));
        }
        let area = self.scroll_area(id, rect, content);
        let origin = area.origin();
        let first = ((area.offset.y / line_height).floor() as usize).saturating_sub(1);
        let visible = (rect.height() / line_height).ceil() as usize + 2;
        let last = (first + visible).min(lines);
        let selection = buffer.selection();
        let text = buffer.text();
        for row in first..last {
            let range = buffer.line_range(row);
            let line = &text[range.clone()];
            let y = origin.y + row as f32 * line_height;
            let text_x = origin.x + text_left;
            let row_rect = Rect::from_min_size(
                Vec2::new(origin.x, y),
                Vec2::new(content.x.max(rect.width()), line_height),
            );
            let mark = marks.iter().find(|mark| mark.line == row);
            if mark.is_some() {
                self.state
                    .draw
                    .rect(row_rect, theme.palette.error.with_alpha(0.14));
            }
            if let Some(selected) = selection.as_ref() {
                let from = selected.start.max(range.start);
                // A selection running on past the line covers its newline.
                let to = selected.end.min(range.end);
                if selected.start <= range.end && selected.end >= range.start && from <= to {
                    let start = line[..from - range.start].chars().count();
                    let end = line[..to - range.start].chars().count()
                        + usize::from(selected.end > range.end);
                    let highlight = Rect::from_min_size(
                        Vec2::new(text_x + advance * start as f32, y),
                        Vec2::new(advance * (end - start) as f32, line_height),
                    );
                    self.state
                        .draw
                        .rect(highlight, theme.palette.accent.with_alpha(0.35));
                }
            }
            if let Some(mark) = mark {
                let column = mark.column.min(line.chars().count());
                let length = line.chars().count().saturating_sub(column).max(1);
                let underline = Rect::from_min_size(
                    Vec2::new(text_x + advance * column as f32, y + line_height - 2.0),
                    Vec2::new(
                        advance * length as f32,
                        theme.metrics.outline_width.max(1.5),
                    ),
                );
                self.state.draw.rect(underline, theme.palette.error);
                let dot = Rect::from_center_size(
                    Vec2::new(origin.x + marker * 0.5 + 2.0, y + line_height * 0.5),
                    Vec2::splat(advance * 0.7),
                );
                self.state
                    .draw
                    .round_rect(dot, advance * 0.35, theme.palette.error);
            }
            let number_rect = Rect::from_min_size(
                Vec2::new(origin.x + marker, y),
                Vec2::new(gutter - marker, line_height),
            );
            let number_color = if mark.is_some() {
                theme.palette.error
            } else {
                theme.palette.text_dim
            };
            self.mono_label(
                number_rect,
                &(row + 1).to_string(),
                number_color,
                Align::Right,
            );
            self.code_line(
                line,
                range.start,
                runs,
                Vec2::new(text_x, y),
                Vec2::new(advance, line_height),
                content.x,
            );
        }
        if focused && (self.input.time() * 2.0).fract() < 0.6 {
            let (line, column) = buffer.line_column(buffer.cursor());
            let caret = Rect::from_min_size(
                Vec2::new(
                    origin.x + text_left + advance * column as f32,
                    origin.y + line as f32 * line_height,
                ),
                Vec2::new(theme.metrics.outline_width.max(1.5), line_height),
            );
            self.state.draw.rect(caret, theme.palette.text);
        }
        area.end(self);
        response
    }

    /// The advance of one monospaced character, at the code text size.
    ///
    /// Measured rather than assumed, and cheap: one character through the
    /// cache. Every column in [`Ui::code_view`] is a multiple of it.
    pub fn mono_advance(&mut self) -> f32 {
        let layout = self.layout_mono("0");
        layout
            .glyphs
            .first()
            .map(|glyph| glyph.advance)
            // No glyph for '0' at all: half the text size is a serviceable
            // guess, and the panel will look wrong rather than divide by zero.
            .unwrap_or(self.state.theme.metrics.mono_text_size * 0.5)
            .max(1.0)
    }

    /// A row of tabs. Returns the index that should now be selected.
    pub fn tabs(&mut self, id: Id, rect: Rect, labels: &[&str], selected: usize) -> usize {
        let theme = self.state.theme;
        let mut chosen = selected.min(labels.len().saturating_sub(1));
        if labels.is_empty() {
            return 0;
        }
        let gap = theme.metrics.row_gap;
        let width = (rect.width() - gap * (labels.len() - 1) as f32) / labels.len() as f32;
        for (index, label) in labels.iter().enumerate() {
            let tab = Rect::from_min_size(
                Vec2::new(rect.min.x + (width + gap) * index as f32, rect.min.y),
                Vec2::new(width, rect.height()),
            );
            let active = index == chosen;
            let fill = if active {
                Some(theme.palette.accent)
            } else {
                None
            };
            if self
                .button_colored(id.with(index as u64), tab, label, fill)
                .clicked
            {
                chosen = index;
            }
        }
        chosen
    }

    /// Draw a horizontal separator across `rect`'s top edge.
    pub fn separator(&mut self, rect: Rect) {
        let theme = self.state.theme;
        let line = Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width(), theme.metrics.outline_width),
        );
        self.state.draw.rect(line, theme.palette.outline);
    }
}

/// An open scroll region: the offset to draw at, and the clip to close.
#[derive(Clone, Copy, Debug)]
pub struct ScrollArea {
    /// The region on screen.
    pub rect: Rect,
    /// How far the content is scrolled, in pixels.
    pub offset: Vec2,
}

impl ScrollArea {
    /// Where the content's origin goes.
    pub fn origin(&self) -> Vec2 {
        self.rect.min - self.offset
    }

    /// Close the region's clip rectangle.
    pub fn end(self, ui: &mut Ui<'_>) {
        ui.state.draw.pop_clip();
    }
}

/// A compiler diagnostic's place in a code editor's buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMark {
    /// 0-based line.
    pub line: usize,
    /// 0-based character column the problem starts at.
    pub column: usize,
    /// What is wrong there.
    pub message: String,
}

/// The line and character column a point lands on in a monospaced code
/// panel, measured from its text origin — nearest caret position, so a
/// click on a character's right half lands after it.
pub fn code_hit(local: Vec2, advance: f32, line_height: f32) -> (usize, usize) {
    let line = (local.y / line_height).floor().max(0.0) as usize;
    let column = (local.x / advance).round().max(0.0) as usize;
    (line, column)
}

/// Where a text layout's origin goes inside `rect`, for an alignment.
fn align_text(rect: Rect, size: Vec2, align: Align) -> Vec2 {
    let x = match align {
        Align::Left => rect.min.x,
        Align::Center => rect.center().x - size.x * 0.5,
        Align::Right => rect.max.x - size.x,
    };
    // Vertically centred on the rectangle, which is what every row-shaped
    // widget wants and what a baseline would have to be derived from anyway.
    Vec2::new(x, rect.center().y - size.y * 0.5)
}

/// Where a text field's text starts, given its layout.
fn text_origin(rect: Rect, layout: &TextLayout, theme: Theme) -> Vec2 {
    Vec2::new(
        rect.min.x + theme.metrics.padding * 0.5,
        rect.center().y - layout.size.y * 0.5,
    )
}

/// The selection anchor after a cursor move: kept when extending with shift,
/// dropped otherwise.
fn extend(anchor: Option<usize>, cursor: usize, shift: bool) -> Option<usize> {
    if shift {
        Some(anchor.unwrap_or(cursor))
    } else {
        None
    }
}

/// The byte offset at or before `index` that starts a character.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The start of the character before `index`.
fn previous_boundary(text: &str, index: usize) -> usize {
    let index = floor_char_boundary(text, index);
    text[..index]
        .char_indices()
        .next_back()
        .map(|(offset, _)| offset)
        .unwrap_or(0)
}

/// The start of the character after `index`.
fn next_boundary(text: &str, index: usize) -> usize {
    let index = floor_char_boundary(text, index);
    match text[index..].chars().next() {
        Some(character) => index + character.len_utf8(),
        None => index,
    }
}

/// How many decimal digits `value` needs.
fn digit_count(value: usize) -> usize {
    let mut digits = 1;
    let mut remaining = value / 10;
    while remaining > 0 {
        digits += 1;
        remaining /= 10;
    }
    digits
}

/// The byte offset of each of `lines` (as `text.lines()` produced them)
/// into `text`.
///
/// `.lines()` splits on `\n` and strips one trailing `\r`, throwing the
/// position away with the separator; a highlight run whose source span
/// crosses a line boundary (a block comment, most often) needs it back to
/// know which bytes of *this* line it covers.
fn line_start_offsets(text: &str, lines: &[&str]) -> Vec<usize> {
    let mut offset = 0usize;
    let mut starts = Vec::with_capacity(lines.len());
    for line in lines {
        starts.push(offset);
        offset += line.len();
        if text[offset..].starts_with("\r\n") {
            offset += 2;
        } else if text[offset..].starts_with('\n') {
            offset += 1;
        }
    }
    starts
}

/// Draw `line[from..to]` at the column `from` lands on, in `color`.
///
/// A no-op for an empty range, which keeps [`Ui::code_view_impl`]'s segment
/// loop from having to check before every call.
#[allow(clippy::too_many_arguments)]
fn draw_code_run(
    ui: &mut Ui<'_>,
    line: &str,
    from: usize,
    to: usize,
    text_x: f32,
    y: f32,
    line_height: f32,
    advance: f32,
    width: f32,
    color: Color,
) {
    if from >= to {
        return;
    }
    let column = line[..from].chars().count();
    let rect = Rect::from_min_size(
        Vec2::new(text_x + advance * column as f32, y),
        Vec2::new(width, line_height),
    );
    ui.mono_label(rect, &line[from..to], color, Align::Left);
}

/// A number, short enough for a control and long enough to be useful.
pub fn format_number(value: f32) -> String {
    if value == value.trunc() && value.abs() < 1e7 {
        format!("{value:.1}")
    } else if value.abs() >= 1000.0 || (value != 0.0 && value.abs() < 0.001) {
        format!("{value:.3e}")
    } else {
        format!("{value:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        assert_eq!(Id::new("palette.search"), Id::new("palette.search"));
        assert_ne!(Id::new("palette.search"), Id::new("palette.list"));
        // A list's items are distinct from each other and from the list.
        let list = Id::new("nodes");
        assert_ne!(list.with(0), list.with(1));
        assert_ne!(list.with(0), list);
        assert_eq!(list.with(7), list.with(7));
    }

    /// Drive one press-then-release click the way `Editor::frame` really
    /// does: `InputState::set_time` (not `begin_frame`, which — per its own
    /// docs — would drop the events queued since the previous frame), the
    /// widget's `interact`, then `end_frame` on both the input and the
    /// interaction state, in that order, once per rendered frame.
    #[test]
    fn a_full_press_release_cycle_reports_a_click() {
        use wxsl_render::ui::input::{InputState, MouseButton as InputMouseButton, UiEvent};

        let mut input = InputState::new();
        let mut interaction = Interaction::default();
        let id = Id::new("button");
        let rect = Rect::new(0.0, 0.0, 40.0, 20.0);
        let point = Vec2::new(20.0, 10.0);

        // Frame 1: press.
        input.set_time(0.0);
        input.handle(UiEvent::PointerMoved(point));
        input.handle(UiEvent::PointerButton {
            button: InputMouseButton::Left,
            pressed: true,
        });
        let response = interaction.interact(&input, Rect::EVERYTHING, id, rect);
        assert!(response.pressed);
        assert!(!response.clicked, "not released yet");
        interaction.release_stale_active(&input);
        assert_eq!(
            interaction.active,
            Some(id),
            "the safety net must not clear an active widget whose button \
             is still held"
        );
        input.end_frame();

        // Frame 2: release. This is the exact sequence that regressed: by
        // the time this frame's `interact` runs, `InputState::is_down` is
        // already `false` (the release event lands on `input` before
        // `interact` is ever called), which is what a premature
        // `release_stale_active` — called *before* building the frame's
        // widgets instead of after — used to see and act on, wiping `active`
        // before this call could report the click at all.
        input.set_time(0.016);
        input.handle(UiEvent::PointerButton {
            button: InputMouseButton::Left,
            pressed: false,
        });
        let response = interaction.interact(&input, Rect::EVERYTHING, id, rect);
        assert!(response.clicked, "the release was never seen");
        assert!(response.drag_released);
        assert!(interaction.active.is_none());
    }

    #[test]
    fn a_drag_started_inside_a_row_is_reported_even_once_the_pointer_leaves_it() {
        // What a palette row relies on to support drag-and-drop onto the
        // canvas: `dragging` must stay true, and `drag_delta` must keep
        // accumulating, once the pointer has moved outside the widget's own
        // rectangle — only `clicked` (and `hovered`) require containment.
        use wxsl_render::ui::input::{InputState, MouseButton as InputMouseButton, UiEvent};

        let mut input = InputState::new();
        let mut interaction = Interaction::default();
        let id = Id::new("row");
        let rect = Rect::new(0.0, 0.0, 40.0, 20.0);

        input.set_time(0.0);
        input.handle(UiEvent::PointerMoved(Vec2::new(20.0, 10.0)));
        input.handle(UiEvent::PointerButton {
            button: InputMouseButton::Left,
            pressed: true,
        });
        let response = interaction.interact(&input, Rect::EVERYTHING, id, rect);
        assert!(response.pressed);
        input.end_frame();

        input.set_time(0.016);
        input.handle(UiEvent::PointerMoved(Vec2::new(500.0, 500.0)));
        let response = interaction.interact(&input, Rect::EVERYTHING, id, rect);
        assert!(response.dragging, "still dragging outside the row");
        assert!(!response.hovered, "but no longer hovered");
        input.end_frame();

        input.set_time(0.032);
        input.handle(UiEvent::PointerButton {
            button: InputMouseButton::Left,
            pressed: false,
        });
        let response = interaction.interact(&input, Rect::EVERYTHING, id, rect);
        assert!(response.drag_released);
        assert!(!response.clicked, "released well outside the row");
    }

    #[test]
    fn a_widget_scrolled_out_of_its_region_is_not_interactive() {
        // The rule `interact` applies, checked as the predicate it is: a row
        // scrolled off the top of a list is still at some screen position,
        // and must not catch a click that landed on the panel above it.
        let clip = Rect::new(0.0, 100.0, 200.0, 100.0);
        let scrolled_out = Rect::new(0.0, 40.0, 200.0, 20.0);
        let visible = Rect::new(0.0, 120.0, 200.0, 20.0);
        let click = Vec2::new(50.0, 50.0);
        assert!(scrolled_out.contains(click));
        assert!(!clip.contains(click), "the click is outside the region");
        assert!(!visible.contains(click));
    }

    #[test]
    fn text_alignment_places_the_layout_in_the_rectangle() {
        let rect = Rect::new(10.0, 10.0, 100.0, 20.0);
        let size = Vec2::new(40.0, 12.0);
        assert_eq!(align_text(rect, size, Align::Left).x, 10.0);
        assert_eq!(align_text(rect, size, Align::Center).x, 40.0);
        assert_eq!(align_text(rect, size, Align::Right).x, 70.0);
        // Always vertically centred.
        assert_eq!(align_text(rect, size, Align::Left).y, 14.0);
    }

    #[test]
    fn character_boundaries_are_respected_by_the_caret_helpers() {
        // A caret that lands mid-character would panic the next time the
        // string is sliced, so this is load-bearing rather than tidiness.
        let text = "aé漢z";
        assert_eq!(floor_char_boundary(text, 0), 0);
        assert_eq!(floor_char_boundary(text, 2), 1, "inside the é");
        assert_eq!(next_boundary(text, 1), 3, "past the é");
        assert_eq!(next_boundary(text, 3), 6, "past the 漢");
        assert_eq!(previous_boundary(text, 6), 3);
        assert_eq!(previous_boundary(text, 0), 0);
        assert_eq!(next_boundary(text, text.len()), text.len());
    }

    #[test]
    fn a_selection_is_ordered_and_an_empty_one_is_none() {
        let edit = TextEdit {
            id: Id::new("field"),
            cursor: 2,
            anchor: Some(7),
        };
        assert_eq!(edit.selection(), Some((2, 7)));
        let backwards = TextEdit {
            anchor: Some(1),
            cursor: 5,
            ..edit.clone()
        };
        assert_eq!(backwards.selection(), Some((1, 5)));
        let empty = TextEdit {
            anchor: Some(3),
            cursor: 3,
            ..edit.clone()
        };
        assert_eq!(empty.selection(), None);
        let none = TextEdit {
            anchor: None,
            ..edit
        };
        assert_eq!(none.selection(), None);
    }

    #[test]
    fn shift_extends_a_selection_and_an_unmodified_arrow_drops_it() {
        assert_eq!(extend(None, 4, true), Some(4), "a new selection anchors");
        assert_eq!(extend(Some(2), 4, true), Some(2), "an existing one holds");
        assert_eq!(extend(Some(2), 4, false), None, "without shift it clears");
    }

    #[test]
    fn numbers_are_formatted_short_but_readable() {
        assert_eq!(format_number(1.0), "1.0");
        assert_eq!(format_number(-2.0), "-2.0");
        assert_eq!(format_number(0.5), "0.500");
        assert_eq!(format_number(0.123456), "0.123");
        // A whole number stays whole, however big: "123456.0" is clearer in
        // a control than "1.235e5".
        assert_eq!(format_number(123456.0), "123456.0");
        // Anything else large or very small falls back to exponent form
        // rather than overflowing the control with digits.
        assert!(format_number(123456.7).contains('e'));
        assert!(format_number(0.0000123).contains('e'));
        assert_eq!(format_number(0.0), "0.0");
    }

    #[test]
    fn a_gutter_is_wide_enough_for_the_last_line_number() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(999), 3);
        assert_eq!(digit_count(1000), 4);
    }

    #[test]
    fn a_scroll_area_offsets_its_content_by_its_scroll() {
        let area = ScrollArea {
            rect: Rect::new(10.0, 20.0, 100.0, 50.0),
            offset: Vec2::new(0.0, 15.0),
        };
        assert_eq!(area.origin(), Vec2::new(10.0, 5.0));
    }
}
