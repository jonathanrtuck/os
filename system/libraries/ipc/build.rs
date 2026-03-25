fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let config_path = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("system_config.rs");

    println!("cargo:rustc-env=SYSTEM_CONFIG={}", config_path.display());
    println!("cargo:rerun-if-changed={}", config_path.display());
}
