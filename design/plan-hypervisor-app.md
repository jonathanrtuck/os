# Plan: Native macOS Hypervisor App

Replace QEMU with a minimal, Swift-native ARM64 hypervisor that uses Metal for display and virglrenderer for GPU translation. The goal is ES 3.1+ rendering (MSAA, compute) for the OS.

## Principle

Validate assumptions at the highest leverage point first (claim 4ef8b1fb). The GPU/Metal/virglrenderer integration is the riskiest part and the entire reason for building this. Spike it first. If it doesn't work, nothing else matters.

## Phase 0: Spike — Can virglrenderer + ANGLE Vulkan + MoltenVK render a triangle via Metal?

**No VM, no virtio, no guest OS.** Just a macOS app that:

1. Creates a CAMetalLayer window
2. Initializes EGL via ANGLE (Vulkan backend, MoltenVK)
3. Creates a virglrenderer context
4. Submits one Gallium3D command buffer (clear + draw triangle)
5. Displays the result in the Metal layer

This validates the entire GPU assumption in isolation. If this fails, we debug it without the complexity of a VM. If it works, everything else is straightforward plumbing.

**Deliverable:** A macOS app window showing a colored triangle rendered through the virglrenderer → ANGLE Vulkan → MoltenVK → Metal chain. Then test with `nr_samples=4` to confirm MSAA works.

## Phase 1: Minimal VM — Boot to serial output

Hypervisor.framework VM that boots the OS kernel to the point where it prints to serial. No devices except a UART.

1. Create VM, map guest memory, load kernel ELF
2. Create 4 vCPUs, run them
3. Handle MMIO exits (dispatch to virtual UART)
4. DTB with just the UART

**Validates:** Hypervisor.framework basics, vCPU execution, MMIO trapping.

## Phase 2: GICv3 + Timer

Add interrupt controller and timer emulation so the scheduler works.

1. GICv3 distributor + redistributor MMIO emulation
2. SGI delivery (cross-core IPIs for scheduler wakeup)
3. ARM generic timer (CNTV\_\* registers, IRQ injection)
4. SMP boot (PSCI CPU_ON handling)

**Validates:** Multi-core execution, interrupt delivery, scheduling.

## Phase 3: virtio MMIO transport

Generic virtio MMIO register emulation (device discovery, feature negotiation, virtqueue setup). Reusable for all virtio devices.

**Validates:** The foundation for all device communication.

## Phase 4: virtio-blk + virtio-9p

File access so the OS can load fonts and test images.

- virtio-blk: simple sector read
- virtio-9p: 9P2000.L host filesystem passthrough

**Validates:** Data can flow from host to guest.

## Phase 5: virtio-input

Keyboard and tablet event forwarding from NSEvent.

**Validates:** Interactive input works.

## Phase 6: virtio-gpu + Metal display

Wire up the Phase 0 spike into the VM:

1. virtio-gpu MMIO device with VIRGL feature bit
2. 3D context creation, resource management, command submission
3. virglrenderer processes Gallium3D commands → ANGLE Vulkan → Metal
4. Scanout to CAMetalLayer
5. Test: boot OS, see the full UI with anti-aliased star edges

**Validates:** The whole point — ES 3.1 GPU rendering in the guest.

## Order rationale

Phase 0 is first because it's the highest-leverage validation. If virglrenderer can't render through ANGLE Vulkan on macOS outside of QEMU, nothing else matters.

Phases 1-5 are ordered by dependency (each needs the previous) and can't block on Phase 0 since they don't involve the GPU.

Phase 6 combines Phase 0's spike with the VM from Phases 1-5.
