//! Turns noisy face boxes into calm, deliberate camera moves.
//!
//! Three layers, like a human camera operator:
//!  1. **Track** – smooth the detector's measurements (size more than position) and only trust a
//!     face that has been seen repeatedly.
//!  2. **Decide** – keep the shot still while the subject stays inside a comfort zone; only re-aim
//!     when they've been outside it for a moment, and then re-centre fully. If the face is about to
//!     leave the crop, re-aim immediately and move faster.
//!  3. **Move** – velocity- and acceleration-limited motion (ease in, glide, ease out). Zoom runs
//!     in log space and slower than pan.

use crate::yunet::Face;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub max_zoom: f32,
    /// Crop height = face height × this.
    pub face_scale: f32,
    /// Seconds to hold the shot after losing the face before drifting back to wide.
    pub hold_s: f32,
}

/// Tunables that shape the feel. Fractions are of the crop (zones) or of the camera frame (speeds).
#[derive(Clone, Copy, Debug)]
pub struct Feel {
    // measurement smoothing time constants (s)
    pub tau_pos: f32,
    pub tau_size: f32,
    // comfort zone, as a fraction of the current crop
    pub zone_x: f32,
    pub zone_y: f32,
    /// allowed size mismatch as a ratio (1.1 = ±10 %)
    pub zone_zoom: f32,
    // how long the subject must be outside the zone before we move (s)
    pub dwell_x: f32,
    pub dwell_y: f32,
    pub dwell_zoom: f32,
    // pan limits, in frame widths per second (and per second²)
    pub pan_vmax: f32,
    pub pan_amax: f32,
    // zoom limits, in ln(height) per second (and per second²)
    pub zoom_vmax: f32,
    pub zoom_amax: f32,
    /// speed multiplier when the face is about to leave the crop
    pub urgent_gain: f32,
    /// speed multiplier for the first framing (camera start, or re-finding someone after going wide)
    pub acquire_gain: f32,
    /// face edge closer than this fraction of the crop to its border → urgent
    pub edge_margin: f32,
}

impl Default for Feel {
    fn default() -> Self {
        Self {
            tau_pos: 0.25,
            tau_size: 0.6,
            zone_x: 0.10,
            zone_y: 0.08,
            zone_zoom: 1.10,
            dwell_x: 0.4,
            dwell_y: 0.6,
            dwell_zoom: 1.2,
            pan_vmax: 0.30,
            pan_amax: 0.45,
            zoom_vmax: 0.25,
            zoom_amax: 0.35,
            urgent_gain: 3.0,
            acquire_gain: 2.0,
            edge_margin: 0.03,
        }
    }
}

/// One motion axis with velocity and acceleration limits.
#[derive(Clone, Copy, Debug, Default)]
struct Axis {
    pos: f32,
    vel: f32,
}

impl Axis {
    fn step(&mut self, target: f32, vmax: f32, amax: f32, dt: f32) {
        let d = target - self.pos;
        // Fastest speed from which we can still brake to a stop at the target.
        let v_brake = (2.0 * amax * d.abs()).sqrt() * 0.95;
        let v_des = d.signum() * vmax.min(v_brake);
        let dv = (v_des - self.vel).clamp(-amax * dt, amax * dt);
        self.vel += dv;
        let next = self.pos + self.vel * dt;
        // Arrived (or would cross the target): settle exactly.
        if (target - next) * d <= 0.0 || (d.abs() < 0.5e-3 && self.vel.abs() < amax * dt) {
            self.pos = target;
            self.vel = 0.0;
        } else {
            self.pos = next;
        }
    }
    fn moving(&self) -> bool {
        self.vel != 0.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Copy, Debug)]
struct Meas {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

pub struct Framer {
    p: Params,
    f: Feel,
    in_w: f32,
    in_h: f32,
    aspect: f32,

    // 1. track (filtered face centre + height), raw last box, hit history
    tx: f32,
    ty: f32,
    th: f32,
    have_track: bool,
    last_meas_t: f64,
    last_face: f64,
    raw: Option<Meas>,
    hits: u8,

    // 2. decisions: where we're aiming (centre x/y in px, ln(crop height))
    aim_x: f32,
    aim_y: f32,
    aim_z: f32,
    out_x: Option<f64>,
    out_y: Option<f64>,
    out_z: Option<f64>,
    urgent: bool,
    /// No shot established yet (start, or after going wide): frame quickly without dwelling.
    acquired: bool,
    acquiring: bool,

    // 3. motion
    mx: Axis,
    my: Axis,
    mz: Axis,

    pub faces: Vec<Face>,
}

impl Framer {
    pub fn new(p: Params) -> Self {
        Self::with_feel(p, Feel::default())
    }

    pub fn with_feel(p: Params, f: Feel) -> Self {
        Self {
            p,
            f,
            in_w: 0.0,
            in_h: 0.0,
            aspect: 16.0 / 9.0,
            tx: 0.0,
            ty: 0.0,
            th: 0.0,
            have_track: false,
            last_meas_t: 0.0,
            last_face: f64::NEG_INFINITY,
            raw: None,
            hits: 0,
            aim_x: 0.0,
            aim_y: 0.0,
            aim_z: 0.0,
            out_x: None,
            out_y: None,
            out_z: None,
            urgent: false,
            acquired: false,
            acquiring: false,
            mx: Axis::default(),
            my: Axis::default(),
            mz: Axis::default(),
            faces: Vec::new(),
        }
    }

    pub fn reset(&mut self, in_w: u32, in_h: u32, out_aspect: f32) {
        let (p, f) = (self.p, self.f);
        *self = Self::with_feel(p, f);
        self.in_w = in_w as f32;
        self.in_h = in_h as f32;
        self.aspect = out_aspect;
        (self.aim_x, self.aim_y, self.aim_z) = (self.in_w / 2.0, self.in_h / 2.0, self.full_h().ln());
        self.mx.pos = self.aim_x;
        self.my.pos = self.aim_y;
        self.mz.pos = self.aim_z;
    }

    pub fn full_h(&self) -> f32 {
        self.in_h.min(self.in_w / self.aspect)
    }
    fn min_h(&self) -> f32 {
        self.full_h() / self.p.max_zoom
    }

    /// New detector result (empty = no face this time).
    pub fn on_faces(&mut self, faces: Vec<Face>, t: f64) {
        self.faces = faces;
        self.hits <<= 1;
        if self.in_w <= 0.0 || self.faces.is_empty() {
            return;
        }
        self.hits |= 1;

        // Union of the main faces, ignoring much smaller ones (posters, people far behind).
        let big = self.faces.iter().map(|f| f.h).fold(0.0, f32::max);
        let mut m = Meas { x0: f32::MAX, y0: f32::MAX, x1: f32::MIN, y1: f32::MIN };
        for f in self.faces.iter().filter(|f| f.h >= big * 0.5) {
            m.x0 = m.x0.min(f.x);
            m.y0 = m.y0.min(f.y);
            m.x1 = m.x1.max(f.x + f.w);
            m.y1 = m.y1.max(f.y + f.h);
        }
        let (cx, cy, h) = ((m.x0 + m.x1) / 2.0, (m.y0 + m.y1) / 2.0, (m.y1 - m.y0).max((m.x1 - m.x0) * 0.8));
        self.raw = Some(m);

        if !self.have_track || t - self.last_face > self.p.hold_s as f64 {
            (self.tx, self.ty, self.th) = (cx, cy, h);
            self.have_track = true;
        } else {
            let dt = (t - self.last_meas_t).max(0.0) as f32;
            let ap = 1.0 - (-dt / self.f.tau_pos).exp();
            let az = 1.0 - (-dt / self.f.tau_size).exp();
            self.tx += ap * (cx - self.tx);
            self.ty += ap * (cy - self.ty);
            self.th += az * (h - self.th);
        }
        self.last_meas_t = t;
        self.last_face = t;
    }

    /// Seen in at least 2 of the last 4 detections.
    fn confident(&self) -> bool {
        (self.hits & 0b1111).count_ones() >= 2
    }

    /// Ideal shot for the tracked face: centre x, centre y (unclamped — where the subject
    /// really is) and ln(crop height).
    fn ideal(&self) -> (f32, f32, f32) {
        let h = (self.th * self.p.face_scale).clamp(self.min_h(), self.full_h());
        let y = self.ty + 0.12 * h; // headroom: eyes about a third from the top
        (self.tx, y, h.ln())
    }

    fn clamp_centre(&self, x: f32, y: f32, h: f32) -> (f32, f32) {
        // Float round-off (exp/ln) can make a full-frame crop a hair larger than the frame.
        fn fit(v: f32, size: f32, total: f32) -> f32 {
            let (lo, hi) = (size / 2.0, total - size / 2.0);
            if lo >= hi { total / 2.0 } else { v.clamp(lo, hi) }
        }
        (fit(x, h * self.aspect, self.in_w), fit(y, h, self.in_h))
    }

    /// Requests a re-aim on one axis once the error has persisted for `dwell` seconds.
    fn dwell(since: &mut Option<f64>, outside: bool, t: f64, dwell: f32) -> bool {
        if !outside {
            *since = None;
            return false;
        }
        let s = *since.get_or_insert(t);
        t - s >= dwell as f64
    }

    pub fn step(&mut self, t: f64, dt: f32) {
        if self.in_w <= 0.0 {
            return;
        }
        let f = self.f;
        let lost = t - self.last_face > self.p.hold_s as f64;

        if lost {
            // Drift back to the wide shot.
            (self.aim_x, self.aim_y, self.aim_z) = (self.in_w / 2.0, self.in_h / 2.0, self.full_h().ln());
            self.urgent = false;
            self.have_track = false;
            self.acquired = false;
        } else if self.have_track && self.confident() && !self.acquired {
            // First confident sighting: go straight to the shot, a bit quicker than normal.
            let (ux, uy, iz) = self.ideal();
            let (ix, iy) = self.clamp_centre(ux, uy, iz.exp());
            (self.aim_x, self.aim_y, self.aim_z) = (ix, iy, iz);
            self.acquired = true;
            self.acquiring = true;
        } else if self.have_track && self.confident() {
            let (ux, uy, iz) = self.ideal();
            // Reachable aim for the shot we'd end up with.
            let (ix, iy) = self.clamp_centre(ux, uy, iz.exp());
            let cur_h = self.mz.pos.exp();
            let cur_w = cur_h * self.aspect;

            // Urgent: the (raw, unsmoothed) face is about to leave the *current* crop.
            if let Some(m) = self.raw {
                let (cx0, cy0) = (self.mx.pos - cur_w / 2.0, self.my.pos - cur_h / 2.0);
                let (mx_, my_) = (f.edge_margin * cur_w, f.edge_margin * cur_h);
                let near_edge =
                    m.x0 < cx0 + mx_ || m.x1 > cx0 + cur_w - mx_ || m.y0 < cy0 + my_ || m.y1 > cy0 + cur_h - my_;
                if near_edge {
                    // Don't fight the frame border: only urgent if moving would actually help.
                    let would_help =
                        (ix - self.mx.pos).abs() > 1.0 || (iy - self.my.pos).abs() > 1.0 || iz > self.mz.pos + 0.01;
                    if would_help {
                        (self.aim_x, self.aim_y, self.aim_z) = (ix, iy, iz.max(self.aim_z));
                        self.urgent = true;
                    }
                }
            }

            // Off-centre beyond the comfort zone, *and* re-aiming would gain something noticeable
            // (near the frame border there may be little room left to pan).
            let out_x = (ux - self.aim_x).abs() > f.zone_x * cur_w && (ix - self.aim_x).abs() > 0.03 * cur_w;
            let out_y = (uy - self.aim_y).abs() > f.zone_y * cur_h && (iy - self.aim_y).abs() > 0.03 * cur_h;
            let fire_x = Self::dwell(&mut self.out_x, out_x, t, f.dwell_x);
            let fire_y = Self::dwell(&mut self.out_y, out_y, t, f.dwell_y);
            let fire_z = Self::dwell(&mut self.out_z, (iz - self.aim_z).abs() > f.zone_zoom.ln(), t, f.dwell_zoom);
            // Re-centre fully on whichever axis fired; a zoom change re-centres everything
            // so the subject stays composed while the crop size changes.
            if fire_z {
                (self.aim_x, self.aim_y, self.aim_z) = (ix, iy, iz);
            } else {
                if fire_x {
                    self.aim_x = ix;
                }
                if fire_y {
                    self.aim_y = iy;
                }
            }
        }

        // Keep the aim reachable for the aimed crop size.
        let (ax, ay) = self.clamp_centre(self.aim_x, self.aim_y, self.aim_z.exp());
        (self.aim_x, self.aim_y) = (ax, ay);

        let g = if self.urgent {
            f.urgent_gain
        } else if self.acquiring {
            f.acquire_gain
        } else {
            1.0
        };
        let (pv, pa) = (f.pan_vmax * self.in_w * g, f.pan_amax * self.in_w * g);
        self.mz.step(self.aim_z, f.zoom_vmax * g, f.zoom_amax * g, dt);
        self.mx.step(self.aim_x, pv, pa, dt);
        self.my.step(self.aim_y, pv, pa, dt);
        if !self.mx.moving() && !self.my.moving() && !self.mz.moving() {
            self.urgent = false;
            self.acquiring = false;
        }
    }

    /// Sub-pixel crop rectangle in camera pixels (sub-pixel keeps slow pans from stepping).
    pub fn crop(&self) -> Crop {
        let h = (self.mz.pos.exp() as f64).clamp(self.min_h() as f64, self.full_h() as f64);
        let (iw, ih) = (self.in_w as f64, self.in_h as f64);
        let w = (h * self.aspect as f64).min(iw);
        let h = h.min(ih);
        Crop {
            x: (self.mx.pos as f64 - w / 2.0).clamp(0.0, iw - w),
            y: (self.my.pos as f64 - h / 2.0).clamp(0.0, ih - h),
            w,
            h,
        }
    }

    pub fn zoom(&self) -> f32 {
        self.full_h() / self.mz.pos.exp().clamp(self.min_h(), self.full_h())
    }
}
