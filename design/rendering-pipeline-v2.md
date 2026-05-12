# Rendering Pipeline v2 — Implementation Plan

Five architectural changes to reach the physical performance limit. Organized
into dependency-ordered waves. Every file, interface, and verification step is
specified — execution is mechanical.

## Measured baseline (from pipeline metrics, 2026-05-12)

| Stage                   | Time       | After v2    |
| ----------------------- | ---------- | ----------- |
| Scene walk + vertex gen | 4-8us      | 4-8us       |
| Atlas upload (dirty)    | 1-2ms      | 1-2ms       |
| **Video frame upload**  | **23ms**   | **0ms**     |
| GPU render + present    | 750us avg  | <500us      |
| VideoToolbox decode     | 200us-25ms | unchanged   |
| Scene read (seqlock)    | ~0 + buggy | ~0, correct |
| Invisible space walk    | 4us wasted | 0           |
| CPU path rasterize      | ~50us/path | ~5us GPU    |

## Dependency graph

```text
Wave 1: Active-space-only scene ─────────┐
Wave 2: Double-buffered scene graph ─────┤
                                         ├──> Wave 4: Async frame submission
Wave 3: Zero-copy video frames ──────────┘
Wave 5: GPU compute path rendering (independent)
```

Waves 1, 2, 3, 5 are independent. Wave 4 benefits from all prior waves being
stable but is not strictly blocked.

---

## Wave 1 — Active-Space-Only Scene

**Goal:** The presenter only builds scene nodes for visible space(s). The
compositor never walks off-screen content.

**Root cause addressed:** Showcase's continuous animations drive 120fps
rendering even when viewing the text or video space. Wasted CPU + GPU + power.

### Changes

**`user/servers/presenter/src/build.rs`**

Current: `build_scene` iterates ALL spaces (line 1237
`for (idx, space) in self.spaces.iter().enumerate()`), builds nodes for each,
positions them at `base_x = idx * display_width` within the strip container.

Change: Only build spaces that are visible or transitioning into view.

```rust
fn visible_space_range(&self) -> (usize, usize) {
    if self.slide_animating {
        let current_pos = self.slide_spring.value();
        let target_pos = self.slide_spring.target();
        let lo = (current_pos.min(target_pos) / self.display_width as f32).floor() as usize;
        let hi = (current_pos.max(target_pos) / self.display_width as f32).ceil() as usize;
        (lo.min(self.spaces.len() - 1), hi.min(self.spaces.len() - 1))
    } else {
        (self.active_space, self.active_space)
    }
}
```

In `build_scene`, replace `for (idx, space) in self.spaces.iter().enumerate()`
with iteration over `visible_space_range()` only. The strip container's width
remains `num_spaces * display_width` (so child_offset_x math is unchanged), but
only visible spaces have nodes allocated.

**`user/servers/presenter/src/main.rs`**

The `needs_continuous_render` check (line 1284) currently checks only the active
space. No change needed — if the active space is Text or Video, `needs_anim` is
false, and the presenter sleeps until the next clock tick. The fix in
`build_scene` ensures the Showcase animations don't exist in the scene graph at
all when not visible.

### Verification

- Boot to text space. Compositor metrics should show `r=1` (clock tick only),
  NOT `r=120`.
- Switch to Showcase. Metrics should show `r=120 s=120` (continuous animation).
- Switch back to text. Metrics should drop to `r=1` within 1 second.
- Slide animation between any two spaces should be smooth (both source and
  target visible during transition).

---

## Wave 2 — Double-Buffered Scene Graph

**Goal:** Replace the seqlock with two scene buffers. Presenter writes to back,
atomically swaps. Compositor reads front. No torn reads, no
writer-blocks-reader.

**Root cause addressed:** The seqlock `commit()` without `clear()` is a no-op
(confirmed bug). Incremental updates (clock) are invisible. The seqlock also
blocks readers during full `build_scene` writes.

### Data structures

**New shared swap header (16 bytes, page-aligned VMO):**

```rust
#[repr(C)]
struct SceneSwapHeader {
    active_index: AtomicU32,  // 0 or 1 — which buffer the compositor should read
    generation: AtomicU32,    // bumped on every swap, so compositor can detect changes
    _pad: [u8; 8],
}
```

The presenter allocates:

- 1 swap header VMO (1 page = 16 KiB, only 16 bytes used)
- 2 scene buffer VMOs (each SCENE_SIZE = 176 KiB, rounded to 192 KiB)

The compositor receives all three handles via the updated `comp::SETUP`.

### Protocol changes

**`user/libraries/render/src/lib.rs` — `comp` module**

Change `comp::SETUP`:

- Old: presenter sends 1 handle (scene VMO)
- New: presenter sends 3 handles (swap header VMO, scene buffer 0, scene
  buffer 1)
- SetupReply stays the same (display_width, display_height, refresh_hz)

**`user/libraries/scene/src/writer.rs`**

Remove `clear()`, `commit()`, `begin_read()`, `end_read()`. These are replaced
by the swap mechanism. `SceneWriter::new()` and `from_existing()` stay.

Add:

```rust
impl SceneWriter<'_> {
    pub fn reset(&mut self) {
        // Zero the header, reset node_count/root/data_used.
        // Does NOT touch generation — that's in the swap header.
    }
}
```

**`user/libraries/scene/src/reader.rs`**

Remove `begin_read()` / `end_read()` seqlock protocol. `SceneReader::new()`
stays. The reader now trusts the buffer is complete (guaranteed by the swap).

### Presenter changes (`user/servers/presenter/src/main.rs`)

Startup:

```rust
let swap_vmo = abi::vmo::create(PAGE_SIZE, 0)?;
let swap_va = abi::vmo::map(swap_vmo, 0, Rights::READ_WRITE_MAP)?;
// Initialize: active_index=0, generation=0
let scene_vmos = [
    abi::vmo::create(scene_vmo_size, 0)?,
    abi::vmo::create(scene_vmo_size, 0)?,
];
let scene_vas = [
    abi::vmo::map(scene_vmos[0], 0, Rights::READ_WRITE_MAP)?,
    abi::vmo::map(scene_vmos[1], 0, Rights::READ_WRITE_MAP)?,
];
let scene_bufs = [
    unsafe { core::slice::from_raw_parts_mut(scene_vas[0] as *mut u8, SCENE_SIZE) },
    unsafe { core::slice::from_raw_parts_mut(scene_vas[1] as *mut u8, SCENE_SIZE) },
];
```

The presenter struct stores both buffers and the swap VA. `build_scene()`:

```rust
fn build_scene(&mut self) {
    // Write to the BACK buffer (the one NOT currently active).
    let active = self.read_active_index();
    let back = 1 - active;
    let mut scene = SceneWriter::new(self.scene_bufs[back]);
    // ... build entire scene into back buffer ...
    // Atomic swap: make back buffer the new front.
    self.swap_generation += 1;
    // Release ordering: all scene writes visible before the swap.
    unsafe {
        let hdr = self.swap_va as *const SceneSwapHeader;
        (*hdr).generation.store(self.swap_generation, Ordering::Release);
        (*hdr).active_index.store(back as u32, Ordering::Release);
    }
}
```

`update_clock()` now works correctly: it writes to the back buffer, swaps. The
generation always advances because it's a separate counter from the scene data.

### Compositor changes (`user/servers/drivers/render/src/main.rs`)

`comp::SETUP` handler: map all 3 VMOs. Store `swap_va`, `scene_vas[2]`.

Replace `check_scene_dirty()`:

```rust
fn check_scene_dirty(&mut self) -> bool {
    let gen = unsafe {
        let hdr = self.swap_va as *const SceneSwapHeader;
        (*hdr).generation.load(Ordering::Acquire)
    };
    if gen != self.last_scene_gen {
        self.last_scene_gen = gen;
        true
    } else {
        false
    }
}
```

Replace `render_frame`'s scene reading:

```rust
let active = unsafe {
    let hdr = self.swap_va as *const SceneSwapHeader;
    (*hdr).active_index.load(Ordering::Acquire)
};
let scene_buf = unsafe {
    core::slice::from_raw_parts(self.scene_vas[active as usize] as *const u8, SCENE_SIZE)
};
let reader = SceneReader::new(scene_buf);
// No begin_read/end_read needed — buffer is complete and immutable
// until the next swap.
```

Remove the seqlock retry logic and `seqlock_retry_count` metric.

### Verification

- All visual regression tests pass (screenshot comparison).
- `update_clock()` changes are visible: compositor shows `s=1` per second (clock
  tick), not `s=0`.
- Rapid typing while scene rebuilds: no torn frames (compare screenshots of
  every 10th frame during fast input).
- Stress test: presenter does build_scene in a tight loop while compositor
  renders. No corruption, no crashes.

---

## Wave 3 — Zero-Copy Video Frames

**Goal:** The compositor references the host's IOSurface-backed Metal texture
directly. No pixel uploads through virtio.

**Root cause addressed:** `upload_image_slot` copies 8MB through 64 synchronous
virtio round-trips, taking 23ms per 1080p frame. The host already has the
decoded frame as a Metal texture in the shared TextureRegistry.

### Wire protocol addition

**`user/libraries/render/src/lib.rs`**

New setup command:

```rust
pub const CMD_BIND_HOST_TEXTURE: u16 = 0x0022;
```

CommandWriter method:

```rust
pub fn bind_host_texture(&mut self, guest_tex_id: u32, host_handle: u32) {
    self.header(CMD_BIND_HOST_TEXTURE, 8);
    self.u32(guest_tex_id);
    self.u32(host_handle);
}
```

Semantics: "Make `guest_tex_id` resolve to the host texture identified by
`host_handle`." After this command, any draw call referencing `guest_tex_id`
uses the host's texture. The host looks up `host_handle` in TextureRegistry.

**`/Users/user/Sites/hypervisor/Sources/VirtioMetal.swift`**

In `dispatchSetupCommand`:

```swift
case .bindHostTexture:
    guard size >= 8 else { return }
    let guestId = payload.loadUnaligned(fromByteOffset: 0, as: UInt32.self)
    let hostHandle = payload.loadUnaligned(fromByteOffset: 4, as: UInt32.self)
    if let tex = textureRegistry.lookup(hostHandle) {
        textureRegistry.register(guest: guestId, texture: tex)
    }
```

Add `bindHostTexture = 0x0022` to `MetalSetupCommand` enum in
`MetalProtocol.swift`.

### Comp protocol addition

**`user/libraries/render/src/lib.rs` — `comp` module**

New method:

```rust
pub const BIND_HOST_TEXTURE: u32 = 7;

pub struct BindHostTextureRequest {
    pub content_id: u32,
    pub host_handle: u32,
    pub width: u16,
    pub height: u16,
}
```

This replaces `UPLOAD_IMAGE` for host-decoded content. The presenter sends the
host texture handle (obtained from the codec-decode CREATE_SESSION reply)
instead of a pixel VMO.

### Video decoder changes (`user/servers/video-decoder/src/main.rs`)

The video decoder's `finish_open` currently:

1. Creates output_vmo for pixel data
2. Sets up hardware session → gets texture_handle from codec-decode
3. Decodes first frame into output_vmo
4. Replies with output_vmo handle

Change: also return the texture_handle in the OpenReply.

**`user/servers/video-decoder/src/lib.rs`** (protocol crate):

```rust
pub struct OpenReply {
    pub width: u32,
    pub height: u32,
    pub ns_per_frame: u64,
    pub total_frames: u32,
    pub host_texture_handle: u32,  // NEW
}
```

The video decoder returns `self.codec_session_id`'s texture_handle from the
codec-decode CREATE_SESSION reply. The host-side VirtioVideoDecode already
updates this texture on every decode via `updateTexture()`.

### Presenter changes (`user/servers/presenter/src/main.rs`)

`load_video_space` / `try_open_video`: Read the new `host_texture_handle` from
OpenReply. Store in `Space::Video`. Send `comp::BIND_HOST_TEXTURE` to the
compositor instead of `comp::UPLOAD_IMAGE`.

```rust
Space::Video {
    decoder_ep,
    frame_vmo: Handle(0),  // no longer needed for pixel transfer
    frame_va: 0,
    content_id,
    width, height,
    total_frames,
    playing: false,
    host_texture_handle,  // NEW
}
```

### Compositor changes (`user/servers/drivers/render/src/main.rs`)

Handle `comp::BIND_HOST_TEXTURE`:

```rust
comp::BIND_HOST_TEXTURE => {
    let req = BindHostTextureRequest::read_from(msg.payload);
    if let Some(idx) = self.find_or_alloc_image_slot(req.content_id) {
        // Send CMD_BIND_HOST_TEXTURE to host
        let dma_buf = /* setup DMA */;
        let len = {
            let mut w = CommandWriter::new(dma_buf);
            w.bind_host_texture(TEX_IMAGE + idx as u32, req.host_handle);
            w.len()
        };
        submit_and_wait(/* setup queue, len */);

        self.images[idx] = ImageSlot {
            content_id: req.content_id,
            tex_id: TEX_IMAGE + idx as u32,
            va: 0,       // no guest mapping needed
            w: req.width,
            h: req.height,
            pixel_size: 0,
            pixel_offset: 0,
            last_gen: 0,
            tex_created: true,
            is_live: true,
            host_bound: true,  // NEW flag
        };
    }
}
```

`refresh_live_images`: For `host_bound` images, skip the gen counter check and
pixel upload entirely. The host's TextureRegistry.update() is called
automatically by VirtioVideoDecode after each decode. The Metal draw call reads
the latest texture on each frame.

But the compositor still needs to know WHEN a new frame is available (to trigger
a re-render). Two options:

**Option A — Generation counter (keep existing VMO):** The video decoder still
bumps the gen counter in the output VMO. The compositor checks it to decide
whether to re-render, but doesn't upload pixels. Cost: one atomic read per vsync
(~0). The output VMO becomes a 16-byte signal buffer instead of an 8MB pixel
buffer.

**Option B — Eliminate the VMO entirely.** Add a new comp IPC method
`FRAME_READY(content_id)` that the video decoder calls after each decode. The
compositor marks the image dirty and re-renders on the next vsync.

Option A is simpler and avoids adding IPC traffic (60 IPC calls/sec at 60fps
video). The gen counter read is a single atomic load.

**Decision: Option A.** Keep the output VMO as a 16-byte signal buffer. Reduce
its allocation size. The compositor reads the gen counter to trigger re-render
but does NOT upload pixels.

Change `refresh_live_images`:

```rust
if img.host_bound {
    // Read gen counter from signal VMO
    let current_gen = unsafe {
        let ptr = img.va as *const AtomicU64;
        (*ptr).load(Ordering::Acquire)
    };
    if current_gen != img.last_gen {
        self.images[i].last_gen = current_gen;
        // NO upload — host texture is already current.
        // Just re-bind in case host rotated the IOSurface.
        let dma_buf = /* setup DMA */;
        let len = {
            let mut w = CommandWriter::new(dma_buf);
            w.bind_host_texture(img.tex_id, img.host_handle);
            w.len()
        };
        submit_and_wait(/* setup queue, len */);
        changed = true;
    }
}
```

Cost per video frame: one atomic read + one 8-byte virtio command. ~10us instead
of 23ms.

### Verification

- Video playback: `lu=` metric should show upload time ~10us, not 23ms.
- Video should play at full frame rate (60fps for the test video).
- Screenshot comparison: video frame should appear identical to the pixel-upload
  path.
- Static images (JPEG) still use the existing UPLOAD_IMAGE path (one-time upload
  at startup). Verify they still render correctly.

---

## Wave 4 — Async Frame Submission

**Goal:** CPU builds frame N+1 while GPU renders frame N.

**Root cause addressed:** `submit_and_wait` blocks the compositor's CPU for the
entire GPU render time (750us avg, 4.5ms max). With async submission, the
compositor can start the next scene walk immediately.

### Guest changes (`user/servers/drivers/render/src/main.rs`)

Two render DMA buffers:

```rust
render_dma: [init::DmaBuf; 2],
render_dma_idx: usize,      // 0 or 1
in_flight: bool,             // true if previous frame not yet completed
```

New functions:

```rust
fn submit_async(device, vq, queue_index, dma_pa, cmd_len) {
    vq.push(dma_pa, cmd_len as u32, false);
    device.notify(queue_index);
    // Return immediately — don't wait for GPU.
}

fn ensure_completed(vq, irq_event, device) {
    if vq.pop_used().is_none() {
        let _ = abi::event::wait(&[(irq_event, 0x1)]);
        device.ack_interrupt();
        let _ = abi::event::clear(irq_event, 0x1);
        vq.pop_used();
    }
}
```

Render loop becomes:

1. `ensure_completed` on the in-flight buffer (blocks only if GPU is slow).
2. Build frame into the now-available buffer.
3. Intermediate blur submissions: `submit_and_wait` (synchronous, same buffer).
4. Final submission: `submit_async` (non-blocking).
5. Swap `render_dma_idx`.

### Host changes (`/Users/user/Sites/hypervisor/Sources/VirtioMetal.swift`)

Replace synchronous `waitUntilCompleted` with semaphore + completion handler:

```swift
private let frameSemaphore = DispatchSemaphore(value: 2)

// In presentAndCommit:
frameSemaphore.wait()
cb.addCompletedHandler { [weak self] _ in
    self?.frameSemaphore.signal()
    // Move virtio completion here:
    self?.completeVirtioRequest(state: state, vm: vm)
}
cb.commit()
// Return immediately — don't wait for GPU.
```

The virtio used-ring write and guest IRQ injection move into the
`addCompletedHandler` callback, firing only when the GPU actually finishes.

**Critical:** The guest's `ensure_completed` now actually waits for the IRQ
(which fires from the completion handler), not just for the notify return. This
is a behavioral change from the current synchronous model where the notify
itself blocks for the full GPU render.

### Verification

- All visual regression tests pass.
- Metrics: time from `submit_async` to next CPU work < 0.1ms.
- Stress test: rapid scene changes at 120Hz. No tearing, no corruption.
- GPU utilization: verify overlap by logging CPU work start time vs previous GPU
  completion time.

---

## Wave 5 — GPU Compute Path Rendering

**Goal:** Replace the CPU scanline path rasterizer with GPU compute tiling.

**Root cause addressed:** The CPU rasterizer (`path.rs`, 330 lines) works for
cached icons but doesn't scale to complex vector content. The compute shader
infrastructure already exists in the protocol and host.

### MSL compute kernel

New shader source added to `MSL_SOURCE` in the render driver:

```metal
// Path tile accumulation kernel.
// Input: flattened line segments as (x0, y0, x1, y1) float4 array.
// Output: coverage texture (R8, same dimensions as target region).
//
// Each thread handles one pixel row within a tile. Accumulates winding
// number from all segments that cross the row, converts to coverage.

kernel void path_coverage(
    device const float4* segments [[buffer(0)]],
    device const uint& segment_count [[buffer(1)]],
    device const float4& bounds [[buffer(2)]],    // x, y, w, h
    texture2d<float, access::write> output [[texture(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    // ... winding accumulation per scanline ...
    // ... 4x oversampling for anti-aliasing ...
    // ... write coverage to output texture ...
}
```

The kernel processes all segments for one output pixel. Grid dimensions = output
texture size. Threadgroup size = (16, 16, 1).

### Guest protocol additions (`user/libraries/render/src/lib.rs`)

Add CommandWriter methods for compute path rendering (these map to existing
host-side command dispatch):

```rust
pub const CMD_BEGIN_COMPUTE_PASS: u16 = 0x0200;
pub const CMD_END_COMPUTE_PASS: u16 = 0x0201;
pub const CMD_SET_COMPUTE_PIPELINE: u16 = 0x0210;
pub const CMD_SET_COMPUTE_TEXTURE: u16 = 0x0211;
pub const CMD_SET_COMPUTE_BYTES: u16 = 0x0212;
pub const CMD_DISPATCH_THREADS: u16 = 0x0220;
pub const CMD_CREATE_COMPUTE_PIPELINE: u16 = 0x0011;
```

### Compositor changes (`user/servers/drivers/render/src/main.rs`)

**Setup:** Compile compute library, create compute pipeline state:

```rust
w.compile_library(H_COMPUTE_LIB, PATH_COMPUTE_MSL);
w.get_function(H_PATH_COVERAGE_FN, H_COMPUTE_LIB, b"path_coverage");
w.create_compute_pipeline(H_PIPE_PATH_COVERAGE, H_PATH_COVERAGE_FN);
```

**Path rasterization** — replace CPU `path::rasterize_path()`:

Current flow (`lookup_or_rasterize_path` in main.rs):

1. Check atlas cache by (content_hash, size)
2. If miss: call `path::rasterize_path()` (CPU) → coverage buffer
3. Pack coverage into atlas
4. Return atlas UV coordinates

New flow:

1. Check atlas cache — same as before.
2. If miss: flatten cubics on CPU (reuse `path::flatten()` — cheap, sequential).
   Upload flattened segments to a small GPU buffer via `CMD_SET_COMPUTE_BYTES`.
3. Dispatch `path_coverage` compute kernel. Output to a temporary R8 texture.
4. Copy the temporary texture region into the atlas texture via blit encoder.
5. Return atlas UV coordinates — same as before.

The interface to the rest of the compositor doesn't change. The atlas still
holds R8 coverage bitmaps. Draw calls still use `Pipe::Glyph` with atlas UVs.

**`user/servers/drivers/render/src/path.rs`**

Keep `flatten()` (cubic subdivision → line segments). Remove `rasterize_path()`
and `accumulate_coverage()` — these move to the GPU kernel.

Add `fn flatten_to_buffer(commands, buf) -> usize` that writes (x0, y0, x1, y1)
float4 tuples into a flat buffer suitable for the compute shader.

### Verification

- All icons render identically (screenshot comparison with CPU rasterizer).
- Cursor shapes (arrow, I-beam, pointer) render correctly.
- Content::Path nodes render correctly (fill and stroke).
- Performance: measure rasterization time for a complex icon set. GPU path
  should be 5-10x faster than CPU for uncached paths.
- Atlas cache still works: repeated lookups don't re-dispatch compute.

---

## Implementation order and time budget

| Wave | Scope                       | Est. effort |
| ---- | --------------------------- | ----------- |
| 1    | Active-space-only scene     | 1 hour      |
| 2    | Double-buffered scene graph | 3-4 hours   |
| 3    | Zero-copy video frames      | 3-4 hours   |
| 4    | Async frame submission      | 2-3 hours   |
| 5    | GPU compute path rendering  | 6-8 hours   |

Total: ~15-20 hours of implementation.

## Cleanup after all waves

- Remove `upload_image_slot` dead code (pixel upload path for live images).
- Remove seqlock `clear()`/`begin_read()`/`end_read()` from scene library.
- Remove `path::rasterize_path()` and `path::accumulate_coverage()`.
- Remove ad-hoc diagnostic logging added during investigation (keep the periodic
  metrics system — it's useful for ongoing monitoring).
- Update `design/rendering-pipeline-plan.md` to mark all phases complete.
- Update `STATUS.md` with new architecture summary.

## Metrics to keep permanently

The `PipelineMetrics` struct in the compositor and the host-side timing in
VirtioMetal/VirtioVideoDecode. These report once per second and cost nothing
when idle. They ensure any future performance regression is immediately visible
in the console output.
