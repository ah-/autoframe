//! What a camera node can deliver, and which mode to ask it for.

use pipewire::spa;
use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use spa::param::video::VideoFormat;
use spa::pod::deserialize::PodDeserializer;
use spa::pod::{ChoiceValue, Pod, Value};
use spa::utils::{ChoiceEnum, Fraction, Id, Rectangle};

/// Camera pixel formats we can consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InFormat {
    Bgrx,
    Bgra,
    Rgbx,
    Rgba,
    Nv12,
    I420,
    Yuy2,
    Mjpg,
}

impl InFormat {
    pub fn from_video(f: VideoFormat) -> Option<Self> {
        Some(match f {
            VideoFormat::BGRx => Self::Bgrx,
            VideoFormat::BGRA => Self::Bgra,
            VideoFormat::RGBx => Self::Rgbx,
            VideoFormat::RGBA => Self::Rgba,
            VideoFormat::NV12 => Self::Nv12,
            VideoFormat::I420 => Self::I420,
            VideoFormat::YUY2 => Self::Yuy2,
            _ => return None,
        })
    }

    /// The raw video format, or None for compressed (MJPEG).
    pub fn video_format(self) -> Option<VideoFormat> {
        Some(match self {
            Self::Bgrx => VideoFormat::BGRx,
            Self::Bgra => VideoFormat::BGRA,
            Self::Rgbx => VideoFormat::RGBx,
            Self::Rgba => VideoFormat::RGBA,
            Self::Nv12 => VideoFormat::NV12,
            Self::I420 => VideoFormat::I420,
            Self::Yuy2 => VideoFormat::YUY2,
            Self::Mjpg => return None,
        })
    }

    /// Relative per-frame cost of turning this into packed RGB (lower is better).
    fn cost(self) -> f32 {
        match self {
            Self::Bgrx | Self::Bgra => 0.0,
            // Same work, but B,G,R order saves a channel swap for BGRx output.
            Self::Rgbx | Self::Rgba => 0.1,
            Self::Nv12 | Self::I420 => 1.0,
            Self::Yuy2 => 1.5,
            Self::Mjpg => 3.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bgrx => "BGRx",
            Self::Bgra => "BGRA",
            Self::Rgbx => "RGBx",
            Self::Rgba => "RGBA",
            Self::Nv12 => "NV12",
            Self::I420 => "I420",
            Self::Yuy2 => "YUY2",
            Self::Mjpg => "MJPEG",
        }
    }
}

/// One concrete camera mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    pub format: InFormat,
    pub width: u32,
    pub height: u32,
    /// Highest frame rate offered for this mode (num, denom), if the camera says.
    pub rate: Option<(u32, u32)>,
}

impl Mode {
    pub fn fps(&self) -> Option<f32> {
        self.rate.filter(|r| r.1 != 0).map(|(n, d)| n as f32 / d as f32)
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}x{}", self.format.name(), self.width, self.height)?;
        if let Some(fps) = self.fps() {
            write!(f, " @ {fps:.0} fps")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- parsing EnumFormat

fn ids(v: &Value) -> Vec<u32> {
    match v {
        Value::Id(Id(x)) => vec![*x],
        Value::Choice(ChoiceValue::Id(c)) => match &c.1 {
            ChoiceEnum::None(Id(x)) => vec![*x],
            ChoiceEnum::Enum { default, alternatives } => {
                let mut out = vec![default.0];
                out.extend(alternatives.iter().map(|i| i.0).filter(|x| *x != default.0));
                out
            }
            _ => vec![],
        },
        _ => vec![],
    }
}

/// Common sizes to try when a camera advertises a continuous size range.
const COMMON_SIZES: [(u32, u32); 8] =
    [(640, 480), (1280, 720), (1280, 960), (1600, 900), (1920, 1080), (2560, 1440), (2592, 1944), (3840, 2160)];

fn sizes(v: &Value) -> Vec<(u32, u32)> {
    let range = |min: &Rectangle, max: &Rectangle| -> Vec<(u32, u32)> {
        let mut out: Vec<_> = COMMON_SIZES
            .iter()
            .copied()
            .filter(|&(w, h)| (min.width..=max.width).contains(&w) && (min.height..=max.height).contains(&h))
            .collect();
        out.push((max.width, max.height));
        out
    };
    match v {
        Value::Rectangle(r) => vec![(r.width, r.height)],
        Value::Choice(ChoiceValue::Rectangle(c)) => match &c.1 {
            ChoiceEnum::None(r) => vec![(r.width, r.height)],
            ChoiceEnum::Enum { default, alternatives } => {
                let mut out = vec![(default.width, default.height)];
                out.extend(alternatives.iter().map(|r| (r.width, r.height)));
                out.dedup();
                out
            }
            ChoiceEnum::Range { min, max, .. } | ChoiceEnum::Step { min, max, .. } => range(min, max),
            _ => vec![],
        },
        _ => vec![],
    }
}

fn max_rate(v: &Value) -> Option<(u32, u32)> {
    let key = |x: &Fraction| if x.denom == 0 { 0.0 } else { x.num as f64 / x.denom as f64 };
    let best =
        |xs: &mut dyn Iterator<Item = &Fraction>| xs.max_by(|a, b| key(a).total_cmp(&key(b))).map(|x| (x.num, x.denom));
    match v {
        Value::Fraction(x) => Some((x.num, x.denom)),
        Value::Choice(ChoiceValue::Fraction(c)) => match &c.1 {
            ChoiceEnum::None(x) => Some((x.num, x.denom)),
            ChoiceEnum::Enum { default, alternatives } => {
                best(&mut std::iter::once(default).chain(alternatives.iter()))
            }
            ChoiceEnum::Range { max, .. } | ChoiceEnum::Step { max, .. } => Some((max.num, max.denom)),
            _ => None,
        },
        _ => None,
    }
}

/// All usable modes described by one `EnumFormat` param.
pub fn parse_enum_format(pod: &Pod) -> Vec<Mode> {
    let Ok((_, Value::Object(obj))) = PodDeserializer::deserialize_any_from(pod.as_bytes()) else {
        return vec![];
    };
    let prop = |key: FormatProperties| obj.properties.iter().find(|p| p.key == key.as_raw()).map(|p| &p.value);

    if prop(FormatProperties::MediaType).map(ids) != Some(vec![MediaType::Video.as_raw()]) {
        return vec![];
    }
    let subtype = prop(FormatProperties::MediaSubtype).map(ids).unwrap_or_default();
    let formats: Vec<InFormat> = if subtype.contains(&MediaSubtype::Raw.as_raw()) {
        prop(FormatProperties::VideoFormat)
            .map(ids)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| InFormat::from_video(VideoFormat::from_raw(id)))
            .collect()
    } else if subtype.contains(&MediaSubtype::Mjpg.as_raw()) {
        vec![InFormat::Mjpg]
    } else {
        vec![]
    };
    let sizes = prop(FormatProperties::VideoSize).map(sizes).unwrap_or_default();
    let rate = prop(FormatProperties::VideoFramerate).and_then(max_rate);

    let mut out = Vec::new();
    for &format in &formats {
        for &(width, height) in &sizes {
            if width >= 2 && height >= 2 && width % 2 == 0 && height % 2 == 0 {
                out.push(Mode { format, width, height, rate });
            }
        }
    }
    out
}

// ---------------------------------------------------------------- choosing

/// Lower is better. Prefers: a usable frame rate, then a size close to `want` (slightly larger is
/// fine, smaller costs more), then the output's aspect ratio, then a cheap pixel format.
fn score(m: &Mode, want: (u32, u32)) -> f32 {
    let fps_penalty = match m.fps() {
        Some(f) if f < 14.5 => 10.0,
        _ => 0.0,
    };
    let ratio = (m.width as f32 * m.height as f32) / (want.0 as f32 * want.1 as f32);
    let size_penalty = if ratio >= 1.0 { (ratio - 1.0) * 0.5 } else { (1.0 - ratio) * 2.0 };
    let aspect_penalty = ((m.width as f32 / m.height as f32) / (want.0 as f32 / want.1 as f32)).ln().abs();
    fps_penalty + size_penalty + aspect_penalty + 0.15 * m.format.cost()
}

/// Best mode for capturing at roughly `want` (width, height).
pub fn choose(modes: &[Mode], want: (u32, u32)) -> Option<Mode> {
    modes.iter().copied().min_by(|a, b| score(a, want).total_cmp(&score(b, want)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(format: InFormat, width: u32, height: u32, fps: Option<u32>) -> Mode {
        Mode { format, width, height, rate: fps.map(|f| (f, 1)) }
    }

    #[test]
    fn exact_size_wins() {
        let modes = [
            m(InFormat::Bgrx, 640, 480, None),
            m(InFormat::Bgrx, 1920, 1080, None),
            m(InFormat::Bgrx, 1280, 720, None),
        ];
        assert_eq!(choose(&modes, (1920, 1080)), Some(modes[1]));
    }

    #[test]
    fn typical_uvc_webcam_picks_mjpeg_1080p() {
        // Raw YUYV is only fast at low resolution on USB 2; MJPEG gets full HD at 30 fps.
        let modes = [
            m(InFormat::Yuy2, 640, 480, Some(30)),
            m(InFormat::Yuy2, 1280, 720, Some(10)),
            m(InFormat::Yuy2, 1920, 1080, Some(5)),
            m(InFormat::Mjpg, 1280, 720, Some(30)),
            m(InFormat::Mjpg, 1920, 1080, Some(30)),
        ];
        assert_eq!(choose(&modes, (1920, 1080)), Some(modes[4]));
    }

    #[test]
    fn prefers_raw_over_mjpeg_at_same_size() {
        let modes = [m(InFormat::Mjpg, 1920, 1080, Some(30)), m(InFormat::Nv12, 1920, 1080, Some(30))];
        assert_eq!(choose(&modes, (1920, 1080)).unwrap().format, InFormat::Nv12);
    }

    #[test]
    fn avoids_huge_modes() {
        let modes = [m(InFormat::Nv12, 3840, 2160, Some(30)), m(InFormat::Nv12, 2560, 1440, Some(30))];
        assert_eq!(choose(&modes, (1920, 1080)).unwrap().width, 2560);
    }

    #[test]
    fn small_camera_still_works() {
        let modes = [m(InFormat::Yuy2, 640, 480, Some(30)), m(InFormat::Yuy2, 320, 240, Some(30))];
        assert_eq!(choose(&modes, (1920, 1080)).unwrap().width, 640);
    }

    #[test]
    fn parses_raw_and_mjpeg_formats() {
        use crate::pod::{mjpg_pod, raw_pod};
        let raw = raw_pod(&[VideoFormat::BGRx, VideoFormat::YUY2], (1920, 1080), None);
        let modes = parse_enum_format(Pod::from_bytes(&raw).unwrap());
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0], m(InFormat::Bgrx, 1920, 1080, None));
        assert_eq!(modes[1].format, InFormat::Yuy2);

        let mjpg = mjpg_pod((1280, 720), Some((30, 1)));
        let modes = parse_enum_format(Pod::from_bytes(&mjpg).unwrap());
        assert_eq!(modes, vec![m(InFormat::Mjpg, 1280, 720, Some(30))]);
    }
}
