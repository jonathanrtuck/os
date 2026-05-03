# Document-Centric OS

A personal exploration of an alternative operating system where documents are first-class citizens and applications are interchangeable tools that attach to content.

## Design

The primary artifact is a coherent OS design, not a shipping product. See `design/philosophy.md` for the root principles.

Key ideas:
- **Document-centric:** OS -> Document -> Tool. No apps own file types.
- **Content-aware:** The OS natively understands content types (mimetypes as fundamental metadata).
- **View-first:** Viewing is default, editing is deliberate. Editors are modal plugins.
- **No POSIX:** Clean-slate APIs built on established standards (mimetypes, URIs, Unicode, arm64).
- **Microkernel:** 5 kernel objects, 25 syscalls, capability-based security.

## Status

Kernel rewrite in progress. See `STATUS.md` for details.

Previous prototype (v0.1-v0.6) preserved at tag `v0.6-pre-rewrite`.

## Target

ARM64 (Apple Silicon M4 Pro), running under a custom hypervisor (`~/Sites/hypervisor`).

## License

Public domain (Unlicense).
