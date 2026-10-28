//! The sliders, drawn over the frame with the renderer's own UI layer
//! (ADR 0013): no GUI toolkit. They sit in two columns, the scene's on
//! the left and the quality's on the right, each shown or hidden by a
//! button above it that is always there. The layout and the hit-testing
//! are plain functions of the window's size and scale, so they test
//! without a device.

use std::error::Error;

use glam::Vec2;
use wxsl::render::ui::text::{FontId, GlyphCache, TextOptions};
use wxsl::render::ui::{Atlas, Color, DrawList, Font, MsdfGenerator, Rect, UiRenderer, UiTarget};
use wxsl::render::ShaderLibrary;

/// One slider: a label, a range, and how its value reads.
pub struct Slider {
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub value: f32,
    /// Which column it sits in.
    pub column: Column,
    format: fn(f32) -> String,
}

/// The panel's two columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// Left: what the scene is.
    Scene = 0,
    /// Right: how well it renders.
    Quality = 1,
}

impl Slider {
    pub fn new(
        label: &'static str,
        min: f32,
        max: f32,
        value: f32,
        format: fn(f32) -> String,
    ) -> Self {
        Slider {
            label,
            min,
            max,
            value: value.clamp(min, max),
            column: Column::Scene,
            format,
        }
    }

    /// How `value` reads on this slider.
    pub fn reading(&self, value: f32) -> String {
        (self.format)(value)
    }

    fn fraction(&self) -> f32 {
        (self.value - self.min) / (self.max - self.min)
    }
}

/// The panel's geometry, in logical pixels before the window's scale.
const MARGIN: f32 = 12.0;
const WIDTH: f32 = 230.0;
const PADDING: f32 = 10.0;
const ROW: f32 = 26.0;
const TITLE: f32 = 20.0;
const TEXT: f32 = 11.0;
const BUTTON: f32 = 22.0;
/// Where a slider's track sits under its label, and how thick it is.
const TRACK: (f32, f32) = (16.0, 3.0);

/// A column's toggle: its width, and its gap to the column under it.
const TOGGLE: (f32, f32) = (80.0, 6.0);
/// Each column's title and toggle label.
const TITLES: [&str; 2] = ["scene", "quality"];

/// Where the toggles, the columns, each slider's track and the buttons
/// sit.
pub struct Layout {
    /// Always there, over each column: shows or hides it.
    pub toggles: [Rect; 2],
    /// Each column, when it is shown.
    pub panels: [Option<Rect>; 2],
    /// Per slider, when its column is shown: the row (what a press
    /// grabs) and the track.
    pub rows: Vec<Option<(Rect, Rect)>>,
    /// Under the scene's sliders, left: saves the next frame.
    pub screenshot: Option<Rect>,
    /// Beside it: saves the scene's state.
    pub save: Option<Rect>,
}

impl Layout {
    /// Whether `pointer` is on the panel's toggles or shown columns.
    pub fn contains(&self, pointer: Vec2) -> bool {
        self.toggles.iter().any(|toggle| toggle.contains(pointer))
            || self
                .panels
                .iter()
                .flatten()
                .any(|panel| panel.contains(pointer))
    }
}

/// `panel` laid out in a window `width` pixels wide.
pub fn layout(panel: &Panel, width: f32, scale: f32) -> Layout {
    let s = scale;
    let top = (MARGIN + BUTTON + TOGGLE.1) * s;
    // The quality column hugs the right edge, never over the scene's.
    let left = [
        MARGIN * s,
        (width - (MARGIN + WIDTH) * s).max((2.0 * MARGIN + WIDTH) * s),
    ];
    let toggles = [
        Rect::new(left[0], MARGIN * s, TOGGLE.0 * s, BUTTON * s),
        Rect::new(
            left[1] + (WIDTH - TOGGLE.0) * s,
            MARGIN * s,
            TOGGLE.0 * s,
            BUTTON * s,
        ),
    ];
    let mut rows = vec![None; panel.sliders.len()];
    let mut panels = [None; 2];
    let mut buttons = None;
    for column in [Column::Scene, Column::Quality] {
        let c = column as usize;
        if !panel.shown[c] {
            continue;
        }
        let members: Vec<usize> = (0..panel.sliders.len())
            .filter(|index| panel.sliders[*index].column == column)
            .collect();
        let count = members.len() as f32;
        let extra = if column == Column::Scene { BUTTON } else { 0.0 };
        let rect = Rect::new(
            left[c],
            top,
            WIDTH * s,
            (PADDING * 2.0 + TITLE + ROW * count + extra) * s,
        );
        for (row_index, index) in members.into_iter().enumerate() {
            let y = rect.min.y + (PADDING + TITLE + ROW * row_index as f32) * s;
            let row = Rect::new(
                rect.min.x + PADDING * s,
                y,
                (WIDTH - 2.0 * PADDING) * s,
                ROW * s,
            );
            let track = Rect::new(row.min.x, y + TRACK.0 * s, row.width(), TRACK.1 * s);
            rows[index] = Some((row, track));
        }
        if column == Column::Scene {
            let y = rect.min.y + (PADDING + TITLE + ROW * count) * s;
            let half = (WIDTH - 3.0 * PADDING) * 0.5 * s;
            let screenshot = Rect::new(rect.min.x + PADDING * s, y, half, BUTTON * s);
            let save = Rect::new(screenshot.max.x + PADDING * s, y, half, BUTTON * s);
            buttons = Some((screenshot, save));
        }
        panels[c] = Some(rect);
    }
    Layout {
        toggles,
        panels,
        rows,
        screenshot: buttons.map(|(screenshot, _)| screenshot),
        save: buttons.map(|(_, save)| save),
    }
}

/// The sliders and the mouse.
pub struct Panel {
    pub sliders: Vec<Slider>,
    /// Whether each [`Column`] is shown.
    pub shown: [bool; 2],
    /// The slider being dragged.
    active: Option<usize>,
}

impl Panel {
    pub fn new(sliders: Vec<Slider>) -> Self {
        Panel {
            sliders,
            shown: [true; 2],
            active: None,
        }
    }

    pub fn value(&self, label: &str) -> f32 {
        self.sliders
            .iter()
            .find(|slider| slider.label == label)
            .map_or(0.0, |slider| slider.value)
    }

    /// A press at `pointer`: true when the panel takes it. A toggle
    /// shows or hides its column.
    pub fn press(&mut self, pointer: Vec2, layout: &Layout) -> bool {
        if let Some(column) = layout
            .toggles
            .iter()
            .position(|toggle| toggle.contains(pointer))
        {
            self.shown[column] = !self.shown[column];
            self.active = None;
            return true;
        }
        self.active = layout
            .rows
            .iter()
            .position(|row| row.is_some_and(|(row, _)| row.contains(pointer)));
        self.drag(pointer, layout);
        self.active.is_some() || layout.contains(pointer)
    }

    /// The pointer moved; a dragged slider follows it. True when one
    /// changed.
    pub fn drag(&mut self, pointer: Vec2, layout: &Layout) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let Some((_, track)) = layout.rows[index] else {
            return false;
        };
        let slider = &mut self.sliders[index];
        let t = ((pointer.x - track.min.x) / track.width()).clamp(0.0, 1.0);
        let value = slider.min + (slider.max - slider.min) * t;
        let changed = value != slider.value;
        slider.value = value;
        changed
    }

    pub fn release(&mut self) {
        self.active = None;
    }

    pub fn dragging(&self) -> bool {
        self.active.is_some()
    }
}

/// The UI layer's renderer, atlas and font.
pub struct Overlay {
    renderer: UiRenderer,
    atlas: Atlas,
    glyphs: GlyphCache,
    font: FontId,
    list: DrawList,
}

impl Overlay {
    pub fn new(device: &wgpu::Device, library: &ShaderLibrary) -> Result<Self, Box<dyn Error>> {
        let atlas = Atlas::new(device, 1024);
        let renderer = UiRenderer::new(device, library, &atlas)?;
        let mut glyphs = GlyphCache::new(MsdfGenerator::cpu());
        let font = glyphs.add_font(Font::from_bytes(
            include_bytes!("../../../resources/fonts/Inter-Regular.ttf").to_vec(),
            0,
        )?);
        Ok(Overlay {
            renderer,
            atlas,
            glyphs,
            font,
            list: DrawList::new(),
        })
    }

    /// Draw `panel` (and `status`, a line under it) over `view`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        (width, height): (u32, u32),
        panel: &Panel,
        status: &str,
        scale: f32,
    ) -> Result<(), Box<dyn Error>> {
        let layout = layout(panel, width as f32, scale);
        let list = &mut self.list;
        list.clear();
        let s = scale;
        let mut texts: Vec<(String, Vec2, f32, f32, Color)> = Vec::new();
        for (column, (toggle, shown)) in layout.toggles.iter().zip(panel.shown).enumerate() {
            let alpha = if shown { 0.6 } else { 0.3 };
            list.round_rect(*toggle, 5.0 * s, Color::rgba(0.03, 0.03, 0.05, 0.72));
            list.round_rect_border(*toggle, 5.0 * s, 1.0 * s, Color::rgba(0.9, 0.3, 0.8, alpha));
            let sign = if shown { "−" } else { "+" };
            texts.push((
                format!("{sign} {}", TITLES[column]),
                toggle.center() - Vec2::new(0.0, TEXT * 0.6 * s),
                TEXT,
                0.5,
                Color::rgba(1.0, 0.75, 0.95, alpha + 0.3),
            ));
        }
        for (column, rect) in layout.panels.iter().enumerate() {
            let Some(rect) = rect else {
                continue;
            };
            list.round_rect(*rect, 8.0 * s, Color::rgba(0.03, 0.03, 0.05, 0.72));
            list.round_rect_border(*rect, 8.0 * s, 1.0 * s, Color::rgba(0.9, 0.3, 0.8, 0.35));
            let title = if column == 0 {
                "wxsl drive"
            } else {
                TITLES[column]
            };
            texts.push((
                title.to_string(),
                rect.min + Vec2::new(PADDING, PADDING) * s,
                13.0,
                0.0,
                Color::rgb(1.0, 0.45, 0.85),
            ));
        }
        for (row, slider) in layout.rows.iter().zip(&panel.sliders) {
            let Some((row, track)) = row else {
                continue;
            };
            texts.push((
                slider.label.to_string(),
                row.min,
                TEXT,
                0.0,
                Color::rgb(0.82, 0.84, 0.9),
            ));
            let value = slider.reading(slider.value);
            texts.push((
                value,
                Vec2::new(row.max.x, row.min.y),
                TEXT,
                1.0,
                Color::rgb(0.4, 0.9, 1.0),
            ));
            list.round_rect(*track, 2.0 * s, Color::rgba(1.0, 1.0, 1.0, 0.18));
            let filled = Rect::new(
                track.min.x,
                track.min.y,
                track.width() * slider.fraction(),
                track.height(),
            );
            list.round_rect(filled, 2.0 * s, Color::rgb(0.25, 0.8, 1.0));
            let knob = Vec2::new(filled.max.x, track.center().y);
            list.circle(knob, 5.0 * s, Color::rgb(0.95, 0.97, 1.0));
        }
        for (button, label) in [
            (layout.screenshot, "screenshot"),
            (layout.save, "save state"),
        ]
        .into_iter()
        .filter_map(|(button, label)| button.map(|button| (button, label)))
        {
            list.round_rect(button, 5.0 * s, Color::rgba(0.9, 0.3, 0.8, 0.25));
            list.round_rect_border(button, 5.0 * s, 1.0 * s, Color::rgba(0.9, 0.3, 0.8, 0.6));
            texts.push((
                label.to_string(),
                button.center() - Vec2::new(0.0, TEXT * 0.6 * s),
                TEXT,
                0.5,
                Color::rgb(1.0, 0.75, 0.95),
            ));
        }
        // Under the scene's column, or its toggle when it is hidden.
        let above = layout.panels[0].unwrap_or(layout.toggles[0]);
        texts.push((
            status.to_string(),
            Vec2::new(above.min.x, above.max.y + 6.0 * s),
            10.0,
            0.0,
            Color::rgba(0.85, 0.85, 0.9, 0.8),
        ));
        // Each text's `align` is where `at` falls along its width: 0 left,
        // 1 right.
        for (text, at, size, align, color) in texts {
            self.glyphs
                .prepare(device, queue, &mut self.atlas, self.font, &text)?;
            let layout = self
                .glyphs
                .layout(self.font, &text, TextOptions::new(size * s));
            let origin = at - Vec2::new(layout.width() * align, 0.0);
            list.text(&layout, origin, color);
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("drive ui"),
        });
        self.renderer.render(
            device,
            queue,
            &mut encoder,
            &UiTarget {
                view,
                format,
                width,
                height,
                clear: None,
            },
            list,
        )?;
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wide enough for both columns side by side.
    const WINDOW: f32 = 1600.0;

    fn panel() -> Panel {
        Panel::new(vec![
            Slider::new("speed", 0.0, 100.0, 50.0, |value| format!("{value}")),
            Slider::new("hour", 0.0, 24.0, 12.0, |value| format!("{value}")),
            Slider {
                column: Column::Quality,
                ..Slider::new("quality", 0.0, 4.0, 2.0, |value| format!("{value}"))
            },
        ])
    }

    #[test]
    fn pressing_a_track_sets_its_slider_and_dragging_follows() {
        let mut panel = panel();
        let layout = layout(&panel, WINDOW, 2.0);
        let (_, track) = layout.rows[1].unwrap();
        assert!(panel.press(Vec2::new(track.min.x, track.center().y), &layout));
        assert_eq!(panel.value("hour"), 0.0);
        assert!(panel.drag(Vec2::new(track.max.x + 50.0, 0.0), &layout));
        assert_eq!(panel.value("hour"), 24.0);
        panel.release();
        assert!(!panel.drag(Vec2::new(track.min.x, 0.0), &layout));
        assert_eq!(panel.value("speed"), 50.0);
    }

    #[test]
    fn the_scene_sits_left_and_the_quality_right() {
        let panel = panel();
        let layout = layout(&panel, WINDOW, 1.0);
        let [scene, quality] = layout.panels.map(Option::unwrap);
        assert!(scene.max.x < quality.min.x);
        assert!(quality.max.x <= WINDOW);
        assert!(scene.contains(layout.rows[0].unwrap().0.center()));
        assert!(quality.contains(layout.rows[2].unwrap().0.center()));
        for (toggle, panel) in layout.toggles.iter().zip([scene, quality]) {
            assert!(toggle.max.y <= panel.min.y);
        }
    }

    #[test]
    fn a_toggle_hides_its_column_and_stays_to_show_it_again() {
        let mut panel = panel();
        let toggle = layout(&panel, WINDOW, 1.0).toggles[1];
        assert!(panel.press(toggle.center(), &layout(&panel, WINDOW, 1.0)));
        assert!(!panel.dragging());
        let hidden = layout(&panel, WINDOW, 1.0);
        assert!(hidden.panels[1].is_none() && hidden.rows[2].is_none());
        assert!(hidden.panels[0].is_some() && hidden.rows[0].is_some());
        assert_eq!(hidden.toggles[1], toggle);
        assert!(panel.press(toggle.center(), &hidden));
        assert!(layout(&panel, WINDOW, 1.0).panels[1].is_some());
    }

    #[test]
    fn the_buttons_sit_side_by_side_under_the_last_scene_slider_and_grab_none() {
        let mut panel = panel();
        let layout = layout(&panel, WINDOW, 2.0);
        let scene = layout.panels[0].unwrap();
        let (screenshot, save) = (layout.screenshot.unwrap(), layout.save.unwrap());
        for button in [screenshot, save] {
            assert!(button.min.y >= layout.rows[1].unwrap().0.max.y);
            assert!(button.max.y <= scene.max.y);
            assert!(button.max.x <= scene.max.x);
            assert!(panel.press(button.center(), &layout));
            assert!(!panel.dragging());
        }
        assert!(screenshot.max.x < save.min.x);
    }

    #[test]
    fn a_press_outside_the_panel_is_the_camera_s() {
        let mut panel = panel();
        let layout = layout(&panel, WINDOW, 1.0);
        assert!(!panel.press(Vec2::new(900.0, 600.0), &layout));
        assert!(!panel.dragging());
    }
}
