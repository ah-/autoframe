# How autoframe frames you

Auto-framing looks good when it behaves like a calm human camera operator: the shot stays still
while you talk, moves once and smoothly when you actually change position, and never lets you
walk out of frame. Jittery framing that follows every small movement is worse than no framing
at all.

autoframe splits the problem into three layers. All the numbers below are the defaults in
`Feel::default()` in [`src/framer.rs`](../src/framer.rs).

## 1. Track: clean up the measurements

YuNet runs about 10 times a second on a small thumbnail (224 px wide). Its face boxes jitter by
a few percent between detections, and occasionally it misses a face or sees one that isn't
there.

- **Smoothing.** Face position is smoothed with a 0.25 s time constant and face *size* with
  0.6 s. Size jitter is what makes zoom "breathe", so it gets smoothed more.
- **Confidence.** A face only counts once it has been seen in at least 2 of the last 4
  detections, so one-off false positives are ignored.
- **Multiple people.** The shot frames the union of all faces at least half the size of the
  largest. Small faces (posters, people far behind you) are ignored.

## 2. Decide: when to move

The ideal shot puts your face at a fixed size (crop height = 2.4 × face height) with your eyes
about a third from the top. autoframe doesn't chase that ideal continuously. It keeps the current
shot as long as you stay inside a **comfort zone** around it:

| Axis | Comfort zone | Must be outside it for |
|---|---|---|
| Sideways | 10 % of the shot width | 0.4 s |
| Vertical | 8 % of the shot height | 0.6 s (so nodding doesn't count) |
| Zoom | ±10 % in face size | 1.2 s |

When you've been outside the zone long enough, the shot **re-centres fully** on that axis. You
get occasional complete moves instead of a stream of small corrections. A zoom change re-centres
all axes so you stay composed while the crop size changes. A move is only triggered if it would
gain something noticeable: near the edge of the camera's view there may be little room left to
pan.

Two situations skip the waiting:

- **First framing.** When the camera starts, or when someone reappears after the shot went back
  to wide, autoframe frames them as soon as the face is confirmed (about 0.2 s), at 2× normal
  speed.
- **About to leave the shot.** If the latest (unsmoothed) face box comes within 3 % of the crop
  edge, autoframe re-aims immediately and moves 3× faster.

If no face has been seen for 2.5 s (`--hold`), the shot drifts back to the full camera view.

## 3. Move: how to move

Each axis (horizontal, vertical, zoom) moves with **limited velocity and acceleration**. It
accelerates smoothly, glides, and brakes so that it arrives exactly at the target without
overshooting. That gives the ease-in/ease-out feel of a real camera move, instead of the abrupt
start and rubber-band settle of a spring.

| | Max speed | Max acceleration | Typical move |
|---|---|---|---|
| Pan | 0.30 frame widths/s | 0.45 frame widths/s² | lean ~10 % → ~0.9 s |
| Zoom | 0.25 ln-units/s | 0.35 ln-units/s² | 1.0× → 1.25× → ~1.6 s |

Zoom runs in log space, so 1.0→1.25× feels the same as 1.2→1.5×. It's slower than panning
because zoom changes are the most noticeable.

The crop is computed with sub-pixel precision and resampled with Lanczos3, so slow pans glide
instead of stepping one pixel at a time.

## Simulated behaviour

`cargo run --release --example sim` plays synthetic head movements through the framer with
realistic detector noise (1.5 % position and 3 % size jitter, 5 % missed detections), averaged
over 10 noise seeds:

| Scene | Moves | Distance moved | Notes |
|---|---|---|---|
| Sitting still | 0 | 0 px | |
| Talking and fidgeting | 0 | 0 px | ±35 px sway |
| Nodding | 0 | 0 px | 1 Hz, ±30 px |
| Glancing aside for 0.3 s | 0 | 0 px | |
| Leaning sideways 220 px | 1 | ~190 px | done in ~0.9 s |
| Leaning back | 1 | slow zoom in | done in ~1.5 s |
| Walking out of frame | 2 | follows, then wide | face cut off for ~4 frames during a fast exit |

First framing after the camera starts settles in about 1.2 s. The tests in
[`tests/framing.rs`](../tests/framing.rs) check these behaviours, so changing the tuning can't
quietly bring back jitter.

## Face brightness

Cameras set exposure for the whole scene. With a bright window behind you, your face comes out
dark. autoframe measures the average brightness of the inner face region on the detector's
thumbnail, which is almost free, and applies a gamma curve to the output that lifts the face
toward a target level (0.48 by default):

- Gamma keeps black and white fixed, so bright areas aren't clipped further.
- It only brightens, never darkens, and the strongest lift is capped (gamma ≥ 0.6).
- Both the measurement and the curve change with a 1 s time constant, and the face is measured
  on the camera image *before* the curve, so there's no feedback loop to oscillate.
- Without a face for 3 s, the curve eases back to neutral.

The curve is applied as a lookup table to the luma plane of the output, which costs well under a
millisecond at 720p.

## Picking a camera mode

autoframe asks the camera for its supported formats and picks the best mode for the requested
capture size (`--capture`, 1920×1080 by default). It prefers, in order:

1. at least 15 fps,
2. a size close to the request (somewhat larger is fine, smaller costs more),
3. the output's aspect ratio,
4. the cheapest pixel format: packed RGB, then NV12/I420, then YUY2, then MJPEG.

On a typical USB webcam this picks MJPEG at 1080p 30 fps rather than raw YUY2, which USB 2 can
only deliver at a few frames per second at that size.
