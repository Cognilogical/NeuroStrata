fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");
    } else if target_os == "linux" || (target_os == "windows" && target_env == "gnu") {
        println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
    }
}