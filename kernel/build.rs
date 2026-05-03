use std::{env, path::PathBuf};

fn main() {
    // Only use the kernel linker script for bare-metal targets.
    // Host-side test builds (aarch64-apple-darwin) use the system linker.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "none" {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let link_ld = manifest_dir.join("link.ld");

        println!("cargo:rustc-link-arg=-T{}", link_ld.display());
    }

    println!("cargo:rerun-if-changed=link.ld");
}
