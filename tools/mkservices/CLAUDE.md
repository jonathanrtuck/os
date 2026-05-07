# mkservices

Host tool that packs flat service binaries into a single archive for the kernel
to embed. Bootstrap mechanism only -- runtime service loading comes later via
the filesystem.

## Usage

```sh
mkservices -o pack.bin name=name.bin console=console.bin blk=blk.bin
```

Input binaries are flat (produced by `rust-objcopy -O binary` from ELFs). The
tool does not handle ELF stripping -- the Makefile does that.

## Pack format

```text
[PackHeader: 16 bytes]
[PackEntry x count: 48 bytes each]
[padding to 16 KiB page boundary]
[service 0 binary, page-aligned]
[padding]
[service 1 binary, page-aligned]
...
```

- Magic: `SVPK` (4 bytes)
- Page size: 16384 (Apple Silicon native)
- Service names: 32 bytes null-padded ASCII (matches protocol::MAX_NAME_LEN)
- Total output is page-aligned

## Testing

```sh
cargo test
```
