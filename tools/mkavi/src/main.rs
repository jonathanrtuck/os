//! mkavi — create a test MJPEG AVI from JPEG files.
//!
//! Usage: mkavi <output.avi> <fps> <frame1.jpg> [frame2.jpg ...]
//!
//! Each input JPEG becomes one video frame. If only one JPEG is given,
//! it is repeated for 5 seconds of video (fps * 5 frames).

use std::fs;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 4 {
        eprintln!("usage: mkavi <output.avi> <fps> <frame1.jpg> [frame2.jpg ...]");

        std::process::exit(1);
    }

    let output_path = &args[1];
    let fps: u32 = args[2].parse().expect("fps must be a number");
    let jpeg_paths = &args[3..];
    let mut jpeg_data: Vec<Vec<u8>> = Vec::new();

    for path in jpeg_paths {
        let data = fs::read(path).unwrap_or_else(|e| {
            eprintln!("error reading {path}: {e}");

            std::process::exit(1);
        });

        if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
            eprintln!("{path}: not a JPEG file");

            std::process::exit(1);
        }

        jpeg_data.push(data);
    }

    let frames: Vec<&[u8]> = if jpeg_data.len() == 1 {
        let count = (fps * 5) as usize;

        (0..count).map(|_| jpeg_data[0].as_slice()).collect()
    } else {
        jpeg_data.iter().map(|d| d.as_slice()).collect()
    };
    let (width, height) = jpeg_dimensions(&jpeg_data[0]).unwrap_or((320, 240));
    let us_per_frame = 1_000_000 / fps;
    let avi = build_avi(width, height, us_per_frame, &frames);

    fs::write(output_path, &avi).unwrap_or_else(|e| {
        eprintln!("error writing {output_path}: {e}");

        std::process::exit(1);
    });

    let mb = avi.len() as f64 / (1024.0 * 1024.0);

    eprintln!(
        "{output_path}: {width}x{height} @ {fps}fps, {} frames, {mb:.1} MB",
        frames.len()
    );
}

fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;

    while i + 4 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }

        let marker = data[i + 1];

        if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
            if i + 9 < data.len() {
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;

                return Some((w, h));
            }
        }

        if marker == 0xD9 || marker == 0xDA {
            break;
        }

        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;

        i += 2 + len;
    }

    None
}

fn write_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_le_bytes());
}

fn write_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_le_bytes());
}

fn write_fourcc(buf: &mut Vec<u8>, cc: &[u8; 4]) {
    buf.extend_from_slice(cc);
}

fn patch_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

fn build_avi(width: u32, height: u32, us_per_frame: u32, frames: &[&[u8]]) -> Vec<u8> {
    let mut buf = Vec::new();

    // RIFF header (patched later).
    write_fourcc(&mut buf, b"RIFF");
    write_u32(&mut buf, 0); // size placeholder
    write_fourcc(&mut buf, b"AVI ");

    // hdrl LIST.
    let hdrl_start = buf.len();

    write_fourcc(&mut buf, b"LIST");
    write_u32(&mut buf, 0); // size placeholder
    write_fourcc(&mut buf, b"hdrl");

    // avih chunk.
    write_fourcc(&mut buf, b"avih");
    write_u32(&mut buf, 56);
    write_u32(&mut buf, us_per_frame);
    write_u32(&mut buf, 0); // max bytes/sec
    write_u32(&mut buf, 0); // padding granularity
    write_u32(&mut buf, 0); // flags
    write_u32(&mut buf, frames.len() as u32);
    write_u32(&mut buf, 0); // initial frames
    write_u32(&mut buf, 1); // streams
    write_u32(&mut buf, 0); // suggested buffer size
    write_u32(&mut buf, width);
    write_u32(&mut buf, height);
    write_u32(&mut buf, 0); // reserved
    write_u32(&mut buf, 0);
    write_u32(&mut buf, 0);
    write_u32(&mut buf, 0);

    // strl LIST.
    let strl_start = buf.len();

    write_fourcc(&mut buf, b"LIST");
    write_u32(&mut buf, 0); // size placeholder
    write_fourcc(&mut buf, b"strl");

    // strh chunk.
    write_fourcc(&mut buf, b"strh");
    write_u32(&mut buf, 56);
    write_fourcc(&mut buf, b"vids");
    write_fourcc(&mut buf, b"MJPG");
    write_u32(&mut buf, 0); // flags
    write_u16(&mut buf, 0); // priority
    write_u16(&mut buf, 0); // language
    write_u32(&mut buf, 0); // initial frames
    write_u32(&mut buf, us_per_frame); // scale
    write_u32(&mut buf, 1_000_000); // rate
    write_u32(&mut buf, 0); // start
    write_u32(&mut buf, frames.len() as u32); // length
    write_u32(&mut buf, 0); // suggested buffer size
    write_u32(&mut buf, 0); // quality
    write_u32(&mut buf, 0); // sample size
    write_u16(&mut buf, 0); // rcFrame
    write_u16(&mut buf, 0);
    write_u16(&mut buf, width as u16);
    write_u16(&mut buf, height as u16);

    // strf chunk (BITMAPINFOHEADER).
    write_fourcc(&mut buf, b"strf");
    write_u32(&mut buf, 40);
    write_u32(&mut buf, 40); // biSize
    write_u32(&mut buf, width);
    write_u32(&mut buf, height);
    write_u16(&mut buf, 1); // planes
    write_u16(&mut buf, 24); // bit count
    write_fourcc(&mut buf, b"MJPG"); // compression
    write_u32(&mut buf, width * height * 3); // image size
    write_u32(&mut buf, 0); // x pels/meter
    write_u32(&mut buf, 0); // y pels/meter
    write_u32(&mut buf, 0); // colors used
    write_u32(&mut buf, 0); // colors important

    let strl_size = (buf.len() - strl_start - 8) as u32;

    patch_u32(&mut buf, strl_start + 4, strl_size);

    let hdrl_size = (buf.len() - hdrl_start - 8) as u32;

    patch_u32(&mut buf, hdrl_start + 4, hdrl_size);

    // movi LIST.
    let movi_start = buf.len();

    write_fourcc(&mut buf, b"LIST");
    write_u32(&mut buf, 0); // size placeholder
    write_fourcc(&mut buf, b"movi");

    let mut frame_offsets: Vec<(u32, u32)> = Vec::new();

    for frame in frames {
        let chunk_offset = buf.len() - movi_start - 8;

        write_fourcc(&mut buf, b"00dc");
        write_u32(&mut buf, frame.len() as u32);

        buf.extend_from_slice(frame);

        if frame.len() % 2 != 0 {
            buf.push(0);
        }

        frame_offsets.push((chunk_offset as u32, frame.len() as u32));
    }

    let movi_size = (buf.len() - movi_start - 8) as u32;

    patch_u32(&mut buf, movi_start + 4, movi_size);

    // idx1.
    write_fourcc(&mut buf, b"idx1");
    write_u32(&mut buf, (frame_offsets.len() * 16) as u32);

    for (offset, size) in &frame_offsets {
        write_fourcc(&mut buf, b"00dc");
        write_u32(&mut buf, 0x10); // AVIIF_KEYFRAME
        write_u32(&mut buf, *offset);
        write_u32(&mut buf, *size);
    }

    // Patch RIFF size.
    let riff_size = (buf.len() - 8) as u32;

    patch_u32(&mut buf, 4, riff_size);

    buf
}
