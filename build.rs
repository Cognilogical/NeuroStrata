fn main() {
    #[cfg(unix)]
    println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
}
