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
        (kernel_dir.join("../user/bench-smp"), "bench-smp")
    } else if bench_el0 {
        (kernel_dir.join("../user/bench"), "bench")
    } else if integration {
        (
            kernel_dir.join("../user/integration-tests"),
            "integration-tests",
        )
    } else {
        (kernel_dir.join("../user/servers/init"), "init")
    };
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let init_bin = out_dir.join("init.bin");

    build_userspace_crate(&init_dir, crate_name, &init_bin);

    if !integration && !bench_el0 && !bench_smp {
        build_service_pack(kernel_dir, &out_dir);
    } else {
        write_empty_service_pack(&out_dir);
    }

    println!("cargo:rerun-if-changed=../user/servers/init/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/init/src/manifest.rs");
    println!("cargo:rerun-if-changed=../user/servers/init/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/init/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/integration-tests/src/main.rs");
    println!("cargo:rerun-if-changed=../user/integration-tests/link.ld");
    println!("cargo:rerun-if-changed=../user/integration-tests/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/bench/src/main.rs");
    println!("cargo:rerun-if-changed=../user/bench/link.ld");
    println!("cargo:rerun-if-changed=../user/bench/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/bench-smp/src/main.rs");
    println!("cargo:rerun-if-changed=../user/bench-smp/link.ld");
    println!("cargo:rerun-if-changed=../user/bench-smp/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/hello/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/hello/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/hello/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/console/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/console/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/console/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/drivers/input/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/input/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/drivers/input/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/drivers/blk/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/blk/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/drivers/blk/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/drivers/rng/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/rng/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/rng/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/drivers/rng/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/drivers/snd/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/snd/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/snd/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/drivers/snd/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/audio/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/audio/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/audio/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/audio/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/drivers/render/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/render/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/drivers/render/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/store/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/store/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/store/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/store/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/document/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/document/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/document/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/document/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/layout/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/layout/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/layout/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/layout/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/test-layout/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/test-layout/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/test-layout/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/presenter/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/presenter/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/presenter/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/presenter/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/test-presenter/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/test-presenter/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/test-presenter/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/editors/text/src/main.rs");
    println!("cargo:rerun-if-changed=../user/editors/text/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/editors/text/link.ld");
    println!("cargo:rerun-if-changed=../user/editors/text/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/test-editor/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/test-editor/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/test-editor/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/png-decoder/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/png-decoder/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/png-decoder/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/png-decoder/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/drivers/9p/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/9p/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/drivers/9p/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/drivers/9p/Cargo.toml");
    println!("cargo:rerun-if-changed=../user/servers/fs/src/main.rs");
    println!("cargo:rerun-if-changed=../user/servers/fs/src/lib.rs");
    println!("cargo:rerun-if-changed=../user/servers/fs/link.ld");
    println!("cargo:rerun-if-changed=../user/servers/fs/Cargo.toml");
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

struct ElfMeta {
    data_offset: u32,
    mem_size: u32,
}

fn read_elf_meta(elf_path: &std::path::Path) -> ElfMeta {
    let data = std::fs::read(elf_path)
        .unwrap_or_else(|e| panic!("failed to read ELF {}: {e}", elf_path.display()));

    assert!(data.len() >= 64, "ELF too small");
    assert!(&data[0..4] == b"\x7fELF", "not an ELF file");

    let e_phoff = u64::from_le_bytes(data[32..40].try_into().unwrap()) as usize;
    let e_phentsize = u16::from_le_bytes(data[54..56].try_into().unwrap()) as usize;
    let e_phnum = u16::from_le_bytes(data[56..58].try_into().unwrap()) as usize;
    let mut rx_end: u64 = 0;
    let mut rw_vaddr: u64 = 0;
    let mut rw_memsz: u64 = 0;
    let mut has_rw = false;

    for i in 0..e_phnum {
        let off = e_phoff + i * e_phentsize;
        let p_type = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());

        if p_type != 1 {
            continue; // PT_LOAD = 1
        }

        let p_flags = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap());
        let p_vaddr = u64::from_le_bytes(data[off + 16..off + 24].try_into().unwrap());
        let p_memsz = u64::from_le_bytes(data[off + 40..off + 48].try_into().unwrap());

        if p_flags & 2 != 0 {
            // PF_W — writable segment
            rw_vaddr = p_vaddr;
            rw_memsz = p_memsz;
            has_rw = true;
        } else {
            rx_end = p_vaddr + p_memsz;
        }
    }

    if !has_rw || rw_memsz == 0 {
        return ElfMeta {
            data_offset: 0,
            mem_size: 0,
        };
    }

    let code_va = 0x0020_0000u64;
    let data_offset = (rw_vaddr - code_va) as u32;
    let mem_size = (rw_vaddr + rw_memsz - code_va) as u32;

    assert!(
        data_offset.is_multiple_of(SVPK_PAGE_SIZE as u32),
        "RW segment not page-aligned: {data_offset:#x} (rodata ends at {rx_end:#x})"
    );

    ElfMeta {
        data_offset,
        mem_size,
    }
}

struct ServiceDef {
    name: &'static str,
    dir: &'static str,
    crate_name: &'static str,
}

const SERVICES: &[ServiceDef] = &[
    ServiceDef {
        name: "name",
        dir: "../user/servers/name",
        crate_name: "name",
    },
    ServiceDef {
        name: "console",
        dir: "../user/servers/console",
        crate_name: "console",
    },
    ServiceDef {
        name: "input",
        dir: "../user/servers/drivers/input",
        crate_name: "input",
    },
    ServiceDef {
        name: "blk",
        dir: "../user/servers/drivers/blk",
        crate_name: "blk",
    },
    ServiceDef {
        name: "render",
        dir: "../user/servers/drivers/render",
        crate_name: "render",
    },
    ServiceDef {
        name: "store",
        dir: "../user/servers/store",
        crate_name: "store-service",
    },
    ServiceDef {
        name: "document",
        dir: "../user/servers/document",
        crate_name: "document-service",
    },
    ServiceDef {
        name: "layout",
        dir: "../user/servers/layout",
        crate_name: "layout-service",
    },
    ServiceDef {
        name: "presenter",
        dir: "../user/servers/presenter",
        crate_name: "presenter-service",
    },
    ServiceDef {
        name: "editor.text",
        dir: "../user/editors/text",
        crate_name: "text-editor",
    },
    ServiceDef {
        name: "png-decoder",
        dir: "../user/servers/png-decoder",
        crate_name: "png-decoder",
    },
    ServiceDef {
        name: "rng",
        dir: "../user/servers/drivers/rng",
        crate_name: "rng",
    },
    ServiceDef {
        name: "snd",
        dir: "../user/servers/drivers/snd",
        crate_name: "snd",
    },
    ServiceDef {
        name: "audio",
        dir: "../user/servers/audio",
        crate_name: "audio-service",
    },
    ServiceDef {
        name: "9p",
        dir: "../user/servers/drivers/9p",
        crate_name: "virtio-9p",
    },
    ServiceDef {
        name: "fs",
        dir: "../user/servers/fs",
        crate_name: "fs-service",
    },
];

fn build_service_pack(kernel_dir: &std::path::Path, out_dir: &std::path::Path) {
    let pack_bin = out_dir.join("services.bin");
    let mut entries: Vec<(&str, Vec<u8>, ElfMeta)> = Vec::new();

    for svc in SERVICES {
        let svc_dir = kernel_dir.join(svc.dir);
        let svc_bin = out_dir.join(format!("{}.bin", svc.crate_name));

        build_userspace_crate(&svc_dir, svc.crate_name, &svc_bin);

        let elf_path = svc_dir.join(format!(
            "target/aarch64-unknown-none/release/{}",
            svc.crate_name
        ));
        let meta = read_elf_meta(&elf_path);

        let data = std::fs::read(&svc_bin)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", svc_bin.display()));

        entries.push((svc.name, data, meta));
    }

    let pack = build_svpk_pack(&entries);

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

fn build_svpk_pack(services: &[(&str, Vec<u8>, ElfMeta)]) -> Vec<u8> {
    let entry_table_size = services.len() * SVPK_ENTRY_SIZE;
    let first_binary_offset = align_up(SVPK_HEADER_SIZE + entry_table_size, SVPK_PAGE_SIZE);
    let mut offsets = Vec::with_capacity(services.len());
    let mut current = first_binary_offset;

    for (_, binary, _) in services {
        offsets.push(current);
        current = align_up(current + binary.len(), SVPK_PAGE_SIZE);
    }

    let total_size = current;
    let mut pack = vec![0u8; total_size];

    pack[0..4].copy_from_slice(SVPK_MAGIC);
    pack[4..8].copy_from_slice(&SVPK_VERSION.to_le_bytes());
    pack[8..12].copy_from_slice(&(services.len() as u32).to_le_bytes());
    pack[12..16].copy_from_slice(&(total_size as u32).to_le_bytes());

    for (i, (name, binary, meta)) in services.iter().enumerate() {
        let entry_offset = SVPK_HEADER_SIZE + i * SVPK_ENTRY_SIZE;
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(32);

        pack[entry_offset..entry_offset + name_len].copy_from_slice(&name_bytes[..name_len]);
        // offset, size, data_offset, mem_size
        pack[entry_offset + 32..entry_offset + 36]
            .copy_from_slice(&(offsets[i] as u32).to_le_bytes());
        pack[entry_offset + 36..entry_offset + 40]
            .copy_from_slice(&(binary.len() as u32).to_le_bytes());
        pack[entry_offset + 40..entry_offset + 44].copy_from_slice(&meta.data_offset.to_le_bytes());
        pack[entry_offset + 44..entry_offset + 48].copy_from_slice(&meta.mem_size.to_le_bytes());

        pack[offsets[i]..offsets[i] + binary.len()].copy_from_slice(binary);
    }

    pack
}

fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}
