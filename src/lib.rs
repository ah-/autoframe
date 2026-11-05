//! autoframe: auto-framing virtual camera for PipeWire.
//!
//! The binary (`src/main.rs`) wires these modules to PipeWire; they are a library so the
//! framing simulator, tests and benchmarks can use them too.

pub mod camera;
pub mod convert;
pub mod detector;
pub mod framer;
pub mod pod;
pub mod relight;
pub mod sim;
pub mod yunet;
