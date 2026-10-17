// Exposes the target triple to the code, so `bird self-update` downloads the
// release archive for the platform this binary was built for.
fn main() {
    println!(
        "cargo:rustc-env=BIRD_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    println!("cargo:rerun-if-changed=build.rs");
}
