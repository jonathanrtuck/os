//! Build script: compiles userspace binaries into ELFs that the kernel embeds
//! via `include_bytes!`. This keeps `cargo run --release` as the only command
//! needed to build and boot the entire system.
//!
//! Each user program is a single-file `#![no_std]` Rust binary linked with a
//! shared userspace linker script. The result is a statically-linked ELF with
//! no relocations.

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// User programs to compile. Each entry is (name, source directory).
const USER_PROGRAMS: &[(&str, &str)] = &[("init", "../user/init"), ("echo", "../user/echo")];

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let rustc = env::var("RUSTC").unwrap();
    // All user programs share the same linker script.
    let link_ld = manifest_dir.join("../user/link.ld");

    for &(name, dir) in USER_PROGRAMS {
        let src_dir = manifest_dir.join(dir);
        let main_rs = src_dir.join("main.rs");
        let elf_path = out_dir.join(format!("{name}.elf"));
        let status = Command::new(&rustc)
            .arg("--target=aarch64-unknown-none")
            .arg("--edition=2021")
            .arg("--crate-type=bin")
            .args(["-C", "panic=abort"])
            .arg(format!("-Clink-arg=-T{}", link_ld.display()))
            .arg("-o")
            .arg(&elf_path)
            .arg(&main_rs)
            .status()
            .unwrap_or_else(|e| panic!("failed to invoke rustc for {name}: {e}"));

        assert!(status.success(), "failed to build {name}.elf");

        println!("cargo:rerun-if-changed={}", main_rs.display());
    }

    println!("cargo:rerun-if-changed={}", link_ld.display());
}
