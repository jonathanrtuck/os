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

    // Create a comprehensive test document using all 32 style slots.
    {
        use piecetable::{
            Style, FLAG_ITALIC, FLAG_STRIKETHROUGH, FLAG_UNDERLINE, FONT_MONO, FONT_SANS,
            FONT_SERIF, ROLE_BODY, ROLE_CODE, ROLE_EMPHASIS, ROLE_HEADING1, ROLE_HEADING2,
            ROLE_HEADING3, ROLE_STRONG,
        };

        // -- Define all 32 styles --------------------------------------------------
        // Style 0: default body (Sans 14pt Regular Black) — set via init_with_text.
        // Styles 1–31: added via add_style after init.
        let extra_styles: [Style; 31] = [
            // 1: Title — Sans 48pt Bold Red
            Style {
                font_family: FONT_SANS,
                role: ROLE_HEADING1,
                weight: 700,
                flags: 0,
                font_size_pt: 48,
                color: [0xFF, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 2: Subtitle — Serif 24pt Regular Blue
            Style {
                font_family: FONT_SERIF,
                role: ROLE_HEADING2,
                weight: 400,
                flags: 0,
                font_size_pt: 24,
                color: [0x00, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 3: Big Mixed — Sans 36pt Bold Green
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 700,
                flags: 0,
                font_size_pt: 36,
                color: [0x00, 0xAA, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 4: Small Mixed — Mono 10pt Regular Orange
            Style {
                font_family: FONT_MONO,
                role: ROLE_CODE,
                weight: 400,
                flags: 0,
                font_size_pt: 10,
                color: [0xFF, 0x88, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 5: Medium Mixed — Serif 18pt Italic Purple
            Style {
                font_family: FONT_SERIF,
                role: ROLE_EMPHASIS,
                weight: 400,
                flags: FLAG_ITALIC,
                font_size_pt: 18,
                color: [0x88, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 6: Mono 16pt Regular Cyan
            Style {
                font_family: FONT_MONO,
                role: ROLE_CODE,
                weight: 400,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0xCC, 0xCC, 0xFF],
                _pad: [0; 2],
            },
            // 7: Sans 20pt Bold Magenta
            Style {
                font_family: FONT_SANS,
                role: ROLE_STRONG,
                weight: 700,
                flags: 0,
                font_size_pt: 20,
                color: [0xFF, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 8: Serif 14pt Italic Red
            Style {
                font_family: FONT_SERIF,
                role: ROLE_EMPHASIS,
                weight: 400,
                flags: FLAG_ITALIC,
                font_size_pt: 14,
                color: [0xFF, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 9: Sans 14pt Regular Red (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0xFF, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 10: Sans 14pt Regular Blue (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0x00, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 11: Sans 14pt Regular Green (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0x00, 0xAA, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 12: Sans 14pt Regular Orange (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0xFF, 0x88, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 13: Sans 14pt Regular Purple (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0x88, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 14: Sans 14pt Regular Cyan (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0x00, 0xCC, 0xCC, 0xFF],
                _pad: [0; 2],
            },
            // 15: Sans 14pt Regular Magenta (color word)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 14,
                color: [0xFF, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 16: Sans 16pt Thin (w100)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 100,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 17: Sans 16pt ExtraLight (w200)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 200,
                flags: 0,
                font_size_pt: 16,
                color: [0x22, 0x22, 0x22, 0xFF],
                _pad: [0; 2],
            },
            // 18: Sans 16pt Light (w300)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 300,
                flags: 0,
                font_size_pt: 16,
                color: [0x44, 0x44, 0x44, 0xFF],
                _pad: [0; 2],
            },
            // 19: Sans 16pt Regular (w400)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 20: Sans 16pt Medium (w500)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 500,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 21: Sans 16pt SemiBold (w600)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 600,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 22: Sans 16pt Bold (w700)
            Style {
                font_family: FONT_SANS,
                role: ROLE_STRONG,
                weight: 700,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 23: Sans 16pt ExtraBold (w800)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 800,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 24: Sans 16pt Black (w900)
            Style {
                font_family: FONT_SANS,
                role: ROLE_BODY,
                weight: 900,
                flags: 0,
                font_size_pt: 16,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 25: Serif 22pt Bold Italic Underline — deep blue
            Style {
                font_family: FONT_SERIF,
                role: ROLE_HEADING3,
                weight: 700,
                flags: FLAG_ITALIC | FLAG_UNDERLINE,
                font_size_pt: 22,
                color: [0x00, 0x44, 0xCC, 0xFF],
                _pad: [0; 2],
            },
            // 26: Mono 12pt Italic Strikethrough — dark red
            Style {
                font_family: FONT_MONO,
                role: ROLE_CODE,
                weight: 400,
                flags: FLAG_ITALIC | FLAG_STRIKETHROUGH,
                font_size_pt: 12,
                color: [0xCC, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 27: Sans 28pt Bold Green
            Style {
                font_family: FONT_SANS,
                role: ROLE_HEADING2,
                weight: 700,
                flags: 0,
                font_size_pt: 28,
                color: [0x00, 0xAA, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 28: Serif 12pt Regular Black
            Style {
                font_family: FONT_SERIF,
                role: ROLE_BODY,
                weight: 400,
                flags: 0,
                font_size_pt: 12,
                color: [0x00, 0x00, 0x00, 0xFF],
                _pad: [0; 2],
            },
            // 29: Mono 14pt Bold Cyan
            Style {
                font_family: FONT_MONO,
                role: ROLE_CODE,
                weight: 700,
                flags: 0,
                font_size_pt: 14,
                color: [0x00, 0xCC, 0xCC, 0xFF],
                _pad: [0; 2],
            },
            // 30: Sans 40pt Italic Magenta
            Style {
                font_family: FONT_SANS,
                role: ROLE_HEADING1,
                weight: 400,
                flags: FLAG_ITALIC,
                font_size_pt: 40,
                color: [0xFF, 0x00, 0xFF, 0xFF],
                _pad: [0; 2],
            },
            // 31: Serif 32pt Bold Underline Orange
            Style {
                font_family: FONT_SERIF,
                role: ROLE_HEADING1,
                weight: 700,
                flags: FLAG_UNDERLINE,
                font_size_pt: 32,
                color: [0xFF, 0x88, 0x00, 0xFF],
                _pad: [0; 2],
            },
        ];

        // -- Build text with tracked byte ranges ----------------------------------
        let mut text = Vec::new();
        let mut ranges: Vec<(usize, usize, u8)> = Vec::new(); // (start, end, style_id)

        // Line 1: Title (48pt bold red)
        let s = text.len();
        text.extend_from_slice(b"Style Stress Test");
        ranges.push((s, text.len(), 1));
        text.extend_from_slice(b"\n");

        // Line 2: Subtitle (24pt serif blue)
        let s = text.len();
        text.extend_from_slice(b"32 Styles, 3 Fonts, 9 Weights, Vivid Colors");
        ranges.push((s, text.len(), 2));
        text.extend_from_slice(b"\n");

        // Line 3: Baseline alignment — mixed sizes on ONE line
        let s = text.len();
        text.extend_from_slice(b"Sans 36pt Bold Green");
        ranges.push((s, text.len(), 3));
        let s = text.len();
        text.extend_from_slice(b" Mono 10pt Orange");
        ranges.push((s, text.len(), 4));
        let s = text.len();
        text.extend_from_slice(b" Serif 18pt Italic Purple");
        ranges.push((s, text.len(), 5));
        text.extend_from_slice(b"\n");

        // Line 4: More mixed sizes
        let s = text.len();
        text.extend_from_slice(b"Sans 48pt Red");
        ranges.push((s, text.len(), 1));
        let s = text.len();
        text.extend_from_slice(b" body 14pt");
        // style 0 (default) — no push
        let _ = s;
        let s = text.len();
        text.extend_from_slice(b" Mono 16pt Cyan");
        ranges.push((s, text.len(), 6));
        text.extend_from_slice(b"\n");

        // Line 5: Font family showcase
        let s = text.len();
        text.extend_from_slice(b"Sans 20pt Bold Magenta");
        ranges.push((s, text.len(), 7));
        let s = text.len();
        text.extend_from_slice(b" Serif 14pt Italic Red");
        ranges.push((s, text.len(), 8));
        let s = text.len();
        text.extend_from_slice(b" Mono 16pt Cyan");
        ranges.push((s, text.len(), 6));
        text.extend_from_slice(b"\n");

        // Line 6: Color parade — each word in its own color
        let s = text.len();
        text.extend_from_slice(b"Red");
        ranges.push((s, text.len(), 9));
        let s = text.len();
        text.extend_from_slice(b" Blue");
        ranges.push((s, text.len(), 10));
        let s = text.len();
        text.extend_from_slice(b" Green");
        ranges.push((s, text.len(), 11));
        let s = text.len();
        text.extend_from_slice(b" Orange");
        ranges.push((s, text.len(), 12));
        let s = text.len();
        text.extend_from_slice(b" Purple");
        ranges.push((s, text.len(), 13));
        let s = text.len();
        text.extend_from_slice(b" Cyan");
        ranges.push((s, text.len(), 14));
        let s = text.len();
        text.extend_from_slice(b" Magenta");
        ranges.push((s, text.len(), 15));
        text.extend_from_slice(b"\n");

        // Line 7: Weight ramp (100–900) all at 16pt
        let s = text.len();
        text.extend_from_slice(b"Thin ");
        ranges.push((s, text.len(), 16));
        let s = text.len();
        text.extend_from_slice(b"ExLight ");
        ranges.push((s, text.len(), 17));
        let s = text.len();
        text.extend_from_slice(b"Light ");
        ranges.push((s, text.len(), 18));
        let s = text.len();
        text.extend_from_slice(b"Regular ");
        ranges.push((s, text.len(), 19));
        let s = text.len();
        text.extend_from_slice(b"Medium ");
        ranges.push((s, text.len(), 20));
        let s = text.len();
        text.extend_from_slice(b"SemiBold ");
        ranges.push((s, text.len(), 21));
        let s = text.len();
        text.extend_from_slice(b"Bold ");
        ranges.push((s, text.len(), 22));
        let s = text.len();
        text.extend_from_slice(b"ExBold ");
        ranges.push((s, text.len(), 23));
        let s = text.len();
        text.extend_from_slice(b"Black");
        ranges.push((s, text.len(), 24));
        text.extend_from_slice(b"\n");

        // Line 8: Flag combinations
        let s = text.len();
        text.extend_from_slice(b"Serif 22pt Bold Italic Underline Blue");
        ranges.push((s, text.len(), 25));
        let s = text.len();
        text.extend_from_slice(b" Mono 12pt Italic Strike Red");
        ranges.push((s, text.len(), 26));
        text.extend_from_slice(b"\n");

        // Line 9: More size mixing
        let s = text.len();
        text.extend_from_slice(b"Sans 28pt Bold Green");
        ranges.push((s, text.len(), 27));
        let s = text.len();
        text.extend_from_slice(b" Serif 12pt Regular");
        ranges.push((s, text.len(), 28));
        let s = text.len();
        text.extend_from_slice(b" Mono 14pt Bold Cyan");
        ranges.push((s, text.len(), 29));
        text.extend_from_slice(b"\n");

        // Line 10: Large italic + large bold underline
        let s = text.len();
        text.extend_from_slice(b"Sans 40pt Italic Magenta");
        ranges.push((s, text.len(), 30));
        text.extend_from_slice(b"\n");

        // Line 11: Serif large bold underline orange
        let s = text.len();
        text.extend_from_slice(b"Serif 32pt Bold Underline Orange");
        ranges.push((s, text.len(), 31));
        text.extend_from_slice(b"\n");

        // Line 12: Mixed paragraph with inline style changes
        // default body (style 0) for plain words, inline colored/styled words
        let s = text.len();
        text.extend_from_slice(b"This is body text with ");
        let _ = s; // stays style 0
        let s = text.len();
        text.extend_from_slice(b"bold magenta");
        ranges.push((s, text.len(), 7));
        text.extend_from_slice(b" and ");
        let s = text.len();
        text.extend_from_slice(b"italic red");
        ranges.push((s, text.len(), 8));
        text.extend_from_slice(b" and ");
        let s = text.len();
        text.extend_from_slice(b"mono cyan");
        ranges.push((s, text.len(), 6));
        text.extend_from_slice(b" inline.\n");

        // -- Initialize piece table ------------------------------------------------
        let mut pt_buf = vec![0u8; 8192];
        let cap = pt_buf.len();
        if !piecetable::init_with_text(&mut pt_buf, cap, &text, &piecetable::default_body_style()) {
            eprintln!("error: failed to init piece table");
            process::exit(1);
        }

        // Add styles 1–31 (body is already at index 0).
        for (i, s) in extra_styles.iter().enumerate() {
            if piecetable::add_style(&mut pt_buf, s).is_none() {
                eprintln!("error: failed to add style {} to piece table", i + 1);
                process::exit(1);
            }
        }

        // Apply styles to tracked ranges.
        for &(start, end, style_id) in &ranges {
            piecetable::apply_style(&mut pt_buf, start as u32, end as u32, style_id);
        }

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
            "  rich  {:?}  welcome  ({} bytes, {} styles, {} ranges)",
            id,
            pt_bytes.len(),
            h.style_count,
            ranges.len()
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
