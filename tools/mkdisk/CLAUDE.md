# tools/mkdisk

Factory disk image builder. Creates pre-populated disk images using the `fs` and `store` libraries.

Usage: `mkdisk <output.img> <share-dir>`

Reads font files and test content from the share directory, creates a formatted filesystem image with those files pre-loaded. The resulting image is used as the virtio-blk disk for QEMU or the hypervisor, replacing the previous 9p-based font loading.

Depends on `system/libraries/fs` and `system/libraries/store` (linked as regular Rust crates via path dependencies).
