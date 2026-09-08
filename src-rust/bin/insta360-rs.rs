fn main() {
    if let Err(error) = insta360_rs::media::cli::run() {
        eprintln!("insta360-rs: {error}");
        std::process::exit(1);
    }
}
