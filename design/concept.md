# Document-Centric OS Design

## The Core Idea

Current operating systems are app-based: `OS → App → Document`. The OS manages apps, and documents live inside apps. To edit a text file, you open a text editor, then open the file within it.

A document-centric OS flips this: `OS → Document → App`. The OS manages files directly. Documents exist independently of any app. To edit a text file, you open the file, then attach an editor to it.

This mirrors the physical desktop analogy. A paper on your desk exists independently of the pen you use to write on it. You can look at (view) a document without owning an editor. But if you want to change it, you need a tool.

## Key Design Principles

**A document is a file. A file has a mimetype. That's the whole model.**

- The OS can natively *view* all common mimetypes: text, images, audio, video, PDFs, etc.
- Editing requires installing an editor that supports that mimetype.
- Editors are tools you bring to documents, not containers that hold them.
- The OS does not need a "project" concept. The filesystem already provides directories for user-level organization.

## Mimetype Evolution

Documents aren't locked into a format at creation time. If you start with a plain text file and use an image tool to add an image, the OS prompts you to confirm changing the mimetype to a richer format. The document evolves based on what you do with it, rather than requiring you to decide upfront what kind of document you're making.

## Layered Rendering

Not all files are equally simple to display. The OS uses a layered rendering approach:

- **Content types** (text, images, audio, video, tabular data) have obvious, natural renderings. The OS handles these natively.
- **Complex formats** (3D models, databases, layered image files) can declare a "simple view" the OS renders by default — a flattened image for a PSD, a default table for a database, etc. Full interaction with the internal structure requires an editor.

The goal: the OS never leaves you staring at a blank icon. You can always peek inside a file, even if you need a specialized editor for full interaction.

## Multimedia Editing Workflow

The interesting use case is editing compound documents. For something like a presentation:

1. Open the document.
2. Open a text editor to update the words on each slide.
3. Close that, open an image editor to resize or adjust images.
4. Open an audio editor to add background music or effects.
5. Save the document.

Each editor operates on the content types it understands within the document. This replaces monolithic apps (like PowerPoint) that try to be mediocre at everything.

## Economics

Open formats are essential — the OS must champion them, or app lock-in returns through proprietary formats. But open formats don't kill the editor market. They change what editors compete on: quality of editing experience rather than file format captivity.

Precedents that validate this:

- **PDF**: Open standard, every OS can render one, but Adobe still sells Acrobat because the editing tools are powerful.
- **Images**: PNG/JPG are open, everyone views them, but people pay for Photoshop because the editing is worth it. GIMP exists as a free alternative.
- **Source code**: Plain text files. Anyone can open them. People still pay for JetBrains IDEs or choose VS Code or vim based on editor quality.

Smaller, specialized editor developers would benefit from this model — they no longer compete against monolithic apps. A niche audio editor competes on audio editing quality, not on being bundled into a larger suite.

## Historical Attempts

- **OpenDoc** (mid-1990s, Apple/IBM): Component-based document editing. Closest to this vision. Killed in 1997 when Jobs returned to Apple.
- **Microsoft OLE/COM**: Embed and edit objects across apps. Became an interop mechanism, never a paradigm shift.
- **Xerox Star** (1981): Genuinely document-centric. You started with documents, not apps.
- **Plan 9**: Everything-is-a-file philosophy taken to its logical conclusion at the systems level. Never reached mainstream.

## Why Previous Attempts Failed

Likely not because the idea was wrong, but because:

- They tried to be universal, forcing every computing task into the document metaphor.
- 1990s component architectures weren't up to the technical challenge.
- Economic incentives of major OS vendors were aligned with app-centric models.

## Open Questions

- **Composability and coordination**: When a text edit inside a compound document changes layout, who coordinates the reflow across components? This is the hardest technical problem.
- **State management**: Undo history, selection context, tool settings — how do these work when switching between editors on the same document?
- **The right compound document format**: Existing formats (docx, etc.) are overcomplicated because they're snapshots of a specific app's internal state, not true document formats. A new format might be needed — a container with a simple manifest, content in natural per-media-type formats, and an intentionally constrained layout description layer.
- **Scope**: This model covers 80-90% of what most people do (writing, reading, photos, music, presentations, light spreadsheets, browsing). It doesn't need to handle everything — 3D production pipelines and other complex multi-file workflows can remain in app-centric territory.

## Possible Starting Point

Rather than building a full OS, this could start as an application — a document shell that manages files, provides viewers for common types, and lets you attach editors. Prove out the workflow, find where the model breaks, then consider OS-level integration.
