//! Small helpers for building SPA pods (format and buffer parameters).

use crate::camera::Mode;
use pipewire::spa;
use spa::param::ParamType;
use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use spa::param::video::VideoFormat;
use spa::pod::serialize::PodSerializer;
use spa::pod::{ChoiceValue, Object, Property, Value};
use spa::sys as spa_sys;
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

pub fn object(type_: SpaTypes, id: ParamType, props: Vec<Property>) -> Vec<u8> {
    let obj = Object { type_: type_.as_raw(), id: id.as_raw(), properties: props };
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .expect("serialize pod")
        .0
        .into_inner()
}

fn prop(key: u32, value: Value) -> Property {
    Property::new(key, value)
}

fn id(v: u32) -> Value {
    Value::Id(Id(v))
}

fn video_props(subtype: MediaSubtype, size: (u32, u32), rate: Option<(u32, u32)>) -> Vec<Property> {
    let mut props = vec![
        prop(FormatProperties::MediaType.as_raw(), id(MediaType::Video.as_raw())),
        prop(FormatProperties::MediaSubtype.as_raw(), id(subtype.as_raw())),
        prop(FormatProperties::VideoSize.as_raw(), Value::Rectangle(Rectangle { width: size.0, height: size.1 })),
    ];
    if let Some((num, denom)) = rate {
        props.push(prop(FormatProperties::VideoFramerate.as_raw(), Value::Fraction(Fraction { num, denom })));
    }
    props
}

/// `EnumFormat` for raw video in one of `formats` (first is preferred).
pub fn raw_pod(formats: &[VideoFormat], size: (u32, u32), rate: Option<(u32, u32)>) -> Vec<u8> {
    let fmt = if formats.len() == 1 {
        id(formats[0].as_raw())
    } else {
        Value::Choice(ChoiceValue::Id(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: Id(formats[0].as_raw()),
                alternatives: formats.iter().map(|f| Id(f.as_raw())).collect(),
            },
        )))
    };
    let mut props = video_props(MediaSubtype::Raw, size, rate);
    props.insert(2, prop(FormatProperties::VideoFormat.as_raw(), fmt));
    object(SpaTypes::ObjectParamFormat, ParamType::EnumFormat, props)
}

/// `EnumFormat` for MJPEG.
pub fn mjpg_pod(size: (u32, u32), rate: Option<(u32, u32)>) -> Vec<u8> {
    object(SpaTypes::ObjectParamFormat, ParamType::EnumFormat, video_props(MediaSubtype::Mjpg, size, rate))
}

/// `EnumFormat` asking a camera for exactly this mode.
pub fn mode_pod(m: &Mode) -> Vec<u8> {
    match m.format.video_format() {
        Some(vf) => raw_pod(&[vf], (m.width, m.height), m.rate),
        None => mjpg_pod((m.width, m.height), m.rate),
    }
}

/// `Buffers` param for single-plane output buffers of `size` bytes.
pub fn buffers_pod(size: i32, stride: i32) -> Vec<u8> {
    object(
        SpaTypes::ObjectParamBuffers,
        ParamType::Buffers,
        vec![
            prop(
                spa_sys::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range { default: 4, min: 2, max: 8 },
                ))),
            ),
            prop(spa_sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            prop(spa_sys::SPA_PARAM_BUFFERS_size, Value::Int(size)),
            prop(spa_sys::SPA_PARAM_BUFFERS_stride, Value::Int(stride)),
            prop(
                spa_sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: (1 << spa_sys::SPA_DATA_MemFd) | (1 << spa_sys::SPA_DATA_MemPtr),
                        flags: vec![],
                    },
                ))),
            ),
        ],
    )
}

/// `Meta` param requesting a buffer header (timestamps, sequence numbers).
pub fn header_meta_pod() -> Vec<u8> {
    object(
        SpaTypes::ObjectParamMeta,
        ParamType::Meta,
        vec![
            prop(spa_sys::SPA_PARAM_META_type, id(spa_sys::SPA_META_Header)),
            prop(spa_sys::SPA_PARAM_META_size, Value::Int(std::mem::size_of::<spa_sys::spa_meta_header>() as i32)),
        ],
    )
}
