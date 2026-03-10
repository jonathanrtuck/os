//! Build script: compiles userspace binaries into ELFs that the kernel embeds
//! via `include_bytes!`. This keeps `cargo run --release` as the only command
//! needed to build and boot the entire system.
//!
//! # Build order
//!
//! 1. Shared libraries: sys, virtio, drawing (rlibs)
//! 2. All user/driver/compositor programs (ELFs)
//! 3. Generate `init_embedded.rs` with `include_bytes!` for ELFs init needs
//! 4. Compile init last (depends on all other ELFs via init_embedded.rs)
//! 5. Kernel embeds only init.elf
//!
//! Init is the proto-OS-service: it embeds all other ELFs so it can spawn
//! them at runtime. The kernel spawns only init (microkernel pattern).

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// ELFs that init embeds (must be a subset of PROGRAMS names).
const INIT_EMBEDDED: &[(&str, &str)] = &[
    ("virtio-blk", "VIRTIO_BLK_ELF"),
    ("virtio-console", "VIRTIO_CONSOLE_ELF"),
    ("virtio-gpu", "VIRTIO_GPU_ELF"),
    ("compositor", "COMPOSITOR_ELF"),
];
/// Programs compiled BEFORE init (init embeds their ELFs).
/// Each entry is (name, source directory, needs_virtio, needs_drawing).
const PROGRAMS: &[(&str, &str, bool, bool)] = &[
    ("echo", "../user/echo", false, false),
    ("virtio-blk", "../platform/drivers/virtio-blk", true, false),
    (
        "virtio-console",
        "../platform/drivers/virtio-console",
        true,
        false,
    ),
    ("virtio-gpu", "../platform/drivers/virtio-gpu", true, false),
    ("compositor", "../platform/compositor", false, true),
];

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let rustc = env::var("RUSTC").unwrap();
    let link_ld = manifest_dir.join("../library/link.ld");
    // Step 1: Compile shared libraries.
    let sys_src = manifest_dir.join("../library/sys/lib.rs");
    let sys_rlib = out_dir.join("libsys.rlib");

    rustc_rlib(&rustc, &sys_src, &sys_rlib, "sys", &[]);

    let virtio_src = manifest_dir.join("../library/virtio/lib.rs");
    let virtio_rlib = out_dir.join("libvirtio.rlib");

    rustc_rlib(
        &rustc,
        &virtio_src,
        &virtio_rlib,
        "virtio",
        &[("sys", &sys_rlib)],
    );

    let drawing_src = manifest_dir.join("../library/drawing/lib.rs");
    let drawing_rlib = out_dir.join("libdrawing.rlib");

    rustc_rlib(&rustc, &drawing_src, &drawing_rlib, "drawing", &[]);

    // Step 2: Compile all non-init programs.
    for &(name, dir, needs_virtio, needs_drawing) in PROGRAMS {
        let src_dir = manifest_dir.join(dir);
        let main_rs = src_dir.join("main.rs");
        let elf_path = out_dir.join(format!("{name}.elf"));
        let mut externs = vec![("sys", sys_rlib.clone())];

        if needs_virtio {
            externs.push(("virtio", virtio_rlib.clone()));
        }
        if needs_drawing {
            externs.push(("drawing", drawing_rlib.clone()));
        }

        rustc_bin(&rustc, &main_rs, &elf_path, &link_ld, &externs, &[]);
        println!("cargo:rerun-if-changed={}", main_rs.display());
    }

    // Step 3: Generate init_embedded.rs with include_bytes! for embedded ELFs.
    let mut embedded_code = String::new();

    for &(name, const_name) in INIT_EMBEDDED {
        let elf_path = out_dir.join(format!("{name}.elf"));

        embedded_code.push_str(&format!(
            "static {const_name}: &[u8] = include_bytes!(\"{}\");\n",
            elf_path.display()
        ));
    }

    let embedded_rs = out_dir.join("init_embedded.rs");

    std::fs::write(&embedded_rs, &embedded_code)
        .unwrap_or_else(|e| panic!("failed to write init_embedded.rs: {e}"));

    // Step 4: Compile init last (it embeds all other ELFs).
    let init_src = manifest_dir.join("../platform/init/main.rs");
    let init_elf = out_dir.join("init.elf");
    let init_env = [(
        "INIT_EMBEDDED_RS",
        embedded_rs.to_str().unwrap().to_string(),
    )];

    rustc_bin(
        &rustc,
        &init_src,
        &init_elf,
        &link_ld,
        &[("sys", sys_rlib.clone())],
        &init_env,
    );
    println!("cargo:rerun-if-changed={}", init_src.display());
    println!("cargo:rerun-if-changed={}", link_ld.display());
    println!("cargo:rerun-if-changed={}", sys_src.display());
    println!("cargo:rerun-if-changed={}", virtio_src.display());
    println!("cargo:rerun-if-changed={}", drawing_src.display());
}
/// Compile a Rust source file as a binary ELF.
fn rustc_bin(
    rustc: &str,
    src: &PathBuf,
    output: &PathBuf,
    link_ld: &PathBuf,
    externs: &[(&str, PathBuf)],
    env_vars: &[(&str, String)],
) {
    let mut cmd = Command::new(rustc);

    cmd.arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=bin")
        .args(["-C", "panic=abort"])
        .arg(format!("-Clink-arg=-T{}", link_ld.display()));

    for (name, path) in externs {
        cmd.arg(format!("--extern={name}={}", path.display()));
    }
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let status = cmd
        .arg("-o")
        .arg(output)
        .arg(src)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "failed to invoke rustc for {}: {e}",
                src.file_name().unwrap().to_str().unwrap()
            )
        });

    assert!(
        status.success(),
        "failed to build {}",
        output.file_name().unwrap().to_str().unwrap()
    );
}
/// Compile a Rust source file as an rlib.
fn rustc_rlib(
    rustc: &str,
    src: &PathBuf,
    output: &PathBuf,
    crate_name: &str,
    externs: &[(&str, &PathBuf)],
) {
    let mut cmd = Command::new(rustc);

    cmd.arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=rlib")
        .arg(format!("--crate-name={crate_name}"))
        .args(["-C", "panic=abort"]);

    for &(name, path) in externs {
        cmd.arg(format!("--extern={name}={}", path.display()));
    }

    let status = cmd
        .arg("-o")
        .arg(output)
        .arg(src)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke rustc for {crate_name}: {e}"));

    assert!(status.success(), "failed to build {crate_name}.rlib");
}
