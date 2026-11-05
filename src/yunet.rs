//! YuNet face detector (libfacedetection), compiled from C++ by build.rs.

const RESULT_BUFFER_SIZE: usize = 0x9000;
const RESULT_STRIDE_SHORTS: usize = 16;

unsafe extern "C" {
    fn yunet_detect(result: *mut u8, bgr: *mut u8, width: i32, height: i32, step: i32) -> *mut i32;
}

#[derive(Clone, Copy, Debug)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: i32,
    /// Mean luma (0..1) of the inner face region; filled in by the detector.
    pub luma: f32,
}

pub struct YuNet {
    result: Vec<u8>,
}

impl Default for YuNet {
    fn default() -> Self {
        Self::new()
    }
}

impl YuNet {
    pub fn new() -> Self {
        Self { result: vec![0; RESULT_BUFFER_SIZE] }
    }

    /// `bgr` is a tightly packed 3-channel B,G,R image.
    pub fn detect(&mut self, bgr: &mut [u8], width: u32, height: u32, out: &mut Vec<Face>) {
        assert!(bgr.len() >= width as usize * height as usize * 3);
        out.clear();
        // SAFETY: buffers are sized as the library requires; the result pointer points into
        // `self.result` and is only read before the next call.
        unsafe {
            let r = yunet_detect(
                self.result.as_mut_ptr(),
                bgr.as_mut_ptr(),
                width as i32,
                height as i32,
                (width * 3) as i32,
            );
            if r.is_null() {
                return;
            }
            let n = *r as usize;
            let shorts = r.add(1) as *const i16;
            for i in 0..n {
                let p = shorts.add(RESULT_STRIDE_SHORTS * i);
                out.push(Face {
                    score: *p as i32,
                    x: *p.add(1) as f32,
                    y: *p.add(2) as f32,
                    w: *p.add(3) as f32,
                    h: *p.add(4) as f32,
                    luma: 0.0,
                });
            }
        }
    }
}
