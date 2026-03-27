# system/share

Shared resources loaded into the OS at boot time. Previously served via 9p; now baked into disk images by `tools/mkdisk/`.

## Contents

- `inter.ttf`, `inter-italic.ttf` -- Inter variable font (sans-serif, used for UI chrome)
- `jetbrains-mono.ttf`, `jetbrains-mono-italic.ttf` -- JetBrains Mono variable font (monospace, used for editor/code)
- `source-serif-4.ttf`, `source-serif-4-italic.ttf` -- Source Serif 4 variable font (serif, used for prose)
- `test.png` -- Test PNG image for the content pipeline (displayed as a second document space)
- `zoey.jpg` -- Test JPEG image (decoder not yet implemented; blocked on mimetype routing)
