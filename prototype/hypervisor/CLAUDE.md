# hypervisor

Native macOS hypervisor app using Hypervisor.framework. Boots the OS kernel with Metal GPU-accelerated rendering via direct Metal command passthrough.

## Current Phase: Metal Render Pipeline

Full VM with Metal passthrough GPU. The guest's metal-render driver emits serialized Metal commands over a custom virtio device (ID 22). The host VirtioMetal device replays them via the Metal API — zero translation layers.

## Build & Run

```sh
# Build + sign
make build && make sign

# Run with Metal GPU display (opens macOS window)
make run

# Run with verbose logging
make run-verbose

# Run without GPU (serial only, no window)
make run-serial
```

Or manually:
```sh
swift build
codesign --entitlements hypervisor.entitlements --force -s - .build/debug/hypervisor

# Metal GPU mode (no external dependencies needed):
.build/debug/hypervisor ../../system/target/aarch64-unknown-none/release/kernel

# Serial-only mode (no GPU):
.build/debug/hypervisor ../../system/target/aarch64-unknown-none/release/kernel --no-gpu
```

## Architecture

```
Sources/
├── main.swift              — Entry point, device registration, threading
├── VirtualMachine.swift    — VM creation, guest memory, ELF loader, GIC
├── VCPU.swift              — vCPU execution loop, MMIO/HVC/timer exits
├── PL011.swift             — PL011 UART (serial output)
├── DTB.swift               — FDT generator
├── VirtioMMIO.swift        — Generic virtio MMIO transport
├── VirtqueueHelper.swift   — Descriptor chain read/write helpers
├── Virtio9P.swift          — 9P2000.L filesystem backend
├── VirtioInput.swift       — Keyboard/tablet input backend
├── MetalProtocol.swift     — Metal command wire format (IDs, enums)
├── VirtioMetal.swift       — Metal passthrough device (deserialize + replay)
├── AppWindow.swift         — NSWindow + CAMetalLayer + input forwarding
├── VirtioGPU.swift         — OLD: virglrenderer path (to be removed)
└── GPUBridge/              — OLD: virglrenderer C bridge (to be removed)
```

## Virtio Device Slots

| Slot | Device         | IRQ (SPI) | Description                         |
|------|----------------|-----------|-------------------------------------|
| 0    | virtio-9p      | 48        | Host filesystem passthrough         |
| 1    | virtio-input   | 49        | Keyboard (evdev)                    |
| 2    | virtio-input   | 50        | Tablet / absolute pointer           |
| 3    | virtio-metal   | 51        | Metal command passthrough (ID 22)   |

## Metal Rendering Chain

```
Guest (metal-render)
  → Metal commands (protocol::metal::CommandBuffer)
  → virtio queue 0 (setup) / queue 1 (per-frame)
  → VirtioMetalBackend (Swift)
  → Metal API (direct replay)
  → CAMetalLayer (NSWindow)
```

No intermediate translation layers. Replaces virglrenderer + ANGLE + MoltenVK.

## Protocol

Wire format: `[u16 method_id][u16 flags][u32 payload_size][payload...]`

- **Queue 0 (setup):** shader compilation, pipeline creation, texture creation/upload
- **Queue 1 (render):** per-frame command buffers (draw calls, present)
- **Handle model:** guest pre-assigns u32 IDs, host maps to real Metal objects
- **DRAWABLE_HANDLE (0xFFFFFFFF):** special handle that acquires the CAMetalLayer drawable

Guest protocol: `system/libraries/protocol/metal.rs`
Host protocol: `Sources/MetalProtocol.swift` + `Sources/VirtioMetal.swift`

## Dependencies

- **Hypervisor.framework** — Apple's hardware virtualization (macOS 15+)
- **Metal.framework** — Apple's GPU API (system framework, no external deps)

## Key Technical Details

- **Platform:** macOS 15+ (for `hv_gic_create`)
- **Entitlement:** `com.apple.security.hypervisor`
- **Guest RAM:** 256 MiB at PA 0x40000000
- **Kernel load:** ELF at PA 0x40080000, DTB at PA 0x40000000
- **GIC:** Hardware-backed via `hv_gic_create` (Apple Silicon native GICv3)
- **Threading:** Main thread = NSApplication, VM boot + vCPUs = background threads
- **GPU:** VirtioMetal processes commands on a dedicated GPU thread
- **MSAA:** 4x, native Metal — render to MSAA texture, resolve to drawable
- **Shaders:** MSL source compiled at runtime via MTLDevice.makeLibrary(source:)
