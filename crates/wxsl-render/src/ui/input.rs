//! Input, without a windowing library.
//!
//! `wxsl-render` and `wxsl-editor` never depend on winit (ADR 0013): the
//! application translates whatever its own event loop produces into
//! [`UiEvent`]s and feeds them to [`InputState`], which accumulates them into
//! the "what happened since the last frame" shape an immediate-mode interface
//! actually consults. `crates/wxsl/examples/editor.rs` is the winit half of
//! that translation, and it is the only file in the workspace that knows
//! winit exists.
//!
//! Everything here is in *physical* pixels, matching the draw list and the
//! render target. The scale factor is carried along ([`InputState::scale`])
//! so that a UI can size text in logical pixels, but no coordinate is ever
//! silently converted.

use glam::Vec2;

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Select, drag, click.
    Left,
    /// Context menu; on the node canvas, also pan.
    Right,
    /// Pan.
    Middle,
}

impl MouseButton {
    /// All three, in declaration order.
    pub const ALL: &'static [MouseButton] =
        &[MouseButton::Left, MouseButton::Right, MouseButton::Middle];

    fn index(self) -> usize {
        match self {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
        }
    }
}

/// Which modifier keys are held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// The Windows or Command key.
    pub logo: bool,
}

impl Modifiers {
    /// Nothing held.
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        logo: false,
    };

    /// Control alone.
    pub const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };

    /// Shift alone.
    pub const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    /// Control and shift.
    pub const CTRL_SHIFT: Modifiers = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::NONE
    };

    /// The "command" modifier for the platform: Command on macOS, Control
    /// everywhere else.
    ///
    /// Read from what is actually held rather than from a compile-time
    /// target, so a shortcut works the way the user's keyboard is labelled.
    pub fn command(&self) -> bool {
        self.ctrl || self.logo
    }

    /// Whether nothing is held.
    pub fn is_none(&self) -> bool {
        *self == Modifiers::NONE
    }
}

/// A key, as an interface cares about it.
///
/// Deliberately small: printable keys are [`Key::Char`] with the *unshifted*
/// lowercase character, so a shortcut is written `Key::Char('s')` and matches
/// however the layout spells the shift key. Text entry does not come through
/// here at all — that is [`UiEvent::Text`], which is what an IME and a dead
/// key produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable key, lowercase.
    Char(char),
    /// Escape.
    Escape,
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete / forward delete.
    Delete,
    /// Insert.
    Insert,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// A function key, `F(1)` through `F(12)`.
    Function(u8),
}

/// One key press, with the modifiers that were held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    /// The key.
    pub key: Key,
    /// Modifiers held at the time.
    pub modifiers: Modifiers,
    /// Whether this is an auto-repeat rather than a fresh press.
    pub repeat: bool,
}

/// Something the application's event loop observed.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// The pointer moved to a position in physical pixels.
    PointerMoved(Vec2),
    /// A pointer button changed state.
    PointerButton {
        /// Which button.
        button: MouseButton,
        /// Down, or up.
        pressed: bool,
    },
    /// The pointer left the window, so nothing is hovered.
    PointerLeft,
    /// A scroll gesture, in physical pixels of content.
    Scroll(Vec2),
    /// A key changed state.
    Key {
        /// Which key.
        key: Key,
        /// Down, or up.
        pressed: bool,
        /// Whether the press is an auto-repeat.
        repeat: bool,
    },
    /// A character was typed. Control characters are filtered out on the way
    /// in, so a text field can insert whatever arrives.
    Text(char),
    /// The modifier state changed.
    ModifiersChanged(Modifiers),
    /// The window was resized, in physical pixels, at this scale factor.
    Resized {
        /// New size in physical pixels.
        size: Vec2,
        /// Physical pixels per logical pixel.
        scale: f32,
    },
    /// The window gained or lost keyboard focus.
    FocusChanged(bool),
}

/// What has happened since the last frame.
///
/// Fed by [`InputState::handle`] as events arrive, and reset by
/// [`InputState::begin_frame`]. The queries are the vocabulary an
/// immediate-mode widget is written in: is the pointer here, was this button
/// pressed this frame, was this shortcut typed.
#[derive(Clone, Debug)]
pub struct InputState {
    pointer: Option<Vec2>,
    previous_pointer: Option<Vec2>,
    down: [bool; 3],
    pressed: [bool; 3],
    released: [bool; 3],
    press_position: [Vec2; 3],
    scroll: Vec2,
    keys: Vec<KeyPress>,
    text: String,
    modifiers: Modifiers,
    size: Vec2,
    scale: f32,
    focused: bool,
    time: f64,
    last_click: Option<(f64, Vec2)>,
    double_clicked: bool,
}

impl Default for InputState {
    fn default() -> Self {
        InputState {
            pointer: None,
            previous_pointer: None,
            down: [false; 3],
            pressed: [false; 3],
            released: [false; 3],
            press_position: [Vec2::ZERO; 3],
            scroll: Vec2::ZERO,
            keys: Vec::new(),
            text: String::new(),
            modifiers: Modifiers::NONE,
            size: Vec2::new(1280.0, 720.0),
            scale: 1.0,
            focused: true,
            time: 0.0,
            last_click: None,
            double_clicked: false,
        }
    }
}

impl InputState {
    /// How far apart in time two clicks can be and still be a double click.
    pub const DOUBLE_CLICK_SECONDS: f64 = 0.4;

    /// How far apart on screen they can be, in physical pixels.
    pub const DOUBLE_CLICK_SLOP: f32 = 6.0;

    /// Fresh state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the frame's timestamp, in seconds.
    ///
    /// Call *before* consuming the frame's input, not after: a caret blinks
    /// off this, and a double click is measured against it.
    pub fn set_time(&mut self, time: f64) {
        self.time = time;
    }

    /// Clear everything that only applies to one frame.
    ///
    /// Call *after* the frame has consumed the input, which is the ordering
    /// that matters and the one easy to get backwards: events arrive between
    /// frames, so clearing at the start of a frame throws away the clicks
    /// that just happened. Button *down* state and the pointer position
    /// survive; presses, releases, scroll, typed text and key presses do not.
    pub fn end_frame(&mut self) {
        self.previous_pointer = self.pointer;
        self.pressed = [false; 3];
        self.released = [false; 3];
        self.scroll = Vec2::ZERO;
        self.keys.clear();
        self.text.clear();
        self.double_clicked = false;
    }

    /// Fold one event in.
    pub fn handle(&mut self, event: UiEvent) {
        match event {
            UiEvent::PointerMoved(position) => self.pointer = Some(position),
            UiEvent::PointerLeft => self.pointer = None,
            UiEvent::PointerButton { button, pressed } => {
                let index = button.index();
                self.down[index] = pressed;
                if pressed {
                    self.pressed[index] = true;
                    let position = self.pointer.unwrap_or(Vec2::ZERO);
                    self.press_position[index] = position;
                    if button == MouseButton::Left {
                        self.double_clicked = self.last_click.is_some_and(|(when, where_)| {
                            self.time - when <= Self::DOUBLE_CLICK_SECONDS
                                && where_.distance(position) <= Self::DOUBLE_CLICK_SLOP
                        });
                        // A third click starts a new pair rather than
                        // reporting a double click again.
                        self.last_click = if self.double_clicked {
                            None
                        } else {
                            Some((self.time, position))
                        };
                    }
                } else {
                    self.released[index] = true;
                }
            }
            UiEvent::Scroll(delta) => self.scroll += delta,
            UiEvent::Key {
                key,
                pressed,
                repeat,
            } => {
                if pressed {
                    self.keys.push(KeyPress {
                        key,
                        modifiers: self.modifiers,
                        repeat,
                    });
                }
            }
            UiEvent::Text(character) => {
                // Control characters are not text: Enter, Tab and Backspace
                // arrive as keys, and a widget that inserted them verbatim
                // would put a control code in the user's string.
                if !character.is_control() {
                    self.text.push(character);
                }
            }
            UiEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            UiEvent::Resized { size, scale } => {
                self.size = size.max(Vec2::ONE);
                self.scale = scale.max(0.1);
            }
            UiEvent::FocusChanged(focused) => {
                self.focused = focused;
                if !focused {
                    // Otherwise a button held while the window loses focus
                    // stays held forever, and the next click drags something.
                    self.down = [false; 3];
                    self.modifiers = Modifiers::NONE;
                }
            }
        }
    }

    /// The pointer position, or `None` if it is outside the window.
    pub fn pointer(&self) -> Option<Vec2> {
        self.pointer
    }

    /// The pointer position, or the origin if it is outside the window.
    ///
    /// For hit tests, where "nowhere" and "the top left corner" behave the
    /// same as long as nothing is hovered.
    pub fn pointer_or_zero(&self) -> Vec2 {
        self.pointer.unwrap_or(Vec2::ZERO)
    }

    /// How far the pointer moved since the last frame.
    pub fn pointer_delta(&self) -> Vec2 {
        match (self.pointer, self.previous_pointer) {
            (Some(now), Some(before)) => now - before,
            _ => Vec2::ZERO,
        }
    }

    /// Whether `button` is held.
    pub fn is_down(&self, button: MouseButton) -> bool {
        self.down[button.index()]
    }

    /// Whether `button` went down this frame.
    pub fn pressed(&self, button: MouseButton) -> bool {
        self.pressed[button.index()]
    }

    /// Whether `button` came up this frame.
    pub fn released(&self, button: MouseButton) -> bool {
        self.released[button.index()]
    }

    /// Where `button` was last pressed — the anchor a drag is measured from.
    pub fn press_position(&self, button: MouseButton) -> Vec2 {
        self.press_position[button.index()]
    }

    /// Whether the left button was double-clicked this frame.
    pub fn double_clicked(&self) -> bool {
        self.double_clicked
    }

    /// Scroll accumulated this frame, in physical pixels.
    pub fn scroll(&self) -> Vec2 {
        self.scroll
    }

    /// Every key pressed this frame.
    pub fn keys(&self) -> &[KeyPress] {
        &self.keys
    }

    /// Text typed this frame, control characters already filtered out.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The modifiers currently held.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The window size in physical pixels.
    pub fn size(&self) -> Vec2 {
        self.size
    }

    /// Physical pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Whether the window has keyboard focus.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// The time [`Self::begin_frame`] was last given, in seconds.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Whether `key` was pressed this frame, with any modifiers.
    pub fn key_pressed(&self, key: Key) -> bool {
        self.keys.iter().any(|press| press.key == key)
    }

    /// Whether `key` was pressed with no modifiers held.
    pub fn key_pressed_plain(&self, key: Key) -> bool {
        self.keys
            .iter()
            .any(|press| press.key == key && press.modifiers.is_none())
    }

    /// Whether the platform's command modifier plus `key` was pressed.
    ///
    /// Ctrl on Windows and Linux, Command on macOS, decided by what is
    /// actually held rather than by the build target.
    pub fn command_pressed(&self, key: Key) -> bool {
        self.keys
            .iter()
            .any(|press| press.key == key && press.modifiers.command() && !press.modifiers.alt)
    }

    /// Whether `key` was pressed with exactly `modifiers`.
    pub fn shortcut(&self, modifiers: Modifiers, key: Key) -> bool {
        self.keys
            .iter()
            .any(|press| press.key == key && press.modifiers == modifiers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_that_arrives_before_a_frame_survives_until_it_is_consumed() {
        // The ordering bug this API is shaped to prevent: events arrive
        // between frames, so a frame that cleared its input first would drop
        // every click that had just happened.
        let mut input = InputState::new();
        input.set_time(0.0);
        input.handle(UiEvent::Key {
            key: Key::Char('d'),
            pressed: true,
            repeat: false,
        });
        input.handle(UiEvent::Text('d'));
        // …the frame runs here, and sees them.
        assert!(input.key_pressed(Key::Char('d')));
        assert_eq!(input.text(), "d");
        // Only once it is over are they gone.
        input.end_frame();
        assert!(!input.key_pressed(Key::Char('d')));
        assert_eq!(input.text(), "");
    }

    #[test]
    fn a_press_lasts_one_frame_but_the_button_stays_down() {
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        input.handle(UiEvent::PointerMoved(Vec2::new(10.0, 20.0)));
        input.handle(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        assert!(input.pressed(MouseButton::Left));
        assert!(input.is_down(MouseButton::Left));
        assert_eq!(
            input.press_position(MouseButton::Left),
            Vec2::new(10.0, 20.0)
        );

        input.end_frame();
        input.set_time(0.016);
        assert!(!input.pressed(MouseButton::Left), "the press was consumed");
        assert!(input.is_down(MouseButton::Left), "but it is still held");

        input.handle(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: false,
        });
        assert!(input.released(MouseButton::Left));
        assert!(!input.is_down(MouseButton::Left));
    }

    #[test]
    fn the_pointer_delta_is_zero_on_the_frame_it_enters() {
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        input.handle(UiEvent::PointerMoved(Vec2::new(100.0, 100.0)));
        assert_eq!(input.pointer_delta(), Vec2::ZERO, "nothing to compare to");

        input.end_frame();
        input.set_time(0.016);
        input.handle(UiEvent::PointerMoved(Vec2::new(110.0, 100.0)));
        assert_eq!(input.pointer_delta(), Vec2::new(10.0, 0.0));

        // And leaving the window means no delta rather than a jump to zero.
        input.end_frame();
        input.set_time(0.032);
        input.handle(UiEvent::PointerLeft);
        assert_eq!(input.pointer(), None);
        assert_eq!(input.pointer_delta(), Vec2::ZERO);
    }

    #[test]
    fn two_quick_clicks_in_the_same_place_are_a_double_click() {
        let mut input = InputState::new();
        let click = |input: &mut InputState, time: f64, at: Vec2| {
            input.end_frame();
            input.set_time(time);
            input.handle(UiEvent::PointerMoved(at));
            input.handle(UiEvent::PointerButton {
                button: MouseButton::Left,
                pressed: true,
            });
            input.handle(UiEvent::PointerButton {
                button: MouseButton::Left,
                pressed: false,
            });
            input.double_clicked()
        };

        assert!(!click(&mut input, 0.0, Vec2::splat(50.0)));
        assert!(click(&mut input, 0.2, Vec2::splat(51.0)), "quick and close");
        // A third click is not a second double click.
        assert!(!click(&mut input, 0.3, Vec2::splat(51.0)));

        // Too slow: a second later is a second single click.
        assert!(!click(&mut input, 10.0, Vec2::splat(50.0)));
        assert!(!click(&mut input, 11.0, Vec2::splat(50.0)));
        // Too far: quick enough, but not the same place.
        assert!(!click(&mut input, 20.0, Vec2::splat(50.0)));
        assert!(!click(&mut input, 20.1, Vec2::splat(500.0)));
    }

    #[test]
    fn control_characters_are_not_text() {
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        for character in ['h', '\n', 'i', '\t', '\u{8}'] {
            input.handle(UiEvent::Text(character));
        }
        assert_eq!(input.text(), "hi");
    }

    #[test]
    fn shortcuts_match_on_the_exact_modifier_set() {
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        input.handle(UiEvent::ModifiersChanged(Modifiers::CTRL));
        input.handle(UiEvent::Key {
            key: Key::Char('s'),
            pressed: true,
            repeat: false,
        });

        assert!(input.shortcut(Modifiers::CTRL, Key::Char('s')));
        assert!(input.command_pressed(Key::Char('s')));
        assert!(!input.shortcut(Modifiers::NONE, Key::Char('s')));
        assert!(!input.shortcut(Modifiers::CTRL_SHIFT, Key::Char('s')));
        assert!(!input.key_pressed_plain(Key::Char('s')));
        assert!(input.key_pressed(Key::Char('s')));
    }

    #[test]
    fn a_key_release_is_not_a_press() {
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        input.handle(UiEvent::Key {
            key: Key::Escape,
            pressed: false,
            repeat: false,
        });
        assert!(!input.key_pressed(Key::Escape));
        assert!(input.keys().is_empty());
    }

    #[test]
    fn losing_focus_releases_everything_that_was_held() {
        // Otherwise alt-tabbing mid-drag leaves the editor dragging a node
        // it can never let go of.
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        input.handle(UiEvent::ModifiersChanged(Modifiers::CTRL));
        input.handle(UiEvent::PointerButton {
            button: MouseButton::Left,
            pressed: true,
        });
        input.handle(UiEvent::FocusChanged(false));
        assert!(!input.is_down(MouseButton::Left));
        assert!(input.modifiers().is_none());
        assert!(!input.focused());
    }

    #[test]
    fn scroll_accumulates_within_a_frame_and_resets_between_them() {
        let mut input = InputState::new();
        input.end_frame();
        input.set_time(0.0);
        input.handle(UiEvent::Scroll(Vec2::new(0.0, -10.0)));
        input.handle(UiEvent::Scroll(Vec2::new(0.0, -5.0)));
        assert_eq!(input.scroll(), Vec2::new(0.0, -15.0));
        input.end_frame();
        input.set_time(0.016);
        assert_eq!(input.scroll(), Vec2::ZERO);
    }

    #[test]
    fn a_resize_never_reports_a_degenerate_size_or_scale() {
        let mut input = InputState::new();
        input.handle(UiEvent::Resized {
            size: Vec2::ZERO,
            scale: 0.0,
        });
        assert_eq!(input.size(), Vec2::ONE);
        assert!(input.scale() > 0.0);
    }
}
