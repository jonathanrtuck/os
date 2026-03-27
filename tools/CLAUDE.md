# tools

Host-side build and development tools. These run on macOS, not on the target OS.

## Subdirectories

- `mkdisk/` -- Factory disk image builder: creates pre-populated disk images with fonts and test content

Tools depend on system libraries (e.g., `fs`, `store`) to produce artifacts consumed by the OS at boot time.
