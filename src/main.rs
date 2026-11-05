//! autoframe — auto-framing virtual camera for PipeWire.
//!
//! ```text
//!  camera node ─pw_stream(in)─► to packed RGB ─► sub-pixel crop + scale + relight ─pw_stream(out)─► virtual camera
//!                                     │                       ▲
//!                                     └─ thumbnail ─► YuNet thread ─► framer (track → decide → move)
//! ```
//!
//! The camera is opened only while something consumes the virtual camera.

use autoframe::camera::{self, InFormat, Mode};
use autoframe::convert::{self, Converter, Plane};
use autoframe::detector::Detector;
use autoframe::framer::{Framer, Params};
use autoframe::pod;
use autoframe::relight::{Relight, RelightParams};
use clap::Parser;
use fast_image_resize as fr;
use pipewire as pw;
use pw::spa;
use pw::stream::{StreamFlags, StreamRc, StreamState};
use pw::sys as pw_sys;
use spa::param::ParamType;
use spa::param::video::{VideoFormat, VideoInfoRaw};
use spa::pod::Pod;
use spa::sys as spa_sys;
use spa::utils::Direction;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------------ options

#[derive(Clone, Copy, Debug)]
struct Size {
    w: u32,
    h: u32,
}

fn parse_size(s: &str) -> Result<Size, String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT, e.g. 1280x720")?;
    let w: u32 = w.parse().map_err(|_| "bad width")?;
    let h: u32 = h.parse().map_err(|_| "bad height")?;
    if w < 16 || h < 16 || w % 2 == 1 || h % 2 == 1 {
        return Err("width and height must be even and at least 16".into());
    }
    Ok(Size { w, h })
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Filter {
    Bilinear,
    CatmullRom,
    Lanczos3,
}

impl Filter {
    fn fr(self) -> fr::FilterType {
        match self {
            Filter::Bilinear => fr::FilterType::Bilinear,
            Filter::CatmullRom => fr::FilterType::CatmullRom,
            Filter::Lanczos3 => fr::FilterType::Lanczos3,
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about = "Auto-framing virtual camera for PipeWire", max_term_width = 100)]
struct Opt {
    /// List cameras and the mode autoframe would use, then exit
    #[arg(short, long)]
    list_cameras: bool,
    /// Camera to use: node name, description or id from --list-cameras (default: the first camera)
    #[arg(short, long)]
    source: Option<String>,
    /// Preferred camera resolution (the closest supported mode is used)
    #[arg(short, long, default_value = "1920x1080", value_parser = parse_size)]
    capture: Size,
    /// Virtual camera resolution
    #[arg(short, long, default_value = "1280x720", value_parser = parse_size)]
    output: Size,
    /// Maximum digital zoom (with 1080p in and 720p out, zoom above 1.5 upscales)
    #[arg(short = 'z', long, default_value_t = 1.8)]
    max_zoom: f32,
    /// Shot height as a multiple of face height (smaller = tighter framing)
    #[arg(short, long, default_value_t = 2.4)]
    face_scale: f32,
    /// Seconds to hold the shot after losing the face before going back to wide
    #[arg(long, default_value_t = 2.5)]
    hold: f32,
    /// Resampling filter for the crop
    #[arg(long, value_enum, default_value_t = Filter::Lanczos3)]
    filter: Filter,
    /// Target face brightness (0-1) for relighting dark or backlit faces
    #[arg(long, default_value_t = 0.48)]
    face_brightness: f32,
    /// Relight strength (0 = off, 1 = full)
    #[arg(long, default_value_t = 1.0)]
    relight: f32,
    /// Face detections per second
    #[arg(long, default_value_t = 10.0)]
    det_fps: f64,
    /// Detector input width in pixels (larger finds smaller/further faces, costs more)
    #[arg(long, default_value_t = 224)]
    det_width: u32,
    /// Detector confidence threshold (0-100)
    #[arg(long, default_value_t = 60)]
    threshold: i32,
    /// Seconds to keep the camera open after the last app stops using the virtual camera
    #[arg(long, default_value_t = 3.0)]
    idle: f64,
    /// PipeWire node name of the virtual camera
    #[arg(long, default_value = "autoframe")]
    name: String,
    /// Name of the virtual camera shown in apps
    #[arg(long, default_value = "Auto-framed Camera")]
    description: String,
    /// Draw detected faces into the output
    #[arg(short, long)]
    debug: bool,
    /// Print statistics every 5 seconds
    #[arg(short, long)]
    verbose: bool,
}

// ------------------------------------------------------------------------ camera discovery

struct CameraInfo {
    id: u32,
    name: String,
    description: String,
    /// libcamera first, then V4L2, then anything else.
    rank: u8,
    priority: i32,
    modes: Vec<Mode>,
}

impl CameraInfo {
    fn matches(&self, s: &str) -> bool {
        self.name == s || self.description == s || self.id.to_string() == s
    }
}

struct Discovery {
    cameras: Vec<CameraInfo>,
    own_node_exists: bool,
}

/// Lists video sources and asks each for its formats (two server round-trips).
fn discover(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    own_name: &str,
) -> Result<Discovery, pw::Error> {
    let cams: Rc<RefCell<Vec<CameraInfo>>> = Rc::default();
    let own = Rc::new(Cell::new(false));
    let proxies: Rc<RefCell<Vec<(pw::node::Node, pw::node::NodeListener)>>> = Rc::default();
    let registry = core.get_registry_rc()?;

    let _rl = registry
        .add_listener_local()
        .global({
            let (cams, own, proxies, registry, own_name) =
                (cams.clone(), own.clone(), proxies.clone(), registry.clone(), own_name.to_owned());
            move |g| {
                if g.type_ != pw::types::ObjectType::Node {
                    return;
                }
                let Some(props) = g.props else { return };
                if props.get("media.class") != Some("Video/Source") {
                    return;
                }
                let name = props.get("node.name").unwrap_or_default().to_owned();
                if name == own_name {
                    own.set(true);
                    return;
                }
                let path = props.get("object.path").unwrap_or_default();
                let rank = if path.starts_with("libcamera") || name.starts_with("libcamera") {
                    0
                } else if path.starts_with("v4l2") || name.starts_with("v4l2") {
                    1
                } else {
                    2
                };
                let idx = {
                    let mut c = cams.borrow_mut();
                    c.push(CameraInfo {
                        id: g.id,
                        description: props.get("node.description").unwrap_or(&name).to_owned(),
                        name,
                        rank,
                        priority: props.get("priority.session").and_then(|p| p.parse().ok()).unwrap_or(0),
                        modes: Vec::new(),
                    });
                    c.len() - 1
                };
                let Ok(node) = registry.bind::<pw::node::Node, _>(g) else { return };
                let listener = node
                    .add_listener_local()
                    .param({
                        let cams = cams.clone();
                        move |_, _, _, _, param| {
                            if let Some(param) = param {
                                cams.borrow_mut()[idx].modes.extend(camera::parse_enum_format(param));
                            }
                        }
                    })
                    .register();
                node.enum_params(0, Some(ParamType::EnumFormat), 0, u32::MAX);
                proxies.borrow_mut().push((node, listener));
            }
        })
        .register();

    // Round-trip 1 delivers the globals (and sends the param requests); round-trip 2 the params.
    let pending = Rc::new(Cell::new(Some(core.sync(0)?)));
    let rounds = Rc::new(Cell::new(0));
    let _cl = core
        .add_listener_local()
        .done({
            let (pending, rounds, core, ml) = (pending.clone(), rounds.clone(), core.clone(), mainloop.clone());
            move |id, seq| {
                if id != pw_sys::PW_ID_CORE || pending.get() != Some(seq) {
                    return;
                }
                rounds.set(rounds.get() + 1);
                if rounds.get() == 1 {
                    pending.set(core.sync(0).ok());
                } else {
                    ml.quit();
                }
            }
        })
        .register();
    mainloop.run();
    drop(proxies.take());

    let mut cameras = std::mem::take(&mut *cams.borrow_mut());
    cameras.sort_by_key(|c| (c.rank, -c.priority, c.id));
    Ok(Discovery { cameras, own_node_exists: own.get() })
}

// ------------------------------------------------------------------------ state

struct State {
    opt: Opt,
    inp: StreamRc,
    t0: Instant,

    mode: Mode,
    capturing: bool,
    out_streaming: bool,
    /// Negotiated camera format and size (set once the camera stream is configured).
    in_fmt: Option<(InFormat, Size)>,
    out_fmt: VideoFormat,
    /// Newest camera buffer, waiting for the output side to consume it.
    held: *mut pw_sys::pw_buffer,
    /// Whether the held frame was converted into `conv.buf` (else it's used in place).
    held_converted: bool,

    framer: Framer,
    relight: Relight,
    det: Detector,
    conv: Converter,
    resizer: fr::Resizer,
    scratch: Vec<u8>,
    last_det: f64,
    last_result: f64,
    last_step: f64,
    seq: u64,
    convert_errors: u32,

    stat_t: f64,
    stat_frames: u32,
    stat_render: f64,
    stat_dets: u32,
    stat_det_ms: f64,
}

/// Mapped data planes of a dequeued buffer.
///
/// # Safety
/// `b` must be a buffer dequeued from a stream connected with `MAP_BUFFERS`; the returned slices
/// are valid until it is queued back.
unsafe fn buffer_planes<'a>(b: *mut pw_sys::pw_buffer, compressed: bool) -> Option<Vec<Plane<'a>>> {
    unsafe {
        let buf = (*b).buffer;
        if buf.is_null() || (*buf).n_datas == 0 {
            return None;
        }
        let mut planes = Vec::with_capacity((*buf).n_datas as usize);
        for i in 0..(*buf).n_datas as usize {
            let d = &*(*buf).datas.add(i);
            if d.data.is_null() || d.chunk.is_null() {
                return None;
            }
            let c = &*d.chunk;
            if (c.flags & spa_sys::SPA_CHUNK_FLAG_CORRUPTED as i32) != 0 || (i == 0 && c.size == 0) {
                return None;
            }
            let offset = (c.offset % d.maxsize.max(1)) as usize;
            let len = if compressed {
                (c.size as usize).min(d.maxsize as usize - offset)
            } else {
                d.maxsize as usize - offset
            };
            let data = std::slice::from_raw_parts((d.data as *const u8).add(offset), len);
            planes.push(Plane { data, stride: c.stride.max(0) as usize });
        }
        Some(planes)
    }
}

impl State {
    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn release_held(&mut self) {
        if !self.held.is_null() {
            // SAFETY: `held` was dequeued from `inp` and not yet returned.
            unsafe { self.inp.queue_raw_buffer(self.held) };
            self.held = std::ptr::null_mut();
        }
    }

    /// The held camera frame as packed 4-byte pixels.
    fn held_frame(&self, size: Size) -> &[u8] {
        let n = size.w as usize * size.h as usize * 4;
        if self.held_converted {
            &self.conv.buf[..n]
        } else {
            // SAFETY: held is a dequeued, mapped buffer; checked to be large enough on arrival.
            let planes = unsafe { buffer_planes(self.held, false) }.expect("held buffer");
            &planes[0].data[..n]
        }
    }

    /// Camera frame arrived. Returns true if the output should be triggered.
    fn on_camera_frame(&mut self) -> bool {
        // Keep only the newest frame.
        let mut b: *mut pw_sys::pw_buffer = std::ptr::null_mut();
        loop {
            // SAFETY: plain pw_stream API; every dequeued buffer is queued back exactly once.
            let t = unsafe { self.inp.dequeue_raw_buffer() };
            if t.is_null() {
                break;
            }
            if !b.is_null() {
                unsafe { self.inp.queue_raw_buffer(b) };
            }
            b = t;
        }
        if b.is_null() {
            return false;
        }
        let Some((format, size)) = self.in_fmt else {
            unsafe { self.inp.queue_raw_buffer(b) };
            return false;
        };

        // SAFETY: `b` is ours until queued back.
        let Some(planes) = (unsafe { buffer_planes(b, format == InFormat::Mjpg) }) else {
            unsafe { self.inp.queue_raw_buffer(b) };
            return false;
        };
        let packed_len = size.w as usize * size.h as usize * 4;
        let zero_copy = convert::is_zero_copy(format, planes[0].stride, size.w) && planes[0].data.len() >= packed_len;
        if !zero_copy {
            if let Err(e) = self.conv.convert(format, &planes, size.w, size.h) {
                self.convert_errors += 1;
                if self.convert_errors <= 3 || self.convert_errors.is_multiple_of(100) {
                    eprintln!("autoframe: dropping camera frame ({e})");
                }
                unsafe { self.inp.queue_raw_buffer(b) };
                return false;
            }
        }
        drop(planes);
        self.release_held();
        self.held = b;
        self.held_converted = !zero_copy;

        let t = self.now();
        // Feed the detector (non-blocking; skipped while it's busy).
        if t - self.last_det >= 1.0 / self.opt.det_fps {
            if let Some(mut thumb) = self.det.take_buffer() {
                let tw = self.opt.det_width.min(size.w) & !1;
                let th = ((size.h as f32 * tw as f32 / size.w as f32).round() as u32).max(2) & !1;
                thumb.resize((tw * th * 4) as usize, 0);
                let mut resizer = std::mem::take(&mut self.resizer);
                {
                    let frame = self.held_frame(size);
                    let src = fr::images::ImageRef::new(size.w, size.h, frame, fr::PixelType::U8x4).unwrap();
                    let mut dst = fr::images::Image::from_slice_u8(tw, th, &mut thumb, fr::PixelType::U8x4).unwrap();
                    let opts = fr::ResizeOptions::new()
                        .resize_alg(fr::ResizeAlg::Interpolation(fr::FilterType::Bilinear))
                        .use_alpha(false);
                    resizer.resize(&src, &mut dst, &opts).unwrap();
                }
                self.resizer = resizer;
                let (sx, sy) = (size.w as f32 / tw as f32, size.h as f32 / th as f32);
                self.det.submit(thumb, tw, th, sx, sy, convert::is_rgb_order(format));
                self.last_det = t;
            }
        }
        if let Some((faces, ms)) = self.det.poll() {
            let since = if self.last_result > 0.0 { (t - self.last_result).min(1.0) as f32 } else { 0.1 };
            self.last_result = t;
            self.relight.on_faces(&faces, t, since);
            self.framer.on_faces(faces, t);
            self.stat_dets += 1;
            self.stat_det_ms += ms;
        }
        let dt = if self.last_step > 0.0 { (t - self.last_step).min(0.2) as f32 } else { 1.0 / 30.0 };
        self.last_step = t;
        self.framer.step(t, dt);
        self.relight.step(t, dt);
        true
    }

    /// The virtual camera wants a frame: crop + scale the held camera frame into it.
    fn on_output(&mut self, out: &pw::stream::Stream) {
        if self.held.is_null() {
            return;
        }
        let Some((format, size)) = self.in_fmt else { return };
        // SAFETY: plain pw_stream API; `ob` is queued back below.
        let ob = unsafe { out.dequeue_raw_buffer() };
        if ob.is_null() {
            self.release_held(); // the app is slow: drop this frame
            return;
        }
        let t_start = Instant::now();
        // SAFETY: output buffers are mapped (MAP_BUFFERS) and ours until queued.
        unsafe {
            let od = &mut *(*(*ob).buffer).datas;
            if !od.data.is_null() {
                let dst = std::slice::from_raw_parts_mut(od.data as *mut u8, od.maxsize as usize);
                let (len, stride) = self.render(size, convert::is_rgb_order(format), dst);
                let chunk = &mut *od.chunk;
                chunk.offset = 0;
                chunk.size = len;
                chunk.stride = stride;
                chunk.flags = 0;
            }
            if let Some(h) = find_header((*ob).buffer) {
                h.pts = monotonic_ns();
                h.flags = 0;
                h.seq = self.seq;
                h.dts_offset = 0;
            }
            out.queue_raw_buffer(ob);
        }
        self.seq += 1;
        self.release_held();

        self.stat_render += t_start.elapsed().as_secs_f64() * 1e3;
        self.stat_frames += 1;
        self.print_stats();
    }

    fn print_stats(&mut self) {
        let t = self.now();
        if t - self.stat_t < 5.0 {
            return;
        }
        if self.opt.verbose {
            let c = self.framer.crop();
            eprintln!(
                "autoframe: {:.1} fps, render {:.2} ms, detect {:.2} ms ({} runs), crop {:.0}x{:.0}+{:.0}+{:.0} \
                 (zoom {:.2}x), faces {}, face brightness {}, gamma {:.2}",
                self.stat_frames as f64 / (t - self.stat_t),
                self.stat_render / self.stat_frames.max(1) as f64,
                self.stat_det_ms / self.stat_dets.max(1) as f64,
                self.stat_dets,
                c.w,
                c.h,
                c.x,
                c.y,
                self.framer.zoom(),
                self.framer.faces.len(),
                self.relight.face_luma().map(|l| format!("{l:.2}")).unwrap_or("-".into()),
                self.relight.gamma()
            );
        }
        self.stat_t = t;
        self.stat_frames = 0;
        self.stat_dets = 0;
        self.stat_render = 0.0;
        self.stat_det_ms = 0.0;
    }

    /// Crops, scales, relights and converts into `dst`. Returns (bytes written, stride).
    fn render(&mut self, size: Size, rgb: bool, dst: &mut [u8]) -> (u32, i32) {
        let (ow, oh) = (self.opt.output.w, self.opt.output.h);
        let i420 = self.out_fmt == VideoFormat::I420;
        let need = if i420 { ow * oh * 3 / 2 } else { ow * oh * 4 } as usize;
        if dst.len() < need {
            return (0, 0);
        }
        let c = self.framer.crop();
        let opts = fr::ResizeOptions::new()
            .resize_alg(fr::ResizeAlg::Convolution(self.opt.filter.fr()))
            .crop(c.x, c.y, c.w, c.h)
            .use_alpha(false);

        let mut scratch = std::mem::take(&mut self.scratch);
        let mut resizer = std::mem::take(&mut self.resizer);
        {
            let frame = self.held_frame(size);
            let src = fr::images::ImageRef::new(size.w, size.h, frame, fr::PixelType::U8x4).unwrap();
            let bgrx: &mut [u8] = if i420 {
                scratch.resize((ow * oh * 4) as usize, 0);
                &mut scratch
            } else {
                &mut dst[..need]
            };
            let mut d = fr::images::Image::from_slice_u8(ow, oh, bgrx, fr::PixelType::U8x4).unwrap();
            resizer.resize(&src, &mut d, &opts).unwrap();
        }
        self.resizer = resizer;

        let packed: &mut [u8] = if i420 { &mut scratch } else { &mut dst[..need] };
        if self.opt.debug {
            let k = ow as f64 / c.w;
            for f in &self.framer.faces {
                let x0 = ((f.x as f64 - c.x) * k) as i32;
                let y0 = ((f.y as f64 - c.y) * k) as i32;
                let x1 = ((f.x as f64 + f.w as f64 - c.x) * k) as i32;
                let y1 = ((f.y as f64 + f.h as f64 - c.y) * k) as i32;
                draw_rect(packed, ow as i32, oh as i32, (x0, y0), (x1, y1));
            }
        }

        let result = if i420 {
            let (ysz, csz) = ((ow * oh) as usize, (ow * oh / 4) as usize);
            let (y, rest) = dst[..need].split_at_mut(ysz);
            let (u, v) = rest.split_at_mut(csz);
            let mut img = yuv::YuvPlanarImageMut {
                y_plane: yuv::BufferStoreMut::Borrowed(y),
                y_stride: ow,
                u_plane: yuv::BufferStoreMut::Borrowed(u),
                u_stride: ow / 2,
                v_plane: yuv::BufferStoreMut::Borrowed(v),
                v_stride: ow / 2,
                width: ow,
                height: oh,
            };
            let to_i420 = if rgb { yuv::rgba_to_yuv420 } else { yuv::bgra_to_yuv420 };
            to_i420(
                &mut img,
                &scratch,
                ow * 4,
                yuv::YuvRange::Limited,
                yuv::YuvStandardMatrix::Bt601,
                yuv::YuvConversionMode::Balanced,
            )
            .unwrap();
            self.relight.apply_y(&mut dst[..ysz]);
            (need as u32, ow as i32)
        } else {
            if rgb {
                // R,G,B,x camera -> B,G,R,x output.
                for px in dst[..need].chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }
            self.relight.apply_4ch(&mut dst[..need]);
            (need as u32, (ow * 4) as i32)
        };
        self.scratch = scratch;
        result
    }
}

/// # Safety
/// `buf` must be a valid, mapped `spa_buffer`.
unsafe fn find_header<'a>(buf: *mut spa_sys::spa_buffer) -> Option<&'a mut spa_sys::spa_meta_header> {
    unsafe {
        for i in 0..(*buf).n_metas as usize {
            let m = &*(*buf).metas.add(i);
            if m.type_ == spa_sys::SPA_META_Header
                && m.size as usize >= std::mem::size_of::<spa_sys::spa_meta_header>()
                && !m.data.is_null()
            {
                return Some(&mut *(m.data as *mut spa_sys::spa_meta_header));
            }
        }
        None
    }
}

fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid pointer to a timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

fn draw_rect(img: &mut [u8], w: i32, h: i32, (x0, y0): (i32, i32), (x1, y1): (i32, i32)) {
    let (x0, x1) = (x0.clamp(0, w - 1), x1.clamp(0, w - 1));
    let (y0, y1) = (y0.clamp(0, h - 1), y1.clamp(0, h - 1));
    let mut put = |x: i32, y: i32| {
        let o = ((y * w + x) * 4) as usize;
        img[o..o + 4].copy_from_slice(&[0, 255, 0, 255]);
    };
    for t in 0..3 {
        for x in x0..=x1 {
            put(x, (y0 + t).min(h - 1));
            put(x, (y1 - t).max(0));
        }
        for y in y0..=y1 {
            put((x0 + t).min(w - 1), y);
            put((x1 - t).max(0), y);
        }
    }
}

// ------------------------------------------------------------------------ capture control

fn start_capture(state: &Rc<RefCell<State>>) {
    let (inp, mode) = {
        let st = state.borrow();
        if st.capturing {
            return;
        }
        (st.inp.clone(), st.mode)
    };
    let bytes = pod::mode_pod(&mode);
    let mut params = [Pod::from_bytes(&bytes).unwrap()];
    match inp.connect(Direction::Input, None, StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS, &mut params) {
        Ok(()) => {
            let mut st = state.borrow_mut();
            st.capturing = true;
            st.last_step = 0.0;
            st.stat_t = st.now();
            eprintln!("autoframe: virtual camera in use, opening camera");
        }
        Err(e) => eprintln!("autoframe: could not connect to the camera: {e}"),
    }
}

fn stop_capture(state: &Rc<RefCell<State>>) {
    let inp = {
        let mut st = state.borrow_mut();
        if !st.capturing {
            return;
        }
        st.release_held();
        st.capturing = false;
        st.in_fmt = None;
        st.inp.clone()
    };
    let _ = inp.disconnect();
    eprintln!("autoframe: virtual camera idle, camera released");
}

// ------------------------------------------------------------------------ main

#[cfg(all(target_arch = "x86_64", feature = "avx2"))]
fn check_cpu() {
    if !(std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")) {
        eprintln!(
            "autoframe: this CPU lacks AVX2/FMA, which this build's face detector needs.\n\
             Rebuild with: cargo build --release --no-default-features"
        );
        std::process::exit(1);
    }
}
#[cfg(not(all(target_arch = "x86_64", feature = "avx2")))]
fn check_cpu() {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opt = Opt::parse();
    check_cpu();
    pw::init();

    // The loop lives for the whole process; leaking it gives timers a 'static borrow.
    let mainloop: &'static pw::main_loop::MainLoopRc = Box::leak(Box::new(pw::main_loop::MainLoopRc::new(None)?));
    let context = pw::context::ContextRc::new(mainloop, None)?;
    let core = context.connect_rc(None).map_err(|e| format!("cannot connect to PipeWire ({e}); is it running?"))?;
    // Lost the PipeWire connection (e.g. PipeWire restarted): exit non-zero so a service manager restarts us.
    let fatal = Rc::new(Cell::new(false));
    let _core_listener = core
        .add_listener_local()
        .error({
            let fatal = fatal.clone();
            move |id, _seq, res, msg| {
                eprintln!("autoframe: PipeWire error on object {id}: {msg} ({res})");
                if id == pw_sys::PW_ID_CORE && res == -libc::EPIPE {
                    fatal.set(true);
                    mainloop.quit();
                }
            }
        })
        .register();

    let found = discover(mainloop, &core, &opt.name)?;
    let want = (opt.capture.w, opt.capture.h);
    if opt.list_cameras {
        if found.cameras.is_empty() {
            println!("No cameras found.");
        }
        for (i, c) in found.cameras.iter().enumerate() {
            println!("{}{} (id {}, name {})", if i == 0 { "* " } else { "  " }, c.description, c.id, c.name);
            match camera::choose(&c.modes, want) {
                Some(m) => println!("    would use: {m}  ({} supported modes)", c.modes.len()),
                None => println!("    no supported formats"),
            }
        }
        return Ok(());
    }
    if found.own_node_exists {
        eprintln!(
            "autoframe: a PipeWire node named \"{}\" already exists; is autoframe already running? \
             (use --name for a second instance)",
            opt.name
        );
        std::process::exit(1);
    }
    let usable = |c: &&CameraInfo| !c.modes.is_empty();
    let cam = match &opt.source {
        Some(s) => found
            .cameras
            .iter()
            .find(|c| c.matches(s))
            .ok_or_else(|| format!("camera \"{s}\" not found; see --list-cameras"))?,
        None => found.cameras.iter().find(usable).ok_or("no usable camera found; see --list-cameras")?,
    };
    let mode = camera::choose(&cam.modes, want)
        .ok_or_else(|| format!("camera \"{}\" offers no supported formats", cam.description))?;
    eprintln!("autoframe: using \"{}\" ({}), {mode}", cam.description, cam.name);

    let inp = StreamRc::new(
        core.clone(),
        "autoframe-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Camera",
            *pw::keys::NODE_NAME => format!("{}.capture", opt.name),
            *pw::keys::TARGET_OBJECT => cam.name.as_str(),
            // Never fall back to the default video source if the camera can't be linked: the
            // default is usually our own virtual camera, which would loop black frames into itself.
            "node.dont-fallback" => "true",
            // ...and ignore "move stream" overrides in the default metadata, which WirePlumber
            // otherwise consults on every rescan (and which made the defined target vanish).
            "node.dont-move" => "true",
        },
    )?;
    let out = StreamRc::new(
        core.clone(),
        &opt.name,
        pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_ROLE => "Camera",
            *pw::keys::NODE_NAME => opt.name.as_str(),
            *pw::keys::NODE_NICK => opt.name.as_str(),
            *pw::keys::NODE_DESCRIPTION => opt.description.as_str(),
        },
    )?;

    let params = Params { max_zoom: opt.max_zoom.max(1.0), face_scale: opt.face_scale.max(1.0), hold_s: opt.hold };
    let relight = RelightParams {
        target: opt.face_brightness.clamp(0.1, 0.9),
        strength: opt.relight.clamp(0.0, 1.0),
        ..Default::default()
    };
    let (out_size, idle) = (opt.output, Duration::from_secs_f64(opt.idle.max(0.001)));
    let state = Rc::new(RefCell::new(State {
        det: Detector::new(opt.threshold),
        framer: Framer::new(params),
        relight: Relight::new(relight),
        opt,
        inp: inp.clone(),
        t0: Instant::now(),
        mode,
        capturing: false,
        out_streaming: false,
        in_fmt: None,
        out_fmt: VideoFormat::Unknown,
        held: std::ptr::null_mut(),
        held_converted: false,
        conv: Converter::default(),
        resizer: fr::Resizer::new(),
        scratch: Vec::new(),
        last_det: 0.0,
        last_result: 0.0,
        last_step: 0.0,
        seq: 0,
        convert_errors: 0,
        stat_t: 0.0,
        stat_frames: 0,
        stat_render: 0.0,
        stat_dets: 0,
        stat_det_ms: 0.0,
    }));

    // Release the camera a little after the last app leaves (browsers often renegotiate).
    let idle_timer = mainloop.loop_().add_timer({
        let s = state.clone();
        move |_| {
            let streaming = s.borrow().out_streaming;
            if !streaming {
                stop_capture(&s);
            }
        }
    });
    let idle_timer: &'static _ = Box::leak(Box::new(idle_timer));

    // Camera went away while in use (unplugged, suspend/resume): retry every 2 s.
    let retry_timer = mainloop.loop_().add_timer({
        let s = state.clone();
        move |_| {
            if s.borrow().out_streaming {
                eprintln!("autoframe: reconnecting camera");
                stop_capture(&s);
                start_capture(&s);
            }
        }
    });
    let retry_timer: &'static _ = Box::leak(Box::new(retry_timer));

    // ---- camera stream
    let out_for_in = out.clone();
    let _in_listener = inp
        .add_local_listener_with_user_data(())
        .state_changed({
            let s = state.clone();
            move |_, _, old, new| {
                let verbose = s.try_borrow().map(|st| st.opt.verbose).unwrap_or(false);
                if verbose || matches!(new, StreamState::Error(_)) {
                    eprintln!("autoframe: camera stream {old:?} -> {new:?}");
                }
                // `capturing` is cleared before deliberate disconnects, so Unconnected while still
                // capturing means the camera went away. Reconnecting from inside the stream's own
                // callback isn't allowed, so defer to the timer.
                let unexpected = match new {
                    StreamState::Error(_) => true,
                    StreamState::Unconnected => s.try_borrow().map(|st| st.capturing).unwrap_or(false),
                    _ => false,
                };
                if unexpected {
                    let _ = retry_timer.update_timer(Some(Duration::from_secs(2)), None);
                } else if matches!(new, StreamState::Streaming) {
                    let _ = retry_timer.update_timer(None, None);
                }
            }
        })
        .param_changed({
            let s = state.clone();
            move |_, _, pid, param| {
                let Some(param) = param else { return };
                if pid != ParamType::Format.as_raw() {
                    return;
                }
                let Some(m) = camera::parse_enum_format(param).into_iter().next() else {
                    eprintln!("autoframe: camera negotiated an unsupported format");
                    return;
                };
                eprintln!("autoframe: camera streaming {m}");
                let mut st = s.borrow_mut();
                st.in_fmt = Some((m.format, Size { w: m.width, h: m.height }));
                let aspect = st.opt.output.w as f32 / st.opt.output.h as f32;
                st.framer.reset(m.width, m.height, aspect);
                st.relight.reset();
                st.last_result = 0.0;
            }
        })
        .process({
            let s = state.clone();
            move |_, _| {
                let (trigger, streaming) = {
                    let mut st = s.borrow_mut();
                    (st.on_camera_frame(), st.out_streaming)
                };
                if trigger && (!streaming || out_for_in.trigger_process().is_err()) {
                    s.borrow_mut().release_held();
                }
            }
        })
        .register()?;

    // ---- virtual camera stream
    let _out_listener = out
        .add_local_listener_with_user_data(())
        .state_changed({
            let s = state.clone();
            move |_, _, old, new| {
                let streaming = matches!(new, StreamState::Streaming);
                let verbose = {
                    let mut st = s.borrow_mut();
                    st.out_streaming = streaming;
                    st.opt.verbose
                };
                if verbose || matches!(new, StreamState::Error(_)) {
                    eprintln!("autoframe: virtual camera {old:?} -> {new:?}");
                }
                if streaming {
                    let _ = idle_timer.update_timer(None, None);
                    start_capture(&s);
                } else if s.borrow().capturing {
                    let _ = idle_timer.update_timer(Some(idle), None);
                }
            }
        })
        .param_changed({
            let s = state.clone();
            move |stream, _, pid, param| {
                let Some(param) = param else { return };
                if pid != ParamType::Format.as_raw() {
                    return;
                }
                let mut info = VideoInfoRaw::new();
                if info.parse(param).is_err() {
                    return;
                }
                let i420 = info.format() == VideoFormat::I420;
                let (w, h) = (out_size.w as i32, out_size.h as i32);
                let (stride, size) = if i420 { (w, w * h * 3 / 2) } else { (w * 4, w * h * 4) };
                s.borrow_mut().out_fmt = info.format();
                if s.borrow().opt.verbose {
                    eprintln!("autoframe: app negotiated {:?} {w}x{h}", info.format());
                }
                let (buffers, meta) = (pod::buffers_pod(size, stride), pod::header_meta_pod());
                let mut params = [Pod::from_bytes(&buffers).unwrap(), Pod::from_bytes(&meta).unwrap()];
                if let Err(e) = stream.update_params(&mut params) {
                    eprintln!("autoframe: update_params failed: {e}");
                }
            }
        })
        .process({
            let s = state.clone();
            move |stream, _| s.borrow_mut().on_output(stream)
        })
        .register()?;

    let size = (out_size.w, out_size.h);
    let fmt_i420 = pod::raw_pod(&[VideoFormat::I420], size, Some((30, 1)));
    let fmt_bgrx = pod::raw_pod(&[VideoFormat::BGRx], size, Some((30, 1)));
    let mut params = [Pod::from_bytes(&fmt_i420).unwrap(), Pod::from_bytes(&fmt_bgrx).unwrap()];
    out.connect(Direction::Output, None, StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS, &mut params)?;
    {
        let st = state.borrow();
        eprintln!(
            "autoframe: virtual camera \"{}\" ready ({}x{}, max zoom {:.2}x)",
            st.opt.description, out_size.w, out_size.h, st.opt.max_zoom
        );
    }

    let _sigint = mainloop.loop_().add_signal_local(pw::loop_::Signal::INT, || mainloop.quit());
    let _sigterm = mainloop.loop_().add_signal_local(pw::loop_::Signal::TERM, || mainloop.quit());
    mainloop.run();

    stop_capture(&state);
    let _ = out.disconnect();
    if fatal.get() {
        eprintln!("autoframe: lost connection to PipeWire, exiting");
        std::process::exit(1);
    }
    Ok(())
}
