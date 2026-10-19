//! IES photometric profiles (LM-63), read on the host and baked into a
//! layer of the light cookie array
//! ([ADR 0097](../../docs/adr/0097-ies-profiles-are-baked-into-a-cookie-layer.md)).
//!
//! A profile is a luminaire's candela over directions. Only type C
//! photometry is read — the common one, and the one whose angles map onto
//! a light's frame without a choice: the vertical angle γ is measured from
//! the light's axis (`Light::spot_direction`, γ = 0) and the horizontal
//! angle C turns around it, from the light's right (C = 0) towards its up
//! (C = 90°). [`IesProfile::texels`] lays that out as u = C / 360°,
//! v = γ / 180°, normalized to the brightest direction, which is what the
//! shader's `light_profile` reads.

use std::fmt;

/// Why an IES file could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IesError(String);

impl fmt::Display for IesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "IES profile: {}", self.0)
    }
}

impl std::error::Error for IesError {}

fn error<T>(message: impl Into<String>) -> Result<T, IesError> {
    Err(IesError(message.into()))
}

/// A type C photometric profile.
#[derive(Clone, Debug, PartialEq)]
pub struct IesProfile {
    /// Vertical angles γ in degrees, ascending, within [0, 180].
    vertical: Vec<f32>,
    /// Horizontal angles C in degrees, ascending from 0; the last one says
    /// which symmetry the file relies on (0, 90, 180 or 360).
    horizontal: Vec<f32>,
    /// Candela, one run of `vertical.len()` values per horizontal angle,
    /// with the file's multiplier applied.
    candela: Vec<f32>,
}

impl IesProfile {
    /// Read an LM-63 file (1986, 1991, 1995 or 2002).
    ///
    /// `TILT=NONE` and `TILT=INCLUDE` are accepted, the tilt data being
    /// skipped; a tilt in a separate file is refused, as are type A and B
    /// photometry and horizontal angles that do not start at 0.
    pub fn parse(text: &str) -> Result<Self, IesError> {
        let mut lines = text.lines();
        let tilt = loop {
            let Some(line) = lines.next() else {
                return error("no TILT= line");
            };
            if let Some(tilt) = line.trim().strip_prefix("TILT=") {
                break tilt.trim().to_owned();
            }
        };
        let rest: Vec<&str> = lines.collect();
        let mut numbers = rest
            .iter()
            .flat_map(|line| line.split(|c: char| c.is_whitespace() || c == ','))
            .filter(|token| !token.is_empty())
            .map(|token| {
                token
                    .parse::<f32>()
                    .map_err(|_| IesError(format!("`{token}` is not a number")))
            });
        let mut next = || {
            numbers
                .next()
                .unwrap_or_else(|| error("the file ends early"))
        };
        match tilt.as_str() {
            "NONE" => {}
            "INCLUDE" => {
                next()?;
                let count = next()? as usize;
                for _ in 0..count * 2 {
                    next()?;
                }
            }
            other => return error(format!("TILT={other} names a separate file")),
        }
        let _lamps = next()?;
        let _lumens = next()?;
        let multiplier = next()?;
        let vertical_count = next()? as usize;
        let horizontal_count = next()? as usize;
        let photometric_type = next()?;
        for _ in 0..4 {
            next()?; // units, width, length, height
        }
        for _ in 0..3 {
            next()?; // ballast factor, future use, input watts
        }
        if photometric_type != 1.0 {
            return error(format!(
                "photometric type {photometric_type} is not type C (1)"
            ));
        }
        if vertical_count == 0 || horizontal_count == 0 {
            return error("no angles");
        }
        let mut read = |count: usize| (0..count).map(|_| next()).collect::<Result<Vec<_>, _>>();
        let vertical = read(vertical_count)?;
        let horizontal = read(horizontal_count)?;
        let candela: Vec<f32> = read(vertical_count * horizontal_count)?
            .into_iter()
            .map(|value| value * multiplier)
            .collect();
        let ascending = |angles: &[f32]| angles.windows(2).all(|pair| pair[0] < pair[1]);
        if !ascending(&vertical) || vertical[0] < 0.0 || vertical[vertical_count - 1] > 180.0 {
            return error("vertical angles must ascend within [0, 180]");
        }
        if !ascending(&horizontal)
            || horizontal[0] != 0.0
            || ![0.0, 90.0, 180.0, 360.0].contains(&horizontal[horizontal_count - 1])
        {
            return error("horizontal angles must ascend from 0 to 0, 90, 180 or 360");
        }
        if candela
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return error("candela values must be finite and not negative");
        }
        Ok(IesProfile {
            vertical,
            horizontal,
            candela,
        })
    }

    /// The brightest direction's candela.
    pub fn max_candela(&self) -> f32 {
        self.candela.iter().copied().fold(0.0, f32::max)
    }

    /// The candela towards vertical angle `gamma` and horizontal angle `c`,
    /// in degrees, interpolated bilinearly; zero outside the vertical
    /// range.
    pub fn candela(&self, gamma: f32, c: f32) -> f32 {
        let (first, last) = (self.vertical[0], self.vertical[self.vertical.len() - 1]);
        if !(first..=last).contains(&gamma) {
            return 0.0;
        }
        // Fold C into the range the file covers by its symmetry.
        let c = c.rem_euclid(360.0);
        let c = match self.horizontal[self.horizontal.len() - 1] {
            0.0 => 0.0,
            90.0 => {
                let half = if c > 180.0 { 360.0 - c } else { c };
                if half > 90.0 {
                    180.0 - half
                } else {
                    half
                }
            }
            180.0 if c > 180.0 => 360.0 - c,
            _ => c,
        };
        let (h0, h1, ht) = bracket(&self.horizontal, c);
        let (v0, v1, vt) = bracket(&self.vertical, gamma);
        let at = |h: usize, v: usize| self.candela[h * self.vertical.len() + v];
        let row = |h: usize| at(h, v0) + (at(h, v1) - at(h, v0)) * vt;
        row(h0) + (row(h1) - row(h0)) * ht
    }

    /// The profile over a `width` × `height` grid, normalized to its
    /// brightest direction: u = C / 360°, v = γ / 180°, at texel centres,
    /// row by row from γ = 0.
    pub fn texels(&self, width: u32, height: u32) -> Vec<f32> {
        let scale = self.max_candela();
        let scale = if scale > 0.0 { 1.0 / scale } else { 0.0 };
        (0..height)
            .flat_map(|row| {
                let gamma = (row as f32 + 0.5) / height as f32 * 180.0;
                (0..width).map(move |column| {
                    let c = (column as f32 + 0.5) / width as f32 * 360.0;
                    self.candela(gamma, c) * scale
                })
            })
            .collect()
    }
}

/// The two entries of ascending `angles` around `x`, and how far between
/// them it lies; the last entry twice past the end.
fn bracket(angles: &[f32], x: f32) -> (usize, usize, f32) {
    let upper = angles.partition_point(|angle| *angle <= x);
    if upper == 0 {
        return (0, 0, 0.0);
    }
    if upper == angles.len() {
        return (upper - 1, upper - 1, 0.0);
    }
    let (a, b) = (angles[upper - 1], angles[upper]);
    (upper - 1, upper, (x - a) / (b - a))
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUADRANT: &str = "IESNA:LM-63-2002
[TEST] quadrant-symmetric, with a tilt to skip
TILT=INCLUDE
1
2
0 90
1.0 1.0
1 1000 2.0 3 2 1 2 0.1 0.1 0.0
1.0 1.0 10
0, 45, 90
0 90
100 50 0
100 80 10
";

    #[test]
    fn a_quadrant_file_reads_with_its_multiplier_and_folds_every_quadrant() {
        let profile = IesProfile::parse(QUADRANT).unwrap();
        assert_eq!(profile.max_candela(), 200.0);
        assert_eq!(profile.candela(0.0, 0.0), 200.0);
        assert_eq!(profile.candela(45.0, 0.0), 100.0);
        assert_eq!(profile.candela(45.0, 90.0), 160.0);
        // Halfway in both angles: the average of the four corners.
        assert_eq!(
            profile.candela(22.5, 45.0),
            (200.0 + 100.0 + 200.0 + 160.0) / 4.0
        );
        // C = 90 + x mirrors C = 90 - x, and C = 360 - x mirrors x.
        for (a, b) in [(120.0, 60.0), (300.0, 60.0), (200.0, 20.0)] {
            assert_eq!(profile.candela(30.0, a), profile.candela(30.0, b));
        }
        assert_eq!(
            profile.candela(91.0, 0.0),
            0.0,
            "past the last vertical angle"
        );
    }

    #[test]
    fn texels_are_normalized_and_laid_out_by_c_then_gamma() {
        let profile = IesProfile::parse(QUADRANT).unwrap();
        let texels = profile.texels(4, 4);
        assert_eq!(texels.len(), 16);
        // Row 0 is γ = 22.5°, row 2 is γ = 112.5°, past the data.
        assert!(texels[..4]
            .iter()
            .all(|value| *value > 0.5 && *value <= 1.0));
        assert!(texels[8..].iter().all(|value| *value == 0.0));
    }

    #[test]
    fn type_b_and_external_tilts_are_refused_by_name() {
        let type_b = QUADRANT.replace("1 1000 2.0 3 2 1 2", "1 1000 2.0 3 2 2 2");
        assert!(IesProfile::parse(&type_b)
            .unwrap_err()
            .to_string()
            .contains("not type C"));
        let tilt = "TILT=lamp.tlt\n";
        assert!(IesProfile::parse(tilt)
            .unwrap_err()
            .to_string()
            .contains("separate file"));
    }
}
