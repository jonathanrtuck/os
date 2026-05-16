//! mkdisk — create pre-formatted disk images for the document-centric OS.
//!
//! Usage: mkdisk <output.img> [assets-dir]
//!
//! Creates a disk image with a formatted COW filesystem and initialized
//! document store. When an assets directory is provided, ingests image
//! files into the store with appropriate media types.

use std::{collections::HashMap, env, fs::File, os::unix::fs::FileExt, path::Path, process};

use fs::{BLOCK_SIZE, BlockDevice, FileId, Filesystem, FsError};
use manifest::{
    AbsoluteProperties, Axis, Child, FlowProperties, Layout, LayoutMode, Manifest, PerAxis,
    Placement,
};
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

struct AssetSpec {
    filename: &'static str,
    media_type: &'static str,
    name: &'static str,
}

const ASSETS: &[AssetSpec] = &[
    AssetSpec {
        filename: "zoey.jpg",
        media_type: "image/jpeg",
        name: "zoey",
    },
    AssetSpec {
        filename: "click.wav",
        media_type: "audio/wav",
        name: "click",
    },
    AssetSpec {
        filename: "video.avi",
        media_type: "video/avi",
        name: "video",
    },
    AssetSpec {
        filename: "zoey.mp4",
        media_type: "video/mp4",
        name: "zoey-video",
    },
];

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: mkdisk <output.img> [assets-dir]");

        process::exit(1);
    }

    let output_path = Path::new(&args[1]);
    let assets_dir = args.get(2).map(|s| Path::new(s.as_str()));
    // 8192 blocks = 128 MiB when assets are included, 512 = 8 MiB otherwise.
    let blocks: u32 = if assets_dir.is_some() { 8192 } else { 512 };
    let device = FileDevice::create(output_path, blocks).unwrap_or_else(|e| {
        eprintln!("error: failed to create {}: {e}", output_path.display());

        process::exit(1);
    });
    let filesystem = Filesystem::format(device).unwrap_or_else(|e| {
        eprintln!("error: format failed: {e}");

        process::exit(1);
    });
    let mut store = Store::init(Box::new(filesystem)).unwrap_or_else(|e| {
        eprintln!("error: store init failed: {e}");

        process::exit(1);
    });
    let mut file_count = 0u32;
    let mut asset_ids: HashMap<&str, FileId> = HashMap::new();

    if let Some(dir) = assets_dir {
        if !dir.is_dir() {
            eprintln!("error: assets directory not found: {}", dir.display());

            process::exit(1);
        }

        for spec in ASSETS {
            let path = dir.join(spec.filename);

            if !path.exists() {
                eprintln!("  skip  {}  (not found)", spec.filename);

                continue;
            }

            let data = std::fs::read(&path).unwrap_or_else(|e| {
                eprintln!("error: failed to read {}: {e}", path.display());

                process::exit(1);
            });
            let id = store.create(spec.media_type).unwrap_or_else(|e| {
                eprintln!("error: store create failed: {e}");

                process::exit(1);
            });

            store.write(id, 0, &data).unwrap_or_else(|e| {
                eprintln!("error: store write failed: {e}");

                process::exit(1);
            });
            store
                .set_attribute(id, "name", spec.name)
                .unwrap_or_else(|e| {
                    eprintln!("error: set_attribute failed: {e}");

                    process::exit(1);
                });

            asset_ids.insert(spec.name, id);

            file_count += 1;

            println!(
                "  {:>5}  {:?}  {}  ({} bytes)",
                spec.media_type.split('/').last().unwrap_or(""),
                id,
                spec.filename,
                data.len()
            );
        }

        if let Some(&image_id) = asset_ids.get("zoey") {
            create_compound_doc(&mut store, image_id);

            file_count += 1;
        }
    }

    store.commit().unwrap_or_else(|e| {
        eprintln!("error: commit failed: {e}");

        process::exit(1);
    });

    println!(
        "created {} ({} blocks, {} KiB, {} files)",
        output_path.display(),
        blocks,
        blocks * BLOCK_SIZE / 1024,
        file_count
    );
}

fn create_compound_doc(store: &mut Store, image_id: FileId) {
    let mpt = |pt: i32| pt * 1024;
    let manifest = Manifest {
        title: Some("Compound Test".into()),
        tags: vec!["test".into()],
        provenance: None,
        attributes: Vec::new(),
        layout: Some(Layout {
            axes: vec![Axis::Height, Axis::Width],
            mode: LayoutMode::Flow(FlowProperties {
                gap: PerAxis {
                    height: Some(mpt(20)),
                    ..Default::default()
                },
                ..Default::default()
            }),
        }),
        children: vec![
            Child {
                uri: format!("store:{}", image_id.0),
                placement: Some(Placement {
                    size: PerAxis {
                        height: Some(mpt(350)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                viewport: None,
            },
            Child {
                uri: format!("store:{}", image_id.0),
                placement: Some(Placement {
                    size: PerAxis {
                        height: Some(mpt(350)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                viewport: None,
            },
        ],
    };
    let bytes = manifest::encode(&manifest);
    let id = store
        .create("application/x-os-document")
        .unwrap_or_else(|e| {
            eprintln!("error: create compound doc failed: {e}");

            process::exit(1);
        });

    store.write(id, 0, &bytes).unwrap_or_else(|e| {
        eprintln!("error: write compound doc failed: {e}");

        process::exit(1);
    });
    store
        .set_attribute(id, "name", "compound-test")
        .unwrap_or_else(|e| {
            eprintln!("error: set_attribute failed: {e}");

            process::exit(1);
        });

    println!(
        "  compound  {:?}  compound-test  ({} bytes manifest, 2 children)",
        id,
        bytes.len()
    );
}
