//! The sliders, drawn over the frame with the renderer's own UI layer
//! (ADR 0013): no GUI toolkit. The layout and the hit-testing are plain
//! functions of the window's size and scale, so they test without a
//! device.

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
    format: fn(f32) -> String,
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

/// Where the panel, each slider's track and the buttons sit.
pub struct Layout {
    pub panel: Rect,
    /// Per slider: the row (what a press grabs) and the track.
    pub rows: Vec<(Rect, Rect)>,
    /// Under the sliders, left: saves the next frame.
    pub screenshot: Rect,
    /// Beside it: saves the scene's state.
    pub save: Rect,
}

pub fn layout(count: usize, scale: f32) -> Layout {
    let s = scale;
    let panel = Rect::new(
        MARGIN * s,
        MARGIN * s,
        WIDTH * s,
        (PADDING * 2.0 + TITLE + ROW * count as f32 + BUTTON) * s,
    );
    let top = panel.min.y + (PADDING + TITLE + ROW * count as f32) * s;
    let half = (WIDTH - 3.0 * PADDING) * 0.5 * s;
    let screenshot = Rect::new(panel.min.x + PADDING * s, top, half, BUTTON * s);
    let save = Rect::new(screenshot.max.x + PADDING * s, top, half, BUTTON * s);
    let rows = (0..count)
        .map(|index| {
            let top = panel.min.y + (PADDING + TITLE + ROW * index as f32) * s;
            let row = Rect::new(
                panel.min.x + PADDING * s,
                top,
                (WIDTH - 2.0 * PADDING) * s,
                ROW * s,
            );
            let track = Rect::new(row.min.x, top + TRACK.0 * s, row.width(), TRACK.1 * s);
            (row, track)
        })
        .collect();
    Layout {
        panel,
        rows,
        screenshot,
        save,
    }
}

/// The sliders and the mouse.
pub struct Panel {
    pub sliders: Vec<Slider>,
    /// The slider being dragged.
    active: Option<usize>,
}

impl Panel {
    pub fn new(sliders: Vec<Slider>) -> Self {
        Panel {
            sliders,
            active: None,
        }
    }

    pub fn value(&self, label: &str) -> f32 {
        self.sliders
            .iter()
            .find(|slider| slider.label == label)
            .map_or(0.0, |slider| slider.value)
    }

    /// A press at `pointer`: true when the panel takes it.
    pub fn press(&mut self, pointer: Vec2, layout: &Layout) -> bool {
        self.active = layout
            .rows
            .iter()
            .position(|(row, _)| row.contains(pointer));
        self.drag(pointer, layout);
        self.active.is_some() || layout.panel.contains(pointer)
    }

    /// The pointer moved; a dragged slider follows it. True when one
    /// changed.
    pub fn drag(&mut self, pointer: Vec2, layout: &Layout) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let track = layout.rows[index].1;
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
        let layout = layout(panel.sliders.len(), scale);
        let list = &mut self.list;
        list.clear();
        let s = scale;
        list.round_rect(layout.panel, 8.0 * s, Color::rgba(0.03, 0.03, 0.05, 0.72));
        list.round_rect_border(
            layout.panel,
            8.0 * s,
            1.0 * s,
            Color::rgba(0.9, 0.3, 0.8, 0.35),
        );
        let mut texts: Vec<(String, Vec2, f32, f32, Color)> = vec![(
            "wxsl drive".to_string(),
            layout.panel.min + Vec2::new(PADDING, PADDING) * s,
            13.0,
            0.0,
            Color::rgb(1.0, 0.45, 0.85),
        )];
        for ((row, track), slider) in layout.rows.iter().zip(&panel.sliders) {
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
        ] {
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
        texts.push((
            status.to_string(),
            Vec2::new(layout.panel.min.x, layout.panel.max.y + 6.0 * s),
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

    fn panel() -> Panel {
        Panel::new(vec![
            Slider::new("speed", 0.0, 100.0, 50.0, |value| format!("{value}")),
            Slider::new("hour", 0.0, 24.0, 12.0, |value| format!("{value}")),
        ])
    }

    #[test]
    fn pressing_a_track_sets_its_slider_and_dragging_follows() {
        let mut panel = panel();
        let layout = layout(2, 2.0);
        let (_, track) = layout.rows[1];
        assert!(panel.press(Vec2::new(track.min.x, track.center().y), &layout));
        assert_eq!(panel.value("hour"), 0.0);
        assert!(panel.drag(Vec2::new(track.max.x + 50.0, 0.0), &layout));
        assert_eq!(panel.value("hour"), 24.0);
        panel.release();
        assert!(!panel.drag(Vec2::new(track.min.x, 0.0), &layout));
        assert_eq!(panel.value("speed"), 50.0);
    }

    #[test]
    fn the_buttons_sit_side_by_side_under_the_last_slider_and_grab_none() {
        let mut panel = panel();
        let layout = layout(2, 2.0);
        for button in [layout.screenshot, layout.save] {
            assert!(button.min.y >= layout.rows[1].0.max.y);
            assert!(button.max.y <= layout.panel.max.y);
            assert!(button.max.x <= layout.panel.max.x);
            assert!(panel.press(button.center(), &layout));
            assert!(!panel.dragging());
        }
        assert!(layout.screenshot.max.x < layout.save.min.x);
    }

    #[test]
    fn a_press_outside_the_panel_is_the_camera_s() {
        let mut panel = panel();
        let layout = layout(2, 1.0);
        assert!(!panel.press(Vec2::new(900.0, 600.0), &layout));
        assert!(!panel.dragging());
    }
}
