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
- `street_lamp.ies`: a synthetic full-cutoff street lamp for the drive
  demo, not measured. Its axis is straight down (γ = 0) and it is
  rotationally symmetric: a "bat wing" peaking near 58° that throws the
  light along the road, a floor of 30% of the peak under the lamp, and
  nothing past 80°. The same terms apply.
