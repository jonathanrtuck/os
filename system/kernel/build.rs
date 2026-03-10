//! Build script: compiles userspace binaries into ELFs that the kernel embeds
//! via `include_bytes!`. This keeps `cargo run --release` as the only command
//! needed to build and boot the entire system.
//!
//! Each user program is a single-file `#![no_std]` Rust binary linked with a
//! shared userspace linker script. All programs link against `libsys` (the
//! shared syscall wrapper crate). Virtio drivers additionally link against
//! `libvirtio` (the shared virtio transport crate).

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// User programs to compile. Each entry is (name, source directory, needs_virtio).
const USER_PROGRAMS: &[(&str, &str, bool)] = &[
    ("init", "../user/init", false),
    ("echo", "../user/echo", false),
    ("virtio-blk", "../platform/drivers/virtio-blk", true),
    ("virtio-console", "../platform/drivers/virtio-console", true),
    ("virtio-gpu", "../platform/drivers/virtio-gpu", true),
];

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let rustc = env::var("RUSTC").unwrap();
    let link_ld = manifest_dir.join("../library/link.ld");
    let libsys_src = manifest_dir.join("../library/libsys/lib.rs");
    let libsys_rlib = out_dir.join("libsys.rlib");
    let libvirtio_src = manifest_dir.join("../library/libvirtio/lib.rs");
    let libvirtio_rlib = out_dir.join("libvirtio.rlib");
    // Step 1: compile libsys as a static library (rlib).
    let status = Command::new(&rustc)
        .arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=rlib")
        .arg("--crate-name=sys")
        .args(["-C", "panic=abort"])
        .arg("-o")
        .arg(&libsys_rlib)
        .arg(&libsys_src)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke rustc for libsys: {e}"));

    assert!(status.success(), "failed to build libsys.rlib");

    // Step 2: compile libvirtio (depends on libsys for panic handler resolution).
    let status = Command::new(&rustc)
        .arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=rlib")
        .arg("--crate-name=virtio")
        .args(["-C", "panic=abort"])
        .arg(format!("--extern=sys={}", libsys_rlib.display()))
        .arg("-o")
        .arg(&libvirtio_rlib)
        .arg(&libvirtio_src)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke rustc for libvirtio: {e}"));

    assert!(status.success(), "failed to build libvirtio.rlib");

    // Step 3: compile each user program.
    for &(name, dir, needs_virtio) in USER_PROGRAMS {
        let src_dir = manifest_dir.join(dir);
        let main_rs = src_dir.join("main.rs");
        let elf_path = out_dir.join(format!("{name}.elf"));
        let mut cmd = Command::new(&rustc);

        cmd.arg("--target=aarch64-unknown-none")
            .arg("--edition=2021")
            .arg("--crate-type=bin")
            .args(["-C", "panic=abort"])
            .arg(format!("-Clink-arg=-T{}", link_ld.display()))
            .arg(format!("--extern=sys={}", libsys_rlib.display()));

        if needs_virtio {
            cmd.arg(format!("--extern=virtio={}", libvirtio_rlib.display()));
        }

        let status = cmd
            .arg("-o")
            .arg(&elf_path)
            .arg(&main_rs)
            .status()
            .unwrap_or_else(|e| panic!("failed to invoke rustc for {name}: {e}"));

        assert!(status.success(), "failed to build {name}.elf");
        println!("cargo:rerun-if-changed={}", main_rs.display());
    }

    println!("cargo:rerun-if-changed={}", link_ld.display());
    println!("cargo:rerun-if-changed={}", libsys_src.display());
    println!("cargo:rerun-if-changed={}", libvirtio_src.display());
}
