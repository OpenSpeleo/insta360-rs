fn main() {
    println!("cargo:rerun-if-changed=src-rust/mnn_shim.cpp");
    #[cfg(any(feature = "underwater-ai", feature = "ai-stitching"))]
    build_mnn();
}

#[cfg(any(feature = "underwater-ai", feature = "ai-stitching"))]
fn build_mnn() {
    let include = std::env::var_os("DEP_MNN_INCLUDE")
        .expect("mnn-src must provide the installed native headers");
    assert_eq!(
        std::env::var("DEP_MNN_VERSION").as_deref(),
        Ok("3.6.2"),
        "AI models require MNN 3.6.2"
    );
    assert_eq!(
        std::env::var("DEP_MNN_LINK_KIND").as_deref(),
        Ok("static"),
        "AI models require statically linked MNN"
    );
    println!(
        "cargo:license_dir={}",
        std::env::var("DEP_MNN_LICENSE_DIR").expect("mnn-src must provide native notices")
    );
    let mut compiler = cc::Build::new();
    compiler
        .cpp(true)
        .std("c++17")
        .include(include)
        .file("src-rust/mnn_shim.cpp")
        .flag_if_supported("-fvisibility=hidden");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // The shim catches C++ exceptions before returning through its C ABI.
        // Match mnn-src and the application's dynamic MSVC runtime.
        compiler.flag("/EHsc").static_crt(false);
    }
    compiler.compile("insta360_mnn");
}
