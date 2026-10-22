//! The drive's state, saved as JSON: every slider, the view, the viewport's
//! size, and where the car was on this frame and the one before. A
//! screenshot from the file renders the frame before first, so the
//! velocity pass blurs what moved between the two as the window did.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ui::Panel;

/// What moves from frame to frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    /// Arc length along the road, metres.
    pub s: f32,
    /// How far the car has driven, for its wheels.
    pub odometer: f32,
    /// The car's speed, m/s, which follows the slider within the car's
    /// limits; none before the first frame.
    pub speed: Option<f32>,
    /// The acceleration the car had, m/s², for the brake lights.
    pub acceleration: f32,
    /// The acceleration the body pitches for, lagging the car's as the
    /// springs do.
    pub sprung: f32,
    /// Seconds since the start, for the car's animation and the shaders.
    pub time: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The viewport, pixels.
    pub width: u32,
    pub height: u32,
    /// Every slider's value, by label.
    pub sliders: BTreeMap<String, f32>,
    /// How far the view is turned around the car: yaw, pitch, radians.
    pub orbit: [f32; 2],
    /// How far the camera is from the car, relative to the chase distance.
    pub zoom: f32,
    pub motion: Motion,
    /// The last frame that moved before this one, and the seconds between:
    /// the velocity pass measures against it.
    pub previous: Motion,
    pub dt: f32,
}

impl Saved {
    /// Set `panel`'s sliders to the saved values; a label the panel does not
    /// have is refused by name, one the file lacks keeps its value.
    pub fn apply_sliders(&self, panel: &mut Panel) -> Result<(), Box<dyn Error>> {
        for (label, value) in &self.sliders {
            let slider = panel
                .sliders
                .iter_mut()
                .find(|slider| slider.label == label)
                .ok_or_else(|| format!("the state has an unknown slider `{label}`"))?;
            slider.value = value.clamp(slider.min, slider.max);
        }
        Ok(())
    }

    pub fn load(file: &Path) -> Result<Self, Box<dyn Error>> {
        let text = std::fs::read_to_string(file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("{} is not a drive state: {error}", file.display()).into())
    }

    /// Write it as the first free `state-NNN.json` in `directory`.
    pub fn save(&self, directory: &Path) -> Result<PathBuf, Box<dyn Error>> {
        std::fs::create_dir_all(directory)?;
        let file = (1..)
            .map(|index| directory.join(format!("state-{index:03}.json")))
            .find(|file| !file.exists())
            .expect("a free name");
        std::fs::write(&file, serde_json::to_string_pretty(self)? + "\n")?;
        Ok(file)
    }
}

/// Every slider's value in `panel`, by label.
pub fn sliders(panel: &Panel) -> BTreeMap<String, f32> {
    panel
        .sliders
        .iter()
        .map(|slider| (slider.label.to_string(), slider.value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Slider;

    fn panel() -> Panel {
        Panel::new(vec![
            Slider::new("speed", 0.0, 100.0, 50.0, |value| format!("{value}")),
            Slider::new("hour", 0.0, 24.0, 12.0, |value| format!("{value}")),
        ])
    }

    #[test]
    fn a_state_reads_back_as_written_and_sets_the_sliders() {
        let mut saved = Saved {
            width: 1280,
            height: 720,
            sliders: sliders(&panel()),
            orbit: [0.4, -0.1],
            zoom: 1.5,
            motion: Motion {
                s: 120.0,
                odometer: 4000.0,
                speed: Some(30.0),
                acceleration: -2.0,
                sprung: -1.5,
                time: 12.25,
            },
            previous: Motion::default(),
            dt: 1.0 / 60.0,
        };
        saved.sliders.insert("hour".into(), 21.5);
        let text = serde_json::to_string_pretty(&saved).unwrap();
        let read: Saved = serde_json::from_str(&text).unwrap();
        assert_eq!(read, saved);

        let mut panel = panel();
        read.apply_sliders(&mut panel).unwrap();
        assert_eq!(panel.value("hour"), 21.5);
        assert_eq!(panel.value("speed"), 50.0);

        saved.sliders.insert("warp".into(), 1.0);
        let error = saved.apply_sliders(&mut panel).unwrap_err();
        assert!(error.to_string().contains("`warp`"), "{error}");
    }
}
