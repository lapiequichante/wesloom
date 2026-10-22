# IES profiles

Photometric profiles (IESNA LM-63, type C) read by `wxsl_frame::ies`
(ADR 0097).

- `headlight.ies`: a synthetic low beam for right-hand traffic, drawn
  for the gallery's `headlights` demo, not measured. Its axis is forward
  (γ = 0), C = 0 to the right and C = 90° up. Its three Gaussian lobes are
  a hot spot just right of and below the axis, a wide spread and a
  foreground fill. The cutoff sits 0.6° below the horizon and rises at
  15° to the right of the axis. It is released under the workspace's
  MIT/Apache-2.0 terms.
- `street_lamp.ies`: a synthetic LED roadway luminaire for the drive
  demo, not measured: a wide batwing, full cutoff, 14 000 lm. Its axis is
  straight down (γ = 0), C = 0 along the road and C = 90° to the street
  side. Each direction carries what lights the deck evenly where it lands,
  for a head mounted 10° from straight down towards the street side, 10 m
  over the lanes' dividing line, lamps 36 m apart, and the renderer's
  26 m range window. Neighbouring lamps cross-fade between 11 m and 25 m
  along the road, the two heads across the median, and the street side
  stops at the parapet. Nothing passes 70°, its peak (about 11 times the
  nadir) is near 60° along the road, and the carriageway's uniformity
  (min / average) is about 0.8. The same terms apply.
