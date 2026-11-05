//! Compiles the YuNet face detector (libfacedetection, C++) into a static library.

fn main() {
    let fd = "third_party/libfacedetection/src";
    if !std::path::Path::new(fd).join("facedetectcnn.cpp").exists() {
        panic!("{fd} is missing. Run: git submodule update --init");
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .opt_level(3)
        .include(fd)
        .include("csrc")
        .warnings(false)
        .file(format!("{fd}/facedetectcnn.cpp"))
        .file(format!("{fd}/facedetectcnn-data.cpp"))
        .file(format!("{fd}/facedetectcnn-model.cpp"))
        .file("csrc/yunet_shim.cpp");

    // libfacedetection has hand-written SIMD kernels selected at compile time.
    match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") if std::env::var_os("CARGO_FEATURE_AVX2").is_some() => {
            build.define("_ENABLE_AVX2", None).flag("-mavx2").flag("-mfma");
        }
        Ok("aarch64") => {
            build.define("_ENABLE_NEON", None);
        }
        _ => {}
    }
    build.compile("yunet");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed={fd}");
}
