//! mkservices — packs flat service binaries into a single archive.
//!
//! Usage: mkservices -o output.bin name=binary.bin [name=binary.bin ...]

mod format;

use std::{env, fs, process};

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut output_path = None;
    let mut services = Vec::new();
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;

                if i >= args.len() {
                    die("--output requires an argument");
                }

                output_path = Some(args[i].clone());
            }
            "-h" | "--help" => {
                eprintln!("Usage: mkservices -o output.bin name=binary.bin [...]");
                process::exit(0);
            }
            arg => {
                if let Some((name, path)) = arg.split_once('=') {
                    if name.is_empty() || path.is_empty() {
                        die(&format!("invalid service spec: {arg}"));
                    }

                    services.push((name.to_string(), path.to_string()));
                } else {
                    die(&format!("unexpected argument: {arg}"));
                }
            }
        }

        i += 1;
    }

    let output_path = output_path.unwrap_or_else(|| die("--output path required"));

    if services.is_empty() {
        die("no services specified");
    }

    let mut builder = format::PackBuilder::new();

    for (name, path) in &services {
        let binary = fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));

        if binary.is_empty() {
            die(&format!("{path}: empty binary"));
        }

        if name.len() > format::MAX_NAME_LEN {
            die(&format!(
                "{name}: name exceeds {} bytes",
                format::MAX_NAME_LEN
            ));
        }

        builder.add_service(name, binary);
    }

    let pack = builder.build();

    fs::write(&output_path, &pack).unwrap_or_else(|e| die(&format!("{output_path}: {e}")));

    eprintln!(
        "pack: {} services, {} bytes ({} pages)",
        builder.service_count(),
        pack.len(),
        pack.len() / format::PAGE_SIZE,
    );

    for i in 0..builder.service_count() {
        let entry = format::read_entry(&pack, i).unwrap();

        eprintln!(
            "  {:>16}  {:>8} bytes  @ offset {:#010x}",
            entry.name_str(),
            entry.size,
            entry.offset,
        );
    }
}

fn die(msg: &str) -> ! {
    eprintln!("mkservices: {msg}");
    process::exit(1);
}
