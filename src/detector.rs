//! Background face detection. The PipeWire thread lends a small BGRA thumbnail to the worker
//! and gets it back with the results, so nothing is allocated per frame and the camera path
//! never waits on the model.

use crate::yunet::{Face, YuNet};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Instant;

struct Job {
    thumb: Vec<u8>,
    w: u32,
    h: u32,
    sx: f32,
    sy: f32,
    rgb: bool,
}

struct Done {
    thumb: Vec<u8>,
    faces: Vec<Face>,
    ms: f64,
}

/// Mean BT.601 luma (0..1) of the inner face (skin, not hair or background), in thumbnail coords.
fn face_luma(bgr: &[u8], w: u32, h: u32, f: &Face) -> f32 {
    let x0 = (f.x + 0.25 * f.w).max(0.0) as u32;
    let x1 = ((f.x + 0.75 * f.w) as u32).min(w);
    let y0 = (f.y + 0.30 * f.h).max(0.0) as u32;
    let y1 = ((f.y + 0.85 * f.h) as u32).min(h);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in y0..y1 {
        let row = &bgr[(y * w * 3) as usize..((y + 1) * w * 3) as usize];
        for x in x0..x1 {
            let p = &row[(x * 3) as usize..(x * 3 + 3) as usize];
            sum += (29 * p[0] as u64 + 150 * p[1] as u64 + 77 * p[2] as u64) >> 8;
            n += 1;
        }
    }
    if n == 0 { 0.5 } else { sum as f32 / n as f32 / 255.0 }
}

pub struct Detector {
    tx: Sender<Job>,
    rx: Receiver<Done>,
    spare: Option<Vec<u8>>,
}

impl Detector {
    pub fn new(threshold: i32) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        std::thread::Builder::new()
            .name("yunet".into())
            .spawn(move || {
                let mut net = YuNet::new();
                let mut bgr = Vec::new();
                let mut raw = Vec::new();
                for job in job_rx {
                    let t0 = Instant::now();
                    let n = (job.w * job.h) as usize;
                    bgr.resize(n * 3, 0);
                    let px = bgr.chunks_exact_mut(3).zip(job.thumb.chunks_exact(4));
                    if job.rgb {
                        px.for_each(|(d, s)| d.copy_from_slice(&[s[2], s[1], s[0]])); // RGBx -> BGR
                    } else {
                        px.for_each(|(d, s)| d.copy_from_slice(&s[..3])); // BGRx -> BGR
                    }
                    net.detect(&mut bgr, job.w, job.h, &mut raw);
                    let faces = raw
                        .iter()
                        .filter(|f| f.score >= threshold)
                        .map(|f| Face {
                            x: f.x * job.sx,
                            y: f.y * job.sy,
                            w: f.w * job.sx,
                            h: f.h * job.sy,
                            score: f.score,
                            luma: face_luma(&bgr, job.w, job.h, f),
                        })
                        .collect();
                    let ms = t0.elapsed().as_secs_f64() * 1e3;
                    if done_tx.send(Done { thumb: job.thumb, faces, ms }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn detector thread");
        Self { tx: job_tx, rx: done_rx, spare: Some(Vec::new()) }
    }

    /// A thumbnail buffer to fill, if the worker is idle.
    pub fn take_buffer(&mut self) -> Option<Vec<u8>> {
        self.spare.take()
    }

    /// Hand a filled thumbnail (BGRx, w*h*4) to the worker. `sx`/`sy` map thumbnail
    /// coordinates back to camera pixels. `rgb`: thumbnail is R,G,B,x instead of B,G,R,x.
    pub fn submit(&mut self, thumb: Vec<u8>, w: u32, h: u32, sx: f32, sy: f32, rgb: bool) {
        let _ = self.tx.send(Job { thumb, w, h, sx, sy, rgb });
    }

    /// Latest finished detection, if any.
    pub fn poll(&mut self) -> Option<(Vec<Face>, f64)> {
        match self.rx.try_recv() {
            Ok(d) => {
                self.spare = Some(d.thumb);
                Some((d.faces, d.ms))
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}
