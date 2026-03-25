//! PNG decoder tests.

#[path = "../../services/decoders/png/png.rs"]
mod png;
use png::{chunk_crc, crc32, png_decode, png_decode_buf_size, png_header, PngError};

// ---------------------------------------------------------------------------
// PngSuite conformance helpers
// ---------------------------------------------------------------------------

const PNGSUITE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/pngsuite");
const PNGSUITE_REF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/pngsuite/reference");

/// Load a PngSuite test image and its reference BGRA data. Returns (png_bytes, ref_bgra).
fn load_pngsuite(name: &str) -> (Vec<u8>, Vec<u8>) {
    let png_path = format!("{}/{}", PNGSUITE_DIR, name);
    let ref_path = format!(
        "{}/{}.bgra",
        PNGSUITE_REF,
        name.strip_suffix(".png").unwrap()
    );
    let png_data =
        std::fs::read(&png_path).unwrap_or_else(|e| panic!("failed to read {}: {}", png_path, e));
    let ref_data =
        std::fs::read(&ref_path).unwrap_or_else(|e| panic!("failed to read {}: {}", ref_path, e));
    (png_data, ref_data)
}

/// Decode a PngSuite image and compare pixel-by-pixel against reference BGRA.
fn assert_pngsuite_matches(name: &str) {
    let (png_data, ref_bgra) = load_pngsuite(name);
    let hdr = png_header(&png_data).unwrap_or_else(|e| panic!("{}: header failed: {:?}", name, e));
    let w = hdr.width as usize;
    let h = hdr.height as usize;
    assert_eq!(
        ref_bgra.len(),
        w * h * 4,
        "{}: reference size mismatch",
        name
    );

    // Compute exact buffer requirement via the decoder's helper.
    let buf_size = png::png_decode_buf_size(&png_data)
        .unwrap_or_else(|e| panic!("{}: buf_size failed: {:?}", name, e));
    let mut output = vec![0u8; buf_size];
    let result =
        png_decode(&png_data, &mut output).unwrap_or_else(|e| panic!("{}: decode failed: {:?}", name, e));
    assert_eq!(result.width as usize, w, "{}: width mismatch", name);
    assert_eq!(result.height as usize, h, "{}: height mismatch", name);

    // Compare pixel by pixel for clear error messages
    let out = &output[..w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let got = &out[i..i + 4];
            let exp = &ref_bgra[i..i + 4];
            if got != exp {
                panic!(
                    "{}: pixel ({},{}) mismatch: got BGRA [{},{},{},{}] expected [{},{},{},{}]",
                    name, x, y, got[0], got[1], got[2], got[3], exp[0], exp[1], exp[2], exp[3],
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PngSuite: basic non-interlaced (basn*)
// ---------------------------------------------------------------------------

#[test]
fn pngsuite_basn0g01() {
    assert_pngsuite_matches("basn0g01.png");
}
#[test]
fn pngsuite_basn0g02() {
    assert_pngsuite_matches("basn0g02.png");
}
#[test]
fn pngsuite_basn0g04() {
    assert_pngsuite_matches("basn0g04.png");
}
#[test]
fn pngsuite_basn0g08() {
    assert_pngsuite_matches("basn0g08.png");
}
#[test]
fn pngsuite_basn0g16() {
    assert_pngsuite_matches("basn0g16.png");
}
#[test]
fn pngsuite_basn2c08() {
    assert_pngsuite_matches("basn2c08.png");
}
#[test]
fn pngsuite_basn2c16() {
    assert_pngsuite_matches("basn2c16.png");
}
#[test]
fn pngsuite_basn3p01() {
    assert_pngsuite_matches("basn3p01.png");
}
#[test]
fn pngsuite_basn3p02() {
    assert_pngsuite_matches("basn3p02.png");
}
#[test]
fn pngsuite_basn3p04() {
    assert_pngsuite_matches("basn3p04.png");
}
#[test]
fn pngsuite_basn3p08() {
    assert_pngsuite_matches("basn3p08.png");
}
#[test]
fn pngsuite_basn4a08() {
    assert_pngsuite_matches("basn4a08.png");
}
#[test]
fn pngsuite_basn4a16() {
    assert_pngsuite_matches("basn4a16.png");
}
#[test]
fn pngsuite_basn6a08() {
    assert_pngsuite_matches("basn6a08.png");
}
#[test]
fn pngsuite_basn6a16() {
    assert_pngsuite_matches("basn6a16.png");
}

// ---------------------------------------------------------------------------
// PngSuite: basic interlaced (basi*)
// ---------------------------------------------------------------------------

#[test]
fn pngsuite_basi0g01() {
    assert_pngsuite_matches("basi0g01.png");
}
#[test]
fn pngsuite_basi0g02() {
    assert_pngsuite_matches("basi0g02.png");
}
#[test]
fn pngsuite_basi0g04() {
    assert_pngsuite_matches("basi0g04.png");
}
#[test]
fn pngsuite_basi0g08() {
    assert_pngsuite_matches("basi0g08.png");
}
#[test]
fn pngsuite_basi0g16() {
    assert_pngsuite_matches("basi0g16.png");
}
#[test]
fn pngsuite_basi2c08() {
    assert_pngsuite_matches("basi2c08.png");
}
#[test]
fn pngsuite_basi2c16() {
    assert_pngsuite_matches("basi2c16.png");
}
#[test]
fn pngsuite_basi3p01() {
    assert_pngsuite_matches("basi3p01.png");
}
#[test]
fn pngsuite_basi3p02() {
    assert_pngsuite_matches("basi3p02.png");
}
#[test]
fn pngsuite_basi3p04() {
    assert_pngsuite_matches("basi3p04.png");
}
#[test]
fn pngsuite_basi3p08() {
    assert_pngsuite_matches("basi3p08.png");
}
#[test]
fn pngsuite_basi4a08() {
    assert_pngsuite_matches("basi4a08.png");
}
#[test]
fn pngsuite_basi4a16() {
    assert_pngsuite_matches("basi4a16.png");
}
#[test]
fn pngsuite_basi6a08() {
    assert_pngsuite_matches("basi6a08.png");
}
#[test]
fn pngsuite_basi6a16() {
    assert_pngsuite_matches("basi6a16.png");
}

// ---------------------------------------------------------------------------
// PngSuite: transparency (t*.png)
// ---------------------------------------------------------------------------

#[test]
fn pngsuite_tbbn0g04() {
    assert_pngsuite_matches("tbbn0g04.png");
}
#[test]
fn pngsuite_tbbn2c16() {
    assert_pngsuite_matches("tbbn2c16.png");
}
#[test]
fn pngsuite_tbbn3p08() {
    assert_pngsuite_matches("tbbn3p08.png");
}
#[test]
fn pngsuite_tbrn2c08() {
    assert_pngsuite_matches("tbrn2c08.png");
}
#[test]
fn pngsuite_tbyn3p08() {
    assert_pngsuite_matches("tbyn3p08.png");
}
#[test]
fn pngsuite_tp0n0g08() {
    assert_pngsuite_matches("tp0n0g08.png");
}
#[test]
fn pngsuite_tp0n2c08() {
    assert_pngsuite_matches("tp0n2c08.png");
}
#[test]
fn pngsuite_tp0n3p08() {
    assert_pngsuite_matches("tp0n3p08.png");
}
#[test]
fn pngsuite_tp1n3p08() {
    assert_pngsuite_matches("tp1n3p08.png");
}
#[test]
fn pngsuite_tm3n3p02() {
    assert_pngsuite_matches("tm3n3p02.png");
}

// ---------------------------------------------------------------------------
// PngSuite: corrupt files should return errors (x*.png)
// ---------------------------------------------------------------------------

#[test]
fn pngsuite_corrupt_files_return_errors() {
    let dir = std::fs::read_dir(PNGSUITE_DIR).expect("can't read pngsuite dir");
    let mut checked = 0;
    for entry in dir {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        if !name.starts_with('x') || !name.ends_with(".png") {
            continue;
        }
        let data = std::fs::read(entry.path()).unwrap();
        let mut output = vec![0u8; 32 * 32 * 4 + 32];
        let result = png_decode(&data, &mut output);
        assert!(
            result.is_err(),
            "corrupt file {} should fail but decoded successfully",
            name
        );
        checked += 1;
    }
    assert!(checked >= 10, "expected at least 10 corrupt files, found {}", checked);
}

// ---------------------------------------------------------------------------
// PngSuite: bulk conformance — every image with a reference
// ---------------------------------------------------------------------------

#[test]
fn pngsuite_bulk_conformance() {
    let dir = std::fs::read_dir(PNGSUITE_REF).expect("can't read reference dir");
    let mut names: Vec<String> = dir
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".bgra").map(|s| format!("{}.png", s))
        })
        .collect();
    names.sort();

    let mut passed = 0;
    let mut failed = Vec::new();

    for name in &names {
        // Use std::panic::catch_unwind so one failure doesn't abort the rest.
        let n = name.clone();
        let result = std::panic::catch_unwind(|| {
            assert_pngsuite_matches(&n);
        });
        if result.is_ok() {
            passed += 1;
        } else {
            failed.push(name.clone());
        }
    }

    if !failed.is_empty() {
        panic!(
            "{}/{} PngSuite images failed:\n  {}",
            failed.len(),
            names.len(),
            failed.join("\n  ")
        );
    }
    assert!(
        passed >= 100,
        "expected at least 100 reference images, found {}",
        passed
    );
}

// ---------------------------------------------------------------------------
// Test data
// ---------------------------------------------------------------------------

// 4x4 RGBA test PNG (filter=None, generated by Python)
const TEST_PNG_4X4_RGBA: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x08, 0x06, 0x00, 0x00, 0x00, 0xa9, 0xf1, 0x9e,
    0x7e, 0x00, 0x00, 0x00, 0x30, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x2d, 0x8b, 0xc9, 0x11, 0x00,
    0x30, 0x08, 0x02, 0xb7, 0x34, 0x4b, 0xdb, 0xd2, 0xec, 0x8c, 0xa8, 0x13, 0xe0, 0xc1, 0x70, 0x10,
    0xc8, 0x91, 0x1c, 0x60, 0x1d, 0x38, 0x91, 0x63, 0xa5, 0xaa, 0xa2, 0xa6, 0xbb, 0x77, 0x20, 0x5f,
    0x73, 0x72, 0x0a, 0xf2, 0x00, 0x81, 0x4b, 0x23, 0xe6, 0xa6, 0x81, 0xd8, 0x2d, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

// 4x4 RGB test PNG (filter=None, generated by Python)
const TEST_PNG_4X4_RGB: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x08, 0x02, 0x00, 0x00, 0x00, 0x26, 0x93, 0x09,
    0x29, 0x00, 0x00, 0x00, 0x28, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xc0,
    0x00, 0xc6, 0x40, 0x00, 0xa4, 0x18, 0x1a, 0xe0, 0xd8, 0xc1, 0xc1, 0xa1, 0xa1, 0xa1, 0xe1, 0xc0,
    0x81, 0x03, 0x20, 0x89, 0xff, 0x0d, 0x40, 0x91, 0xff, 0x40, 0x0a, 0x88, 0x01, 0xd6, 0x80, 0x14,
    0x74, 0x98, 0xeb, 0xef, 0xc4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
    0x82,
];

// 4x5 RGBA test PNG with all 5 filter types (None, Sub, Up, Average, Paeth)
const TEST_PNG_ALL_FILTERS: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x05, 0x08, 0x06, 0x00, 0x00, 0x00, 0x62, 0xad, 0x4d,
    0xdb, 0x00, 0x00, 0x00, 0x3f, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x38, 0x61, 0x24, 0xf7,
    0x1f, 0x19, 0x33, 0x1a, 0xa5, 0x04, 0xfc, 0xb7, 0x61, 0x60, 0x60, 0x80, 0x61, 0x26, 0x2e, 0x2e,
    0x2e, 0x06, 0x64, 0xcc, 0xec, 0xf6, 0xdb, 0xdb, 0xf3, 0x9b, 0x08, 0xff, 0xd3, 0x1b, 0xba, 0xfc,
    0x4f, 0x77, 0xb9, 0xf1, 0x3f, 0x65, 0x79, 0x23, 0xb7, 0xeb, 0x0d, 0xc3, 0x1b, 0x23, 0x06, 0x86,
    0x5d, 0x10, 0x0c, 0x00, 0x39, 0x9f, 0x18, 0xde, 0xbc, 0x00, 0x72, 0x5f, 0x00, 0x00, 0x00, 0x00,
    0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

// ---------------------------------------------------------------------------
// PNG header parsing
// ---------------------------------------------------------------------------

#[test]
fn png_header_parses_rgba() {
    let hdr = png_header(TEST_PNG_4X4_RGBA).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);
    assert_eq!(hdr.bit_depth, 8);
    assert_eq!(hdr.color_type, 6); // RGBA
}

#[test]
fn png_header_parses_rgb() {
    let hdr = png_header(TEST_PNG_4X4_RGB).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);
    assert_eq!(hdr.bit_depth, 8);
    assert_eq!(hdr.color_type, 2); // RGB
}

#[test]
fn png_header_parses_all_filters() {
    let hdr = png_header(TEST_PNG_ALL_FILTERS).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 5);
    assert_eq!(hdr.bit_depth, 8);
    assert_eq!(hdr.color_type, 6); // RGBA
}

#[test]
fn png_invalid_magic_returns_err() {
    let bad_data = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let result = png_header(&bad_data);
    assert_eq!(result.unwrap_err(), PngError::InvalidSignature);
}

#[test]
fn png_decode_invalid_magic_returns_err() {
    let bad_data = [0x00; 64];
    let mut output = [0u8; 256];
    let result = png_decode(&bad_data, &mut output);
    assert_eq!(result.unwrap_err(), PngError::InvalidSignature);
}

#[test]
fn png_truncated_before_signature_returns_err() {
    let data = &[0x89, 0x50, 0x4e];
    assert_eq!(png_header(data).unwrap_err(), PngError::Truncated);
}

#[test]
fn png_truncated_ihdr_returns_err() {
    let data = &TEST_PNG_4X4_RGBA[..20];
    assert_eq!(png_header(data).unwrap_err(), PngError::Truncated);
}

#[test]
fn png_truncated_idat_returns_err() {
    let data = &TEST_PNG_4X4_RGBA[..50];
    let mut output = [0u8; 4096];
    let result = png_decode(data, &mut output);
    assert!(result.is_err(), "truncated IDAT should return Err");
}

/// After mutating IHDR fields (bytes 16..29), recompute the IHDR CRC
/// so CRC validation passes and the field-specific error is tested.
fn fix_ihdr_crc(png: &mut [u8]) {
    // IHDR CRC covers chunk_type (12..16) + chunk_data (16..29), stored at 29..33.
    let crc = chunk_crc(&png[12..16], &png[16..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
}

#[test]
fn png_zero_width_returns_err() {
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[16] = 0;
    bad_png[17] = 0;
    bad_png[18] = 0;
    bad_png[19] = 0;
    fix_ihdr_crc(&mut bad_png);
    assert_eq!(png_header(&bad_png).unwrap_err(), PngError::ZeroDimensions);
}

#[test]
fn png_zero_height_returns_err() {
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[20] = 0;
    bad_png[21] = 0;
    bad_png[22] = 0;
    bad_png[23] = 0;
    fix_ihdr_crc(&mut bad_png);
    assert_eq!(png_header(&bad_png).unwrap_err(), PngError::ZeroDimensions);
}

#[test]
fn png_unsupported_format_returns_err() {
    // Color type 7 is not valid per PNG spec.
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[25] = 7;
    fix_ihdr_crc(&mut bad_png);
    let mut output = [0u8; 4096];
    assert_eq!(
        png_decode(&bad_png, &mut output).unwrap_err(),
        PngError::UnsupportedFormat
    );
}

#[test]
fn png_invalid_bit_depth_for_color_type_returns_err() {
    // Bit depth 3 is not valid for any color type.
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    bad_png[24] = 3;
    fix_ihdr_crc(&mut bad_png);
    let mut output = [0u8; 4096];
    assert_eq!(
        png_decode(&bad_png, &mut output).unwrap_err(),
        PngError::UnsupportedFormat
    );
}

#[test]
fn png_decode_rgba_4x4_pixel_values() {
    let buf_size = png_decode_buf_size(TEST_PNG_4X4_RGBA).unwrap();
    let mut output = vec![0u8; buf_size];
    let hdr = png_decode(TEST_PNG_4X4_RGBA, &mut output).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);

    let px = &output[0..4];
    assert_eq!(px[0], 0);
    assert_eq!(px[1], 0);
    assert_eq!(px[2], 255);
    assert_eq!(px[3], 255);

    let px = &output[4..8];
    assert_eq!(px[0], 0);
    assert_eq!(px[1], 255);
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);

    let px = &output[8..12];
    assert_eq!(px[0], 255);
    assert_eq!(px[1], 0);
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);

    let px = &output[12..16];
    assert_eq!(px, &[255, 255, 255, 255]);

    let row1_start = 4 * 4;
    let px = &output[row1_start + 4..row1_start + 8];
    assert_eq!(px[0], 0);
    assert_eq!(px[1], 0);
    assert_eq!(px[2], 255);
    assert_eq!(px[3], 128);
}

#[test]
fn png_decode_rgb_4x4_pixel_values() {
    let buf_size = png_decode_buf_size(TEST_PNG_4X4_RGB).unwrap();
    let mut output = vec![0u8; buf_size];
    let hdr = png_decode(TEST_PNG_4X4_RGB, &mut output).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 4);

    let px = &output[0..4];
    assert_eq!(px[0], 0);
    assert_eq!(px[1], 0);
    assert_eq!(px[2], 255);
    assert_eq!(px[3], 255);

    let px = &output[4..8];
    assert_eq!(px[0], 0);
    assert_eq!(px[1], 255);
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);

    let px = &output[8..12];
    assert_eq!(px[0], 255);
    assert_eq!(px[1], 0);
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);

    let row1_start = 4 * 4;
    let px = &output[row1_start + 12..row1_start + 16];
    assert_eq!(px[0], 128);
    assert_eq!(px[1], 0);
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);
}

#[test]
fn png_decode_all_filter_types() {
    let buf_size = png_decode_buf_size(TEST_PNG_ALL_FILTERS).unwrap();
    let mut output = vec![0u8; buf_size];
    let hdr = png_decode(TEST_PNG_ALL_FILTERS, &mut output).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 5);

    fn check_pixel(output: &[u8], row: usize, col: usize, r: u8, g: u8, b: u8, a: u8) {
        let stride = 4 * 4;
        let offset = row * stride + col * 4;
        assert_eq!(output[offset], b, "pixel ({},{}) B", col, row);
        assert_eq!(output[offset + 1], g, "pixel ({},{}) G", col, row);
        assert_eq!(output[offset + 2], r, "pixel ({},{}) R", col, row);
        assert_eq!(output[offset + 3], a, "pixel ({},{}) A", col, row);
    }

    check_pixel(&output, 0, 0, 200, 50, 30, 255);
    check_pixel(&output, 0, 1, 200, 50, 30, 255);
    check_pixel(&output, 0, 2, 200, 50, 30, 255);
    check_pixel(&output, 0, 3, 200, 50, 30, 255);

    check_pixel(&output, 1, 0, 50, 100, 80, 255);
    check_pixel(&output, 1, 1, 110, 100, 80, 255);
    check_pixel(&output, 1, 2, 170, 100, 80, 255);
    check_pixel(&output, 1, 3, 230, 100, 80, 255);

    check_pixel(&output, 2, 0, 60, 110, 90, 255);
    check_pixel(&output, 2, 1, 120, 110, 90, 255);
    check_pixel(&output, 2, 2, 180, 110, 90, 255);
    check_pixel(&output, 2, 3, 240, 110, 90, 255);

    check_pixel(&output, 3, 0, 100, 50, 120, 200);
    check_pixel(&output, 3, 1, 100, 100, 120, 200);
    check_pixel(&output, 3, 2, 100, 150, 120, 200);
    check_pixel(&output, 3, 3, 100, 200, 120, 200);

    check_pixel(&output, 4, 0, 80, 80, 50, 180);
    check_pixel(&output, 4, 1, 80, 80, 100, 180);
    check_pixel(&output, 4, 2, 80, 80, 150, 180);
    check_pixel(&output, 4, 3, 80, 80, 200, 180);
}

#[test]
fn png_decode_buffer_too_small_returns_err() {
    // Buffer must be smaller than png_decode_buf_size to trigger BufferTooSmall.
    let mut output = [0u8; 16];
    let result = png_decode(TEST_PNG_4X4_RGBA, &mut output);
    assert_eq!(result.unwrap_err(), PngError::BufferTooSmall);
}

#[test]
fn png_empty_data_returns_err() {
    let result = png_header(&[]);
    assert_eq!(result.unwrap_err(), PngError::Truncated);
}

#[test]
fn png_decode_empty_data_returns_err() {
    let mut output = [0u8; 256];
    let result = png_decode(&[], &mut output);
    assert_eq!(result.unwrap_err(), PngError::Truncated);
}

#[test]
fn png_decode_test_image_from_file() {
    let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../share/test.png"));
    if let Ok(data) = data {
        let hdr = png_header(&data).unwrap();
        assert!(hdr.width > 0 && hdr.height > 0);
        assert_eq!(hdr.bit_depth, 8);
        assert!(hdr.color_type == 2 || hdr.color_type == 6);

        let buf_size = png_decode_buf_size(&data).unwrap();
        let mut output = vec![0u8; buf_size];
        let result = png_decode(&data, &mut output);
        assert!(
            result.is_ok(),
            "failed to decode test.png: {:?}",
            result.unwrap_err()
        );

        let a = output[3];
        assert!(a > 0, "pixel (0,0) alpha should be > 0, got {}", a);

        let cx = hdr.width / 2;
        let cy = hdr.height / 2;
        let center = (cy as usize * hdr.width as usize + cx as usize) * 4;
        let center_a = output[center + 3];
        assert!(center_a > 0, "center pixel alpha should be > 0");
    }
}

#[test]
fn png_decode_to_surface_correct_colors() {
    let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../share/test.png"));
    if let Ok(data) = data {
        let hdr = png_header(&data).unwrap();
        let buf_size = png_decode_buf_size(&data).unwrap();
        let mut output = vec![0u8; buf_size];
        let _ = png_decode(&data, &mut output).unwrap();

        let mut non_zero = 0;
        for i in 0..(hdr.width * hdr.height) as usize {
            let r = output[i * 4 + 2];
            let g = output[i * 4 + 1];
            let b = output[i * 4];
            if r > 0 || g > 0 || b > 0 {
                non_zero += 1;
            }
        }
        assert!(
            non_zero > 100,
            "decoded image should have many non-zero pixels, got {}",
            non_zero
        );

        let px00_r = output[2];
        let cx = hdr.width / 2;
        let cy = hdr.height / 2;
        let center = (cy as usize * hdr.width as usize + cx as usize) * 4;
        let px_center_r = output[center + 2];
        assert_ne!(px00_r, px_center_r, "corner and center should differ");
    }
}

// ---------------------------------------------------------------------------
// CRC32 validation tests
// ---------------------------------------------------------------------------

#[test]
fn crc32_empty_input() {
    // CRC32 of empty data is 0x00000000.
    assert_eq!(crc32(&[]), 0x0000_0000);
}

#[test]
fn crc32_known_test_vectors() {
    // "123456789" → 0xCBF43926 (canonical CRC32 test vector, ITU-T V.42).
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    // Single byte 0x00.
    assert_eq!(crc32(&[0x00]), 0xD202_EF8D);
    // "IHDR" — quick sanity check against a known chunk type.
    assert_eq!(crc32(b"IHDR"), 0xA8A1_AE0A);
}

#[test]
fn crc32_incremental_matches_single_pass() {
    // Verify that chunk_crc(type, data) == crc32(type ++ data).
    let chunk_type = b"PLTE";
    let chunk_data: &[u8] = &[255, 0, 0, 0, 255, 0, 0, 0, 255];
    let mut combined = Vec::new();
    combined.extend_from_slice(chunk_type);
    combined.extend_from_slice(chunk_data);
    let single = crc32(&combined);
    let incremental = chunk_crc(chunk_type, chunk_data);
    assert_eq!(single, incremental);
}

#[test]
fn png_corrupt_ihdr_crc_returns_err() {
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    // Flip a bit in the IHDR CRC (bytes 29..33).
    bad_png[29] ^= 0x01;
    assert_eq!(png_header(&bad_png).unwrap_err(), PngError::CrcMismatch);
}

#[test]
fn png_corrupt_idat_crc_returns_err() {
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    // Find IDAT chunk and corrupt its CRC.
    // IHDR is at offset 8, length 13, so IHDR ends at 8+4+4+13+4 = 33.
    // The next chunk starts at 33.
    let mut pos = 33;
    while pos + 8 <= bad_png.len() {
        let chunk_len = u32::from_be_bytes([
            bad_png[pos],
            bad_png[pos + 1],
            bad_png[pos + 2],
            bad_png[pos + 3],
        ]) as usize;
        let chunk_type = &bad_png[pos + 4..pos + 8];
        if chunk_type == b"IDAT" {
            // CRC is at pos + 8 + chunk_len.
            let crc_pos = pos + 8 + chunk_len;
            bad_png[crc_pos] ^= 0xFF;
            break;
        }
        pos += 8 + chunk_len + 4;
    }
    let mut output = vec![0u8; 4096];
    assert_eq!(
        png_decode(&bad_png, &mut output).unwrap_err(),
        PngError::CrcMismatch
    );
}

#[test]
fn png_corrupt_single_data_byte_detected_by_crc() {
    // Corrupt a data byte inside IDAT — the CRC should catch it.
    let mut bad_png = TEST_PNG_4X4_RGBA.to_vec();
    let mut pos = 33;
    while pos + 8 <= bad_png.len() {
        let chunk_len = u32::from_be_bytes([
            bad_png[pos],
            bad_png[pos + 1],
            bad_png[pos + 2],
            bad_png[pos + 3],
        ]) as usize;
        let chunk_type = &bad_png[pos + 4..pos + 8];
        if chunk_type == b"IDAT" && chunk_len > 4 {
            // Flip a bit in the middle of the compressed data.
            bad_png[pos + 8 + chunk_len / 2] ^= 0x01;
            break;
        }
        pos += 8 + chunk_len + 4;
    }
    let mut output = vec![0u8; 4096];
    let result = png_decode(&bad_png, &mut output);
    assert!(result.is_err(), "corrupted IDAT byte should be detected");
    assert_eq!(result.unwrap_err(), PngError::CrcMismatch);
}
