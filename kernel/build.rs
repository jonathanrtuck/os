use std::{
    env,
    path::PathBuf,
    process::Command,
};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "none" {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let link_ld = manifest_dir.join("link.ld");
        println!("cargo:rustc-link-arg=-T{}", link_ld.display());

        build_init(&manifest_dir);
    }

    println!("cargo:rerun-if-changed=link.ld");
}

fn build_init(kernel_dir: &std::path::Path) {
    let init_dir = kernel_dir.join("../init");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let init_bin = out_dir.join("init.bin");

    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--manifest-path",
            &init_dir.join("Cargo.toml").display().to_string(),
        ])
        .status()
        .expect("failed to build init crate");

    if !status.success() {
        panic!("init crate build failed");
    }

    let init_elf = init_dir.join("target/aarch64-unknown-none/release/init");

    let status = Command::new("rust-objcopy")
        .args([
            "-O",
            "binary",
            &init_elf.display().to_string(),
            &init_bin.display().to_string(),
        ])
        .status()
        .expect("failed to run rust-objcopy on init binary");

    if !status.success() {
        panic!("rust-objcopy failed on init binary");
    }

    println!("cargo:rerun-if-changed=../init/src/main.rs");
    println!("cargo:rerun-if-changed=../init/link.ld");
    println!("cargo:rerun-if-changed=../init/Cargo.toml");
}
