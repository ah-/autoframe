//! Camera frames → tightly packed 4-byte pixels (B,G,R,x or R,G,B,x) for scaling and detection.

use crate::camera::InFormat;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

/// One mapped data plane of a camera buffer.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    /// Bytes per row (0 = unknown / not applicable).
    pub stride: usize,
}

/// Whether frames in `format` can be used in place (packed RGB without row padding).
pub fn is_zero_copy(format: InFormat, stride: usize, width: u32) -> bool {
    matches!(format, InFormat::Bgrx | InFormat::Bgra | InFormat::Rgbx | InFormat::Rgba)
        && (stride == 0 || stride == width as usize * 4)
}

/// True if the packed output of `format` is R,G,B,x ordered (else B,G,R,x).
pub fn is_rgb_order(format: InFormat) -> bool {
    // MJPEG decodes to RGBA: the decoder supports that for both YCbCr and RGB JPEGs.
    matches!(format, InFormat::Rgbx | InFormat::Rgba | InFormat::Mjpg)
}

#[derive(Default)]
pub struct Converter {
    pub buf: Vec<u8>,
    rgb: Vec<u8>,
}

impl Converter {
    /// Converts into `self.buf` (w*h*4, byte order per [`is_rgb_order`]). Call only when
    /// [`is_zero_copy`] is false.
    pub fn convert(&mut self, format: InFormat, planes: &[Plane], w: u32, h: u32) -> Result<(), String> {
        let (wu, hu) = (w as usize, h as usize);
        self.buf.resize(wu * hu * 4, 0);
        let p0 = planes.first().ok_or("no data plane")?;
        let range = yuv::YuvRange::Limited;
        let matrix = yuv::YuvStandardMatrix::Bt601;
        let err = |e: yuv::YuvError| format!("{format:?} conversion: {e:?}");
        match format {
            InFormat::Bgrx | InFormat::Bgra | InFormat::Rgbx | InFormat::Rgba => {
                // Packed RGB with padded rows: drop the padding.
                let row = wu * 4;
                for (dst, src) in self.buf.chunks_exact_mut(row).zip(p0.data.chunks(p0.stride)) {
                    dst.copy_from_slice(src.get(..row).ok_or("short frame")?);
                }
            }
            InFormat::Yuy2 => {
                let stride = if p0.stride == 0 { wu * 2 } else { p0.stride };
                let img = yuv::YuvPackedImage { yuy: p0.data, yuy_stride: stride as u32, width: w, height: h };
                yuv::yuyv422_to_bgra(&img, &mut self.buf, w * 4, range, matrix).map_err(err)?;
            }
            InFormat::Nv12 => {
                let ys = if p0.stride == 0 { wu } else { p0.stride };
                let (y, uv, uvs) = match planes.get(1) {
                    Some(p1) => (p0.data, p1.data, if p1.stride == 0 { ys } else { p1.stride }),
                    None => {
                        let (y, uv) = p0.data.split_at((ys * hu).min(p0.data.len()));
                        (y, uv, ys)
                    }
                };
                let img = yuv::YuvBiPlanarImage {
                    y_plane: y,
                    y_stride: ys as u32,
                    uv_plane: uv,
                    uv_stride: uvs as u32,
                    width: w,
                    height: h,
                };
                yuv::yuv_nv12_to_bgra(&img, &mut self.buf, w * 4, range, matrix, yuv::YuvConversionMode::Balanced)
                    .map_err(err)?;
            }
            InFormat::I420 => {
                let ys = if p0.stride == 0 { wu } else { p0.stride };
                let (y, u, v, cs) = if planes.len() >= 3 {
                    let cs = if planes[1].stride == 0 { ys / 2 } else { planes[1].stride };
                    (p0.data, planes[1].data, planes[2].data, cs)
                } else {
                    let cs = ys / 2;
                    let csize = cs * hu.div_ceil(2);
                    let d = p0.data;
                    let ysize = (ys * hu).min(d.len());
                    let (y, rest) = d.split_at(ysize);
                    let (u, v) = rest.split_at(csize.min(rest.len()));
                    (y, u, v, cs)
                };
                let img = yuv::YuvPlanarImage {
                    y_plane: y,
                    y_stride: ys as u32,
                    u_plane: u,
                    u_stride: cs as u32,
                    v_plane: v,
                    v_stride: cs as u32,
                    width: w,
                    height: h,
                };
                yuv::yuv420_to_bgra(&img, &mut self.buf, w * 4, range, matrix).map_err(err)?;
            }
            InFormat::Mjpg => {
                let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
                let mut dec = JpegDecoder::new_with_options(ZCursor::new(p0.data), opts);
                dec.decode_headers().map_err(|e| format!("MJPEG header: {e:?}"))?;
                let info = dec.info().ok_or("MJPEG: no image info")?;
                if (info.width as u32, info.height as u32) != (w, h) {
                    return Err(format!("MJPEG frame is {}x{}, expected {w}x{h}", info.width, info.height));
                }
                if dec.decode_into(&mut self.buf).is_err() {
                    // RGB-coded JPEGs (rare in webcams) can't go straight to RGBA: decode to RGB
                    // and expand.
                    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
                    let mut dec = JpegDecoder::new_with_options(ZCursor::new(p0.data), opts);
                    self.rgb.resize(wu * hu * 3, 0);
                    dec.decode_into(&mut self.rgb).map_err(|e| format!("MJPEG decode: {e:?}"))?;
                    for (d, s) in self.buf.chunks_exact_mut(4).zip(self.rgb.chunks_exact(3)) {
                        d.copy_from_slice(&[s[0], s[1], s[2], 255]);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_yuy2_becomes_grey_bgrx() {
        let (w, h) = (4u32, 2u32);
        let yuy2: Vec<u8> = [128u8, 128, 128, 128].repeat((w * h / 2) as usize);
        let mut c = Converter::default();
        c.convert(InFormat::Yuy2, &[Plane { data: &yuy2, stride: (w * 2) as usize }], w, h).unwrap();
        for px in c.buf.chunks_exact(4) {
            assert!(px[..3].iter().all(|&v| (125..=135).contains(&v)), "{px:?}");
        }
    }

    #[test]
    fn nv12_contiguous_and_split_planes_agree() {
        let (w, h) = (4u32, 4u32);
        let mut nv12 = vec![200u8; 16];
        nv12.extend([100u8, 160].repeat(4));
        let mut a = Converter::default();
        a.convert(InFormat::Nv12, &[Plane { data: &nv12, stride: 4 }], w, h).unwrap();
        let mut b = Converter::default();
        b.convert(
            InFormat::Nv12,
            &[Plane { data: &nv12[..16], stride: 4 }, Plane { data: &nv12[16..], stride: 4 }],
            w,
            h,
        )
        .unwrap();
        assert_eq!(a.buf, b.buf);
    }

    #[test]
    fn padded_rgb_rows_are_packed() {
        let (w, h) = (2u32, 2u32);
        let stride = 12; // 8 bytes of pixels + 4 bytes padding per row
        let data: Vec<u8> = (0..24).collect();
        let mut c = Converter::default();
        c.convert(InFormat::Bgrx, &[Plane { data: &data, stride }], w, h).unwrap();
        assert_eq!(c.buf, [0, 1, 2, 3, 4, 5, 6, 7, 12, 13, 14, 15, 16, 17, 18, 19]);
    }
}
