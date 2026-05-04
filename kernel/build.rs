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
    let init_dir = if integration {
        kernel_dir.join("../userspace/integration-tests")
    } else {
        kernel_dir.join("../userspace/init")
    };
    let crate_name = if integration {
        "integration-tests"
    } else {
        "init"
    };
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let init_bin = out_dir.join("init.bin");
    let link_ld = init_dir.join("link.ld");
    let rustflags = format!(
        "-C link-arg=-T{} -C link-arg=-nostdlib -C link-arg=--no-rosegment",
        link_ld.display()
    );
    let status = Command::new("cargo")
        .current_dir(&init_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env("CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUSTFLAGS", &rustflags)
        .args(["build", "--release"])
        .status()
        .expect("failed to build init crate");

    if !status.success() {
        panic!("init crate build failed");
    }

    let init_elf = init_dir.join(format!("target/aarch64-unknown-none/release/{crate_name}"));
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

    println!("cargo:rerun-if-changed=../userspace/init/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/init/link.ld");
    println!("cargo:rerun-if-changed=../userspace/init/Cargo.toml");
    println!("cargo:rerun-if-changed=../userspace/integration-tests/src/main.rs");
    println!("cargo:rerun-if-changed=../userspace/integration-tests/link.ld");
    println!("cargo:rerun-if-changed=../userspace/integration-tests/Cargo.toml");
}
