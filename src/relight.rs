//! Face-aware brightness: the camera exposes for the whole scene (often a bright window), so a
//! backlit face comes out dark. We already know where the face is, so we lift it towards a target
//! level with a gamma curve. Gamma keeps black and white fixed, so already-bright areas don't clip.
//!
//! Open loop: the face is measured on the camera image *before* our curve, so there is no
//! feedback to oscillate. Measurements are smoothed and the curve changes slowly.

use crate::yunet::Face;

#[derive(Clone, Copy, Debug)]
pub struct RelightParams {
    /// Target face luma, 0..1 (full range).
    pub target: f32,
    /// 0 = off, 1 = bring the face all the way to the target.
    pub strength: f32,
    /// Strongest lift allowed (smallest gamma).
    pub min_gamma: f32,
    /// Smoothing time constant for the face brightness (s).
    pub tau: f32,
}

impl Default for RelightParams {
    fn default() -> Self {
        Self { target: 0.48, strength: 1.0, min_gamma: 0.6, tau: 1.0 }
    }
}

pub struct Relight {
    p: RelightParams,
    luma: Option<f32>,
    last_seen: f64,
    gamma: f32,
    lut_gamma: f32,
    lut_y: [u8; 256],
    lut_full: [u8; 256],
}

impl Relight {
    pub fn new(p: RelightParams) -> Self {
        let mut r = Self {
            p,
            luma: None,
            last_seen: f64::NEG_INFINITY,
            gamma: 1.0,
            lut_gamma: f32::NAN,
            lut_y: [0; 256],
            lut_full: [0; 256],
        };
        r.rebuild();
        r
    }

    pub fn reset(&mut self) {
        self.luma = None;
        self.gamma = 1.0;
        self.rebuild();
    }

    /// Feed a detection result (the faces the framer is using).
    pub fn on_faces(&mut self, faces: &[Face], t: f64, dt: f32) {
        let big = faces.iter().map(|f| f.h).fold(0.0, f32::max);
        let (mut sum, mut wsum) = (0.0, 0.0);
        for f in faces.iter().filter(|f| f.h >= big * 0.5) {
            let a = f.w * f.h;
            sum += f.luma * a;
            wsum += a;
        }
        if wsum <= 0.0 {
            return;
        }
        let m = (sum / wsum).clamp(0.02, 0.98);
        let a = 1.0 - (-dt / self.p.tau).exp();
        self.luma = Some(match self.luma {
            Some(l) => l + a * (m - l),
            None => m,
        });
        self.last_seen = t;
    }

    /// Advance the curve. Without a face for a while, ease back to neutral.
    pub fn step(&mut self, t: f64, dt: f32) {
        let want = match self.luma {
            Some(l) if t - self.last_seen < 3.0 && self.p.strength > 0.0 => {
                // gamma g maps l -> target:  l^g = target  =>  g = ln(target)/ln(l). Only brighten.
                let g = (self.p.target.ln() / l.ln()).clamp(self.p.min_gamma, 1.0);
                1.0 + self.p.strength * (g - 1.0)
            }
            _ => 1.0,
        };
        let a = 1.0 - (-dt / self.p.tau).exp();
        self.gamma += a * (want - self.gamma);
        if (self.gamma - self.lut_gamma).abs() > 0.003 {
            self.rebuild();
        }
    }

    pub fn gamma(&self) -> f32 {
        self.gamma
    }
    pub fn face_luma(&self) -> Option<f32> {
        self.luma
    }
    pub fn active(&self) -> bool {
        self.lut_gamma < 0.997
    }

    fn rebuild(&mut self) {
        let g = self.gamma;
        for i in 0..256 {
            // full range (BGRx output)
            let v = (i as f32 / 255.0).powf(g);
            self.lut_full[i] = (v * 255.0).round() as u8;
            // limited-range luma (I420 output): 16..235
            let y = ((i as f32 - 16.0) / 219.0).clamp(0.0, 1.0).powf(g);
            self.lut_y[i] = if (16..=235).contains(&i) { (16.0 + y * 219.0).round() as u8 } else { i as u8 };
        }
        self.lut_gamma = g;
    }

    /// Apply to a limited-range Y plane.
    pub fn apply_y(&self, y: &mut [u8]) {
        if !self.active() {
            return;
        }
        for v in y {
            *v = self.lut_y[*v as usize];
        }
    }

    /// Apply to packed 4-byte pixels (colour channels only).
    pub fn apply_4ch(&self, px: &mut [u8]) {
        if !self.active() {
            return;
        }
        for p in px.chunks_exact_mut(4) {
            p[0] = self.lut_full[p[0] as usize];
            p[1] = self.lut_full[p[1] as usize];
            p[2] = self.lut_full[p[2] as usize];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(luma: f32) -> Face {
        Face { x: 0.0, y: 0.0, w: 100.0, h: 120.0, score: 90, luma }
    }

    fn settle(r: &mut Relight, luma: f32) {
        let mut t = 0.0;
        for i in 0..200 {
            if i % 2 == 0 {
                r.on_faces(&[face(luma)], t, 0.1);
            }
            r.step(t, 0.05);
            t += 0.05;
        }
    }

    #[test]
    fn dark_face_is_lifted_to_target() {
        let mut r = Relight::new(RelightParams::default());
        settle(&mut r, 0.30);
        let lifted = 0.30f32.powf(r.gamma());
        assert!((lifted - 0.48).abs() < 0.02, "face lifted to {lifted}");
    }

    #[test]
    fn bright_face_is_left_alone() {
        let mut r = Relight::new(RelightParams::default());
        settle(&mut r, 0.6);
        assert!(!r.active(), "gamma {}", r.gamma());
    }

    #[test]
    fn very_dark_face_is_capped() {
        let mut r = Relight::new(RelightParams::default());
        settle(&mut r, 0.05);
        assert!(r.gamma() >= 0.6 - 1e-3);
    }

    #[test]
    fn black_and_white_points_are_preserved() {
        let mut r = Relight::new(RelightParams::default());
        settle(&mut r, 0.25);
        assert_eq!(r.lut_full[0], 0);
        assert_eq!(r.lut_full[255], 255);
        assert_eq!(r.lut_y[16], 16);
        assert_eq!(r.lut_y[235], 235);
    }
}
