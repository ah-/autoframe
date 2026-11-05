//! Offline framing simulator: synthetic head motion + realistic detector noise. Used by the
//! regression tests (`tests/framing.rs`) and `cargo run --release --example sim`.

use crate::framer::{Feel, Framer, Params};
use crate::yunet::Face;

pub const FPS: f64 = 18.5;
pub const DET_HZ: f64 = 10.0;
pub const DUR: f64 = 20.0;
pub const W: f32 = 1920.0;
pub const H: f32 = 1080.0;

/// Ground-truth face (centre x, centre y, height) at time t, or None when absent.
pub type Scene = fn(f64) -> Option<(f32, f32, f32)>;

/// Seated at a laptop: a face about a third of the frame tall (settles around 1.25x zoom).
pub const BASE: (f32, f32, f32) = (960.0, 450.0, 360.0);

fn ease(t: f64, t0: f64, dur: f64) -> f32 {
    let x = ((t - t0) / dur).clamp(0.0, 1.0);
    (x * x * (3.0 - 2.0 * x)) as f32
}

pub fn sit_still(_: f64) -> Option<(f32, f32, f32)> {
    Some(BASE)
}
pub fn fidget(t: f64) -> Option<(f32, f32, f32)> {
    let t = t as f32;
    Some((
        BASE.0 + 35.0 * (t * 0.7).sin() + 20.0 * (t * 1.9).sin(),
        BASE.1 + 15.0 * (t * 1.3).sin(),
        BASE.2 + 15.0 * (t * 0.4).sin(),
    ))
}
pub fn nodding(t: f64) -> Option<(f32, f32, f32)> {
    let dy = if (5.0..12.0).contains(&t) { 30.0 * (t as f32 * std::f32::consts::TAU).sin() } else { 0.0 };
    Some((BASE.0, BASE.1 + dy, BASE.2))
}
pub fn glance(t: f64) -> Option<(f32, f32, f32)> {
    Some((BASE.0 + if (6.0..6.3).contains(&t) { 250.0 } else { 0.0 }, BASE.1, BASE.2))
}
pub fn lean_right(t: f64) -> Option<(f32, f32, f32)> {
    Some((BASE.0 + 220.0 * ease(t, 6.0, 0.5), BASE.1, BASE.2))
}
pub fn lean_back(t: f64) -> Option<(f32, f32, f32)> {
    Some((BASE.0, BASE.1, BASE.2 - 80.0 * ease(t, 6.0, 1.0)))
}
pub fn walk_off(t: f64) -> Option<(f32, f32, f32)> {
    if t < 7.2 { Some((BASE.0 + 790.0 * ease(t, 6.0, 1.0), BASE.1, BASE.2)) } else { None }
}

pub fn scenes() -> Vec<(&'static str, Scene, &'static str)> {
    vec![
        ("sit still", sit_still, "no movement"),
        ("fidget/talk", fidget, "no movement"),
        ("nodding", nodding, "no movement"),
        ("glance 0.3s", glance, "no movement"),
        ("lean right", lean_right, "one smooth pan"),
        ("lean back", lean_back, "one slow zoom in (face got smaller)"),
        ("walk off", walk_off, "follow fast, hold, go wide"),
    ]
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// Separate motion episodes after the settle window.
    pub moves: u32,
    /// Total distance the crop centre travelled (px).
    pub path_px: f64,
    /// Total |Δ ln(crop height)|.
    pub zoom_path: f64,
    /// Peak crop-centre speed (px/s).
    pub max_v: f64,
    /// Frames in which the true face was not fully inside the crop.
    pub cut_frames: u32,
    pub first_move: Option<f64>,
    pub settled: Option<f64>,
    /// Final zoom factor.
    pub final_zoom: f32,
    /// When the initial framing (from camera start) came to rest.
    pub acquired_at: Option<f64>,
}

struct Rng(u64);
impl Rng {
    fn u(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn n(&mut self) -> f32 {
        ((-2.0 * self.u().max(1e-12).ln()).sqrt() * (std::f64::consts::TAU * self.u()).cos()) as f32
    }
}

pub fn default_params() -> Params {
    Params { max_zoom: 1.8, face_scale: 2.4, hold_s: 2.5 }
}

/// Runs one scene. Detector noise: 1.5 % of face height on position, 3 % on size, 5 % misses.
pub fn run(params: Params, feel: Feel, scene: Scene, seed: u64) -> Stats {
    let mut fr = Framer::with_feel(params, feel);
    fr.reset(W as u32, H as u32, 16.0 / 9.0);
    let mut rng = Rng(seed.wrapping_mul(7919).wrapping_add(1));
    let dt = 1.0 / FPS;
    let mut next_det = 0.0;
    let mut prev: Option<(f64, f64, f64, f64)> = None;
    let mut s = Stats::default();
    let mut moving = false;
    let mut t = 0.0;
    while t < DUR {
        if t >= next_det {
            next_det += 1.0 / DET_HZ;
            let faces = match scene(t) {
                Some((x, y, h)) if rng.u() > 0.05 => {
                    let (fx, fy, fh) = (x + rng.n() * 0.015 * h, y + rng.n() * 0.015 * h, h * (1.0 + rng.n() * 0.03));
                    let fw = fh * 0.78;
                    vec![Face { x: fx - fw / 2.0, y: fy - fh / 2.0, w: fw, h: fh, score: 90, luma: 0.4 }]
                }
                _ => vec![],
            };
            fr.on_faces(faces, t);
        }
        fr.step(t, dt as f32);
        let c = fr.crop();
        let c = (c.x, c.y, c.w, c.h);
        if let Some(p) = prev {
            let d = ((c.0 + c.2 / 2.0 - p.0 - p.2 / 2.0).powi(2) + (c.1 + c.3 / 2.0 - p.1 - p.3 / 2.0).powi(2)).sqrt();
            let dz = (c.3.ln() - p.3.ln()).abs();
            let now_moving = d > 0.3 || dz > 2e-4;
            if moving && !now_moving && s.acquired_at.is_none() {
                s.acquired_at = Some(t);
            }
            if t > 3.0 {
                s.path_px += d;
                s.zoom_path += dz;
                s.max_v = s.max_v.max(d / dt);
                if now_moving && !moving {
                    s.moves += 1;
                    s.first_move.get_or_insert(t);
                }
                if !now_moving && moving {
                    s.settled = Some(t);
                }
            }
            moving = now_moving;
        }
        if let Some((x, y, h)) = scene(t) {
            let w = h * 0.78;
            let (x0, y0, x1, y1) = (x - w / 2.0, y - h / 2.0, x + w / 2.0, y + h / 2.0);
            if x0 < c.0 as f32 || x1 > (c.0 + c.2) as f32 || y0 < c.1 as f32 || y1 > (c.1 + c.3) as f32 {
                s.cut_frames += 1;
            }
        }
        prev = Some(c);
        t += dt;
    }
    s.final_zoom = fr.zoom();
    s
}
