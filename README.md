# autoframe

**Auto-framing for any webcam on Linux.** autoframe adds a virtual camera to PipeWire that
follows you around the frame: it finds your face, keeps you nicely framed with smooth, calm
camera moves, and brightens your face when you're backlit. It works with the camera you already
have.

- **Calm, deliberate framing.** It ignores fidgeting, nodding and quick glances, re-frames with
  one smooth move when you actually shift position, and reacts quickly if you're about to leave
  the shot.
- **Face-aware brightness.** A face in front of a bright window gets lifted to a good level
  without blowing out the background.
- **Lightweight.** Face detection uses [YuNet](https://github.com/ShiqiYu/libfacedetection), a
  tiny CNN (about 85k parameters), running on a small thumbnail 10 times a second. The whole
  pipeline takes a few milliseconds per frame on a laptop CPU, with no GPU and no ML runtime.
- **Only uses the camera when needed.** The real camera is opened only while an app is using the
  virtual camera, and released a few seconds after it stops.
- **Private.** Everything runs locally. autoframe makes no network connections.

## How it works

```text
              ┌──────────────────────────── autoframe ────────────────────────────┐
real camera ─►│ convert to RGB ─► crop + scale (Lanczos) ─► relight ─► I420/BGRx  │─► "Auto-framed Camera"
 (PipeWire)   │        │                    ▲                  ▲                  │    (PipeWire node)
              │        └► 224px thumbnail ─► YuNet (thread) ─► framer, relight    │
              └───────────────────────────────────────────────────────────────────┘
```

autoframe reads your camera through PipeWire, crops a moving window out of each frame and
publishes the result as a new PipeWire camera. Video apps pick **Auto-framed Camera** like any
other webcam. See [docs/design.md](docs/design.md) for how the framing decides when and how to
move.

## Requirements

- Linux with **PipeWire** (0.3.44 or newer) and WirePlumber, the default on current Fedora,
  Ubuntu, Debian, Arch and most other distributions.
- A camera that PipeWire knows about: any USB (UVC) webcam, or a laptop camera supported by
  libcamera (e.g. Intel IPU6/IPU7). Supported formats: MJPEG, YUY2, NV12, I420 and packed RGB.
- To build: Rust 1.87+, a C++ compiler, `pkg-config`, the PipeWire development headers and
  libclang (used to generate the PipeWire bindings).
- An x86-64 CPU with AVX2 (2013 or newer), or ARM64. Older x86 CPUs work with a
  slower build (see [Building](#building)).

## Installation

Install the build dependencies:

```sh
# Fedora
sudo dnf install cargo gcc-c++ pkgconf-pkg-config pipewire-devel clang-devel git

# Debian / Ubuntu
sudo apt install cargo g++ pkg-config libpipewire-0.3-dev libclang-dev git

# Arch
sudo pacman -S rust gcc pkgconf pipewire clang git
```

Then build and install autoframe to `~/.local/bin`:

```sh
git clone --recurse-submodules https://github.com/ah-/autoframe.git
cd autoframe
cargo install --locked --path . --root ~/.local
```

If you cloned without `--recurse-submodules`, run `git submodule update --init` first.

### Run it

```sh
autoframe
```

Then pick **Auto-framed Camera** in your video app. Leave autoframe running; it only turns your
camera on while an app is using the virtual camera.

### Start automatically at login

```sh
cp autoframe.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now autoframe
```

Logs: `journalctl --user -u autoframe -f`. The service restarts autoframe if PipeWire restarts
or the camera isn't available yet. To change options, edit the `ExecStart=` line in
`~/.config/systemd/user/autoframe.service`, then run
`systemctl --user daemon-reload && systemctl --user restart autoframe`.

## Using the virtual camera

Apps that get cameras through PipeWire see **Auto-framed Camera** directly:

| App | Notes |
|---|---|
| Firefox | Works out of the box on recent versions. If the camera doesn't show up, set `media.webrtc.camera.allow-pipewire` to `true` in `about:config`. |
| Chrome / Chromium | Enable `chrome://flags/#enable-webrtc-pipewire-camera`, then restart the browser. |
| OBS Studio | Add a **Video Capture Device (PipeWire)** source. |
| GNOME Camera (Snapshot) | Use the switch-camera button next to the shutter. |
| Zoom | Works (tested by the author). |

Apps that only talk to `/dev/video*` (V4L2) devices won't see it directly. For those, try
running them through PipeWire's V4L2 compatibility wrapper, `pw-v4l2 <app>`, or bridge the
virtual camera to a [v4l2loopback](https://github.com/umlaeute/v4l2loopback) device.

## Options

Run `autoframe --help` for the full list. The most useful ones:

| Option | Default | What it does |
|---|---|---|
| `-l`, `--list-cameras` | | Show cameras and the mode autoframe would pick, then exit. |
| `-s`, `--source <CAMERA>` | first camera | Which camera to use: node name, description or id from `--list-cameras`. |
| `-c`, `--capture <WxH>` | `1920x1080` | Preferred camera resolution; the closest supported mode is used. |
| `-o`, `--output <WxH>` | `1280x720` | Resolution of the virtual camera. |
| `-z`, `--max-zoom <N>` | `1.8` | Maximum digital zoom. Beyond capture/output size (1.5 by default) the image is upscaled and gets softer. |
| `-f`, `--face-scale <N>` | `2.4` | Shot height as a multiple of your face height. Smaller means tighter framing. |
| `--hold <SECONDS>` | `2.5` | How long to keep the shot after losing your face before going back to wide. |
| `--relight <0-1>` | `1` | Strength of face brightening; `0` turns it off. |
| `--face-brightness <0-1>` | `0.48` | Target brightness for your face. |
| `--filter <NAME>` | `lanczos3` | Scaling filter: `lanczos3` (sharpest), `catmull-rom` or `bilinear` (cheapest). |
| `--name`, `--description` | `autoframe`, `Auto-framed Camera` | PipeWire node name and the name apps show. Use these to run a second instance for another camera. |
| `-d`, `--debug` | | Draw boxes around detected faces. |
| `-v`, `--verbose` | | Print frame rate, timings, zoom and brightness every 5 seconds. |

Example: tighter framing with a sharper picture, never upscaling:

```sh
autoframe --face-scale 2.0 --max-zoom 1.5
```

## Troubleshooting

**No camera found / wrong camera.** Run `autoframe --list-cameras`. If your camera isn't listed,
check it shows up in `wpctl status` under *Video → Sources*. Pick a specific one with
`--source`.

**"A PipeWire node named autoframe already exists."** autoframe is already running, perhaps as
the systemd service (`systemctl --user status autoframe`). To run a second copy, give it its own
`--name` and `--description`.

**The virtual camera doesn't show up in my app.** See [Using the virtual camera](#using-the-virtual-camera).
Some browsers need a flag, and V4L2-only apps need a bridge.

**The camera light stays on.** autoframe keeps the camera open while any app uses the virtual
camera, plus 3 seconds (`--idle`). Check whether a browser tab or app still has it open.

**Picture is soft when zoomed in.** Digital zoom can only use the pixels the camera delivers.
Lower `--max-zoom` (1.5 never upscales with the default sizes), raise `--face-scale` for a wider
shot, or capture at a higher resolution with `--capture` if your camera supports it.

**"This CPU lacks AVX2/FMA".** Rebuild with the portable detector: see [Building](#building).

**Seeing what it does.** `autoframe -v -d` prints statistics every 5 seconds and draws the
detected faces into the picture.

## Building

```sh
git submodule update --init
cargo build --release                         # binary: target/release/autoframe
cargo build --release --no-default-features   # for x86-64 CPUs without AVX2 (detector ~3x slower)
```

The `avx2` feature (on by default) compiles YuNet's hand-written AVX2 kernels. autoframe checks
for AVX2 at startup and prints a clear message instead of crashing. The Rust parts (scaling,
colour conversion, JPEG decoding) detect CPU features at runtime. On ARM64, YuNet's NEON kernels
are used automatically.

## Development

```sh
cargo test --release                  # unit tests + framing regression tests
cargo run --release --example sim     # framing behaviour per simulated scene
cargo run --release --example bench   # per-stage timings
cargo fmt && cargo clippy --all-targets
```

The framing feel is tuned in `Feel::default()` in [src/framer.rs](src/framer.rs). The simulator
([src/sim.rs](src/sim.rs)) plays synthetic head movements (sitting still, fidgeting, nodding,
glancing, leaning, walking off) through the framer with realistic detector noise. The tests in
[tests/framing.rs](tests/framing.rs) check it stays still when it should and makes one clean move
when it should, so tuning can't quietly bring back jitter.

| File | Purpose |
|---|---|
| `src/main.rs` | Command line, camera discovery, PipeWire streams, rendering |
| `src/camera.rs` | Parsing camera formats and choosing the best mode |
| `src/convert.rs` | MJPEG / YUY2 / NV12 / I420 / RGB → packed RGB |
| `src/detector.rs` | Background face-detection thread |
| `src/yunet.rs` | Bindings to the YuNet detector (C++, built by `build.rs`) |
| `src/framer.rs` | Tracking, framing decisions and camera motion |
| `src/relight.rs` | Face-aware brightness |
| `src/pod.rs` | PipeWire format/buffer parameter helpers |
| `src/sim.rs` | Offline framing simulator used by tests and examples |

### Performance

Per-stage timings from `cargo run --release --example bench` on an Intel Core Ultra X7 358H, one
performance core:

| Stage | Time |
|---|---|
| Crop and scale 1080p → 720p (Lanczos3, 1.25x zoom) | 1.8 ms |
| Convert to I420 for the app | 0.14 ms |
| YuNet face detection, 256×144 (10 times a second, own thread) | 2.7 ms |
| YuNet without AVX2 (`--no-default-features`) | 8.0 ms |

In practice autoframe uses a few percent of one CPU core.

## License

autoframe is licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

The YuNet face detector ([libfacedetection](https://github.com/ShiqiYu/libfacedetection), included
as a git submodule) is © Shiqi Yu and contributors, under the
[3-clause BSD license](https://github.com/ShiqiYu/libfacedetection/blob/master/LICENSE).
