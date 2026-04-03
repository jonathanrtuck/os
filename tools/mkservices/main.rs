//! mkservices — pack service ELFs into a flat binary archive.
//!
//! Usage: mkservices <output.pack> <page_size> [role_id:path.elf ...]
//!
//! Produces a single binary with:
//!   - 16-byte header: [magic, version, count, pad]
//!   - N x 16-byte entries: [role_id, offset, length, pad]
//!   - Concatenated ELF data, each page-aligned
//!
//! The output is spliced into the kernel ELF as a LOAD segment.

use std::{env, fs, process};

mod pack_format {
    #![allow(dead_code)]
    include!("../../pack_format.rs");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("usage: mkservices <output.pack> <page_size> [role_id:path.elf ...]");
        process::exit(1);
    }

    let output_path = &args[1];
    let page_size: usize = args[2].parse().unwrap_or_else(|e| {
        eprintln!("bad page_size '{}': {e}", args[2]);
        process::exit(1);
    });

    // Parse role_id:path pairs.
    let mut entries: Vec<(u32, Vec<u8>)> = Vec::new();
    for arg in &args[3..] {
        let (role_str, path) = arg.split_once(':').unwrap_or_else(|| {
            eprintln!("expected role_id:path, got '{arg}'");
            process::exit(1);
        });
        let role_id: u32 = role_str.parse().unwrap_or_else(|e| {
            eprintln!("bad role_id '{role_str}': {e}");
            process::exit(1);
        });
        let data = fs::read(path).unwrap_or_else(|e| {
            eprintln!("failed to read '{path}': {e}");
            process::exit(1);
        });
        entries.push((role_id, data));
    }

    let count = entries.len() as u32;
    let header_size = pack_format::PACK_HEADER_SIZE as usize
        + count as usize * pack_format::PACK_ENTRY_SIZE as usize;

    // First ELF starts at the next page boundary after the header + entry table.
    let mut data_offset = align_up(header_size, page_size);

    // Build the entry table and compute offsets.
    let mut entry_table: Vec<[u32; 4]> = Vec::with_capacity(count as usize);
    let mut elf_offsets: Vec<(usize, usize)> = Vec::with_capacity(count as usize);

    for (role_id, elf_data) in &entries {
        let length = elf_data.len() as u32;
        entry_table.push([*role_id, data_offset as u32, length, 0]);
        elf_offsets.push((data_offset, elf_data.len()));
        data_offset = align_up(data_offset + elf_data.len(), page_size);
    }

    let total_size = data_offset;

    // Build the output buffer.
    let mut buf = vec![0u8; total_size];

    // Write header.
    write_u32(&mut buf, 0, pack_format::PACK_MAGIC);
    write_u32(&mut buf, 4, pack_format::PACK_VERSION);
    write_u32(&mut buf, 8, count);
    write_u32(&mut buf, 12, 0); // pad

    // Write entry table.
    for (i, entry) in entry_table.iter().enumerate() {
        let base =
            pack_format::PACK_HEADER_SIZE as usize + i * pack_format::PACK_ENTRY_SIZE as usize;
        write_u32(&mut buf, base, entry[0]);
        write_u32(&mut buf, base + 4, entry[1]);
        write_u32(&mut buf, base + 8, entry[2]);
        write_u32(&mut buf, base + 12, entry[3]);
    }

    // Write ELF data at page-aligned offsets.
    for ((offset, len), (_, elf_data)) in elf_offsets.iter().zip(entries.iter()) {
        buf[*offset..*offset + *len].copy_from_slice(elf_data);
    }

    fs::write(output_path, &buf).unwrap_or_else(|e| {
        eprintln!("failed to write '{output_path}': {e}");
        process::exit(1);
    });

    eprintln!(
        "mkservices: {count} services, {total_size} bytes ({} pages)",
        total_size / page_size
    );
}

fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
