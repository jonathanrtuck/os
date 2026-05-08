use std::{env, path::PathBuf, process::Command};

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
    let integration = env::var("CARGO_FEATURE_INTEGRATION_TESTS").is_ok();
    let bench_el0 = env::var("CARGO_FEATURE_BENCH_EL0").is_ok();
    let bench_smp = env::var("CARGO_FEATURE_BENCH_SMP").is_ok();
    let (init_dir, crate_name) = if bench_smp {
        (kernel_dir.join("../userspace/bench-smp"), "bench-smp")
    } else if bench_el0 {
        (kernel_dir.join("../userspace/bench"), "bench")
    } else if integration {
        (
            kernel_dir.join("../userspace/integration-tests"),
            "integration-tests",
        )
    } else {
        (kernel_dir.join("../userspace/servers/init"), "init")
    };
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let init_bin = out_dir.join("init.bin");

    build_userspace_crate(&init_dir, crate_name, &init_bin);

    if !integration && !bench_el0 && !bench_smp {
        build_service_pack(kernel_dir, &out_dir);
    } else {
        write_empty_service_pack(&out_dir);
    }

    println!("cargo:rerun-if-changed=../userspace/servers/init/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/init/src/manifest.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/init/link.ld");
    println!("cargo:rerun-if-changed=../userspace/servers/init/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/integration-tests/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/integration-tests/link.ld");
    println!("cargo:rerun-if-changed=../userspace/integration-tests/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/bench/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/bench/link.ld");
    println!("cargo:rerun-if-changed=../userspace/bench/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/bench-smp/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/bench-smp/link.ld");
    println!("cargo:rerun-if-changed=../userspace/bench-smp/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/servers/hello/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/hello/link.ld");
    println!("cargo:rerun-if-changed=../userspace/servers/hello/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/servers/console/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/console/link.ld");
    println!("cargo:rerun-if-changed=../userspace/servers/console/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/input/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/input/link.ld");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/input/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/blk/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/blk/link.ld");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/blk/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/render/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/render/link.ld");
    println!("cargo:rerun-if-changed=../userspace/servers/drivers/render/Cargo.toml");
}

fn build_userspace_crate(crate_dir: &std::path::Path, crate_name: &str, output: &std::path::Path) {
    let link_ld = crate_dir.join("link.ld");
    let rustflags = format!(
        "-C link-arg=-T{} -C link-arg=-nostdlib -C link-arg=--no-rosegment",
        link_ld.display()
    );
    let status = Command::new("cargo")
        .current_dir(crate_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env("CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUSTFLAGS", &rustflags)
        .args(["build", "--release"])
        .status()
        .unwrap_or_else(|e| panic!("failed to build {crate_name}: {e}"));

    assert!(status.success(), "{crate_name} build failed");

    let elf = crate_dir.join(format!("target/aarch64-unknown-none/release/{crate_name}"));
    let status = Command::new("rust-objcopy")
        .args([
            "-O",
            "binary",
            &elf.display().to_string(),
            &output.display().to_string(),
        ])
        .status()
        .expect("failed to run rust-objcopy");

    assert!(status.success(), "rust-objcopy failed on {crate_name}");
}

struct ServiceDef {
    name: &'static str,
    dir: &'static str,
    crate_name: &'static str,
}

const SERVICES: &[ServiceDef] = &[
    ServiceDef {
        name: "name",
        dir: "../userspace/servers/name",
        crate_name: "name",
    },
    ServiceDef {
        name: "console",
        dir: "../userspace/servers/console",
        crate_name: "console",
    },
    ServiceDef {
        name: "input",
        dir: "../userspace/servers/drivers/input",
        crate_name: "input",
    },
    ServiceDef {
        name: "blk",
        dir: "../userspace/servers/drivers/blk",
        crate_name: "blk",
    },
    ServiceDef {
        name: "render",
        dir: "../userspace/servers/drivers/render",
        crate_name: "render",
    },
];

fn build_service_pack(kernel_dir: &std::path::Path, out_dir: &std::path::Path) {
    let pack_bin = out_dir.join("services.bin");
    let mut binaries: Vec<(&str, Vec<u8>)> = Vec::new();

    for svc in SERVICES {
        let svc_dir = kernel_dir.join(svc.dir);
        let svc_bin = out_dir.join(format!("{}.bin", svc.crate_name));

        build_userspace_crate(&svc_dir, svc.crate_name, &svc_bin);

        let data = std::fs::read(&svc_bin)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", svc_bin.display()));

        binaries.push((svc.name, data));
    }

    let pack = build_svpk_pack(&binaries);

    std::fs::write(&pack_bin, &pack)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", pack_bin.display()));
}

fn write_empty_service_pack(out_dir: &std::path::Path) {
    let pack = build_svpk_pack(&[]);

    std::fs::write(out_dir.join("services.bin"), &pack).expect("failed to write empty pack");
}

// ── SVPK pack format (matches tools/mkservices) ─────────────────

const SVPK_MAGIC: &[u8; 4] = b"SVPK";
const SVPK_VERSION: u32 = 1;
const SVPK_PAGE_SIZE: usize = 16384;
const SVPK_HEADER_SIZE: usize = 16;
const SVPK_ENTRY_SIZE: usize = 48;

fn build_svpk_pack(services: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let entry_table_size = services.len() * SVPK_ENTRY_SIZE;
    let first_binary_offset = align_up(SVPK_HEADER_SIZE + entry_table_size, SVPK_PAGE_SIZE);
    let mut offsets = Vec::with_capacity(services.len());
    let mut current = first_binary_offset;

    for (_, binary) in services {
        offsets.push(current);
        current = align_up(current + binary.len(), SVPK_PAGE_SIZE);
    }

    let total_size = current;
    let mut pack = vec![0u8; total_size];

    // Header
    pack[0..4].copy_from_slice(SVPK_MAGIC);
    pack[4..8].copy_from_slice(&SVPK_VERSION.to_le_bytes());
    pack[8..12].copy_from_slice(&(services.len() as u32).to_le_bytes());
    pack[12..16].copy_from_slice(&(total_size as u32).to_le_bytes());

    // Entries + binary data
    for (i, (name, binary)) in services.iter().enumerate() {
        let entry_offset = SVPK_HEADER_SIZE + i * SVPK_ENTRY_SIZE;
        // Name (32 bytes, null-padded)
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(32);

        pack[entry_offset..entry_offset + name_len].copy_from_slice(&name_bytes[..name_len]);
        // offset, size, entry_point, flags
        pack[entry_offset + 32..entry_offset + 36]
            .copy_from_slice(&(offsets[i] as u32).to_le_bytes());
        pack[entry_offset + 36..entry_offset + 40]
            .copy_from_slice(&(binary.len() as u32).to_le_bytes());
        // entry_point = 0, flags = 0 (already zeroed)

        // Binary data
        pack[offsets[i]..offsets[i] + binary.len()].copy_from_slice(binary);
    }

    pack
}

fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}
