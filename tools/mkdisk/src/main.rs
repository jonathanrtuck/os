//! mkdisk — create pre-formatted disk images for the document-centric OS.
//!
//! Usage: mkdisk <output.img> [blocks]
//!
//! Creates a disk image with a formatted COW filesystem and initialized
//! document store. Default: 512 blocks (8 MiB).

use std::{env, fs::File, os::unix::fs::FileExt, path::Path, process};

use fs::{BlockDevice, Filesystem, FsError, BLOCK_SIZE};
use store::Store;

struct FileDevice {
    file: File,
    blocks: u32,
}

impl FileDevice {
    fn create(path: &Path, blocks: u32) -> Result<Self, FsError> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|_| FsError::Io)?;
        file.set_len(u64::from(blocks) * u64::from(BLOCK_SIZE))
            .map_err(|_| FsError::Io)?;

        Ok(Self { file, blocks })
    }
}

impl BlockDevice for FileDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if index >= self.blocks {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks,
            });
        }

        self.file
            .read_exact_at(buf, u64::from(index) * u64::from(BLOCK_SIZE))
            .map_err(|_| FsError::Io)
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        if index >= self.blocks {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks,
            });
        }

        self.file
            .write_all_at(data, u64::from(index) * u64::from(BLOCK_SIZE))
            .map_err(|_| FsError::Io)
    }

    fn flush(&mut self) -> Result<(), FsError> {
        self.file.sync_all().map_err(|_| FsError::Io)
    }

    fn block_count(&self) -> u32 {
        self.blocks
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: mkdisk <output.img> [blocks]");
        process::exit(1);
    }

    let output_path = Path::new(&args[1]);
    let blocks: u32 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);

    let device = FileDevice::create(output_path, blocks).unwrap_or_else(|e| {
        eprintln!("error: failed to create {}: {e}", output_path.display());
        process::exit(1);
    });

    let filesystem = Filesystem::format(device).unwrap_or_else(|e| {
        eprintln!("error: format failed: {e}");
        process::exit(1);
    });

    let store = Store::init(Box::new(filesystem)).unwrap_or_else(|e| {
        eprintln!("error: store init failed: {e}");
        process::exit(1);
    });

    store.into_inner().commit().unwrap_or_else(|e| {
        eprintln!("error: commit failed: {e}");
        process::exit(1);
    });

    println!(
        "created {} ({} blocks, {} KiB)",
        output_path.display(),
        blocks,
        blocks * BLOCK_SIZE / 1024
    );
}
