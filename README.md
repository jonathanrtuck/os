# os

A personal exploration of an alternative operating system where documents are
first-class citizens and applications are interchangeable tools that attach to
content.

## design

The primary artifact is a coherent OS design, not a shipping product. See
`design/philosophy.md` for the root principles.

Key ideas:

- **Document-centric:** OS -> Document -> Tool. No apps own file types.
- **Content-aware:** The OS natively understands content types (mimetypes as
  fundamental metadata).
- **View-first:** Viewing is default, editing is deliberate. Editors are modal
  plugins.
- **No POSIX:** Clean-slate APIs built on established standards (mimetypes,
  URIs, Unicode, arm64).
- **Microkernel:** 5 kernel objects, 25 syscalls, capability-based security.

## status

Kernel rewrite in progress. See `STATUS.md` for details.

Previous prototype (v0.1-v0.6) preserved at tag `v0.6-pre-rewrite`.

## prerequisites

- **Rust nightly** with `aarch64-unknown-none` target (handled automatically by
  `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- **[hypervisor](https://github.com/jonathanrtuck/hypervisor)** (`make install`
  from that repo) — native Metal GPU rendering on macOS

## build

```sh
cargo b
```

## run

```sh
cargo r
```

This builds the kernel and launches it in the hypervisor with Metal GPU
rendering. Close the window or Cmd+Q to exit.

## test

```sh
make test-all
```
