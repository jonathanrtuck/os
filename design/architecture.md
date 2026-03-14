# System Architecture

How to think about this system. Read this before touching any code.

---

## The Pipeline

The system is a one-way pipeline with five stages. Data flows in one direction — from the user's intent to pixels on screen. No stage reaches back into a previous stage.

```
User Input → Editor → OS Service → Scene Graph → Compositor → Pixels
```

Each stage has one job:

| Stage | Job | Knows about |
|-------|-----|-------------|
| **Editor** | Translate user intent into write requests | Its content type |
| **OS Service** | Apply writes, maintain document state, lay out content | Documents, content types, layout |
| **Scene Graph** | Intermediate representation: positioned visual elements | Nothing — it's data, not a process |
| **Compositor** | Render positioned elements to pixels | Geometry, color, blending |

The scene graph is not a process — it's the data boundary between the OS service and compositor. The OS service writes it. The compositor reads it. They never communicate any other way.

---

## What Each Component Understands

The core architectural rule: **each component is ignorant of everything outside its concern.** This isn't aspirational — it's the test for whether a responsibility is in the right place.

### The Kernel

The kernel manages hardware resources: memory, CPU time, interrupts, process isolation. It is **semantically ignorant** — it does not know what a document is, what a mimetype is, or what pixels look like. It provides shared memory pages and doorbells (signals). It does not look inside the data that flows through them.

### Editors

An editor understands one content type. A text editor knows what "insert character at offset" means. An image editor knows what "rotate 90 degrees" means. An editor **never writes to a document directly** — it sends write requests to the OS service via IPC. The editor has read-only access to the document (zero-copy memory mapping). This is not a safety feature bolted on after the fact — it is the only path. There is no write API for editors.

Editors are the adaptation layer between human creative intent and the edit protocol. They are structurally symmetric with drivers: a driver translates device registers into OS primitives, an editor translates user gestures into write requests. Both are untrusted, restartable leaf nodes.

### The OS Service

The OS service is the center of the system. It is the **sole writer** to document state. All content flows through it.

Its responsibilities:
- **Document state:** Owns the document buffer. Applies write requests from editors. Manages snapshots for undo.
- **Content understanding:** Knows what content types are. Knows that text has lines, that images have dimensions, that compound documents have parts with relationships.
- **Layout:** Computes where content elements are positioned. For text: line breaking, wrapping, glyph positioning. For compound documents: spatial/temporal/logical arrangement of parts. For all content types: this is where "understanding the content" translates into "knowing where everything goes on screen."
- **Input routing:** Receives input events, decides where they go (editor, system gesture, navigation).
- **View state:** Tracks focus, cursor position, selection, scroll offset. Translates view state into scene graph mutations.

The OS service compiles document state into a scene graph — a tree of positioned, decorated, content-agnostic visual nodes. This compilation is the boundary between "understanding content" and "rendering pixels."

**The compiler analogy:** The OS service is a compiler. Documents are source code. The scene graph is machine code. The compiler (OS service) does all the thinking — parsing, type checking, optimization, layout. The CPU (compositor) just executes. The CPU doesn't know what a function is; it knows what an instruction is. The compositor doesn't know what text is; it knows what a positioned rectangle with a background color is.

### The Scene Graph

The scene graph is data, not a process. It is a tree of `Node` values in shared memory:

- Each node has geometry (position, size), visual decoration (background, border, corner radius, opacity), and optional content (rasterized text, pixel buffer, vector path).
- Tree structure is encoded via first-child/next-sibling indices.
- Variable-length data (text runs, pixel buffers) lives in a data buffer referenced by offset+length.
- The scene graph is a **compiled output** of the document model, not the document model itself. The document has semantic content (logical relationships, metadata, temporal sync). The screen has visual elements (chrome, cursor, selection) that aren't in any document. The OS service compiles one into the other.

**The screen is the root compound document.** The entire visual output is a tree — system chrome and document content are its children. The compositor doesn't know it's rendering "the screen." It renders a tree of nodes. Multi-document views are just different subtrees.

### The Compositor

The compositor is a **content-agnostic pixel pump.** It walks the scene graph and produces pixels. It knows about:

- Rectangles, colors, alpha blending
- Rasterizing glyphs at given positions (position decided by OS service)
- Clipping, scrolling (as viewport offset)
- Compositing layers back-to-front

It does **not** know about:
- Documents, mimetypes, or content types
- Text layout (line breaking, wrapping, where lines start)
- Cursors, selections, or editing state
- What any visual element *means*

If you find yourself adding content-type awareness to the compositor, the responsibility is in the wrong place. Move it to the OS service.

**How text reaches pixels:** The OS service sends *positioned text runs* in the scene graph — each run is a (x, y, text, advances) tuple for one line of text. The compositor walks each run, rasterizing glyphs at the given positions. The OS service has font metrics (advance widths — small data). The compositor has the glyph cache (rasterized coverage maps — big data). Layout in the OS service, rasterization in the compositor. Cursor and selection are regular positioned rectangles — the compositor doesn't know they relate to text.

### The GPU Driver

The GPU driver transfers pixel buffers from the compositor to the display. It knows about DMA, virtqueues, and display hardware. It does not know what the pixels represent.

---

## One-Way Data Flow

This is the system's most important structural property. Trace any piece of data and it flows in one direction:

```
Keystroke → Input driver → OS Service → Editor → OS Service → Scene Graph → Compositor → GPU → Display
```

The only apparent loop is input → OS service → editor → OS service. But this is not a loop — it's a request/response across a process boundary. The OS service sends an input event; the editor sends back a write request. These are different message types on the same bidirectional channel. The data being written (document content) never flows backward.

**Why this matters:** One-way flow means no component needs to synchronize with or wait on a downstream component. The OS service never asks the compositor "where is byte 45 on screen?" because the OS service did the layout and already knows. The compositor never asks the OS service "what content type is this?" because the compositor doesn't need to know. Each component has all the information it needs to do its job.

**Click hit testing** illustrates this. When the user clicks at pixel (x, y):
1. The input driver sends the pointer event to the OS service.
2. The OS service knows the layout (it computed it). It maps (x, y) to a byte offset in the document.
3. The OS service updates the cursor position in document state.
4. The OS service rebuilds the affected scene graph nodes.
5. The compositor renders the updated scene graph.

At no point does data flow backward. The OS service doesn't ask the compositor for layout information. The compositor doesn't know a click happened.

---

## The Adaptation Layer

The system has a clean core (OS service) surrounded by adapters on all sides:

| Direction | Adapter | Translates |
|-----------|---------|------------|
| Below | Drivers | Device registers → OS primitives |
| Above | Editors | Human creative intent → write requests |
| Above | Shell | Human navigational intent → document lifecycle ops |
| Inward | Translators | External formats (.docx, .html) → manifests + content files |
| Outward | Translators | Manifests + content files → external formats |
| Below | Compositor | Scene graph → pixels |

Drivers and editors are structural mirrors. Both are untrusted, restartable, leaf nodes that translate between an unpredictable external reality and the OS service's clean internal model. A driver's external reality is hardware. An editor's external reality is a human.

The compositor is also an adapter — it translates the OS service's semantic output (scene graph) into the GPU's input (pixel buffers). It sits between two well-defined interfaces and adds no semantic knowledge of its own.

---

## Content Type Understanding

The OS service understands content types at the mimetype level. This means:

- It knows text has characters, lines, and wrapping behavior.
- It knows images have pixel dimensions.
- It knows compound documents have parts with spatial/temporal/logical relationships.
- It does **not** know about codec internals, compression algorithms, or format-specific structures. Those are leaf-node concerns handled by decoders inside the adaptation layer.

**Layout is content understanding.** Computing where text lines break, where an image sits within a flow layout, how slides are sequenced — all of this requires understanding what the content *is*. That's why layout belongs in the OS service, not the compositor. The compositor renders the *result* of layout (positioned elements). It doesn't participate in the layout process.

Every content type the OS natively understands gets a layout handler in the OS service. Text gets line breaking and wrapping. Images get dimension-aware placement. Compound documents get the three-axis layout engine. A content type without a layout handler falls back to "opaque rectangle" — the OS can still display it (via a decoder that produces pixels), it just can't flow text around it intelligently.

---

## Process Boundaries

```
┌─────────────────────────────────────────────────┐
│                    Kernel (EL1)                  │
│   Memory, scheduling, IPC, interrupts            │
└─────────────────────────────────────────────────┘
        ▲               ▲               ▲
        │               │               │
┌───────┴───────┐ ┌─────┴─────┐ ┌───────┴───────┐
│  OS Service   │ │Compositor │ │  GPU Driver   │
│  (EL0)        │ │  (EL0)    │ │  (EL0)        │
│               │ │           │ │               │
│ Documents     │ │ Scene     │ │ DMA transfer  │
│ Layout        │ │ rendering │ │ Display ctrl  │
│ Edit protocol │ │ Text      │ │               │
│ View state    │ │ rasterize │ │               │
│ Input routing │ │ Composite │ │               │
│ Scene graph   │ │           │ │               │
│ build         │ │           │ │               │
└───────┬───────┘ └─────┬─────┘ └───────┬───────┘
        │               │               │
   Scene graph      Pixel buffer    Display
  (shared memory)  (shared memory)   output
        │               │
        └───────┬───────┘
         Double-buffered
         generation swap
```

The scene graph lives in shared memory. The OS service writes to the back buffer, swaps (bumps generation counter with a release fence), and signals the compositor. The compositor reads the front buffer (acquires, reads generation, reads data). They never touch the same buffer. No locks.

---

## What Goes Where (Decision Checklist)

When adding a new capability, ask:

1. **Does it require understanding what content *is*?** → OS service.
2. **Does it translate between a user and the OS service?** → Editor or shell.
3. **Does it translate between hardware and the OS service?** → Driver.
4. **Does it translate between an external format and the OS model?** → Translator.
5. **Does it turn positioned visual elements into pixels?** → Compositor.
6. **Does it manage hardware resources (memory, CPU, isolation)?** → Kernel.

If a capability spans two of these, the design isn't finished. Find the interface that separates them.
