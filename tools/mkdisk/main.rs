//! Factory image builder — creates pre-populated disk images using the store library.
//!
//! Usage: mkdisk <output.img> <share-dir>

use std::env;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process;

use fs::{BlockDevice, Filesystem, FsError, BLOCK_SIZE};
use store::Store;

// ── FileDevice ──────────────────────────────────────────────────────

/// File-backed block device for host-side disk image creation.
struct FileDevice {
    file: File,
    blocks: u32,
}

impl FileDevice {
    /// Create a new device file at `path` with `blocks` zero-filled blocks.
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
        if buf.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: buf.len(),
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
        if data.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: data.len(),
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

// ── Font metadata ───────────────────────────────────────────────────

struct FontSpec {
    filename: &'static str,
    name: &'static str,
    style: &'static str,
}

const FONTS: &[FontSpec] = &[
    FontSpec {
        filename: "jetbrains-mono.ttf",
        name: "JetBrains Mono",
        style: "mono",
    },
    FontSpec {
        filename: "jetbrains-mono-italic.ttf",
        name: "JetBrains Mono Italic",
        style: "mono",
    },
    FontSpec {
        filename: "inter.ttf",
        name: "Inter",
        style: "sans",
    },
    FontSpec {
        filename: "inter-italic.ttf",
        name: "Inter Italic",
        style: "sans",
    },
    FontSpec {
        filename: "source-serif-4.ttf",
        name: "Source Serif 4",
        style: "serif",
    },
    FontSpec {
        filename: "source-serif-4-italic.ttf",
        name: "Source Serif 4 Italic",
        style: "serif",
    },
];

// ── Main ────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: mkdisk <output.img> <share-dir>");
        process::exit(1);
    }

    let output_path = Path::new(&args[1]);
    let share_dir = Path::new(&args[2]);

    if !share_dir.is_dir() {
        eprintln!("error: share directory not found: {}", share_dir.display());
        process::exit(1);
    }

    // 4096 blocks * 16 KiB = 64 MiB
    let device = FileDevice::create(output_path, 4096).unwrap_or_else(|e| {
        eprintln!("error: failed to create {}: {e}", output_path.display());
        process::exit(1);
    });

    let filesystem = Filesystem::format(device).unwrap_or_else(|e| {
        eprintln!("error: failed to format filesystem: {e}");
        process::exit(1);
    });

    let mut store = Store::init(Box::new(filesystem)).unwrap_or_else(|e| {
        eprintln!("error: failed to init store: {e}");
        process::exit(1);
    });

    let mut file_count = 0u32;

    // Ingest fonts.
    for font in FONTS {
        let font_path = share_dir.join(font.filename);
        if !font_path.exists() {
            eprintln!("warning: font not found, skipping: {}", font_path.display());
            continue;
        }
        let data = std::fs::read(&font_path).unwrap_or_else(|e| {
            eprintln!("error: failed to read {}: {e}", font_path.display());
            process::exit(1);
        });

        let id = store.create("font/ttf").unwrap();
        store.write(id, 0, &data).unwrap();
        store.set_attribute(id, "name", font.name).unwrap();
        store.set_attribute(id, "role", "system").unwrap();
        store.set_attribute(id, "style", font.style).unwrap();

        file_count += 1;
        println!("  font  {:?}  {}  ({} bytes)", id, font.name, data.len());
    }

    // Create a sample text/rich document showcasing all 7 styles.
    {
        let sample_text = b"Rich Text Demo\nTypography\nBody text. Bold text. Italic text. Bold italic. Inline code.\nParagraphs\nSecond paragraph to verify newline handling.\n";
        //                   ^0            ^14          ^25       ^36        ^47          ^60             ^75
        // Byte offsets:
        //   "Rich Text Demo\n"          = 0..15   (15 bytes) → heading1 (style 1)
        //   "Typography\n"              = 15..26  (11 bytes) → heading2 (style 2)
        //   "Body text. "               = 26..37  (11 bytes) → body (style 0)
        //   "Bold text. "               = 37..48  (11 bytes) → bold (style 3)
        //   "Italic text. "             = 48..62  (14 bytes) → italic (style 4)
        //   "Bold italic. "             = 62..76  (14 bytes) → bold-italic (style 5)
        //   "Inline code."              = 76..88  (12 bytes) → code (style 6)
        //   "\nParagraphs\n"            = 88..101 (13 bytes) → heading2 (style 2)
        //   "Second paragraph...\n"     = 101..end          → body (style 0)
        let mut pt_buf = vec![0u8; 4096];

        let cap = pt_buf.len();
        if !piecetable::init_with_text(
            &mut pt_buf,
            cap,
            sample_text,
            &piecetable::default_body_style(),
        ) {
            eprintln!("error: failed to init piece table");
            process::exit(1);
        }

        // Add styles 1-6 (body is already at index 0).
        let extra_styles = [
            piecetable::heading1_style(),
            piecetable::heading2_style(),
            piecetable::bold_style(),
            piecetable::italic_style(),
            piecetable::bold_italic_style(),
            piecetable::code_style(),
        ];
        for s in &extra_styles {
            if piecetable::add_style(&mut pt_buf, s).is_none() {
                eprintln!("error: failed to add style to piece table");
                process::exit(1);
            }
        }

        // Apply styles to ranges (byte offsets verified by script).
        piecetable::apply_style(&mut pt_buf, 0, 15, 1); // heading1
        piecetable::apply_style(&mut pt_buf, 15, 26, 2); // heading2
                                                         // 26..37 body (stays style 0)
        piecetable::apply_style(&mut pt_buf, 37, 48, 3); // bold
        piecetable::apply_style(&mut pt_buf, 48, 61, 4); // italic
        piecetable::apply_style(&mut pt_buf, 61, 74, 5); // bold-italic
        piecetable::apply_style(&mut pt_buf, 74, 86, 6); // code
        piecetable::apply_style(&mut pt_buf, 86, 98, 2); // heading2
                                                         // 98..143 body (stays style 0)

        // Compute the actual used size from the header fields.
        let h = piecetable::header(&pt_buf);
        let used = piecetable::HEADER_SIZE
            + (h.style_count as usize) * core::mem::size_of::<piecetable::Style>()
            + (h.piece_count as usize) * core::mem::size_of::<piecetable::Piece>()
            + h.original_len as usize
            + h.add_len as usize;
        let pt_bytes = &pt_buf[..used];

        let id = store.create("text/rich").unwrap();
        store.write(id, 0, pt_bytes).unwrap();
        store.set_attribute(id, "name", "welcome").unwrap();

        file_count += 1;
        println!(
            "  rich  {:?}  welcome  ({} bytes, {} styles)",
            id,
            pt_bytes.len(),
            7
        );
    }

    // Ingest test.png if present.
    let png_path = share_dir.join("test.png");
    if png_path.exists() {
        let data = std::fs::read(&png_path).unwrap_or_else(|e| {
            eprintln!("error: failed to read {}: {e}", png_path.display());
            process::exit(1);
        });

        let id = store.create("image/png").unwrap();
        store.write(id, 0, &data).unwrap();
        store.set_attribute(id, "name", "test").unwrap();
        store.set_attribute(id, "role", "test").unwrap();

        file_count += 1;
        println!("  image {:?}  test.png  ({} bytes)", id, data.len());
    }

    store.commit().unwrap_or_else(|e| {
        eprintln!("error: failed to commit store: {e}");
        process::exit(1);
    });

    println!("created {} with {file_count} files", output_path.display());
}
