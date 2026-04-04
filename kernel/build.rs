//! Kernel build script — standalone or as part of the OS build.
//!
//! Responsibilities:
//! 1. Generate the linker script from `link.ld.in` + `system_config.rs`.
//! 2. Pass PIE linker flags for KASLR.
//! 3. Expose `SYSTEM_CONFIG` env var so kernel source can `include!` it.
//! 4. Provide an init ELF: either `OS_INIT_ELF` (from the OS build) or a
//!    built-in stub that calls `thread_exit` (standalone mode).
//! 5. Optionally link a service pack: `OS_SERVICE_PACK` (from the OS build).
//!
//! # Standalone build
//!
//! ```sh
//! cd kernel && cargo build --release
//! ```
//!
//! Produces a bootable kernel with a minimal stub init. The stub init
//! immediately exits — useful for verifying the kernel boots and for
//! building your own init on top.
//!
//! # OS build integration
//!
//! The OS's `build.rs` builds userspace, then sets:
//! - `OS_INIT_ELF` — path to the full init binary
//! - `OS_SERVICE_PACK` — path to the packed service archive (.o file)
//!
//! These override the standalone defaults.

use std::{
    env,
    path::{Path, PathBuf},
};

/// System-wide constants (SSOT: system_config.rs).
mod system_config {
    #![allow(dead_code)]
    include!("system_config.rs");
}

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let config_path = manifest_dir.join("system_config.rs");

    // --- Linker script ---
    generate_linker_script(&manifest_dir.join("link.ld.in"), &out_dir.join("kernel.ld"));

    println!("cargo:rustc-link-arg=-T{}/kernel.ld", out_dir.display());

    // PIE for KASLR — generates R_AARCH64_RELATIVE entries in .rela.dyn.
    // boot.S processes these at runtime to adjust absolute addresses by the
    // KASLR slide. -z notext allows text-segment relocations from non-PIC
    // sysroot objects.
    println!("cargo:rustc-link-arg=--pie");
    println!("cargo:rustc-link-arg=-z");
    println!("cargo:rustc-link-arg=notext");
    // Expose system_config.rs path so kernel source can include! it.
    println!("cargo:rustc-env=SYSTEM_CONFIG={}", config_path.display());

    // --- Init ELF ---
    let init_elf = out_dir.join("init.elf");
    if let Ok(os_init) = env::var("OS_INIT_ELF") {
        // OS build provides the full init binary.
        std::fs::copy(&os_init, &init_elf)
            .unwrap_or_else(|e| panic!("failed to copy init ELF from {os_init}: {e}"));

        println!("cargo:rerun-if-changed={os_init}");
    } else {
        // Standalone mode: generate a minimal stub init.
        generate_stub_init(&init_elf);
    }

    // --- Service pack (optional) ---
    if let Ok(pack_obj) = env::var("OS_SERVICE_PACK") {
        println!("cargo:rustc-link-arg={pack_obj}");
        println!("cargo:rerun-if-changed={pack_obj}");
    }
    // If OS_SERVICE_PACK is not set, the .services section is empty
    // (_services_start == _services_end). The kernel handles this gracefully.

    // --- Rebuild triggers ---
    println!("cargo:rerun-if-changed={}", config_path.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("link.ld.in").display()
    );
}

/// Substitute system_config constants into the linker script template.
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
            "@KERNEL_VA_OFFSET_HEX@",
            &format!("0x{:016X}", system_config::KERNEL_VA_OFFSET),
        );

    std::fs::write(output_path, result)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", output_path.display()));
}

/// Generate a minimal aarch64 ELF64 that calls syscall EXIT (nr=0).
///
/// This is the smallest valid init: the kernel boots, spawns this as PID 1,
/// the stub calls `svc #0` with x8=0 (thread_exit), and the kernel idles.
///
/// The ELF is constructed byte-by-byte to avoid any dependency on external
/// toolchains — the standalone kernel builds with nothing beyond `rustc`.
fn generate_stub_init(path: &Path) {
    let entry_va = system_config::USER_CODE_BASE;
    // aarch64 instructions (little-endian):
    //   mov x8, #0     → 0xD2800008  (EXIT syscall number)
    //   svc #0         → 0xD4000001
    //   b .            → 0x14000000  (infinite loop, unreachable)
    let code: [u8; 12] = [
        0x08, 0x00, 0x80, 0xD2, // mov x8, #0
        0x01, 0x00, 0x00, 0xD4, // svc #0
        0x00, 0x00, 0x00, 0x14, // b .
    ];
    let ehdr_size: u64 = 64;
    let phdr_size: u64 = 56;
    let code_offset = ehdr_size + phdr_size; // 120
    let total_size = code_offset + code.len() as u64; // 132
    let mut elf = Vec::with_capacity(total_size as usize);

    // ---- ELF64 Header (64 bytes) ----
    elf.extend_from_slice(&[0x7F, b'E', b'L', b'F']); // e_ident magic
    elf.push(2); // EI_CLASS: ELFCLASS64
    elf.push(1); // EI_DATA: ELFDATA2LSB
    elf.push(1); // EI_VERSION
    elf.push(0); // EI_OSABI
    elf.extend_from_slice(&[0u8; 8]); // EI_ABIVERSION + padding
    elf.extend_from_slice(&2u16.to_le_bytes()); // e_type: ET_EXEC
    elf.extend_from_slice(&183u16.to_le_bytes()); // e_machine: EM_AARCH64
    elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
    elf.extend_from_slice(&(entry_va + code_offset).to_le_bytes()); // e_entry
    elf.extend_from_slice(&ehdr_size.to_le_bytes()); // e_phoff
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
    elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    elf.extend_from_slice(&(ehdr_size as u16).to_le_bytes()); // e_ehsize
    elf.extend_from_slice(&(phdr_size as u16).to_le_bytes()); // e_phentsize
    elf.extend_from_slice(&1u16.to_le_bytes()); // e_phnum
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_shentsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_shstrndx

    assert_eq!(elf.len(), ehdr_size as usize);

    // ---- Program Header (56 bytes) ----
    elf.extend_from_slice(&1u32.to_le_bytes()); // p_type: PT_LOAD
    elf.extend_from_slice(&5u32.to_le_bytes()); // p_flags: PF_R | PF_X
    elf.extend_from_slice(&0u64.to_le_bytes()); // p_offset
    elf.extend_from_slice(&entry_va.to_le_bytes()); // p_vaddr
    elf.extend_from_slice(&entry_va.to_le_bytes()); // p_paddr
    elf.extend_from_slice(&total_size.to_le_bytes()); // p_filesz
    elf.extend_from_slice(&total_size.to_le_bytes()); // p_memsz
    elf.extend_from_slice(&(system_config::PAGE_SIZE).to_le_bytes()); // p_align

    assert_eq!(elf.len(), code_offset as usize);

    // ---- Code ----
    elf.extend_from_slice(&code);

    assert_eq!(elf.len(), total_size as usize);

    std::fs::write(path, &elf).unwrap_or_else(|e| panic!("failed to write stub init ELF: {e}"));
}
