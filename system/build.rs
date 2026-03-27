//! Build script: compiles userspace binaries into ELFs that the kernel embeds
//! via `include_bytes!`. This keeps `cargo run --release` as the only command
//! needed to build and boot the entire system.
//!
//! # Build order
//!
//! 1. Shared libraries: sys, virtio, drawing (rlibs)
//!    1b. Cargo-managed libraries: fonts (with harfrust dependency tree)
//! 2. All user/driver/compositor programs (ELFs)
//! 3. Generate `init_embedded.rs` with `include_bytes!` for ELFs init needs
//! 4. Compile init last (depends on all other ELFs via init_embedded.rs)
//! 5. Kernel embeds only init.elf
//!
//! Init is the proto-OS-service: it embeds all other ELFs so it can spawn
//! them at runtime. The kernel spawns only init (microkernel pattern).

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

/// System-wide constants (SSOT). Included as a module so build.rs can use
/// the values for linker script generation and linker flags.
mod system_config {
    #![allow(dead_code)]
    include!("system_config.rs");
}

/// Output of building a Cargo-managed library for the bare-metal target.
#[allow(dead_code)]
struct CargoLibOutput {
    /// Path to the main rlib file.
    rlib: PathBuf,
    /// Path to the deps directory containing transitive dependency rlibs.
    deps_dir: PathBuf,
}

/// ELFs that init embeds (must be a subset of PROGRAMS names).
const INIT_EMBEDDED: &[(&str, &str)] = &[
    ("document", "DOCUMENT_ELF"),
    ("virtio-blk", "VIRTIO_BLK_ELF"),
    ("virtio-console", "VIRTIO_CONSOLE_ELF"),
    ("virtio-input", "VIRTIO_INPUT_ELF"),
    ("virtio-9p", "VIRTIO_9P_ELF"),
    ("core", "CORE_ELF"),
    ("cpu-render", "CPU_RENDER_ELF"),
    ("virgil-render", "VIRGIL_RENDER_ELF"),
    ("metal-render", "METAL_RENDER_ELF"),
    ("png-decode", "PNG_DECODE_ELF"),
    ("text-editor", "TEXT_EDITOR_ELF"),
    ("stress", "STRESS_ELF"),
    ("fuzz", "FUZZ_ELF"),
];
/// Programs compiled BEFORE init (init embeds their ELFs).
/// Each entry is (name, source directory, needs_virtio, needs_drawing).
/// ORDER MATTERS: fuzz-helper must be before fuzz (fuzz embeds it).
const PROGRAMS: &[(&str, &str, bool, bool)] = &[
    ("echo", "user/echo", false, false),
    ("document", "services/document", true, false),
    ("virtio-blk", "services/drivers/virtio-blk", true, false),
    (
        "virtio-console",
        "services/drivers/virtio-console",
        true,
        false,
    ),
    ("virtio-input", "services/drivers/virtio-input", true, false),
    ("virtio-9p", "services/drivers/virtio-9p", true, false),
    ("core", "services/core", false, true),
    ("cpu-render", "services/drivers/cpu-render", true, true),
    (
        "virgil-render",
        "services/drivers/virgil-render",
        true,
        true,
    ),
    ("metal-render", "services/drivers/metal-render", true, true),
    ("png-decode", "services/decoders/png", false, false),
    ("text-editor", "user/text-editor", false, false),
    ("stress", "user/stress", false, false),
    ("fuzz-helper", "user/fuzz-helper", false, false),
    ("fuzz", "user/fuzz", false, false),
];

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let rustc = env::var("RUSTC").unwrap();

    let config_path = manifest_dir.join("system_config.rs");
    let config_path_str = config_path.to_str().unwrap();

    // Every child rustc process inherits this, so include!(env!("SYSTEM_CONFIG"))
    // resolves in all userspace libraries and programs.
    std::env::set_var("SYSTEM_CONFIG", config_path_str);

    // --- Generate linker scripts from templates ---
    generate_linker_script(
        &manifest_dir.join("kernel/link.ld.in"),
        &out_dir.join("kernel.ld"),
    );
    generate_linker_script(
        &manifest_dir.join("libraries/link.ld.in"),
        &out_dir.join("userspace.ld"),
    );

    // Tell Cargo to pass the kernel linker script and SYSTEM_CONFIG env var.
    println!("cargo:rustc-link-arg=-T{}/kernel.ld", out_dir.display());
    println!("cargo:rustc-env=SYSTEM_CONFIG={config_path_str}");

    // Rebuild when the config or templates change.
    println!("cargo:rerun-if-changed={config_path_str}");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("kernel/link.ld.in").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("libraries/link.ld.in").display()
    );

    let link_ld = out_dir.join("userspace.ld");
    // Step 1: Compile shared libraries.
    let sys_src = manifest_dir.join("libraries/sys/lib.rs");
    let sys_rlib = out_dir.join("libsys.rlib");

    rustc_rlib(&rustc, &sys_src, &sys_rlib, "sys", &[]);

    let protocol_src = manifest_dir.join("libraries/protocol/lib.rs");
    let protocol_rlib = out_dir.join("libprotocol.rlib");

    rustc_rlib(&rustc, &protocol_src, &protocol_rlib, "protocol", &[]);

    let animation_src = manifest_dir.join("libraries/animation/lib.rs");
    let animation_rlib = out_dir.join("libanimation.rlib");

    rustc_rlib(&rustc, &animation_src, &animation_rlib, "animation", &[]);

    let layout_src = manifest_dir.join("libraries/layout/lib.rs");
    let layout_rlib = out_dir.join("liblayout.rlib");

    rustc_rlib(&rustc, &layout_src, &layout_rlib, "layout", &[]);

    let piecetable_src = manifest_dir.join("libraries/piecetable/lib.rs");
    let piecetable_rlib = out_dir.join("libpiecetable.rlib");

    rustc_rlib(
        &rustc,
        &piecetable_src,
        &piecetable_rlib,
        "piecetable",
        &[],
    );

    let virtio_src = manifest_dir.join("libraries/virtio/lib.rs");
    let virtio_rlib = out_dir.join("libvirtio.rlib");

    rustc_rlib(
        &rustc,
        &virtio_src,
        &virtio_rlib,
        "virtio",
        &[("sys", &sys_rlib)],
    );

    let scene_src = manifest_dir.join("libraries/scene/lib.rs");
    let scene_rlib = out_dir.join("libscene.rlib");

    rustc_rlib(&rustc, &scene_src, &scene_rlib, "scene", &[]);

    let ipc_src = manifest_dir.join("libraries/ipc/lib.rs");
    let ipc_rlib = out_dir.join("libipc.rlib");

    rustc_rlib(&rustc, &ipc_src, &ipc_rlib, "ipc", &[("sys", &sys_rlib)]);

    let fs_src = manifest_dir.join("libraries/fs/lib.rs");
    let fs_rlib = out_dir.join("libfs.rlib");

    rustc_rlib(&rustc, &fs_src, &fs_rlib, "fs", &[]);

    let store_src = manifest_dir.join("libraries/store/lib.rs");
    let store_rlib = out_dir.join("libstore.rlib");

    rustc_rlib(
        &rustc,
        &store_src,
        &store_rlib,
        "store",
        &[("fs", &fs_rlib)],
    );

    // Step 1b: Build Cargo-managed libraries (libraries with external deps).
    // These use `cargo build` to resolve dependency graphs, then we link the
    // resulting rlibs alongside hand-compiled libraries.
    let fonts_output = cargo_lib(&manifest_dir.join("libraries/fonts"));

    // Drawing library depends on protocol and fonts (for rasterize API).
    // The fonts library has transitive dependencies (read-fonts, etc.) in
    // deps_dir (aarch64-unknown-none), plus proc-macro dependencies in the
    // host release deps dir.
    let drawing_src = manifest_dir.join("libraries/drawing/lib.rs");
    let drawing_rlib = out_dir.join("libdrawing.rlib");
    let fonts_host_deps = manifest_dir.join("libraries/fonts/target/release/deps");

    rustc_rlib_with_search(&rustc, &drawing_src, &drawing_rlib, "drawing", &[], &[]);

    // Render library: scene graph rendering, compositing, glyph rasterization.
    // Depends on drawing, scene, protocol, fonts (no sys, no ipc).
    let render_src = manifest_dir.join("libraries/render/lib.rs");
    let render_rlib = out_dir.join("librender.rlib");

    rustc_rlib_with_search(
        &rustc,
        &render_src,
        &render_rlib,
        "render",
        &[
            ("drawing", &drawing_rlib),
            ("scene", &scene_rlib),
            ("protocol", &protocol_rlib),
            ("fonts", &fonts_output.rlib),
        ],
        &[&fonts_output.deps_dir, &fonts_host_deps],
    );

    // Step 2: Compile all non-init programs.
    // fuzz-helper must be compiled before fuzz (fuzz embeds it).
    for &(name, dir, needs_virtio, needs_drawing) in PROGRAMS {
        let src_dir = manifest_dir.join(dir);
        let main_rs = src_dir.join("main.rs");
        let elf_path = out_dir.join(format!("{name}.elf"));
        let mut externs = vec![
            ("sys", sys_rlib.clone()),
            ("ipc", ipc_rlib.clone()),
            ("protocol", protocol_rlib.clone()),
        ];

        if needs_virtio {
            externs.push(("virtio", virtio_rlib.clone()));
        }
        if needs_drawing {
            externs.push(("drawing", drawing_rlib.clone()));
            externs.push(("scene", scene_rlib.clone()));
            externs.push(("fonts", fonts_output.rlib.clone()));
        }
        if name == "cpu-render"
            || name == "virgil-render"
            || name == "metal-render"
            || name == "core"
        {
            externs.push(("render", render_rlib.clone()));
        }
        if name == "core" {
            externs.push(("animation", animation_rlib.clone()));
            externs.push(("layout", layout_rlib.clone()));
            externs.push(("piecetable", piecetable_rlib.clone()));
        }
        if name == "document" {
            externs.push(("fs", fs_rlib.clone()));
            externs.push(("store", store_rlib.clone()));
        }

        // Fuzz embeds fuzz-helper (generate embedded RS, same pattern as init).
        let mut env_vars = Vec::new();

        if name == "fuzz" {
            let helper_elf = out_dir.join("fuzz-helper.elf");
            let fuzz_embedded = format!(
                "static HELPER_ELF: &[u8] = include_bytes!(\"{}\");\n",
                helper_elf.display()
            );
            let fuzz_embedded_rs = out_dir.join("fuzz_embedded.rs");
            std::fs::write(&fuzz_embedded_rs, &fuzz_embedded)
                .unwrap_or_else(|e| panic!("failed to write fuzz_embedded.rs: {e}"));

            env_vars.push((
                "FUZZ_EMBEDDED_RS",
                fuzz_embedded_rs.to_str().unwrap().to_string(),
            ));
        }

        // Add fonts library search paths for programs that need drawing.
        let search_paths: Vec<&Path> = if needs_drawing {
            vec![&fonts_output.deps_dir, &fonts_host_deps]
        } else {
            vec![]
        };

        rustc_bin(
            &rustc,
            &main_rs,
            &elf_path,
            &link_ld,
            &externs,
            &env_vars,
            &search_paths,
        );
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
    let init_src = manifest_dir.join("services/init/main.rs");
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
        &[
            ("sys", sys_rlib.clone()),
            ("ipc", ipc_rlib.clone()),
            ("protocol", protocol_rlib.clone()),
            ("scene", scene_rlib.clone()),
        ],
        &init_env,
        &[],
    );
    println!("cargo:rerun-if-changed={}", init_src.display());
    println!("cargo:rerun-if-changed={}", sys_src.display());
    println!("cargo:rerun-if-changed={}", virtio_src.display());
    println!("cargo:rerun-if-changed={}", drawing_src.display());
    for inc in &[
        "palette.rs",
        "gamma_tables.rs",
        "neon.rs",
        "blend.rs",
        "blit.rs",
        "blur.rs",
        "coverage.rs",
        "fill.rs",
        "gradient.rs",
        "line.rs",
        "transform.rs",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join("libraries/drawing").join(inc).display()
        );
    }
    println!("cargo:rerun-if-changed={}", ipc_src.display());
    println!("cargo:rerun-if-changed={}", fs_src.display());
    println!("cargo:rerun-if-changed={}", store_src.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("libraries/store/serialize.rs").display()
    );
    for fs_mod in &[
        "block.rs",
        "crc32.rs",
        "alloc_mod.rs",
        "superblock.rs",
        "inode.rs",
        "snapshot.rs",
        "filesystem.rs",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join("libraries/fs").join(fs_mod).display()
        );
    }
    println!("cargo:rerun-if-changed={}", protocol_src.display());
    println!("cargo:rerun-if-changed={}", animation_src.display());
    println!("cargo:rerun-if-changed={}", layout_src.display());
    println!("cargo:rerun-if-changed={}", scene_src.display());
    println!("cargo:rerun-if-changed={}", render_src.display());
    for render_mod in &[
        "scene_render.rs",
        "compositing.rs",
        "surface_pool.rs",
        "damage.rs",
        "cursor.rs",
        "frame_scheduler.rs",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir
                .join("libraries/render")
                .join(render_mod)
                .display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("libraries/fonts/src/lib.rs").display()
    );
    for fonts_src in &["rasterize.rs", "cache.rs"] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir
                .join("libraries/fonts/src")
                .join(fonts_src)
                .display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("libraries/fonts/Cargo.toml").display()
    );
}
/// Compile a Rust source file as a binary ELF.
fn rustc_bin(
    rustc: &str,
    src: &PathBuf,
    output: &PathBuf,
    link_ld: &Path,
    externs: &[(&str, PathBuf)],
    env_vars: &[(&str, String)],
    extra_search_paths: &[&Path],
) {
    let mut cmd = Command::new(rustc);

    cmd.arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=bin")
        .args(["-C", "panic=abort"])
        .args(["-C", "opt-level=s"])
        .arg(format!("-Clink-arg=-T{}", link_ld.display()))
        .arg(format!(
            "-Clink-arg=-zmax-page-size={}",
            system_config::PAGE_SIZE
        ))
        .arg(format!(
            "-Clink-arg=-zcommon-page-size={}",
            system_config::PAGE_SIZE
        ));

    // Add search path so rustc can resolve transitive rlib dependencies.
    if let Some(first) = externs.first() {
        if let Some(dir) = first.1.parent() {
            cmd.arg(format!("-L{}", dir.display()));
        }
    }
    for path in extra_search_paths {
        cmd.arg(format!("-L{}", path.display()));
    }

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
/// Compile a Rust source file as an rlib with additional library search paths.
fn rustc_rlib_with_search(
    rustc: &str,
    src: &PathBuf,
    output: &PathBuf,
    crate_name: &str,
    externs: &[(&str, &PathBuf)],
    search_paths: &[&Path],
) {
    let mut cmd = Command::new(rustc);

    cmd.arg("--target=aarch64-unknown-none")
        .arg("--edition=2021")
        .arg("--crate-type=rlib")
        .arg(format!("--crate-name={crate_name}"))
        .args(["-C", "panic=abort"])
        .args(["-C", "opt-level=s"]);

    if let Some(first) = externs.first() {
        if let Some(dir) = first.1.parent() {
            cmd.arg(format!("-L{}", dir.display()));
        }
    }

    for path in search_paths {
        cmd.arg(format!("-L{}", path.display()));
    }

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
        .args(["-C", "panic=abort"])
        .args(["-C", "opt-level=s"]);

    if let Some(first) = externs.first() {
        if let Some(dir) = first.1.parent() {
            cmd.arg(format!("-L{}", dir.display()));
        }
    }

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

/// Build a Cargo-managed library for the bare-metal target.
///
/// Invokes `cargo build --target aarch64-unknown-none --release` inside the
/// library's directory. Returns the path to the main rlib and the deps
/// directory containing transitive dependency rlibs.
fn cargo_lib(crate_dir: &Path) -> CargoLibOutput {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let crate_name = crate_dir.file_name().unwrap().to_str().unwrap();

    let status = Command::new(&cargo)
        .current_dir(crate_dir)
        .arg("build")
        .arg("--target=aarch64-unknown-none")
        .arg("--release")
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke cargo for {crate_name}: {e}"));

    assert!(status.success(), "cargo build failed for {crate_name}");

    let target_dir = crate_dir.join("target/aarch64-unknown-none/release");
    let rlib = target_dir.join(format!("lib{crate_name}.rlib"));
    let deps_dir = target_dir.join("deps");

    assert!(rlib.exists(), "rlib not found at {}", rlib.display());

    CargoLibOutput { rlib, deps_dir }
}

/// Generate a linker script from a template by substituting @PLACEHOLDER@ tokens
/// with values from system_config.rs.
fn generate_linker_script(template_path: &Path, output_path: &Path) {
    let template = std::fs::read_to_string(template_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", template_path.display()));

    let result = template
        .replace("@PAGE_SIZE@", &system_config::PAGE_SIZE.to_string())
        .replace(
            "@PAGE_SIZE_HEX@",
            &format!("0x{:X}", system_config::PAGE_SIZE),
        )
        .replace(
            "@USER_CODE_BASE_HEX@",
            &format!("0x{:X}", system_config::USER_CODE_BASE),
        )
        .replace(
            "@KERNEL_VA_OFFSET_HEX@",
            &format!("0x{:016X}", system_config::KERNEL_VA_OFFSET),
        );

    std::fs::write(output_path, result)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", output_path.display()));
}
