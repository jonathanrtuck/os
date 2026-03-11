# OS Design — Decision Register

This document tracks every design decision — settled, tentative, and abandoned. It exists to map the decision space, clarify dependencies, and lay out tradeoffs honestly.

**Exploration methodology:** Design decisions are iterative, like evaluating chess moves. A tentative position is adopted and its consequences explored. If the lines work out, it's confirmed. If they reveal problems, it's revised or abandoned. Each decision records not just the current position but also **considered and rejected** alternatives with reasoning, so we don't revisit discredited lines. Settled decisions can be reopened if new information warrants it, but the bar is high — the rejection reasoning must be addressed, not just ignored.

---

## Implementation Readiness

Which decisions are stable enough to write code against? This guides when to code vs. when to keep designing.

| Decision               | Status    | Readiness            | Notes                                                                                                                              |
| ---------------------- | --------- | -------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| #1 Audience & Goals    | Settled   | N/A                  | Meta-decision, not directly implementable                                                                                          |
| #2 Data Model          | Settled   | **Safe**             | The axiom. Everything flows from this.                                                                                             |
| #3 Compatibility       | Settled   | **Safe**             | No POSIX. Standard interfaces only. Clear constraints.                                                                             |
| #4 Complexity          | Settled   | N/A                  | Design principle, not directly implementable                                                                                       |
| #5 File Understanding  | Settled   | **Behind interface** | Mimetype registry concept is firm. Storage mechanism depends on §16.                                                               |
| #6 View vs Edit        | Settled   | **Behind interface** | Concept is firm. Concrete API depends on §11 (rendering) and §16 (tech foundation).                                                |
| #7 File Organization   | Settled   | **Behind interface** | Query model is firm. Can prototype the API shape. Storage backend depends on §16.                                                  |
| #8 Editor Model        | Settled   | **Behind interface** | Architecture is firm. Plugin API depends on §11 and §16.                                                                           |
| #9 Edit Protocol       | Settled   | **Behind interface** | Protocol shape is firm. IPC mechanism depends on §16.                                                                              |
| #10 View State         | Unsettled | **Not safe**         | Leaning toward opaque blobs, but not committed.                                                                                    |
| #11 Rendering Tech     | Settled   | **Behind interface** | Architecture firm (web engine as substrate, adaptation layer). Engine choice deferred to prototype.                                |
| #12 Undo & History     | Settled   | **Behind interface** | Depends on COW filesystem choice (§16). Concept is firm.                                                                           |
| #13 Collaboration      | Settled   | **Not safe**         | "Design for, build later." Nothing to implement yet.                                                                               |
| #14 Compound Documents | Settled   | **Behind interface** | Uniform manifest model + three-axis relationships (spatial/temporal/logical). Rendering depends on §11. Open sub-questions remain. |
| #15 Layout Engine      | Unsettled | **Not safe**         | Depends on §11 (rendering technology).                                                                                             |
| #16 Tech Foundation    | Partial   | **Partially safe**   | Most sub-decisions settled (incl. driver model, filesystem placement). Remaining: filesystem COW on-disk design.                   |
| #17 Interaction Model  | Exploring | **Not safe**         | Shell placement leaning (blue-layer, pluggable). Compound editing model unresolved. Nothing settled yet.                           |

**Readiness key:**

- **Safe** — This won't change. Code freely.
- **Behind interface** — Concept is settled, but implementation details depend on unsettled decisions. Code against an abstraction; expect the concrete implementation to change.
- **Not safe** — Unsettled or depends on unsettled decisions. Design only, or research spikes.

## Reversibility & Risk

How confident are we in each settled decision? What would trigger revisiting? What's the fallback? Only lists decisions where risk is meaningful — axioms and their direct consequences are omitted.

| Decision                                  | Confidence  | Revisit trigger                                     | Fallback                            | Blast radius                      |
| ----------------------------------------- | ----------- | --------------------------------------------------- | ----------------------------------- | --------------------------------- |
| Edit protocol (#9)                        | High        | beginOp/endOp granularity wrong for real editors    | Adjust boundary semantics           | Undo model, IPC messages          |
| Compound docs (#14)                       | Medium-High | Five layout models miss a real use case             | Add or merge models                 | Layout engine                     |
| Undo: COW snapshots (#12)                 | High        | Snapshots too expensive for fine-grained ops        | Operation-log undo                  | Filesystem integration            |
| File org: queries (#7)                    | High        | Query performance unacceptable                      | Path-based fallback                 | Metadata DB, shell                |
| Handles (#16)                             | High        | Need sub-document access granularity                | Extend rights model                 | Handle table, access control      |
| IPC: ring buffers (#16)                   | Medium-High | Complexity unmanageable or perf insufficient        | Syscall-based message passing       | IPC layer, editor-OS interface    |
| IPC: fixed 64-byte messages (#16)         | Medium-High | Control messages regularly exceed 60-byte payload   | Variable-size messages              | ipc library, all message types    |
| IPC: separate pages per direction (#16)   | High        | Memory pressure from 2 pages per channel            | Single page split in half           | Kernel channel code, ipc library  |
| IPC: one mechanism (no config path) (#16) | High        | Ring buffer overhead unacceptable for simple config | Separate config struct mechanism    | Init, all services                |
| Process arch: one OS service (#16)        | Medium      | Crashes require component isolation                 | Split into multiple services        | IPC topology                      |
| From-scratch kernel (#16)                 | Medium      | Driver/hardware blockers                            | Existing kernel (Zircon, Linux)     | Large, but behind syscall API     |
| Rust (#16)                                | High        | Bare-metal Rust impractical at scale                | C kernel, Rust userspace            | Kernel source only                |
| Scheduling: EEVDF + contexts (#16)        | Medium-High | EEVDF overhead excessive or contexts too complex    | Priority scheduler + no billing     | Scheduler, handle table           |
| Rendering: web engine substrate (#11)     | Medium-High | Engine integration proves impossible/impractical    | Different engine or native renderer | Layout engine, prototype approach |

**Key principle:** Decisions marked "Behind interface" in Implementation Readiness are inherently lower risk — the interface is stable even if the implementation changes. Decisions that _define_ interfaces are higher risk because changing them ripples outward.

---

The decisions are color-coded by tier in the accompanying diagram:

- **Tier 0 (dark):** Foundational — everything flows from here
- **Tier 1 (purple):** Core philosophy — shapes the system's character
- **Tier 2 (blue):** Structural model — how the pieces relate
- **Tier 3 (green):** Mechanisms — how things work
- **Tier 4 (amber):** Dependent design — built on top of earlier decisions
- **Tier 5 (red):** Implementation — informed by everything above

---

## 1. Audience & Goals

_Tier 0. Affects everything._ **SETTLED.**

**Decision:** Personal design project. The primary artifact is a coherent, complete OS design. Implementation is selective — build to validate uncertain assumptions, stub or use off-the-shelf for the rest.

**Success criteria (in priority order):**

1. Coherent design — thorough, defensible, internally consistent
2. Working prototype — prove out the uncertain parts with real code
3. Deep learning — understand OS design deeply through the process

**Non-goal:** Daily driver. Not replacing actual computing environment.

**Target use cases:** Personal workstation for creative and knowledge work — text, images, audio, video, email, calendar, messaging, videoconferencing, web browsing, coding. All content modeled internally as files (Plan 9 philosophy). Desktop only, not mobile or server.

**Key constraints adopted:**

- Personal project freedom, but with design-rigor discipline (documents must be defensible, not hand-waved)
- Everything-is-files is architectural, not UX — the interface presents domain abstractions, not files and paths
- File paths are metadata (like creation date), not the organizing principle
- The GUI and CLI are equally fundamental OS interfaces, not applications

---

## 2. Data Model

_Tier 1. Depends on: Goals. Feeds into: File understanding, view/edit, organization, interaction model._ **SETTLED.**

**Decision:** Document-centric. This is the main axiom of the entire design.

OS → Document → Tool. Documents have independent identity. Tools are interchangeable. The OS manages content directly; applications are tools you attach to content, not containers you put content inside.

This is the most consequential design decision. It cascades through the file model, editor architecture, compound documents, the GUI, and how users think about their work. Nearly every downstream decision is constrained (in useful ways) by this choice.

---

## 3. Compatibility Stance

_Tier 1. Depends on: Goals. Feeds into: Technical foundation._ **SETTLED.**

**Decision:** Rethink everything. Build on established standard _interfaces_, not existing _implementations_. No POSIX. The OS defines its own native APIs designed to serve the document-centric model from the ground up. Development happens on a host OS (macOS). Self-hosting is not a goal.

**Established standards adopted as interfaces:**

- Mimetypes (content identity)
- URIs (resource addressing)
- HTTP (network protocol)
- Unicode (text encoding)
- Standard file formats (PNG, PDF, etc.)
- Hardware target (arm64)
- Possibly ZFS or similar (storage, if not designing a custom FS)

**The principle:** Reuse of interfaces >> reuse of implementations. Standards let you focus on rethinking the _relationships between_ components and welding a cohesive system, rather than solving problems that would take lifetimes to address independently.

**Why not POSIX:** The POSIX filesystem API is path-addressed (`open("/path/to/file")`), which directly conflicts with the tag/query-based organization model. The POSIX process model has no concept of editor sessions or document attachment. POSIX pipes are untyped byte streams, but this OS has typed content. Adopting POSIX would mean building the interesting parts of the OS _on top of_ a layer that actively pulls in the wrong direction, then fighting the seams forever.

**Why this is tractable:** The prototype only needs native components (viewers, editors, shell, file query interface) — not reimplementations of POSIX tools. There is no need for `grep`, `git`, or `curl` running _inside_ the OS. Development uses host OS tools. The list of things to build is exactly the list of novel things worth designing.

---

## 4. Complexity Philosophy

_Tier 1. Depends on: Goals. Feeds into: Editor model, technical foundation._ **SETTLED.**

**Decision:** Simple everywhere is the goal. Complexity at any layer is a design smell — an indication the design hasn't been fully resolved. When conflicts between user simplicity and developer simplicity arise, users win, but the conflict itself should be interrogated.

Essential complexity goes into **leaf nodes** behind simple interfaces. A PNG decoder, a font shaper, a Unicode implementation — these are complex inside but expose simple interfaces ("decode these bytes into pixels"). That's fine. The complexity doesn't leak.

The **connective tissue** — protocols, APIs, and relationships between components — must be simple. If a system-wide interface (like the edit protocol or file addressing API) is complex, the design isn't done yet.

**The test:** Complex leaf node behind a simple interface = OK. Complex system-level interface that everything touches = design smell.

**Implication for downstream decisions:** The edit protocol should have a small number of composable primitives, not a complex per-mimetype schema. The file API should have a simple, composable query model, not a complex query language. Ambitious functionality achieved through genuinely simple designs, not complex machinery hidden behind a clean surface.

---

## 5. File Understanding

_Tier 2. Depends on: Data model. Feeds into: Editor model, compound documents._ **SETTLED.**

**Decision:** The OS natively understands content types. Every file has an OS-managed mimetype as fundamental metadata (like size or creation date), not a userspace convention that applications may or may not honor. This is what enables the document-centric model — the OS can view, organize, and dispatch editors for any file because it knows what the file _is_.

This differs from Unix, where the OS is genuinely agnostic about file contents. Extensions are just filename characters, and all type awareness (Launch Services, desktop environment MIME databases, the `file` command) is bolted on in userspace — inconsistent, fragile, and optional. This OS makes type awareness native and authoritative.

**Type assignment:** Types are declared at creation (by user, editor, or creation context) with content detection as fallback for imported/untagged files. Unknown content gets a generic type (`application/octet-stream`), never rejected.

**Type mutability:** A document's type can change over time as its content evolves (e.g., plain text → markdown → compound document). The mechanism for this (mutable mimetype vs. new file) is a lower-level implementation detail to be resolved later.

**Interop:** No issues. Files use standard formats — a PNG is a PNG regardless of metadata. On import, the OS assigns a mimetype from available hints (Content-Type headers, extensions, content detection). On export, the file's bytes are fully usable on any other OS; the OS-managed mimetype metadata doesn't travel, but doesn't need to — receiving systems determine types the same way they always do.

---

## 6. View vs Edit Distinction

_Tier 2. Depends on: Data model. Feeds into: Editor model._ **SETTLED.**

**Decision:** View is default, edit is deliberate. The OS provides viewers for all content types natively. Editing requires explicitly attaching a tool.

This applies to all content, including live and streaming content:

- A **text document** is viewed (read) by default; attach a text editor to modify it.
- A **chat conversation** is a collaborative document — viewing is reading the history, editing is adding messages. The text editor used here could be the same one used for markdown or XML — editors operate on content types, not use cases.
- A **video stream** (including videoconference) is content you view (watch/listen) or edit (contribute your own video/audio feed).

The OS's own interfaces (GUI and CLI) are **not** documents and the view/edit distinction does not apply to them.

**Friction mitigation:** The OS can remember that a document has an editor attached and restore that state. The deliberate step is attaching an editor the first time; switching away and back doesn't require re-attaching.

**Implication for editors:** Editors may be more generic than expected — a text editor operates on text content regardless of whether the context is a standalone document, a chat message, or an email reply. Editors bind to content types, not use cases.

---

## 7. File Organization

_Tier 2. Depends on: Data model. Feeds into: Interaction model._ **SETTLED.**

**Decision:** The filesystem has rich, queryable metadata. Users navigate by query, not by path. Paths exist as metadata (like creation date or size), not as the organizing principle.

**Metadata sources:**

- **Automatic** — dates, size, mimetype (OS-managed)
- **Content extraction** — EXIF, ID3, document metadata (extracted by leaf-node parsers)
- **User-applied** — arbitrary key-value attributes and tags

All metadata is queryable through the same interface.

**Query model:** The system API exposes a simple query interface (equality, comparison, AND/OR on attributes). Backed by an embedded database engine (like SQLite) as a leaf node — complexity contained behind a simple interface. Power users and advanced tools can access raw SQL as an escape hatch.

**Historical context:** WinFS (Microsoft, cancelled 2006) attempted this and failed due to performance overhead, backward compatibility burden, and universal scope. BeOS/BFS succeeded at a smaller scale. This project avoids WinFS's failure modes: no backward compatibility requirement, no need to handle every edge case, designed around rich metadata from the start.

**Open question for Tier 5:** Is the database _the_ filesystem, or a metadata index _over_ a conventional filesystem? Design-level answer is "rich queryable metadata" regardless of implementation.

---

## 8. Editor Model

_Tier 2. Depends on: File understanding, view/edit distinction, complexity philosophy. Feeds into: Edit protocol, view state, rendering technology._ **SETTLED.**

**Decision:** Editors are plugins that augment the OS's viewer. The OS always renders the document as a pure function of state (file bytes + mimetype + view state → visual output). Editors add tool-specific UI and intercept modification input, but never replace the rendering.

**The desktop analogy:** A document is on your desk, open to a page. You pick up a pen — you can now write where you're looking. Put it down, pick up a highlighter — you can now highlight where you're looking. Put down all tools — you can still look at the page and flip pages, you just can't change anything. Where you are in the document is unrelated to which tool you're holding.

**Input handling:**

- **Navigation input** (scroll, page, move cursor/focus) → always handled by the OS. Works with or without an editor attached.
- **Modification input** (keystrokes while editing, brush strokes) → goes to the active editor, which interprets it as an operation and issues it through the edit protocol. The OS applies the operation and re-renders.
- **No editor attached** → modification input is ignored or handled as OS-level shortcuts.

**OS-provided interaction primitives** (shared across all editors of that content type):

- Text: cursor positioning, text selection
- Images: selection regions
- Audio/video: playhead position
- These are part of the OS's understanding of the content type, not editor-specific.

**Tools are modal.** One active editor per document at a time. You put one tool down before picking up another. When switching editors, the new editor reads current file state. This eliminates concurrent-editor composition as a design concern for the edit protocol.

**No pending changes — edits are immediately durable.** There is no separate "working state" and "persisted state." When an editor issues an operation, the file on disk is updated immediately. The COW filesystem makes this cheap (only changed blocks written) and reversible (previous versions retained as snapshots). There is no "save" action. This eliminates "unsaved changes," save-before-closing dialogs, and data-loss bugs from crashes.

**Editor overlays:** Editors can still draw temporary visual chrome — crop bounds, selection highlights, tool cursors, filter previews — but these are tool UI, not document content. They never affect the file. When the tool is put down, the overlay disappears.

**The game engine analogy:** The OS is like a game engine — it owns the world state (file on disk), the rendering pipeline, and versioning (COW snapshots). Editors are like players: they interact through clearly defined input interfaces (the edit protocol) to describe changes. Those changes update the game state, which causes the engine to re-render. Players never write pixels directly; the engine always mediates.

**One way to do things:** There is one path for editors to interact with the system — operations through the edit protocol, rendering by the OS, persistence by the OS. No alternative rendering or save paths. Providing two ways to do the same thing adds complexity without capability.

**Open questions for lower tiers:**

- Rendering technology: how does the OS actually render? (Tier 3)
- Plugin API design: what's the interface between editor and OS? (Tier 3)
- Relationship between OS and web engine, if one is used (Tier 3)

---

## 9. Edit Protocol

_Tier 3. Depends on: Editor model. Feeds into: Undo/history, collaboration, compound documents._ **SETTLED.**

**Decision:** Modal tools with immediate writes. The OS provides a thin operation-boundary protocol; the filesystem (COW) handles versioning and undo.

**The protocol:**

- Editor calls `beginOperation(document, description)`.
- Editor modifies the file through OS file APIs.
- Editor calls `endOperation()`.
- The OS snapshots at operation boundaries (COW makes this cheap).
- The operation log records: which editor, when, which document, description.

**Key commitments:**

- **Tools are modal.** One active editor per document at a time. The "pen on desk" metaphor — put one down, pick up another. This eliminates concurrent-editor composition as a protocol concern.
- **No pending changes.** Operations write to the file immediately. There is no working state vs persisted state. No "save." Every edit is durable the moment it happens.
- **Undo is global and sequential.** The OS walks the operation log backward regardless of which editor produced each operation. COW filesystem restores the previous version. The originating editor doesn't need to be active.
- **The OS is semantically ignorant.** It doesn't understand what operations mean — just tracks boundaries, ordering, and attribution. Simple connective tissue.

**Upgrade path for selective undo and collaboration:** Content-type handlers (leaf nodes) can optionally provide rebase logic — adjusting later operations when earlier ones are removed or concurrent operations arrive. This is the same machinery needed for both selective undo and multi-user collaboration. Text rebasing is a solved problem (OT/CRDTs). Audio/video (1D time-axis) are structurally similar. Images (2D regions) are less proven but tractable. Without a handler, a content type gracefully degrades to sequential-only undo.

**Cross-type interactions are layout's job.** Resizing an image that causes text reflow is handled by the layout engine, not the edit protocol. The protocol only handles same-type, same-region conflicts.

**Considered and rejected:**

- **Operation-based protocol** (OS defines operation types per content type): Hit the "Photoshop problem" — the OS can't anticipate all operations for all content types. Grows unboundedly. Rejected because it makes the connective tissue complex.
- **Direct mutation** (editors modify files directly, no protocol): Simplest possible approach, but forecloses collaboration and makes undo editor-dependent. Rejected because it blocks the upgrade path.
- **Concurrent multi-editor composition** (multiple editors active on one document): Explored early, but creates massive protocol complexity — editors must coordinate, operations must be merged. Rejected when the "pen on desk" metaphor revealed that modal tools are the natural model and eliminate this entire problem class.

**Key insight preserved:** Cross-tool composition and cross-user collaboration are the same problem (both require rebaseable operations). Modal tools defer the composition problem; content-type handlers solve it incrementally when needed.

---

## 10. View State

_Tier 3. Depends on: Editor model. Feeds into: (relatively self-contained)._

| Option                                                           | Tradeoffs                                                                                                                                                                    |
| ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Per-tool opaque blobs with shared convention**                 | Flexible, extensible, no OS schema to maintain. Tools that follow the convention get seamless transitions. But convention compliance is voluntary — fragmentation is likely. |
| **OS-defined minimal state per mimetype**                        | Consistent. Every tool knows what to expect. But the OS has to define and maintain schemas, and edge cases will be missed.                                                   |
| **No view state persistence**                                    | Simplest. Tools always open fresh. May be fine for a personal OS. But the "pick up a pen" metaphor breaks — you lose your place.                                             |
| **View state as document state** (everything is a file mutation) | Philosophically pure. But performance problems (scrolling = file writes), polluted undo history, and shared-reference conflicts make it impractical.                         |

**Initial leaning:** Per-tool opaque blobs with shared convention.
**Why this matters:** Relatively contained decision. Mainly affects the UX of switching between tools.

---

## 11. Rendering Technology

_Tier 3. Depends on: Editor model. Feeds into: Compound documents, layout engine, technical foundation, interaction model._ **SETTLED.**

**Decision:** An existing web engine is part of the rendering architecture, integrated via an adaptation layer. The exact role of the engine — whether it's the rendering substrate (compound documents translate outward to HTML/CSS for display) or a parser/interpreter (web content translates inward to the compound document format, rendered natively) — is an open sub-question. Both directions are viable; both use the existing translator pattern (Decision #14). Prototype on macOS (where web engines and native rendering are both available); the bare-metal kernel needs correct interfaces, not a built-in engine.

**Key insight: a webpage is a compound document.** The compound document model (Decision #14) maps structurally to web content. HTML is the manifest with layout rules, CSS provides layout (flow, grid, fixed positioning — covering 4 of 5 fundamental layouts), and images/video/fonts are referenced content. This structural equivalence means web content can be handled through the same translator pattern as .docx or .pptx — translated into the internal compound document representation at the boundary. "Browsing" is viewing HTML documents through the same rendering path as any other compound document.

**The adaptation layer (red/blue/black):**

- **Red (external reality):** The web platform — HTML, CSS, JavaScript. Reasonable coverage of common features is needed — enough that missing support would be noticeable in normal browsing.
- **Blue (adaptation layer):** A web engine (Servo, WebKit, CEF, or future option) adapted to speak the OS's interfaces. This is where engine complexity lives. The blue layer is large, and that's acceptable: total complexity is conserved (Decision #4), and this is the adaptation layer earning its keep.
- **Black (OS core):** The OS service, compound document model, layout engine, document-centric interfaces. Clean, simple, designed from first principles.

**What this design uniquely enables:**

- **Unified compositing** — One compositor (OS service) renders all content through one pipeline, regardless of origin content type.
- **Content-type-aware rendering budgets** — Scheduling contexts (Decision #16) give the renderer content-type-informed time budgets. The OS knows a tab is playing video vs. sitting idle.
- **Cross-content-type embedding** — A web page inside a document inside a presentation, with the OS mediating all rendering and layout.
- **Metadata extraction from web** — The OS natively indexes content types (Decision #7). Web pages have rich metadata (OpenGraph, structured data) that becomes OS-level queryable content, not trapped in a browser's history database.

**The renderer/driver asymmetry:** Rendering and drivers face opposite constraints under the "rethink everything" stance (Decision #3). Drivers need narrow scope (just my hardware), each is a bounded problem, and first-principles design is an advantage. Rendering needs broad scope (reasonable web feature coverage), can't feasibly be built from scratch, and must accommodate external reality. The adaptation layer resolves this: push engine complexity into the blue layer, keep the OS core clean.

**Prototype strategy:** Design the architecture and interfaces on the bare-metal kernel. Prototype the rendering pipeline on macOS, where both web engines and native rendering frameworks (Quartz/Core Graphics) are available. Development on host OS is already the working mode (Decision #1). Self-hosting is not a goal. The bare-metal kernel validates the architecture (process model, IPC, scheduling); the macOS prototype validates the rendering integration.

**Open sub-questions:**

- **Rendering direction.** Two viable approaches, leaning toward B:
  - **(A) Web engine as rendering substrate:** The OS uses a web engine to render everything. Compound documents translate outward to HTML/CSS for display. Gets CSS layout for free. But the web engine owns the rendering pipeline, creating tension with "OS renders everything." The OS can only do what the engine supports — custom rendering behavior means patching the engine or hoping for extension points. The OS is downstream of the engine's architectural decisions.
  - **(B) Native renderer, web translated inward:** The OS has its own renderer (Quartz-like). HTML/CSS/JS is translated into the compound document format at the boundary, just like .docx or .pptx (Decision #14's translator pattern). "OS renders everything" holds cleanly. The OS defines what's possible — the native renderer can express layout behaviors, compositing effects, and content-type-specific rendering beyond what CSS describes. Web translation is a lossy import (maps what it can, same as any format translator). Requires building a native rendering pipeline.
  - A hybrid is also possible (e.g., web engine for layout calculation, native renderer for compositing).
  - **Why leaning B:** The compound document model (five layouts, manifests, referenced content) is the internal truth. External formats — .docx, .pptx, .html — are translations inward at the boundary. The OS doesn't think in HTML any more than it thinks in .docx. Approach B preserves this: the OS owns the rendering model, and the renderer can do things CSS can't express (analogous to how Safari adds proprietary CSS extensions, except here the OS isn't constrained by a web engine's architecture at all). Approach A inverts the power relationship — the OS must express everything through the engine's model, making the engine the de facto rendering authority.
- **Which engine?** Servo (Rust, embeddable, incomplete), WebKit (lighter, macOS-native for prototyping), Chromium/CEF (mature, enormous), or something else. Engine choice is partially coupled with the rendering direction — Approach A needs a full rendering engine; Approach B may only need a parser/layout engine. Deferred to prototype phase.
- **The protocol between engine and OS service.** Depends on rendering direction. Under Approach A, this is the rendering API. Under Approach B, this is the translation interface. Either way, significant design work. Related to the overlay protocol and editor plugin API (see journal).
- **GPU acceleration.** How does rendering integrate with GPU hardware? On macOS this may be straightforward (Metal); on bare-metal this is a harder problem (needs GPU driver).

**Considered and rejected:**

- **Embedded web engine as full application ("browser" process):** Preserves the architecture on paper (separate process, IPC communication) but recreates the "app within an OS" problem. The browser would contain its own renderer, compositor, and process model — the OS service just frames its output. Defeats the purpose of unified rendering.
- **Native toolkit (GTK, Qt, custom) without web engine:** Lighter and more controllable, but requires building text layout, rendering pipeline, accessibility, font shaping, and input handling from scratch. Years of work for basics. And still doesn't handle web content — you'd need a web engine anyway.
- **Custom web engine from scratch:** Maximum control, minimum dependency. But even reasonable web feature coverage is astronomically complex (CSS layout alone is enormous). Not feasible for a personal project.
- **Full web engine as "the OS" (Electron-style):** Gives up the document-centric architecture, scheduler control, IPC design, and everything that makes this OS interesting. The design is the goal, not the shortcut.
- **No web engine at all:** Would mean no web browsing capability. Conflicts with target use cases (Decision #1 lists web browsing).

---

## 12. Undo & History

_Tier 4. Depends on: Edit protocol. Feeds into: (relatively self-contained)._ **SETTLED.**

**Decision:** COW filesystem snapshots at operation boundaries for sequential undo. Operation log for metadata (attribution, descriptions). Filesystem snapshot retention for cross-session history.

**How it works:**

- Every `endOperation()` creates a COW snapshot (nearly free).
- Undo = restore file to previous snapshot. Redo = restore to next snapshot.
- Undo is global — walks the log regardless of which editor made each operation.
- Cross-session history ("show me this file last Tuesday") = filesystem-level snapshot query.

**Selective undo (upgrade path):** Requires content-type rebase handlers. Same investment as collaboration — building one unlocks both. Without a handler, sequential undo only. See Decision #9.

**Why this settled cleanly:** The edit protocol's "immediate writes on COW filesystem" makes undo almost free. No separate undo machinery needed — the filesystem IS the undo system for the base case.

---

## 13. Collaboration Model

_Tier 4. Depends on: Edit protocol._ **SETTLED.**

**Decision:** Design for it, build later. The architecture supports it; the implementation is deferred.

**Why this is now settled (not just a leaning):** The edit protocol (Decision #9) and undo model (Decision #12) were designed with collaboration in mind. The same content-type rebase handlers needed for selective undo are exactly the machinery needed for multi-user collaboration. The upgrade path is clear and incremental:

1. Sequential undo works for all types (done by default).
2. Text rebase handler enables selective undo + text collaboration (adopt existing OT/CRDT work).
3. Other content-type handlers added incrementally.

**Key insight:** Collaboration and selective undo are the same problem — both require rebaseable operations. Building one unlocks both. Cross-type conflicts (image resize causing text reflow) are mediated by the layout engine, not the collaboration protocol.

---

## 14. Compound Documents

_Tier 4. Depends on: File understanding, rendering technology, edit protocol. Feeds into: Layout engine._ **SETTLED.**

**Decision:** All documents are manifests with references. The manifest model is uniform — a document with one content reference (a text file) and a document with many (a slide deck) are structurally the same. What varies is how many content files the manifest references and which relationship axes describe the connections between parts. The "simple vs compound" distinction is an internal property, not a user-facing concept.

**Uniform manifest model:** Every document is backed by a manifest that references one or more content files plus metadata. Users see documents; files are implementation details. The metadata query system indexes only manifests — one kind of thing to index, one kind of thing to query. Content files are the source of truth for content; a separate full-text content index (maintained by the OS service, triggered by `endOperation`) enables searching within documents.

**Static and virtual manifests.** Manifests can be static (real files on disk, COW-snapshotted) or virtual (filesystem entries whose content is generated on demand, like Plan 9's `/proc`). Static manifests back user-created content and persisted imports (text files, slide decks, projects, viewed webpages). Virtual manifests back system-derived views (inbox, search results, "recent documents," system dashboard) and externally-sourced streams (streaming video = manifest with remote reference, content fetched on demand). Both are files, both are documents, both participate in the metadata query system. The user doesn't know which kind backs a given document — virtual vs static is an implementation detail, not a user-facing concept. Virtual documents don't need their own COW history; their "state at time T" is recovered by re-evaluating the query against the COW snapshot of the world at time T. **Design constraint:** rewind performance must be uniform across static and virtual documents, or the abstraction leaks. This requires the metadata DB to live on the COW filesystem so historical queries read from past DB snapshots at the same cost as current queries (see Decision #16).

**All documents are persistent.** There is no "transient" document concept. All documents — including viewed webpages — are written to the COW filesystem and subject to retention policies. Webpages get a shorter retention (e.g., 30 days); user-created content gets permanent retention. The COW pruning system handles cleanup. This eliminates a potential abstraction leak: if transient documents existed, the user would need to know a document's persistence type to predict behavior. With uniform persistence + retention policies, all documents are searchable, rewindable, and available offline. Browsers already cache page assets to disk — this model structures that same data as first-class documents instead of an opaque cache.

**Import creates a manifest.** When a file enters the system (airdrop, download, USB), the OS wraps it: creates a static manifest pointing to the raw file, extracts metadata, indexes it. A file comes in; a document emerges.

**Content-type registration via metadata.** Editors are files too. Their metadata declares which content types they handle (e.g., `handles: [text/plain, text/markdown]`). The metadata query system IS the registry — no separate mechanism. Same system used to find documents is used to find editors.

**Three-axis relationship model:** The relationships between parts are described along three orthogonal, composable axes (see foundations.md for full details):

- **Spatial** — where parts are positioned (flow, fixed canvas, grid, freeform canvas, or none)
- **Temporal** — when parts are active (simultaneous, sequential, timed, or none)
- **Logical** — how parts are grouped (flat, sequential, hierarchical, graph, or none)

Every document is a point in this three-dimensional space. A slide deck = spatial (fixed canvas) + temporal (sequential) + logical (flat). A source code project = logical (hierarchical tree). A video editing project = spatial (2D frame) + temporal (timed) + logical (grouped by track). The original five layout types (flow, fixed canvas, timeline, grid, freeform canvas) mapped to four spatial sub-types plus one temporal sub-type (timeline = timed). The three-axis model extends this to cover organizational documents (projects, albums, playlists) that use the logical axis without spatial layout.

**Version history is orthogonal to layout.** COW snapshots are an OS-level mechanism that applies to all documents regardless of their layout axes. Content temporality (an audio waveform) and version history (edits to the audio) are fundamentally different — one is the document's structure, the other is the OS's undo system.

**Key properties:**

- Content files are real files in the filesystem, managed by the OS, not directly accessible to users.
- "Resize image in a slideshow" is a spatial layout operation (manifest change), not an image operation.
- The layout engine mediates cross-type interactions along whichever axes are active.
- Axes can interact: spatial arrangement varying over time (animation), logical structure driving spatial visibility (collapsible sections). The layout model declares which axes are present; content-type-specific editors handle the coupling.

**Compound documents vs atomic content types:** Any content can be decomposed further (video → frames, image → pixels, text → code points → bytes), but this is a spectrum — taken to its conclusion, it's just Unix. Documents compose at the **mimetype level**: their parts are things that have mimetypes. A video _editing project_ (timed temporal layout + video clips + audio clips) is a document with multiple parts. A video _file_ (`video/mp4`) is an atomic content type — its internal structure (temporal compression, frame dependencies, codec) is the content type's concern, not a layout model the OS decomposes. The line is pragmatic but anchored to the IANA mimetype registry, the same way Unix's byte boundary is pragmatic but anchored to hardware addressing.

**Interop:** Translators (leaf nodes) convert between internal representation and external formats at import/export boundaries. Import .pptx → extract content as files, generate manifest with spatial + temporal layout. Export to .pptx → pack manifest + files into pptx structure. Translation is inherently lossy. New format support = new translator; the OS doesn't change.

**Remaining sub-decisions:**

- What happens when a referenced content file is moved or deleted?
- How does the user create/author multi-part documents? (Interaction model question)
- **Referenced vs owned parts.** A slideshow might reference independent photos (shared, survive deletion of the document) or contain text blocks that only exist within it (owned, deleted with the document). Is this a property of the reference, a user choice, or two distinct relationship types?
- **Mimetype of the whole (partially resolved).** Imported documents retain their original external mimetype as metadata. OS-native documents get custom OS mimetypes (e.g., `application/x-os-presentation`). The document-level mimetype drives editor binding. On export, user selects target format; OS pre-selects original mimetype where available. Remaining: systematic IANA mimetype → OS document type mapping; naming convention for OS-native mimetypes; how simple documents' mimetypes relate to their content file's mimetype.
- **Manifest format.** Internal to the OS, no external interop requirement. Binary for performance or text for debuggability. Leaf-node decision — design the interface, choose format later.
- **COW atomicity for multi-part documents.** An edit operation on a multi-part document might touch multiple content files. The `endOperation` snapshot needs document-level atomicity, not just file-level. The OS service coordinates this since it manages both the edit protocol and the filesystem interface.
- **Filesystem organization of manifests and content files.** Users don't see paths. The OS is free to organize storage however it wants: content-addressable, flat with UUIDs, by mimetype, etc. Connects to filesystem COW design (Decision #16).
- **Retention policies.** All documents are persistent, subject to retention policies for cleanup. Questions: what are the default retention tiers (permanent, 30-day, 7-day, etc.)? How does the user configure retention per document type? How does retention interact with COW snapshot pruning (same mechanism or layered)? What about storage pressure — does the OS aggressively prune low-retention documents when storage is low?

---

## 15. Layout Engine

_Tier 4. Depends on: Compound documents, rendering technology._

The layout engine is scoped by Decision #14's three-axis relationship model. It must handle relationships along three composable axes:

- **Spatial axis:** flow, fixed canvas, grid, freeform canvas sub-types
- **Temporal axis:** simultaneous, sequential, timed sub-types
- **Logical axis:** flat, sequential, hierarchical, graph sub-types

It also mediates cross-content-type interactions along whichever axes are active (image resize → text reflow on spatial axis, time range removal → synchronized track trimming on temporal axis, section collapse → child visibility on logical axis).

| Option                                                | Tradeoffs                                                                                                                                                                                                       |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **CSS (via web engine)**                              | Covers spatial axis well (flow, grid, flex, absolute positioning). No support for temporal or logical axes.                                                                                                     |
| **Custom layout engine**                              | Full control over all three axes. Can design temporal and logical layouts natively. But enormous engineering effort for spatial layouts that CSS handles well.                                                  |
| **CSS for spatial + custom temporal/logical engines** | CSS handles spatial axis (4 sub-types). Custom engines handle temporal (timed synchronization, sequential transitions) and logical (tree navigation, graph layout). Three systems but each focused on one axis. |

**Initial leaning:** CSS for spatial + custom temporal/logical engines.
**Why this matters:** The layout engine is a critical system component — it mediates cross-type interactions in compound documents (Decision #14). The three-axis model expands scope beyond the original five layout types. Closely tied to rendering technology choice (Decision #11). Logical axis support is what enables organizational documents (projects, albums) — without it, the compound document model only covers compositional use cases.

---

## 16. Technical Foundation

_Tier 5. Depends on: Compatibility stance, rendering technology, complexity philosophy._

This decision is being resolved incrementally through a bare-metal research spike. Sub-decisions range from settled to open.

### Settled sub-decisions

**Soft realtime (not hard RT).** Hard RT optimizes worst-case latency at the expense of average throughput. For a desktop workstation: bulk operations run slower (scheduler constantly servicing RT deadlines), low-priority tasks starve under RT load, dynamic plugin loading fights provable timing bounds (can't admit code without timing proof), and bounded-time requirements push lock-free complexity into connective tissue (violating Decision #4). Soft RT achieves sub-1ms scheduling latency on modern hardware — indistinguishable from hard RT for audio/video playback. Human perceptual threshold for audio glitches is ~5-10ms.

**No hypervisor (EL1, not EL2).** A hypervisor-based approach adds Stage-2 page tables, VM exit/enter overhead, and grant-table IPC. VM-boundary IPC is expensive, conflicting with immediate writes and the thin edit protocol. The "hypervisor-as-kernel" model (editors in separate VMs) works against "editors attach directly to content." A partitioning hypervisor running Linux alongside would contradict Decision #3.

**Preemptive multitasking with cooperative yield.** Not either/or. Preemptive as the safety net (a buggy editor can't freeze the system), cooperative yield at natural edit protocol boundaries (beginOp/endOp) as an optimization for cheaper context switches. The preemptive infrastructure is built (full context save/restore in boot.S). Cooperative yield is additive and doesn't require reworking anything.

**Privilege model: Traditional — all non-kernel code at EL0.** Hardware isolation via EL0/EL1 boundary. Editors, viewers, and all userspace code run at EL0 and interact with the kernel via syscalls. This is the arm64-standard approach, provides one simple programming model (no two-tier trust), and maximally tests the from-scratch kernel commitment (forces syscall interface, per-process address spaces, IPC — the hard parts). Syscall overhead on editor operations is acceptable because the edit protocol batches between beginOp/endOp boundaries (coarser-grained than per-keystroke).

**Address space model: Split TTBR — TTBR1 for kernel, TTBR0 per-process.** Follows directly from the privilege model. Each process gets its own TTBR0 (lower VA range, userspace mappings); TTBR1 (upper VA range, kernel mappings) stays constant across all processes. Context switch swaps TTBR0 + ASID. On syscall entry (EL0→EL1), kernel pages are already mapped via TTBR1 — no page table switch needed. This is the architectural reason arm64 provides two translation table base registers. Implemented in the research spike — kernel at upper VA (TTBR1), per-process TTBR0 with ASID tagging, scheduler swaps TTBR0 on context switch.

**Access control: OS-mediated handles.** The kernel maintains a per-process handle table. When the OS opens a document for viewing, the viewer process receives a read handle (integer index into its table). When an editor is attached, it receives a write handle to that specific document. The kernel validates the handle and checks rights on every document operation. Revocation on editor detach is trivial — the kernel clears the table entry. This enforces view/edit (Decision #6) and the edit protocol (Decision #9) at the kernel level, not just as UI convention. Handles are simpler connective tissue than full capabilities (Decision #4) because the OS is architecturally centralized — it mediates all document access, so distributed authority mechanisms (delegation, fine-grained narrowing) aren't needed. Handles can extend to cover IPC endpoints and devices if needed, growing incrementally toward capabilities without committing to the full type system upfront.

**IPC mechanism: Shared memory ring buffers with handle-based access control.** A "channel" is a pair of shared memory pages — one per direction — each containing a SPSC (single-producer, single-consumer) ring buffer of fixed 64-byte messages. Accessed via handles in each participant's handle table. The kernel creates channels, maps both pages into both participants, issues handles, and validates message structure at trust boundaries (editor ↔ OS service). The kernel is in the control plane (setup, access control, validation), not the data plane — after setup, communication flows directly through shared memory. Notification uses a futex-like mechanism (check shared flag first, syscall only when actually needing to sleep). One mechanism for all IPC — no separate configuration-passing vs conversation paths. Configuration is the opening message(s) on the ring buffer (Singularity contract pattern: state machine starts with init messages, then transitions to steady-state). Documents are separately memory-mapped into both editor and OS service address spaces; file data does not flow through ring buffers. Ring buffers carry control messages only: edit protocol calls (beginOp/endOp), input events, overlay descriptions, configuration. Prior art: io_uring (shared ring buffers, fixed-size entries, separate SQ/CQ), Fuchsia channels (handle-based async messaging), LMAX Disruptor (fixed-size SPSC, cache-line separation), Singularity (typed channel contracts with state machines).

**IPC message format: Fixed 64-byte messages, split architecture.** Each message is one AArch64 cache line: 4-byte type tag + 60-byte payload. The `ipc` library (shared) owns ring buffer mechanics (init, produce, consume, head/tail management, memory barriers). Per-protocol crates define message types and payload structs that fit within 60 bytes. Adding a new protocol means defining new `msg_type` constants and payload structs — the ring buffer infrastructure doesn't change. 62 message slots per direction per channel (4 KiB page minus 128-byte header). Pressure point: messages >60 bytes. If genuinely needed, the answer is shared memory + reference through the ring — documented as a known tension, not pre-built. Runtime protocol state validation (Singularity-style contract checking) deferred until edit protocol exists. See also Considered and rejected below for alternatives evaluated.

**Process architecture: Three-layer (kernel, OS service, editors).** "The OS renders everything" means an OS service process renders, not the kernel. One OS service process at EL0 handles rendering, metadata DB, input routing, and compositing. Editors are separate EL0 processes, one per attached tool. The OS service is trusted (it IS the OS, running at EL0 for crash isolation from kernel); editors are untrusted. The trust boundary that matters is OS service ↔ editors, not between OS service components internally. The kernel (EL1) handles hardware, memory, scheduling, IPC channel creation, handle management, and message validation at the editor ↔ OS service boundary. The primary IPC relationship is editor ↔ OS service via shared memory ring buffers. The kernel creates and authorizes these channels but is not in the data path. Security is a side effect of good architecture: handles enforce access, EL0/EL1 provides crash isolation, per-process address spaces provide memory isolation, kernel message validation protects the OS service from malformed editor messages — all natural consequences of the design, not additional security machinery. Can split the OS service into multiple processes later if isolation needs arise (reversible decision).

**Binary format: ELF64.** Industry-standard executable format for aarch64. Validated by the research spike (step 7) — the kernel loads standalone ELF binaries with a pure functional parser. Well-tooled (standard linkers, debuggers, objdump), well-documented, and the default output of every compiler targeting aarch64. No reason to deviate.

**From-scratch kernel.** Initially tentative — the research spike was designed to encounter real problems before evaluating alternatives (seL4, Zircon, Linux-as-runtime). All 5 roadmap phases completed: SMP, memory management, cleanup, VM, device I/O. 30 source files, 140+ host tests, 4-core QEMU smoke tests passing. Validated the full stack from boot to scheduling. Promoted from tentative to committed as the production kernel.

**Rust as kernel language.** Compile-time memory safety at the hardware boundary, strong type system for code review, built-in `aarch64-unknown-none` target. ~99 unsafe blocks all justified and auditable (DESIGN.md §7.1). Validated by the research spike: `no_std` ecosystem sufficient, bare-metal Rust practical at kernel scale. Promoted from tentative to committed.

**Scheduling: EEVDF selection + scheduling contexts (combined model).** Two-layer scheduling design. **Scheduling contexts** are kernel objects (budget, period, remaining budget, replenishment time) accessed via handles — time becomes a resource held through the same handle mechanism as IPC channels. Threads can only run when their scheduling context has remaining budget. **EEVDF** (Earliest Eligible Virtual Deadline First) is the selection algorithm among threads with budget: each thread has a virtual runtime, weight, and requested time slice; the eligible thread with the earliest virtual deadline runs next. Shorter time slice requests produce earlier deadlines, giving latency-sensitive work (foreground editor) lower latency without consuming more than its fair share. Together: contexts answer "may this thread run?" (budget check), EEVDF answers "which runnable thread runs next?" (fairness + latency). **Context donation:** When the OS service processes a message from editor X's IPC channel, it explicitly borrows X's scheduling context via syscall, billing that work to X's budget. Prevents noisy-neighbor: a greedy editor can only exhaust its own time. The OS service self-budgets from the total system allocation. **Content-type-aware scheduling:** The OS service knows each document's mimetype and state, so it sets appropriate budgets — tight period for audio playback, relaxed for text editing, trickle for background indexing. Budgets adjust dynamically as document state changes (video playing → paused demotes to background levels). A genuine advantage of the document-centric model: the OS can make informed scheduling decisions because it knows what each process is doing at a semantic level. **Best-effort admission:** No hard admission control. All scheduling contexts admitted. Under overload, everyone gets proportionally less — EEVDF handles fairness. Budgets are minimum guarantees under contention, not hard reservations. Strict admission can be added later (additive change) if needed. **Shared contexts:** An editor's threads share one scheduling context (the document's budget), regardless of internal thread organization. **Reversible aspects:** Context donation model (explicit syscall → automatic on channel_recv) and admission control (best-effort → strict) are painless to change — same kernel mechanism, different trigger point.

**Multi-core: SMP with 4 cores.** Implemented. PSCI CPU_ON for secondary cores. Ticket spinlock for shared state. Per-core kernel stacks, idle threads, TPIDR_EL1. Global run queue (fine for ≤8 cores).

**Driver model: Userspace drivers with kernel-mediated hardware access.** Each driver runs as a separate EL0 process. The kernel maps device MMIO pages into the driver's address space via a device handle — register reads/writes are direct memory operations with zero overhead versus in-kernel drivers. Hardware interrupts are delivered to EL1 (unavoidable on ARM), where a minimal kernel handler masks the interrupt and signals the driver's notification handle. The driver wakes, services the device through its mapped MMIO pages, and acknowledges the interrupt via syscall (unmasks it). DMA buffers are physically contiguous frames from the buddy allocator, mapped into the driver's address space at setup time. Why userspace: fault isolation (driver crash kills the driver process, not the kernel), unsafe minimization (MMIO through mapped pages requires no `unsafe` in driver code), smaller kernel TCB. Performance overhead is one extra context switch per interrupt (~1-2μs on ARM), negligible for desktop I/O rates. Scheduling contexts give drivers appropriate budgets for latency-sensitive work. New kernel primitives needed: device handles (MMIO mapping + interrupt notification), interrupt acknowledgment syscall, DMA buffer allocation, event multiplexing syscall (`wait_any`). Prior art validates performance: QNX (userspace drivers, automotive hard RT), Fuchsia (all userspace, consumer hardware), seL4 (all userspace, military/aerospace).

**Filesystem placement: Userspace service.** The filesystem runs as an EL0 process managing on-disk layout (B-tree structure, block allocation, snapshot metadata, space accounting, pruning). The kernel owns page-level COW mechanics: the page fault handler performs copy-on-write (allocate new page, copy, remap), demand paging manages physical frames, memory mapping places file pages in process address spaces. The hot path for "no save" (editor writes to a memory-mapped document page) is handled entirely by the kernel's VM layer — no filesystem involvement. The filesystem is only involved in: snapshot creation at `endOperation` (metadata update), background writeback (flushing dirty pages to disk), and file open/close — all infrequent relative to editor writes. Why userspace: filesystem code is complex (B-trees, journaling/logging, crash recovery, space accounting) — exactly the code you don't want in the trusted computing base. The performance-critical path (COW page faults) never crosses the process boundary.

**Microkernel convergence.** The kernel is a microkernel — not by ideology, but by convergence. Each sub-decision independently pushed complexity outward: drivers to userspace (fault isolation + unsafe minimization), filesystem to userspace (complex code outside TCB, hot path in kernel VM), rendering to the OS service (not in-kernel), editors to separate processes (untrusted). What remains in the kernel is exactly the microkernel set: address spaces, threads, IPC, scheduling, interrupt forwarding, and handle-based access control. The kernel's role is multiplexing hardware resources behind handles and providing a single event-driven wait mechanism. Everything semantic (content types, document state, filesystem layout, driver protocols) lives in userspace. The kernel doesn't understand what any resource is _for_ — it just manages access to it.

### Open sub-decisions

**Filesystem COW design.** Research complete (see `design/research-cow-filesystems.md`). Placement decided (userspace service). On-disk format not yet designed. Key requirements: birth time in block pointers (non-negotiable for efficient snapshots), per-document snapshot scoping, `beginOp`/`endOp` map to COW transaction boundaries, efficient pruning (ZFS-style dead lists). Open sub-questions: snapshot naming/addressing, pruning policy, compound document atomicity, page cache placement (kernel-managed vs. filesystem-managed), interaction with memory-mapped I/O.

### Considered and rejected

- **Hard realtime:** Throughput cost, task starvation, fights dynamic plugin loading, lock-free complexity in connective tissue, no perceptible benefit for desktop A/V. See insights log.
- **Hypervisor-based (EL2):** VM-boundary IPC too expensive for immediate-write model, adds complexity layer, partitioning contradicts no-POSIX stance. See insights log.
- **Cooperative-only multitasking:** Can't guarantee responsiveness — a buggy non-yielding editor freezes the system.
- **Language-safety privilege (everything at EL1):** Requires all code to be in a verifiable language. `unsafe` breaks all guarantees. Blocks non-Rust editors. Unsolved research problem for extensibility (Singularity OS required Sing#). Displaces hardware isolation complexity into verification/sandbox complexity — total complexity conserved, but in a harder-to-reason-about form.
- **Hybrid privilege (kernel+viewers at EL1, editors at EL0):** Creates two programming models — viewers and editors have different execution environments. Two ways to do the same thing (Decision #4). The trust boundary (view=safe, edit=unsafe) is a policy choice that can be expressed through capabilities within the traditional model, not an architectural split.
- **Full capability-based access control (seL4/Fuchsia style):** Capabilities as the universal access mechanism for all resources. Over-engineers the connective tissue for a centralized-authority OS — query/discovery tension with metadata access (Decision #7), complex bootstrapping (initial capability distribution), capability type system sprawl across documents/IPC/devices/memory. Full capabilities solve distributed authority problems (delegation, confinement) that don't arise when the OS mediates all access. OS-mediated handles provide the same security guarantees (per-document, rights-specific, revocable) with far less machinery.
- **Centralized permissions (Unix-style ACLs):** Permissions stored with resources, checked against process identity. Requires defining process identity (what is a "UID" in a single-user OS?). Ambient authority means editors can write outside beginOp/endOp boundaries — edit protocol becomes unenforceable convention. Doesn't naturally express "this editor can write to this specific document" without per-document ACLs naming specific processes.
- **No access control beyond hardware:** EL0/EL1 provides crash isolation but not semantic protection. View/edit distinction unenforced. Edit protocol advisory only. Any process can corrupt any document. Too permissive even for a personal OS — buggy editors shouldn't have unlimited blast radius.
- **Synchronous message passing (L4/seL4-style IPC):** Register-sized messages, sender blocks until receiver ready. Ultra-fast for tiny messages but can't deliver input events to a busy editor (requires receiver to be waiting in receive()). Size-limited (~120 bytes in registers); anything larger needs a second mechanism, violating one-mechanism principle. Total complexity displaced to userspace marshaling and buffering.
- **Asynchronous queued messages (Mach-style IPC):** Kernel-managed message queues with out-of-line data and port rights. Powerful but copies data twice (sender→kernel→receiver), complex kernel involvement per message, historically slow. Kernel complexity disproportionate to this OS's needs.
- **Star-only IPC topology (all data through kernel):** Every message flows process→kernel→process. Kernel becomes data-path bottleneck. Doesn't scale to editor↔OS service communication volume. Rejected in favor of kernel-mediated setup with direct shared memory communication.
- **CFS (Completely Fair Scheduler, Linux pre-6.6):** Picks smallest virtual runtime ("who got least CPU?"). Fair but no mechanism for latency differentiation without heuristics and tunables. EEVDF is strictly more informative (adds eligibility + deadline) and subsumes CFS.
- **Strict priority scheduling (current kernel):** Simple but no fairness guarantees. High-priority threads starve everything below. No per-workload isolation. Replaced by EEVDF + scheduling contexts.
- **Stride scheduling (Waldspurger '95):** Deterministic proportional-share. Clean and simple, but no inherent latency differentiation — same limitation as CFS. EEVDF's virtual deadline mechanism solves this.
- **Lottery scheduling (Waldspurger '94):** Probabilistic proportional-share. Interesting ticket transfer concept (used in context donation instead) but high short-term variance. Deterministic algorithms preferred.
- **EEVDF alone (no scheduling contexts):** Good fairness and latency, but no per-workload temporal isolation and no server billing. The OS service becomes a shared resource with no per-editor accounting — noisy neighbor problem. Scheduling contexts solve this.
- **Scheduling contexts alone (no EEVDF):** Budget isolation works, but selection among threads with budget is just priority-based — no fairness or latency differentiation within a priority level. Two dimensions to manage (priority + budget) with no algorithmic help.
- **SCHED_DEADLINE (EDF + CBS, Linux):** Tasks declare runtime/deadline/period. Clean for soft RT, but pure deadline scheduling doesn't provide proportional fairness among non-deadline tasks. The combined model uses EEVDF for fairness and contexts for isolation — covering both needs.
- **Full seL4 scheduling contexts (time as pure capability):** Elegant but seL4's synchronous IPC makes context donation natural (server wakes on client's context). Our async ring buffers require explicit borrow/return, making the pure capability model less clean. Adopted the useful parts (budget/period, handle-based, passable) without requiring synchronous IPC.
- **Apple Clutch thread groups:** Three-level hierarchy (QoS bucket → thread group → thread). Interesting but the three-level scheduling hierarchy is complex connective tissue (Decision #4). Content-type-aware budget setting achieves similar results (foreground = interactive, background = utility) without architectural complexity.
- **Hard admission control:** Reject scheduling context creation when total committed budget exceeds CPU capacity. Guarantees are real but "you can't open another document" is bad UX for a personal OS. Best-effort with EEVDF fairness under overload is simpler and more aligned with Decision #4.
- **Multiple OS service processes (split renderer, metadata, input):** More isolation between OS components, but creates microkernel IPC explosion (L4 cautionary tale) without demonstrated need. The trust boundary is OS service↔editors, not between OS components. Reversible — can split later if stability requires it.
- **In-kernel drivers (monolithic):** Fast (no IPC per device interaction) and simple (current virtio implementation uses this model). But driver bugs crash the kernel — drivers are the largest source of kernel bugs in production OSes. Conflicts with unsafe minimization discipline (DESIGN.md §7.1) and inflates TCB. Desktop I/O rates don't justify the latency savings over userspace drivers.
- **Hybrid driver model (critical in-kernel, rest userspace):** Pragmatic but creates two driver programming models. Decision #4 (simple connective tissue) says one model. Where to draw the "critical" line is a judgment call with no stable answer — it shifts as hardware and workloads change.
- **In-kernel filesystem:** Puts complex code (B-trees, crash recovery, space accounting) in the TCB. Performance argument is weak because the hot path (COW page faults on memory-mapped documents) is in the kernel's VM layer regardless of where the filesystem lives. Filesystem operations (snapshot creation, writeback, open/close) are infrequent and tolerate IPC overhead.

---

## 17. Interaction Model

_Tier 5. Depends on: Rendering technology, data model, file organization._

### Leanings under discussion (2026-03-10/11)

**Shell is blue-layer (leaning, not settled).** The shell (GUI/CLI) is an untrusted process (EL0) in the blue layer, like editors. It's pluggable — different shells can provide different interaction models on top of the same OS service interfaces. The interaction model is a shell design question, not an OS service design question. However, the shell requires _system gestures_ (switch document, invoke search, close document) that must always work. Current thinking: system gestures are baked into the OS service's input routing (a handful of always-available actions), while the shell provides the visual UI for navigation (what search looks like, how documents are listed). This splits the shell into mechanism (OS service, not pluggable) and presentation (shell tool, pluggable). Needs more exploration.

**One-document-at-a-time (leaning).** Strong initial inclination toward a non-windowed, non-tiled UI — you're looking at one document at a time, like macOS fullscreen Spaces but for documents. Switching documents goes through the shell. Not committed — raises questions about the shell's relationship to input routing.

**Compound document editing (unresolved tension).** Three settled principles conflict for compound documents: "editors bind to content types" (one text editor for all text), "one editor per document" (modal, simple), and "OS provides content-type interaction primitives" (cursor, selection, playhead). For simple documents these coexist. For compound documents (presentation with text + images), either: (A) the compound editor handles everything including sub-content editing (violates content-type binding, re-implements text editing), (B) content-type editors nest within compound context (violates one-editor-per-document, creates nesting complexity), or (C) the OS provides rich editing primitives per content type (OS becomes much more complex). Initial instinct leans toward B (nesting) — one text editor used everywhere, including within compound documents. Needs dedicated design exploration.

### Open questions

- What does the "desktop" look like? Is there one?
- How do you open a file if there's no file browser with folders?
- How do tags and queries surface in the GUI?
- What does navigation between open documents look like?
- Is it windowed, tiled, fullscreen-per-workspace, or something else?
- What does "launching" look like without an app launcher?
- How does the CLI integrate with the GUI? (Possibly one shell handles both, or separate shells for CLI and GUI)
- Where exactly is the boundary between OS service input routing (system gestures) and shell input handling?
- How does editor nesting work for compound documents? Who handles layout vs content? How does input routing work between levels?

### Considered and rejected

- **Shell as part of OS service (trusted, black):** Mixes adaptation logic (messy user-facing UI) into the clean core. The OS service becomes responsible for interaction model design, violating the principle that the OS is semantically ignorant of user intent. Blue-layer concerns contaminate black-layer code.
- **Shell purely modal with editors (explored 2026-03-10):** Initially proposed that the shell and editors are in the same modal slot — either the shell is active or an editor is, never both. But switching documents requires the shell to intercept input while an editor is active (a gesture, shortcut, etc.). The shell must be ambient, not modal. The system gesture/shell UI split resolves this.

---

## Decision Dependencies (Key Chains)

Reading the diagram top-to-bottom, the critical decision chains are:

1. **Data model → File understanding → Editor model → Edit protocol → Undo/Collaboration**
   This is the spine. The document-centric choice cascades all the way through.

2. **Editor model → Rendering technology → Compound documents → Layout**
   How editors work determines what rendering approach makes sense, which determines how compound documents get laid out.

3. **Compatibility stance → Technical foundation**
   How much existing software you want to run constrains kernel, ABI, and language choices.

4. **Data model + File organization → Interaction model**
   What the user sees depends on whether the system is document-centric and how files are organized.

The single most influential decision is **#2 (Data Model)**. If you're confident in document-centric, almost everything else is constrained in useful ways. If you're not, that uncertainty propagates everywhere.
