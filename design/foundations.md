# OS Design Foundations

A living document capturing the high-level architecture, beliefs, and decisions for a personal operating system project.

---

## Guiding Beliefs

1. **The OS exists to help you work with your data, not to host apps.** Documents and content are first-class citizens. Applications are tools you attach to content, not containers you put content inside.

2. **Files are real byte streams underneath, but the OS understands what kind of content they are** and can present them without needing a dedicated app.

3. **Viewing is the default; editing is a deliberate second step.** Opening a file shows it. Editing requires an explicit action — like picking up a tool.

4. **The GUI and CLI are both interfaces to the same underlying system.** Neither is "the real one." Both talk to the same services and APIs. The CLI may expose more than the GUI, but nothing in the GUI is inaccessible from the CLI.

5. **Internally opinionated, externally compatible.** The OS can structure data and workflows however it wants internally, but at its boundaries it speaks standard formats (common mimetypes). Nothing is trapped. An audio file created here plays on macOS or Windows.

6. **Simple everywhere. Complexity is a design smell.** The system should be simple at every layer. Essential complexity is pushed into leaf nodes (a PNG decoder, a font shaper) behind simple interfaces — complex inside, simple to use, and the complexity doesn't leak. The connective tissue — protocols, APIs, relationships between components — must be simple. If a system-wide interface is complex, the design isn't finished. When user simplicity and developer simplicity conflict, users win, but the conflict itself signals the design needs more work. But leaf nodes must earn their complexity — total system complexity is conserved, not eliminated by displacement. See "The adaptation layer" under External Boundaries.

7. **Built to learn from, not to ship.** This is a personal project — an exploration of what's possible and what breaks. Decisions should favor clarity and interestingness over market viability.

8. **Everything is a file — but the user doesn't need to know that.** Internally, the OS models all content as files: local documents, email messages, calendar events, chat streams, network connections. This is an architectural principle, not a UX principle. The interface presents domain-appropriate abstractions (documents, conversations, meetings), not files and paths.

9. **File paths are metadata, not the organizing principle.** A file's path is just another attribute — like its creation date or size. Available when useful, but not how users find or organize their work.

---

## Glossary

These terms have specific meanings in this design. They are layered — each builds on the one before it.

**File** — a byte stream at a location on disk. The storage primitive. What the filesystem (e.g., ZFS) manages. Files have no inherent meaning at this layer — meaning comes from the mimetype system above.

**Document** — an independent thing from the user's perspective. Creating, opening, viewing, editing, or deleting a document is a single action because a document is a single conceptual unit. Documents are the user-level concept; files are the storage-level concept. A document is always backed by one or more files, but the user thinks in documents, not files.

**Simple document** — a document that maps 1:1 to a single file with a single mimetype. A PNG image, a markdown file, an MP3 audio clip. The OS renders it directly via the content category viewer for its mimetype.

**Compound document** — a document presented as a single thing to the user, but backed by a manifest file + referenced content files + a layout model. One user-level entity, multiple files underneath. A slideshow (manifest + images + text files + fixed canvas layout) is a compound document.

### Open terminology questions

- **Is "compound" intrinsic or contextual?** A PDF contains text, images, and vector graphics — it feels compound. But it has a single mimetype (`application/pdf`) and the OS can render it as a unit without decomposing it. Is it always compound, or only compound when you crack it open for part-by-part editing? Same question for ZIP archives and mp4 files.
- **Referenced vs owned parts.** In a compound document, are the referenced files independent (deleting the compound leaves them intact) or owned (deleting the compound deletes its parts)? Both cases seem real — a slideshow referencing photos from your library vs. text blocks that exist only within the slideshow.
- **Mimetype relationship.** Simple documents have a mimetype directly. Compound documents have a manifest (with its own mimetype) plus parts with their own mimetypes. Does the compound-as-a-whole have a user-facing mimetype? How do top-level MIME types (image, video, text, application) vs subtypes (image/png, video/mp4) relate to these categories?

---

## External Boundaries (Adopted Standards)

The OS does not exist in a vacuum. It builds on external standards that it adopts rather than reinvents. These are the constraints imposed by external reality.

**Hardware (non-negotiable):**
- arm64 (Apple Silicon). CPU, GPU (Metal), storage (NVMe), display, input (USB HID), networking (WiFi, Ethernet, Bluetooth), boot (UEFI).

**Networking (non-negotiable for internet connectivity):**
- TCP/IP, HTTP/HTTPS, DNS, TLS, WebSocket.

**Text (adopted deeply):**
- Unicode for character encoding. OpenType/TrueType for fonts.

**Content identity (adopted deeply — foundational to the OS model):**
- IANA mimetype registry. The OS's content type system is built on this. See the Decomposition Spectrum for why this is the chosen abstraction boundary.

**Media and file formats (adopted at interop boundary):**
- Image: PNG, JPEG, WebP, SVG, etc.
- Audio: AAC, MP3, FLAC, WAV, etc.
- Video: H.264/H.265, VP9, AV1, etc.
- Document: PDF, HTML/CSS.
- Each format has a decoder/encoder as a leaf node. New format support = new leaf node; the OS doesn't change.

**Other:**
- Cryptography: standard primitives (AES, SHA-256, RSA/EC).
- Time: UTC, IANA timezone database.
- Color: sRGB, Display P3, ICC profiles.

**Explicitly rejected:** POSIX — its filesystem API, process model, and untyped pipes conflict with the document-centric model. See Decision #3 in the decision register.

### Adoption heuristic

When the OS is forced to adopt a pattern at one external boundary, the first instinct should be: use the same pattern internally, if the domains are similar enough. This avoids doing the same thing two different ways. But "similar enough" is the key qualifier — mimetypes generalize well (forced at interop, useful everywhere internally). URIs are useful for web addressing but may not generalize to all internal addressing needs.

### The adaptation layer

Between external reality and the OS's internal model sits an adaptation layer: drivers, decoders, translators, protocol handlers. This is where most "leaf node complexity" (belief #6) lives. It smooths external messiness into internal consistency.

**Total system complexity is conserved.** The external world's complexity is fixed — you can only choose where the adaptation cost lives. Making the core simpler by pushing everything into adapters doesn't reduce complexity; it displaces it. The optimal core isn't the smallest possible core — it's the one where moving anything out would cost more in adaptation complexity than it saves in core simplicity.

The L4 microkernel illustrates the failure mode: a minimal kernel creates a beautifully simple core but explodes the adaptation layer with IPC protocols, serialization, and cross-process error handling. The total system got more complex, not less. A clean core and messy adaptation layer isn't better than a slightly less clean core with a thinner adaptation layer.

**The design metric:** minimize total irregularity across both the core and its adaptation layer, jointly.

---

## Content Model

### The Type System (Three Layers)

The OS understands content through three layers that build on each other:

**Layer 1 — Byte streams (storage).** At the bottom, files are byte streams stored on a conventional filesystem. This preserves Unix-style composability and compatibility. The filesystem (e.g., ZFS) handles storage, integrity, and permissions.

**Layer 2 — Mimetype registry (identity).** Above storage, the OS maintains a registry that maps every file to a mimetype. This is determined by a combination of declared type (metadata/tags) and content detection (inspecting magic bytes), with declaration taking priority and detection as fallback for untagged files. The mimetype tree provides a natural hierarchy: `image/png` is a specific case of `image/*`.

**Layer 3 — Content categories (interaction).** The OS groups mimetypes into a small set of content categories that it natively understands how to present: plain text, rich text, image, audio, video, structured data (tables), and compound documents (containers combining multiple content types). Each category has a built-in viewer. This is the layer that makes the OS feel document-centric rather than app-centric.

The key insight: the byte-stream layer and the structured-understanding layer don't conflict. One is the storage model, the other is the interaction model. Unix got the first one right. The OS builds the second on top without undermining the first.

### The Decomposition Spectrum

Any content type can be decomposed further. A video is frames arranged in time. An image is a grid of pixels. Text is a sequence of code points. Code points are bytes. Bytes are bits. Taken to its logical conclusion, everything decomposes to raw data — and you've reinvented Unix.

This is a real spectrum, not a false dilemma. Unix drew its line at the byte level, aligned with hardware addressing (the CPU addresses bytes, not bits). That line is pragmatic, not mathematically fundamental, but it's anchored to something external and stable — hardware.

This OS draws its line at the **mimetype level**, aligned with the IANA mimetype registry — decades of industry consensus about where meaningful content boundaries are. `image/png`, `video/mp4`, `text/plain` exist because the industry converged on those as useful units of content identity. We didn't invent these boundaries; we take them seriously as an architectural concept.

**The principle:** the OS's content understanding stops decomposing where further decomposition stops being useful to the user. The OS understanding "this is an image" lets it show you the image without a dedicated app. The OS understanding "this is a grid of pixels" doesn't help anyone. Both are valid decompositions; only one earns its keep.

**Compound documents compose at the mimetype level.** Their parts are things that have mimetypes — a slideshow references `image/png` and `text/plain` files, not pixel rows or code point sequences. An atomic content type (a video file, an image, a text file) has internal structure, but that structure is the content type's own concern, not the OS's. A video file's temporal compression, frame dependencies, and codec metadata are properties of `video/mp4`, not a layout model the OS decomposes.

**The `application/octet-stream` escape hatch.** The mimetype system has a bottom: `application/octet-stream` means "unknown bytes." This is the escape hatch back to Unix-level agnosticism. But it's self-penalizing — labeling something `application/octet-stream` opts out of everything the OS provides: no viewer, no editor binding, no content extraction, no meaningful composition in compound documents. You *can* bypass the type system, but you only hurt yourself. The escape hatch exists for genuinely unknown data (e.g., importing a file the OS has never seen), not as a useful alternative path.

**The parallel:** Unix aligned its abstraction boundary with hardware (bytes). This OS aligns its abstraction boundary with an established content identity standard (mimetypes). Both lines are pragmatic but anchored to something external and stable. Both allow going deeper, but doing so stops serving the system's purposes.

### File Organization

The filesystem has rich, queryable metadata. Users find and organize files by querying attributes, not by navigating directory trees. Metadata comes from three sources: automatic (dates, size, mimetype), content extraction (EXIF, ID3, embedded document metadata), and user-applied (arbitrary key-value attributes and tags).

All metadata is queryable through a simple system API (equality, comparison, AND/OR on attributes), backed by an embedded database engine as a leaf node. Power users can access raw SQL as an escape hatch.

Paths exist because the underlying storage requires them, but they are just another attribute — like creation date or file size. Available when useful, not the organizing principle.

### Mimetypes as the External API

Standard mimetypes serve as the interoperability boundary between this OS and the outside world. Internally, the OS can represent and organize data however it wants. When importing or exporting, it translates to/from common formats. This gives freedom without isolation.

---

## Viewing and Editing

### Viewer-First Design

Opening any file presents it using the OS's built-in viewer for that content category. The viewer is always the OS's own — it provides a consistent rendering experience regardless of what editors are installed.

### Editor Augmentation Model

When the user chooses to edit, the **viewer stays**. The editor augments the view by adding tools and intercepting modification input, rather than replacing the viewer with its own UI.

The OS renderer is a **pure function of state**: file bytes + mimetype + view state → visual output. No side effects, no accumulated state. When an operation is committed, the file changes, and the renderer produces a new output from the new state.

**The desktop analogy:** A document is on your desk, open to a page. You pick up a pen — you can write where you're looking. Put it down, pick up a highlighter — you can highlight where you're looking. Put down all tools — you can still look and flip pages. Where you are in the document is independent of which tool you hold.

**Input routing:**
- Navigation (scroll, page, cursor movement) → always the OS, with or without an editor attached
- Modification (keystrokes, brush strokes) → the active editor, which issues operations through the edit protocol
- No editor attached → modification input ignored or handled as OS shortcuts

**OS-provided interaction primitives** shared across all editors of a content type: cursor positioning and text selection (text), selection regions (images), playhead (audio/video). These are part of the OS's content-type understanding, not editor-specific.

**No pending changes — edits are immediately durable.** There is no separate "working state" and "persisted state." When an editor issues an operation, the OS applies it to the file immediately. The file on disk is always current. There is no "save" action — every edit is durable the moment it happens. The COW filesystem makes this cheap (only changed blocks are written) and reversible (previous versions are retained as snapshots). This eliminates "unsaved changes," "save before closing?" dialogs, and the entire class of data-loss bugs from crashes before saving.

**Editor overlays:** Editors can draw temporary visual chrome — crop bounds, selection highlights, tool cursors — but these are tool UI, not document content. They never affect the file.

### Editor-to-Content Binding

Editors declare which mimetypes (or mimetype patterns) they operate on. Each editor brings its own set of tools for that content type. Dispatch follows mimetype specificity:

- A `text/xml` editor takes priority over a general `text/*` editor for XML files.
- An `image/*` editor handles any image type.
- If multiple editors match at the same specificity, the user can choose.

Editors can be narrow (an XML-specific editor for `text/xml`) or broad (a media editor that handles both `image/*` and `video/*` for shared operations like cropping or color adjustment).

### The Edit Protocol

Editors don't own files directly. They issue **operations** through a protocol. The OS mediates between editors and data.

**Tools are modal.** Only one editor is active on a document at a time — the "pen on the desk" metaphor. You put one tool down before picking up another. This eliminates concurrent-editor composition as a protocol concern and makes the operation log a simple sequential list.

**The protocol is thin:**
- Editor calls `beginOperation(document, description)`.
- Editor modifies the file through OS file APIs.
- Editor calls `endOperation()`.
- The OS snapshots at operation boundaries (COW filesystem makes this cheap).
- The operation log records: which editor, when, which document, human-readable description.

The OS is logistics — it doesn't understand what operations mean, it just tracks boundaries, ordering, and attribution. This keeps the protocol as simple connective tissue.

**Undo is global, not per-editor.** The OS walks backward through the operation log regardless of which editor produced each operation. This matches the user's mental model: "undo the last thing I did." The COW filesystem restores the previous version. The originating editor does not need to be active for undo to work.

**Content-type handlers (optional, for advanced features).** Sequential undo works for all content types with zero additional machinery. For selective undo (undo operation A while keeping later operations B and C) and future collaboration, content-type handlers provide rebase logic — the ability to adjust operations when earlier operations are removed or concurrent operations arrive. These handlers are leaf nodes (complex inside, simple interface), analogous to how git understands text merging. Text rebase is a solved problem (OT/CRDTs). Audio and video (1D time-axis content) are structurally similar to text. Image operations (2D regions) are less battle-tested but tractable. Content types without a rebase handler gracefully degrade to sequential-only undo.

**Cross-content-type interactions are layout's job, not the edit protocol's.** When resizing an image causes text to reflow in a compound document, the layout engine handles that — not the image editor or the text content-type handler. The edit protocol only needs to handle same-type, same-region operation conflicts.

---

## Undo, History, and (Future) Collaboration

### Sequential Undo (Base Case)

Every edit operation is recorded in an ordered log. Undo and redo walk this log backward and forward. Because edits are immediately durable on a COW filesystem, each operation boundary is a snapshot. Undo = restore the file to the previous snapshot. This works for all content types with zero additional machinery.

Undo is global — the OS undoes the most recent operation regardless of which editor produced it. The originating editor does not need to be active.

### Selective Undo (Upgrade Path)

Selectively undoing an earlier operation while keeping later ones requires **rebasing** — adjusting later operations to account for the removal. This is only possible when a content-type handler provides rebase logic:

- **Text:** Solved problem. OT and CRDTs handle positional rebasing (git merge is a simpler version of this).
- **Audio/Video:** Structurally similar to text — 1D sequence along a time axis. The same rebase principles apply with different domain primitives.
- **Images:** 2D region-based operations. Less battle-tested but tractable — two operations on non-overlapping regions are independent; overlapping regions conflict.
- **No handler:** Graceful degradation to sequential-only undo.

### Cross-Session History

File history is provided by the COW filesystem's snapshot retention. "Show me this document as it was last Tuesday" is a filesystem query, not an application feature. The operation log handles fine-grained in-session undo; the filesystem handles long-term history.

### Collaboration-Ready Architecture

The architecture supports future multi-user collaboration because:
1. The operation log captures every change with attribution and ordering.
2. Content-type handlers with rebase logic can resolve concurrent edits (the same machinery needed for selective undo).
3. Cross-type conflicts are mediated by the layout engine, not the edit protocol.

The networking and conflict-resolution layers are deferred. Collaboration and selective undo require the same investment (content-type rebase handlers), so building one unlocks both. The system is built for one user first, with the structural capacity to grow.

---

## Compound Documents

Compound documents are not special file types — they are a small number of **layout models** applied to simple content types. A slideshow is "fixed canvas layout + text + images." A word document is "flow layout + text + images." A video project is "timeline layout + video + audio."

### Layout Models

The OS provides a small set of fundamental layout models:

- **Flow** — content reflows when things change. Text wraps around embedded objects. Documents, articles, emails, web pages, ebooks.
- **Fixed canvas** — objects at specific positions on fixed-size pages. Presentations, posters, flyers, PDFs.
- **Timeline** — content arranged along a time axis with synchronized tracks. Video editing, audio production, animation.
- **Grid** — rows and columns with content in cells. Spreadsheets, dashboards, data reports.
- **Freeform canvas** — arbitrary positioning on an unbounded surface. Whiteboards, design tools, mind maps.

If the rendering technology is a web engine, CSS already provides flow, fixed, and grid natively.

### Structure

A compound document is a **manifest** (metadata file) that references:
- Content files (real, independent files — a PNG, a text file, an audio clip)
- A layout model (one of the five above)
- Arrangement rules (positions, sizes, flow constraints, track synchronization)

The content files are real files in the filesystem, queryable and usable independently. The manifest describes how they're arranged. "Resize image in a slideshow" is a layout operation (changing the manifest's arrangement rules), not an image operation — the image bytes don't change.

### Layout as Cross-Type Mediator

The OS's layout engine handles cross-content-type interactions:
- Resize an image → text reflows around it (flow layout engine responds)
- Remove a time range in a video → corresponding audio is trimmed (timeline layout enforces track synchronization)
- Reorder slides → sequence updates (canvas layout adjusts)

This means cross-type operations are never the edit protocol's problem. The layout engine mediates. Only same-type, same-region conflicts need content-type rebase handlers.

### Interop

At boundaries, **translators** convert between the OS's internal representation and external formats:
- Import .pptx → extract content as individual files, generate manifest with fixed canvas layout
- Export to .pptx → read manifest + referenced files, pack into pptx structure
- Import .docx → extract content, generate manifest with flow layout
- Export to .html → nearly trivial if rendering is already web-based

Each translator is a leaf node — complex inside (parsing docx is genuinely hard), simple interface. New format support = new translator; the OS doesn't change. Translation is inherently lossy (some features won't map between formats), which is true of all format conversion.

---

## Open Questions (To Be Resolved)

### Technical Stack
The beliefs and content model above should guide these choices, but they haven't been made yet:

- Kernel architecture (microkernel vs. monolithic)
- POSIX compatibility (full, partial, or clean break)
- Binary format and ABI
- Implementation language(s)
- Display server / compositor
- IPC mechanism (especially important given the editor protocol)

### Interaction Model
The GUI's look, feel, and navigation are not yet defined:

- Windowed, fullscreen-per-workspace, tiling, or something else?
- How does the user navigate between open documents?
- What does "launching" something look like in a system with no app launcher?
- How do tags and queries surface in the GUI?

---

## Audience, Goals, and Scope

**Audience:** Personal design project.

**Primary artifact:** A coherent, complete OS design. Implementation is selective — build to validate uncertain assumptions, stub or use off-the-shelf for the rest.

**Success criteria (in priority order):**
1. **Coherent design** — the design documents are thorough and defensible; when you pull on one thread, the whole thing holds together
2. **Working prototype** — the most uncertain or interesting parts of the design are proven out with real code
3. **Deep learning** — the designer understands OS design deeply through the process

**Non-goal:** A daily driver. This OS does not need to replace the designer's actual computing environment.

**Target use cases:** Personal workstation. Everything a single person does at a desktop computer for creative and knowledge work:
- Text documents (reading, writing, editing)
- Images (viewing, editing)
- Audio (listening, editing)
- Video (watching, editing)
- Email
- Calendar
- Messaging (chat, Slack-like)
- Videoconferencing (edge case — live sessions, not documents)
- Web browsing
- Coding and development

All content is modeled internally as files (local or mounted from remote services), following the Plan 9 philosophy. Not targeting mobile devices or servers initially.

**Comparable prior art:** BeOS — designed for creative professionals doing media work on a personal machine.

**Prototype scope:** The prototype needs depth, not breadth. If the system can view and simply edit text, view and rotate an image, and demonstrate that the concept works and scales to the full use case list, that is success. The design documents carry the breadth; the prototype proves the architecture.

**Development model:** The OS is developed on a host OS (macOS). There is no self-hosting goal. The prototype does not need to reimagine dev tools — it needs to prove out the novel parts of the design (file addressing, editor protocol, viewer framework, type-aware shell).

---

## Influences and References

- **Mercury OS** (Jason Yuan) — speculative vision of an OS with no apps or folders, assembling content and actions fluidly based on user intention.
- **Ideal OS** (Josh Marinacci) — argument that desktop OSes are bloated with legacy cruft and should be rebuilt from scratch, learning from past lessons.
- **OpenDoc** (Apple/IBM, mid-1990s) — component-based document editing where editors were embedded parts, not standalone apps. Closest historical attempt at this document-centric model. Failed due to technical limitations of the era and economic headwinds.
- **Xerox Star** (1981) — genuinely document-centric desktop. Users started with documents, not apps.
- **Plan 9** (Bell Labs) — everything-is-a-file philosophy taken to its logical conclusion. Radical simplicity at the systems level.
- **BeOS / BFS** — rich queryable metadata built into the filesystem, enabling attribute-based file discovery rather than path-based navigation.
