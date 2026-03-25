# OS Landscape Research

Research into non-Rust operating systems with ideas relevant to our document-centric OS design. Conducted 2026-03-08. Complements the Rust OS comparison (flosse/rust-os-comparison) and COW filesystem research (cow-filesystems.md).

---

## Systems Studied

| System      | Era       | Core Relevance                                                               |
| ----------- | --------- | ---------------------------------------------------------------------------- |
| Phantom OS  | 2009–     | Orthogonal persistence — alternative "no save" approach                      |
| BeOS/Haiku  | 1996–     | Typed file attributes, query navigation, translators, MIME, media scheduling |
| Singularity | 2003–2015 | Software-isolated processes, typed IPC channel contracts                     |
| Midori      | 2008–2015 | Async-everything, capability security, error model, three safeties           |
| Oberon      | 1986      | Text-as-command, radical minimalism, CLI/GUI unity                           |
| Spring      | 1993      | Doors (synchronous IPC), VM/IPC unification, subcontracts, naming            |

---

## Phantom OS — Orthogonal Persistence

**What it is:** The entire userland address space is persistent. RAM is a cache over disk-backed virtual memory. Applications run in a bytecode VM and literally do not see reboots. No files, no save, no open/close, no serialization. If you have a pointer to an object, it is valid forever.

**Snapshot mechanism:** Kernel continuously snapshots the persistent memory region. Rolling three-snapshot scheme (current, previous, in-progress). Only dirty pages written. On restart, last consistent snapshot loaded, all VM threads resume. Snapper v2 (Genode fork) adds incremental snapshots with reference counting, retention policies, and integrity hashing.

**Problems encountered:**

1. **The Ratchet Problem.** Bugs that corrupt state persist forever. No clean restart. "A single bug that negatively impacts state is no longer fixable by rebooting." Silent corruption can manifest months later.
2. **Schema Evolution.** When code updates, persistent objects have old structure. Requires migration logic — "eventually reinvents some kind of filesystem" (David Chisnall).
3. **Object Graph Integrity.** "Hard to maintain an internal object graph without corruption for an effectively infinite runtime."
4. **GC at Scale.** Garbage collecting terabytes of persistent object graph is unsolved.
5. **Network State.** Connections timeout regardless of persistence. Applications still need reconnection logic.
6. **Persistent Malware.** Corrupted/malicious state survives reboots.
7. **Status:** PoC quality, IA-32 only, ~6 contributors.

**Key takeaway:** Phantom validates the _desire_ for "no save" but reveals persistent objects are the wrong mechanism. Our approach (immediate writes to COW filesystem) gets the same UX without the systemic fragility. Files provide isolation (corrupt one doc, not the system), format boundaries (schema evolution via format versioning), and natural undo points (COW snapshots). **Files are a feature, not a limitation.**

---

## BeOS/Haiku — Rich Metadata, Queries, Translators

### BFS Attributes

Typed name/value pairs stored in the filesystem. Every file, directory, symlink can carry arbitrary attributes. Small attributes stored inline in the inode (free I/O cost). Overflow goes to attribute directory blocks.

**Types:** string, int (8/16/32/64), float, double, bool, time, MIME string, raw data. Typed at the filesystem level — not opaque blobs like Linux xattrs.

**Standard attributes:** `BEOS:TYPE` (MIME), `BEOS:PREF_APP` (preferred handler), icons, email headers (`MAIL:from`, `MAIL:when`), contact fields (`META:name`, `META:email`), audio tags (`Audio:Artist`), image dimensions.

**Indexing:** Per-attribute B+ trees, filesystem-global. `mkindex -t string MAIL:from` creates a B+ tree mapping values to inodes. Near-instantaneous lookups. Limitation: indexes are NOT retroactive (only track files written after creation).

**Querying:** SQL-like syntax: `"(MAIL:from == \"*clara*\") && (MAIL:when >= %2 months%)"`. Operators: `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`, `!`, wildcards. Every query requires at least one indexed attribute. Free indexes: `name`, `size`, `last_modified`.

**Live queries:** Register for real-time updates. Kernel sends `B_QUERY_UPDATE` messages when files enter/leave result set. Powered by kernel-level node monitoring. Used for IM online status, email arrival, dynamic file views.

**Biggest problem:** Attributes lost when copying to non-BFS volumes. "Files which cannot be copied from system to system are largely pointless."

### Translation Kit

System-level plugin architecture for format conversion. Not app-specific.

- `BTranslatorRoster` discovers installed translator add-ons
- Each translator: `Identify()` (can I handle this?) + `Translate()` (convert it)
- Formats categorized by media group (bitmap, text, sound, etc.)
- Each group has an interchange format (e.g., `BBitmap` for images)
- Apps work with interchange formats; translators handle conversion to/from specific formats
- Quality/capability scores (0.0–1.0) for translator selection

**Key limitation:** No automatic chaining. GIF → PNG requires two explicit steps through the interchange format.

### MIME Types

First-class filesystem metadata. Set via explicit declaration, extension mapping, or content sniffing. System MIME database stores: type definitions, icon associations, preferred handlers, sniffer rules, per-type attribute definitions (what attributes files of this type carry).

**Per-type attribute definitions:** The MIME database records what attributes a given type carries. Tracker knows which columns to show based on file type. Custom filetypes define their attributes, and the system immediately knows how to display them.

### Media Performance

Dataflow graph of processing nodes connected via shared memory buffers. 120 priority levels (1-99 time-sharing, 100-120 real-time). Real-time threads are non-preemptible. Hardware timer precision for exact wakeup timing. Pervasive multithreading.

**Weakness our design avoids:** Fixed priority scheduling with non-preemptible RT threads. A runaway RT thread starves the entire system. Our EEVDF + scheduling contexts prevent this via budget caps.

### Relevance Summary

| BeOS Feature                   | Our Equivalent                   | Status                            |
| ------------------------------ | -------------------------------- | --------------------------------- |
| BFS typed attributes           | Decision #7 (queryable metadata) | Converged independently           |
| B+ tree indexes                | Embedded DB                      | Same concept, different mechanism |
| Live queries                   | Not yet designed                 | **Should adopt**                  |
| Translation Kit                | Decision #14 (translators)       | Converged independently           |
| MIME as filesystem metadata    | Decision #5 (file understanding) | Converged independently           |
| Per-type attribute definitions | Not yet designed                 | **Worth adopting**                |
| Media priority scheduling      | EEVDF + scheduling contexts      | Our design is stronger            |

---

## Singularity — Software Isolation & Typed Channels

### Software Isolated Processes (SIPs)

All processes run in ring 0, single address space, paging off. Isolation enforced by language (verified MSIL bytecode), not hardware. Sealed processes (no dynamic code loading). Per-SIP garbage collector. ~4.7% CPU overhead for software isolation vs ~38% for hardware isolation.

**Not for us:** Requires total ecosystem buy-in to managed language. We need editors in arbitrary languages. Hardware isolation (EL0/EL1) provides defense in depth.

### Contract-Based Channels (HIGH RELEVANCE)

Bidirectional typed message channels with exactly two endpoints. Contracts are state machines defining:

- Valid messages (with typed payloads)
- Direction (`in` = client→server, `out` = server→client)
- Valid ordering as a finite state machine

```rust
contract KeyboardDeviceContract {
    in message GetKey();
    out message AckKey(char key);
    out message NakKey();
    state Ready { GetKey? -> (AckKey! or NakKey!) -> Ready; }
}
```

Compiler verifies: correct protocol state, endpoint agreement, no protocol-deadlocks, type match. Analysis of 90+ contracts found only 2 with realizability issues.

**Exchange heap + linear types:** Shared memory region with manual management. Single-owning-pointer discipline enforced by type system. Sending transfers ownership — sender's pointer invalidated. **Zero-copy IPC.**

**Relevance:** Our edit protocol (beginOp/endOp) is already a state machine. Our IPC message format is undesigned. Formalizing as contracts (even without compiler enforcement) would: prevent editors from deadlocking OS service, document the editor plugin API precisely, enable runtime validation at trust boundary. Schema file → generated validation code would capture the value.

---

## Midori — Async Everything, Capabilities, Error Model

### Async Model

Synchronous blocking disallowed. Single-threaded event loops per process. Type system makes latency visible: `T` (sync), `Async<T>` (async), `Result<T>` (fallible), `AsyncResult<T>` (both). Compiler proves a UI thread with no `Async` types never blocks. No demand paging (memory access never blocks).

**Linked stacks** for async: start at 128 bytes, grow in 8KB chunks. Three assembly instructions to link. An async method that doesn't await allocates nothing.

**Resource management was never fully solved.** Removing blocking exposes "latent concurrency" that can cause fork-bomb dynamics. Our EEVDF + budget enforcement addresses this better.

### Capability-Based Security

Objects as unforgeable capability tokens. If you don't have a reference, you can't access it. No ambient authority — mutable statics banned at compile time. No `DateTime.Now`, no global singletons. All capabilities flow through constructor/method parameters.

**Comparison:** Our handle tables are runtime-enforced capabilities. Midori's are compile-time-enforced. Both prevent forgery. We have defense in depth (hardware isolation); Midori did not.

### Error Model (RELEVANT FOR OS SERVICE DESIGN)

Two mechanisms:

1. **Abandonment (bugs):** Null deref, bounds violation, contract failure → kill process immediately. No recovery. State is corrupt.
2. **Statically checked exceptions (recoverable errors):** Network failure, parse error → part of method signature, compiler enforces handling.

Maps to our three layers:

- **Kernel:** Panic on invariant violation (Rust's default)
- **OS service:** Abandonment for bugs (restart), typed errors for recoverable failures
- **Editors:** Crash freely (untrusted, isolated, restartable)

### Three Safeties (Type, Memory, Concurrency)

Concurrency safety via permissions: `mutable`, `readonly`, `immutable` (deep, transitive). `isolated` = single unaliased reference. Compiler freezes immutable objects into readonly binary segment (~10% code size reduction).

### Performance

AOT compilation (not JIT). Bounds check elimination via value range analysis. Escape analysis (10% heap→stack). Reflection eliminated (30% image size savings). Profile-guided optimization (30-40% speedup). **Safety and performance are not in fundamental tension** — type information enables optimizations impossible in C/C++.

### What Killed Midori

Windows inertia, no backward compatibility, organizational politics. Duffy's biggest regret: not open-sourcing. Ideas survived in C# async/await, Task<T>, and influenced Rust's `Result<T, E>`.

---

## Oberon — Text as Command, Radical Minimalism

### Text-as-Command (DIRECTLY ADDRESSES DECISION #17)

Any text on screen is potentially a command. Middle-click on `Module.Procedure` anywhere — in a document, in log output, in a tool text — and it executes. The system loads the module dynamically, finds the procedure, runs it. Parameters follow the command text, terminated by `~`.

**Tool texts** are editable documents containing commonly-used commands. User-configurable menus that are just text files. `System.Tool` is the default; users create project-specific tool texts.

**Key insight:** There is no CLI/GUI distinction. Text IS the interface. Every document is simultaneously a potential command palette. Every command output is simultaneously a document.

Rob Pike carried this into Plan 9's Acme editor. Robert Griesemer (Go co-designer) was Wirth's PhD student.

**Relevance to Decision #17:** Our OS has "CLI and GUI are equally fundamental interfaces." Oberon's answer: eliminate the distinction. Text is both content and command. View a tool text = see your commands. Activate one = execute. This maps to our "view is default, edit is deliberate" model. The OS could recognize "command references" within text content via content-type awareness.

### Radical Minimalism

~12,277 lines for the entire OS (kernel, compiler, editor, utilities). Kernel: ~2,000 lines. Complete system fits on a floppy. Two part-time programmers, three years.

Achieved via: single user, single address space, cooperative multitasking, no overlapping windows, language-enforced safety. "A single person can know and implement the whole system."

**Not for us** in specifics (no hardware isolation, no preemption, no multi-user). But the _principle_ — radical simplicity in connective tissue, complexity only in leaf nodes — aligns with our Decision #4.

### Viewer/Frame Model

Screen → tracks (vertical columns) → viewers (horizontal rectangles) → frames (nested content areas). Message-based communication. The OS manages layout; modules manage their own content. Separates "viewer management" from "content handling."

---

## Spring — Doors, VM/IPC Unification, Naming

### Doors (IPC)

Capability-based endpoints for synchronous cross-domain procedure calls. A door = program counter entry point + integer datum (object ID). `door_call` trap: kernel transfers control directly to target domain's door PC. No scheduler involved.

**Shuttles:** Thread identity crosses domain boundaries. Calling thread continues execution in target domain. Same thread, different address space. Eliminates Mach's problem of waking server threads.

**Performance:** ~11μs round-trip (fast path) vs Mach's ~95μs (9x slower). Fast path: ≤16 simple values, ~100 SPARC instructions. Bulk path: VM remapping (COW) for large data.

**Not directly applicable:** Our editors and OS service are independent processes with different scheduling contexts. Async ring buffers fit better. But Spring's shuttle concept (thread identity crossing domains) is intellectually related to our context donation (billing crossing domains).

### Subcontracts

Programmable layer between client stubs and transport. Controls marshaling, invocation, and management. Different subcontracts for: simple local door call, table-based grouping, replicated objects, cached objects. Decouple _what_ (interface) from _how_ (protocol).

**Relevance:** Our content-type rebase handlers serve a similar role — customize behavior (text rebase, image rebase, audio rebase) behind the stable edit protocol interface.

### VM/IPC Unification (VALIDATES OUR ARCHITECTURE)

File data and IPC data share the same memory abstraction. A file handed between processes references the same physical pages — no double caching. Files as memory objects mapped directly into address spaces.

**Our design already does this:** Documents memory-mapped into both editor and OS service. Ring buffers carry only control messages. Spring validates this separation.

### Naming Contexts

Contexts are objects containing name→object bindings. First-class, composable, hierarchical. Ordered merge contexts (union mounts). Access control per binding.

**Relevance:** Our query-based navigation (Decision #7) rejects hierarchical paths as the organizing principle. Spring's naming contexts show another approach to composable, non-hierarchical naming.

---

## Actionable Takeaways

### Should Adopt

1. **Typed IPC contracts** (from Singularity) — formalize edit protocol as state machine, generate validation code
2. **Live queries** (from BeOS) — design requirement for our query API (Decision #7)
3. **Per-type attribute definitions** (from BeOS) — MIME database records what attributes each type carries
4. **Retroactive indexing** (lesson from BeOS's failure to do this)

### Should Inform Design Discussions

5. **Text-as-command** (from Oberon) — concrete model for Decision #17 (Interaction Model), CLI/GUI parity
6. **Error model** (from Midori) — bugs vs recoverable errors framing for OS service design
7. **Translator chaining** (from BeOS limitation) — should our translators compose automatically?
8. **Attribute portability** (from BeOS problem) — can metadata survive export to external formats?
9. **Files are a feature** (from Phantom's failure) — files provide isolation, format boundaries, natural undo points

### Validates Existing Decisions

10. Decision #5 (File Understanding) — BeOS MIME as filesystem metadata, proven for 25+ years
11. Decision #7 (File Organization) — BeOS query-based navigation, proven in practice
12. Decision #14 (Compound Documents / Translators) — BeOS Translation Kit, same pattern
13. IPC: ring buffers for control + memory mapping for data — Spring's VM/IPC unification validates this
14. "No save" via COW filesystem — Phantom's problems prove our approach is safer
15. EEVDF + scheduling contexts — stronger than BeOS's fixed priorities and Midori's unsolved resource management
16. Hardware isolation — right choice vs Singularity/Midori's language-only isolation (we need multi-language editors)
