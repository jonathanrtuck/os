//! Structural invariant test: no architecture-specific code outside arch/.
//!
//! This test enforces the arch boundary established by v0.6 Phase 1. After
//! extraction, ALL inline assembly and ARM64 register names must live inside
//! kernel/arch/. Any asm! outside arch/ means someone bypassed the interface.
//!
//! This test reads source files directly — it's a static analysis check, not
//! a runtime test. It runs on the host like all other tests.

use std::path::{Path, PathBuf};

/// Kernel source root (relative to test crate).
fn kernel_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("kernel")
        .canonicalize()
        .expect("kernel directory must exist")
}

/// Collect all .rs files in a directory (non-recursive — kernel is flat + arch/).
fn rs_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files
}

/// Check that a file contains no inline assembly.
fn check_no_asm(path: &Path) -> Vec<String> {
    let content = std::fs::read_to_string(path).expect("readable");
    let mut violations = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // Skip comments.
        if trimmed.starts_with("//") {
            continue;
        }
        // Detect core::arch::asm! or asm! invocations.
        // Exclude global_asm!(include_str!("arch/...")) — these are arch
        // file includes in main.rs, not inline arch code.
        if trimmed.contains("global_asm!") && trimmed.contains("arch/") {
            continue;
        }
        if trimmed.contains("core::arch::asm!") || trimmed.contains("asm!(") {
            violations.push(format!(
                "{}:{}: asm! found outside arch/: {}",
                path.file_name().unwrap().to_str().unwrap(),
                i + 1,
                trimmed,
            ));
        }
    }
    violations
}

/// ARM64 system register names that must not appear outside arch/.
const ARCH_REGISTER_NAMES: &[&str] = &[
    "tpidr_el1",
    "tpidr_el0",
    "ttbr0_el1",
    "ttbr1_el1",
    "daif",
    "daifset",
    "daifclr",
    "mpidr_el1",
    "cntvct_el0",
    "cntv_tval_el0",
    "cntv_ctl_el0",
    "cntfrq_el0",
    "cntkctl_el1",
    "spsr_el1",
    "elr_el1",
    "esr_el1",
    "far_el1",
    "sctlr_el1",
    "tcr_el1",
    "mair_el1",
    "vbar_el1",
    "par_el1",
    "sp_el0",
    "icc_iar1_el1",
    "icc_eoir1_el1",
    "icc_sre_el1",
    "icc_pmr_el1",
    "icc_ctlr_el1",
    "icc_igrpen1_el1",
    "icc_sgi1r_el1",
];

/// Check that a file contains no ARM64 register name references in code.
fn check_no_arch_registers(path: &Path) -> Vec<String> {
    let content = std::fs::read_to_string(path).expect("readable");
    let lower = content.to_lowercase();
    let mut violations = Vec::new();
    for (i, line) in lower.lines().enumerate() {
        let trimmed = line.trim();
        // Skip comments and doc comments.
        if trimmed.starts_with("//") {
            continue;
        }
        // Skip string literals (debug_assert messages, panic strings).
        if trimmed.contains('"') {
            continue;
        }
        for reg in ARCH_REGISTER_NAMES {
            if trimmed.contains(reg) {
                violations.push(format!(
                    "{}:{}: arch register '{}' found outside arch/",
                    path.file_name().unwrap().to_str().unwrap(),
                    i + 1,
                    reg,
                ));
            }
        }
    }
    violations
}

#[test]
fn no_asm_outside_arch() {
    let kernel = kernel_dir();
    let files = rs_files_in(&kernel);
    assert!(!files.is_empty(), "should find kernel .rs files");

    let mut all_violations = Vec::new();
    for file in &files {
        // main.rs is allowed to call arch:: functions but must not contain asm!
        all_violations.extend(check_no_asm(file));
    }

    if !all_violations.is_empty() {
        panic!(
            "arch boundary violation: {} asm! sites outside kernel/arch/:\n{}",
            all_violations.len(),
            all_violations.join("\n"),
        );
    }
}

#[test]
fn no_arch_registers_outside_arch() {
    let kernel = kernel_dir();
    let files = rs_files_in(&kernel);
    assert!(!files.is_empty(), "should find kernel .rs files");

    let mut all_violations = Vec::new();
    for file in &files {
        all_violations.extend(check_no_arch_registers(file));
    }

    if !all_violations.is_empty() {
        panic!(
            "arch boundary violation: {} register references outside kernel/arch/:\n{}",
            all_violations.len(),
            all_violations.join("\n"),
        );
    }
}
