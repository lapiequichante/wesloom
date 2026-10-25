# Drive scenarios

Keyframed animations of the drive, in seconds into their music; the
format is described in `src/scenario.rs`.

Play and edit `long-take.json` in the window (Space plays, Left/Right
step, K keys what changed while paused):

```sh
cargo run -p wxsl-drive --release -- --scenario crates/wxsl-drive/scenarios/long-take.json
```

Record its frames at the file's `fps` and `size`, then make the video
with its music:

```sh
cargo run -p wxsl-drive --release -- --scenario crates/wxsl-drive/scenarios/long-take.json --record screenshots/long-take
ffmpeg -framerate 25 -start_number 0 -i screenshots/long-take/frame-%05d.png \
  -ss 33.5 -i resources/music/wxsl.mp3 -map 0:v -map 1:a -c:a libmp3lame -b:a 320k -shortest \
  -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p screenshots/long-take.mp4
```

The recording prints this ffmpeg line with the scenario's own rate and
start, so after changing `fps` (60 for the final cut) copy the printed
one. `--from S --to S` records part of it, numbered so the frames slot
into the whole.
