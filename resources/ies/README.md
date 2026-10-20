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
  demo, not measured: IES type II medium, cutoff, 14 000 lm. Its axis is
  straight down (γ = 0), C = 0 along the road and C = 90° to the street
  side. Two beams peak at γ = 66°, toed in 8° towards the street side; a
  fill lights the lanes across, a shield dims the house side to 60%, and
  it fades out from 68° to nothing at 85°, with a 2.5% ripple from the
  lens array. Mounted 10° from straight down towards the street side at
  10 m and 36 m apart, it gives the carriageway a uniformity
  (min / average) of about 0.6. The same terms apply.
