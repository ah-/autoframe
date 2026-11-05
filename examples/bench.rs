//! Microbenchmark of each pipeline stage on synthetic data.
use autoframe::yunet;
use fast_image_resize as fr;
use std::time::Instant;

fn time<F: FnMut()>(name: &str, n: u32, mut f: F) {
    f();
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    println!("{name:<40} {:7.3} ms", t.elapsed().as_secs_f64() * 1e3 / n as f64);
}

fn main() {
    let (w, h) = (1920u32, 1080u32);
    let mut src = vec![0u8; (w * h * 4) as usize];
    for (i, b) in src.iter_mut().enumerate() {
        *b = (i * 7 % 251) as u8;
    }
    let mut r = fr::Resizer::new();
    let mut out = vec![0u8; 1280 * 720 * 4];
    let mut thumb = vec![0u8; 320 * 180 * 4];
    let s = fr::images::ImageRef::new(w, h, &src, fr::PixelType::U8x4).unwrap();

    for (name, alg) in [
        ("resize 1080p->720p conv bilinear", fr::ResizeAlg::Convolution(fr::FilterType::Bilinear)),
        ("resize 1080p->720p interp bilinear", fr::ResizeAlg::Interpolation(fr::FilterType::Bilinear)),
    ] {
        let o = fr::ResizeOptions::new().resize_alg(alg).use_alpha(false).crop(100.5, 50.25, 1600.0, 900.0);
        time(name, 100, || {
            let mut d = fr::images::Image::from_slice_u8(1280, 720, &mut out, fr::PixelType::U8x4).unwrap();
            r.resize(&s, &mut d, &o).unwrap();
        });
    }
    for (name, ft) in [
        ("crop 1.25x->720p Bilinear", fr::FilterType::Bilinear),
        ("crop 1.25x->720p Hamming", fr::FilterType::Hamming),
        ("crop 1.25x->720p CatmullRom", fr::FilterType::CatmullRom),
        ("crop 1.25x->720p Mitchell", fr::FilterType::Mitchell),
        ("crop 1.25x->720p Lanczos3", fr::FilterType::Lanczos3),
    ] {
        let o = fr::ResizeOptions::new()
            .resize_alg(fr::ResizeAlg::Convolution(ft))
            .use_alpha(false)
            .crop(200.3, 100.7, 1536.0, 864.0);
        time(name, 100, || {
            let mut d = fr::images::Image::from_slice_u8(1280, 720, &mut out, fr::PixelType::U8x4).unwrap();
            r.resize(&s, &mut d, &o).unwrap();
        });
    }
    let o =
        fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Interpolation(fr::FilterType::Bilinear)).use_alpha(false);
    time("thumb 1080p->320x180 interp bilinear", 100, || {
        let mut d = fr::images::Image::from_slice_u8(320, 180, &mut thumb, fr::PixelType::U8x4).unwrap();
        r.resize(&s, &mut d, &o).unwrap();
    });
    let mut yuvbuf = vec![0u8; 1280 * 720 * 3 / 2];
    {
        let mode = yuv::YuvConversionMode::Balanced;
        time(&format!("bgra->i420 720p {mode:?}"), 100, || {
            let (y, rest) = yuvbuf.split_at_mut(1280 * 720);
            let (u, v) = rest.split_at_mut(1280 * 720 / 4);
            let mut img = yuv::YuvPlanarImageMut {
                y_plane: yuv::BufferStoreMut::Borrowed(y),
                y_stride: 1280,
                u_plane: yuv::BufferStoreMut::Borrowed(u),
                u_stride: 640,
                v_plane: yuv::BufferStoreMut::Borrowed(v),
                v_stride: 640,
                width: 1280,
                height: 720,
            };
            yuv::bgra_to_yuv420(&mut img, &out, 1280 * 4, yuv::YuvRange::Limited, yuv::YuvStandardMatrix::Bt601, mode)
                .unwrap();
        });
    }
    let mut net = yunet::YuNet::new();
    let mut faces = Vec::new();
    for (tw, th) in [(320u32, 180u32), (256, 144), (160, 90)] {
        let mut bgr = vec![0u8; (tw * th * 3) as usize];
        for (i, b) in bgr.iter_mut().enumerate() {
            *b = (i * 13 % 255) as u8;
        }
        time(&format!("yunet {tw}x{th}"), 30, || net.detect(&mut bgr, tw, th, &mut faces));
    }
}
