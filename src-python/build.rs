fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // Wheel repair replaces short @rpath dependencies with longer
        // @loader_path/.dylibs paths. Reserve room in the Mach-O load commands.
        println!("cargo:rustc-link-arg-cdylib=-Wl,-headerpad_max_install_names");
    }
}
