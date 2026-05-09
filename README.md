# os

A personal exploration of an alternative operating system where documents are
first-class citizens and applications are interchangeable tools that attach to
content.

## design

The primary artifact is a coherent OS design, not a shipping product. See
`design/philosophy.md` for the root principles.

Key ideas:

- **document-centric:** OS → Document → Tool. No apps own file types.
- **content-aware:** The OS natively understands content types (mimetypes as
  fundamental metadata).
- **view-first:** Viewing is default, editing is deliberate. Editors are modal
  plugins.
- **no POSIX:** Clean-slate APIs built on established standards (mimetypes,
  URIs, Unicode, arm64).
- **microkernel:** 5 kernel objects, 34 syscalls, capability-based security.

## prerequisites

- **Rust nightly** with `aarch64-unknown-none` target (handled automatically by
  `rust-toolchain.toml`. just [install Rust](https://rustup.rs/))
- **[hypervisor](https://github.com/jonathanrtuck/hypervisor)** (`make install`
  from that repo) - native Metal GPU rendering on macOS

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
