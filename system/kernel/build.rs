//! Build script: compiles the init userspace binary into an ELF that the
//! kernel embeds via `include_bytes!`. This keeps `cargo run --release` as
//! the only command needed to build and boot the entire system.
//!
//! Approach: generate a minimal Rust wrapper that pulls in init.S via
//! `global_asm!`, then invoke rustc directly targeting aarch64-unknown-none.
//! The result is a statically-linked ELF with no relocations.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let init_dir = manifest_dir.join("../user/init");
    let init_s = init_dir.join("init.S");
    let link_ld = init_dir.join("link.ld");
    let rustc = env::var("RUSTC").unwrap();
    // Generate a wrapper .rs that includes the assembly source.
    // The panic handler is dead code but required by the no_std binary ABI.
    let wrapper_path = out_dir.join("init_wrapper.rs");
    let wrapper = format!(
        "#![no_std]\n#![no_main]\ncore::arch::global_asm!(include_str!(\"{}\"));\n#[panic_handler]\nfn _p(_:&core::panic::PanicInfo)->!{{loop{{}}}}\n",
        init_s.display()
    );

    std::fs::write(&wrapper_path, wrapper).expect("failed to write wrapper");

    let elf_path = out_dir.join("init.elf");
    let status = Command::new(&rustc)
        .arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=bin")
        .args(["-C", "panic=abort"])
        .arg(format!("-Clink-arg=-T{}", link_ld.display()))
        .arg("-o")
        .arg(&elf_path)
        .arg(&wrapper_path)
        .status()
        .expect("failed to invoke rustc");

    assert!(status.success(), "failed to build init.elf");

    println!("cargo:rerun-if-changed={}", init_s.display());
    println!("cargo:rerun-if-changed={}", link_ld.display());
}
