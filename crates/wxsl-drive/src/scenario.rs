//! A scenario: the drive animated over time, to record a video frame by
//! frame and cut it to music. It is a JSON file of keyframes in seconds,
//! each holding only what it changes: a slider by its label, or the
//! camera by [`CAMERA`]'s names (the `--look`, `--pitch` and `--zoom`
//! options', then the right-drag's turn and tilt).
//!
//! ```json
//! { "fps": 25, "size": [1280, 720], "start": 33.5, "end": 78,
//!   "audio": "../../../resources/music/wxsl.mp3", "at": 0,
//!   "keys": [
//!     { "t": 33.5, "speed": 6, "car color": "hornet", "zoom": 3 },
//!     { "t": 40.5, "ease": "out", "zoom": 0.8 }
//!   ] }
//! ```
//!
//! A property goes from one key that sets it to the next by the later
//! key's `ease` ([`Ease`]) and holds before its first key and after its
//! last. A name is how the slider reads, a paint's for instance, and
//! always steps. `start` and `end` bound what is recorded, `audio` is
//! relative to the file and `at` is where on the road the car is at
//! `start`.
//!
//! In a scenario `speed` is the car's own speed rather than the one it
//! heads for. Where the car is follows from it, integrated from `start`,
//! so any frame renders on its own and the same at any frame rate. A camera property that steps is a cut: the frame
//! after it has no motion blur from the camera's jump.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::Motion;
use crate::ui::Panel;

/// The camera's properties: around the car (yaw, pitch), how far from it,
/// and the camera's own turn (yaw, pitch).
pub const CAMERA: [&str; 5] = ["look", "pitch", "zoom", "turn", "tilt"];

/// Every property's value, by name.
pub type Values = BTreeMap<String, f32>;

/// What a key sets a property to: a number, or how a slider reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Setting {
    Number(f32),
    Name(String),
}

/// How a property goes from the key before to this one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ease {
    #[default]
    Linear,
    /// Slow out of the key before and into this one.
    Smooth,
    /// Slow out of the key before.
    In,
    /// Slow into this one.
    Out,
    /// The value before, then this one at the key.
    Step,
}

impl Ease {
    fn is_linear(&self) -> bool {
        *self == Ease::Linear
    }

    /// How far along, 0 to 1, at `x` of the way.
    fn along(self, x: f32) -> f32 {
        match self {
            Ease::Linear => x,
            Ease::Smooth => x * x * (3.0 - 2.0 * x),
            Ease::In => x * x,
            Ease::Out => 1.0 - (1.0 - x) * (1.0 - x),
            Ease::Step => 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// Seconds into the music.
    pub t: f32,
    #[serde(default, skip_serializing_if = "Ease::is_linear")]
    pub ease: Ease,
    /// What changes here.
    #[serde(flatten)]
    pub set: BTreeMap<String, Setting>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    /// Frames a second, recorded.
    pub fps: f32,
    /// The frame, pixels.
    pub size: [u32; 2],
    /// What is recorded, seconds into the music.
    pub start: f32,
    pub end: f32,
    /// The music, relative to the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<PathBuf>,
    /// Where the car is on the road at `start`, metres.
    #[serde(default)]
    pub at: f32,
    pub keys: Vec<Key>,
}

impl Scenario {
    pub fn load(file: &Path) -> Result<Self, Box<dyn Error>> {
        let text = std::fs::read_to_string(file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        let scenario: Scenario = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not a scenario: {error}", file.display()))?;
        if !(scenario.fps > 0.0 && scenario.end > scenario.start) {
            return Err(format!("{} needs fps > 0 and end > start", file.display()).into());
        }
        Ok(scenario)
    }

    /// Write it back, a key a line.
    pub fn save(&self, file: &Path) -> Result<(), Box<dyn Error>> {
        let head = serde_json::to_string_pretty(&Scenario {
            keys: Vec::new(),
            ..self.clone()
        })?;
        let keys = self
            .keys
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        let text = head.replacen(
            "\"keys\": []",
            &format!("\"keys\": [\n    {}\n  ]", keys.join(",\n    ")),
            1,
        );
        std::fs::write(file, text + "\n")?;
        Ok(())
    }

    /// The music, beside `file`, the scenario's own.
    pub fn audio(&self, file: &Path) -> Option<PathBuf> {
        let audio = self.audio.as_ref()?;
        Some(file.parent().unwrap_or(Path::new(".")).join(audio))
    }

    /// Set `changes` at `t`: into the key there, within half a frame, or
    /// a new one.
    pub fn set(&mut self, t: f32, changes: BTreeMap<String, Setting>) {
        let near = 0.5 / self.fps;
        match self.keys.iter_mut().find(|key| (key.t - t).abs() < near) {
            Some(key) => key.set.extend(changes),
            None => {
                let index = self.keys.partition_point(|key| key.t < t);
                self.keys.insert(
                    index,
                    Key {
                        t,
                        ease: Ease::Linear,
                        set: changes,
                    },
                );
            }
        }
    }
}

/// One key of one property.
#[derive(Clone, Copy, Debug)]
struct Point {
    t: f32,
    value: f32,
    ease: Ease,
}

/// A scenario's keys as numbers, by property.
pub struct Timeline {
    tracks: BTreeMap<String, Vec<Point>>,
}

/// The integration's step, seconds.
const STEP: f32 = 1.0 / 240.0;

impl Timeline {
    /// The keys of `scenario`, its names read on `panel`'s sliders; a
    /// property or a name neither has is refused, with the key's time.
    pub fn new(scenario: &Scenario, panel: &Panel) -> Result<Self, Box<dyn Error>> {
        let mut tracks: BTreeMap<String, Vec<Point>> = BTreeMap::new();
        for key in &scenario.keys {
            for (property, setting) in &key.set {
                let slider = panel.sliders.iter().find(|slider| slider.label == property);
                if slider.is_none() && !CAMERA.contains(&property.as_str()) {
                    return Err(format!("the key at {} s sets `{property}`, which is neither a slider nor one of {}", key.t, CAMERA.join(", ")).into());
                }
                let point = match setting {
                    Setting::Number(value) => Point {
                        t: key.t,
                        value: *value,
                        ease: key.ease,
                    },
                    Setting::Name(name) => {
                        let slider = slider.ok_or_else(|| {
                            format!(
                                "the key at {} s names `{name}` for the camera's `{property}`",
                                key.t
                            )
                        })?;
                        let names = steps(slider.min, slider.max)
                            .map(|value| (value, slider.reading(value)));
                        let value = names
                            .clone()
                            .find(|(_, reading)| reading == name)
                            .map(|(value, _)| value)
                            .ok_or_else(|| {
                                let names: Vec<String> =
                                    names.map(|(_, reading)| reading).collect();
                                format!(
                                    "the key at {} s sets `{property}` to `{name}`, not one of: {}",
                                    key.t,
                                    names.join(", ")
                                )
                            })?;
                        Point {
                            t: key.t,
                            value,
                            ease: Ease::Step,
                        }
                    }
                };
                tracks.entry(property.clone()).or_default().push(point);
            }
        }
        for points in tracks.values_mut() {
            points.sort_by(|a, b| a.t.total_cmp(&b.t));
        }
        Ok(Timeline { tracks })
    }

    /// `property` at `t`, or `None` when no key sets it.
    pub fn value(&self, property: &str, t: f32) -> Option<f32> {
        let points = self.tracks.get(property)?;
        let next = points.partition_point(|point| point.t <= t);
        Some(
            match (
                next.checked_sub(1).map(|index| points[index]),
                points.get(next).copied(),
            ) {
                (Some(before), Some(after)) => {
                    let x = (t - before.t) / (after.t - before.t).max(1e-6);
                    before.value + (after.value - before.value) * after.ease.along(x)
                }
                (Some(point), None) | (None, Some(point)) => point.value,
                (None, None) => unreachable!("a track has a key"),
            },
        )
    }

    /// Whether the camera steps after `from` and by `to`: a cut, which
    /// the motion blur must not smear.
    pub fn cuts(&self, from: f32, to: f32) -> bool {
        CAMERA.iter().any(|property| {
            self.tracks.get(*property).is_some_and(|points| {
                points
                    .iter()
                    .any(|point| point.ease == Ease::Step && from < point.t && point.t <= to)
            })
        })
    }

    /// Every property at `t`: `baseline`, with what the keys set over it.
    pub fn at(&self, t: f32, baseline: &Values) -> Values {
        let mut values = baseline.clone();
        for property in self.tracks.keys() {
            if let Some(value) = self.value(property, t) {
                values.insert(property.clone(), value);
            }
        }
        values
    }

    /// Where the car is at `t`, from `at` metres at `start`: its speed
    /// integrated since, `baseline`'s where no key sets it.
    pub fn motion(&self, start: f32, at: f32, t: f32, baseline: &Values) -> Motion {
        let speed = |t: f32| {
            self.value(crate::SPEED, t)
                .or_else(|| baseline.get(crate::SPEED).copied())
                .unwrap_or(1.0)
                .max(0.0)
                / 3.6
        };
        let steps = ((t - start) / STEP).ceil().max(1.0);
        let h = (t - start).max(0.0) / steps;
        let mut motion = Motion {
            s: at,
            speed: Some(speed(start)),
            time: start,
            ..Motion::default()
        };
        let mut before = speed(start);
        for step in 1..=steps as u32 {
            let now = start + h * step as f32;
            let speed = speed(now);
            // Trapezoids, so that the frame rate does not move the car.
            let travel = 0.5 * (before + speed) * h;
            motion.s += travel;
            motion.odometer += travel;
            motion.time = now;
            if h > 0.0 {
                motion.acceleration = (speed - before) / h;
            }
            let settle = (h / crate::SPRING_LAG).min(1.0);
            motion.sprung += (motion.acceleration - motion.sprung) * settle;
            motion.speed = Some(speed);
            before = speed;
        }
        motion
    }
}

/// The whole numbers from `min` to `max`, a discrete slider's steps.
fn steps(min: f32, max: f32) -> impl Iterator<Item = f32> + Clone {
    (min.ceil() as i32..=max.floor() as i32).map(|step| step as f32)
}

/// How a key writes `value` for `property`: a slider whose every step
/// reads without a digit by its name, anything else as a number.
pub fn setting(panel: &Panel, property: &str, value: f32) -> Setting {
    let named = panel
        .sliders
        .iter()
        .find(|slider| slider.label == property)
        .filter(|slider| {
            steps(slider.min, slider.max)
                .all(|step| !slider.reading(step).contains(|c: char| c.is_ascii_digit()))
        });
    match named {
        Some(slider) => Setting::Name(slider.reading(value.round())),
        None => Setting::Number((value * 1000.0).round() / 1000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Slider;

    fn panel() -> Panel {
        Panel::new(vec![
            Slider::new(crate::SPEED, 0.0, 260.0, 100.0, |value| {
                format!("{value:.0} km/h")
            }),
            Slider::new("car color", 0.0, 2.0, 0.0, |value| {
                ["yellow", "red", "blue"][value.round() as usize].to_string()
            }),
        ])
    }

    fn scenario(keys: serde_json::Value) -> Scenario {
        serde_json::from_value(serde_json::json!({
            "fps": 25, "size": [320, 180], "start": 10, "end": 20, "keys": keys
        }))
        .unwrap()
    }

    #[test]
    fn a_property_eases_between_its_keys_and_holds_outside_them() {
        let scenario = scenario(serde_json::json!([
            { "t": 10, "zoom": 2, "car color": "yellow" },
            { "t": 12, "zoom": 4 },
            { "t": 14, "ease": "smooth", "zoom": 0, "car color": "blue" },
            { "t": 15, "ease": "step", "zoom": 1 },
        ]));
        let timeline = Timeline::new(&scenario, &panel()).unwrap();
        let zoom = |t| timeline.value("zoom", t).unwrap();
        assert_eq!(zoom(5.0), 2.0);
        assert_eq!(zoom(11.0), 3.0);
        assert_eq!(zoom(13.0), 2.0);
        assert!((zoom(12.5) - 4.0 * (1.0 - 0.15625)).abs() < 1e-5);
        assert_eq!(zoom(14.9), 0.0);
        assert_eq!(zoom(15.0), 1.0);
        assert_eq!(zoom(99.0), 1.0);
        // A name steps, whatever the key's ease.
        let color = |t| timeline.value("car color", t).unwrap();
        assert_eq!(color(13.9), 0.0);
        assert_eq!(color(14.0), 2.0);
        assert_eq!(timeline.value("look", 12.0), None);
        assert!(timeline.cuts(14.96, 15.0));
        assert!(!timeline.cuts(15.0, 15.04) && !timeline.cuts(13.96, 14.0));
        let baseline = Values::from([("look".to_string(), 0.5), ("zoom".to_string(), 9.0)]);
        let values = timeline.at(11.0, &baseline);
        assert_eq!((values["look"], values["zoom"]), (0.5, 3.0));
    }

    #[test]
    fn an_unknown_property_or_name_is_refused_with_its_key() {
        for (keys, wanted) in [
            (serde_json::json!([{ "t": 11, "warp": 1 }]), "`warp`"),
            (
                serde_json::json!([{ "t": 12, "car color": "green" }]),
                "yellow, red, blue",
            ),
            (serde_json::json!([{ "t": 13, "zoom": "far" }]), "`far`"),
        ] {
            let error = Timeline::new(&scenario(keys), &panel())
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(wanted), "{error}");
        }
    }

    #[test]
    fn the_car_goes_as_far_as_its_speed_takes_it_whatever_the_frame() {
        let scenario = scenario(serde_json::json!([
            { "t": 10, "speed": 36 },
            { "t": 12, "speed": 72 },
            { "t": 13.5, "ease": "step", "speed": 0 },
        ]));
        let timeline = Timeline::new(&scenario, &panel()).unwrap();
        let baseline = Values::new();
        let motion = |t| timeline.motion(10.0, 100.0, t, &baseline);
        // 10 m/s to 20 m/s over two seconds: 30 m, at 5 m/s².
        let at = motion(12.0);
        assert!((at.s - 130.0).abs() < 1e-2, "{}", at.s);
        assert!((at.acceleration - 5.0).abs() < 1e-2);
        assert_eq!(at.speed, Some(20.0));
        // Stopped from 13.5 s: the car stays put while the world's time
        // goes on.
        let stopped = motion(13.5);
        let later = motion(19.0);
        assert!((later.s - stopped.s).abs() < 1e-3);
        assert!((later.time - 19.0).abs() < 1e-2);
        // The same frame, reached in any number of steps.
        assert!((motion(11.0).s - 112.5).abs() < 1e-2);
        assert_eq!(motion(10.0).s, 100.0);
    }

    #[test]
    fn a_key_set_in_the_editor_reads_back_from_the_file() {
        let panel = panel();
        let mut scenario = scenario(serde_json::json!([{ "t": 10, "speed": 50 }]));
        scenario.set(
            10.01,
            BTreeMap::from([("car color".to_string(), setting(&panel, "car color", 1.2))]),
        );
        scenario.set(
            11.0,
            BTreeMap::from([("speed".to_string(), setting(&panel, "speed", 80.123_45))]),
        );
        assert_eq!(scenario.keys.len(), 2);
        assert_eq!(
            scenario.keys[0].set["car color"],
            Setting::Name("red".to_string())
        );
        assert_eq!(scenario.keys[1].set["speed"], Setting::Number(80.123));
        let file = std::env::temp_dir().join(format!("drive-scenario-{}.json", std::process::id()));
        scenario.save(&file).unwrap();
        let read = Scenario::load(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(read, scenario);
    }
}
