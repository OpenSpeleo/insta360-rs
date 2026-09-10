fn main() {
    println!("cargo:rerun-if-env-changed=MNN_ROOT");
    println!("cargo:rerun-if-changed=src-rust/underwater/mnn_shim.cpp");
    #[cfg(feature = "underwater-ai")]
    build_mnn();
}

#[cfg(feature = "underwater-ai")]
fn build_mnn() {
    let root = std::path::PathBuf::from(std::env::var_os("MNN_ROOT").expect(
        "underwater-ai requires MNN_ROOT from python3 scripts/ci/build-mnn.py --output <directory>",
    ));
    let marker = std::fs::read_to_string(root.join("insta360-mnn-version.txt"))
        .expect("MNN_ROOT is missing its verified source-build marker");
    assert_eq!(
        marker.trim(),
        "3.6.1 d407447ed56c4121a11ccbd266dc184ca1ead0c2",
        "MNN_ROOT must contain the pinned MNN 3.6.1 CPU build"
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("insta360-mnn-version.txt").display()
    );
    let mut compiler = cc::Build::new();
    compiler
        .cpp(true)
        .std("c++17")
        .include(root.join("include"))
        .file("src-rust/underwater/mnn_shim.cpp")
        .flag_if_supported("-fvisibility=hidden");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // The shim catches C++ exceptions before returning through its C ABI.
        // Match MNN_WIN_RUNTIME_MT=OFF in the pinned source builder.
        compiler.flag("/EHsc").static_crt(false);
    }
    compiler.compile("insta360_underwater_mnn");
    println!(
        "cargo:rustc-link-search=native={}",
        root.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=MNN");
}
