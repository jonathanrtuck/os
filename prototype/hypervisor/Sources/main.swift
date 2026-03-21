/// Native macOS hypervisor — boots the OS kernel with Metal GPU-accelerated rendering.
///
/// Architecture:
///   Main thread:       NSApplication run loop (AppKit window + Metal display)
///   Background thread: VM boot + vCPU execution
///
/// Virtio device slots:
///   0: virtio-9p   (host filesystem access)
///   1: virtio-input (keyboard)
///   2: virtio-input (tablet / absolute pointer)
///   3: virtio-metal (Metal command passthrough — device ID 22)

import Foundation
import AppKit
import Hypervisor
import Metal

func main() throws {
    let args = CommandLine.arguments
    let noGpu = args.contains("--no-gpu")

    guard args.filter({ !$0.hasPrefix("-") }).count >= 2 else {
        print("Usage: hypervisor <kernel-elf> [--verbose] [--no-gpu]")
        print("")
        print("  kernel-elf: Path to the OS kernel ELF binary")
        print("              (system/target/aarch64-unknown-none/release/kernel)")
        print("  --verbose:  Enable verbose logging")
        print("  --no-gpu:   Boot without GPU (serial only, no window)")
        exit(1)
    }

    let kernelPath = args.first(where: { !$0.hasPrefix("-") && $0 != args[0] })!
    let verbose = args.contains("--verbose")

    print("Hypervisor — Native macOS ARM64 VM")
    print("")

    // Load kernel ELF
    let kernelData = try Data(contentsOf: URL(fileURLWithPath: kernelPath))
    print("  Loaded kernel: \(kernelPath) (\(kernelData.count) bytes)")

    // Create and configure the VM
    let vm = try VirtualMachine(
        ramSize: 256 * 1024 * 1024,  // 256 MiB
        ramBase: 0x4000_0000,
        verbose: verbose
    )

    // Load kernel ELF into guest memory
    let entry = try vm.loadKernelELF(kernelData)
    print("  Kernel entry point: 0x\(String(entry, radix: 16))")

    // ── Virtio devices ──────────────────────────────────────────────────

    // Resolve system/share directory from kernel path
    let absKernelPath: String = {
        if kernelPath.hasPrefix("/") { return kernelPath }
        return FileManager.default.currentDirectoryPath + "/" + kernelPath
    }()
    let resolvedShareDir: String = {
        if let range = absKernelPath.range(of: "/system/target/") {
            return String(absKernelPath[..<range.lowerBound]) + "/system/share"
        }
        return (absKernelPath as NSString).deletingLastPathComponent + "/share"
    }()

    // Slot 0: virtio-9p
    let virtio9p = Virtio9PBackend(rootPath: resolvedShareDir)
    vm.addVirtioDevice(slot: 0, backend: virtio9p)
    print("  9P share dir: \(resolvedShareDir)")

    // Slot 1: virtio-input keyboard
    let keyboard = VirtioInputBackend(name: "virtio-keyboard", keyboard: true)
    vm.addVirtioDevice(slot: 1, backend: keyboard)

    // Slot 2: virtio-input tablet
    let tablet = VirtioInputBackend(name: "virtio-tablet", keyboard: false)
    vm.addVirtioDevice(slot: 2, backend: tablet)

    // Slot 3: Metal GPU (if GPU mode)
    var metalBackend: VirtioMetalBackend?
    var appWindow: AppWindow?

    if !noGpu {
        // Create AppWindow on main thread — provides MTLDevice + CAMetalLayer
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)

        let window = AppWindow()
        appWindow = window

        let backend = VirtioMetalBackend(device: window.metalDevice, layer: window.metalLayer)
        backend.verbose = verbose
        metalBackend = backend
        vm.addVirtioDevice(slot: 3, backend: backend)
        print("  GPU: Metal passthrough (slot 3)")
    }

    // ── DTB ─────────────────────────────────────────────────────────────

    var dtbDevices: [DTB.DeviceInfo] = []
    for (slot, transport) in vm.virtioDevices.sorted(by: { $0.key < $1.key }) {
        dtbDevices.append(DTB.DeviceInfo(slot: slot, deviceId: transport.backend.deviceId))
    }

    let dtb = DTB.minimal(ramBase: 0x4000_0000, ramSize: 256 * 1024 * 1024,
                          virtioDevices: dtbDevices)
    let dtbAddr: UInt64 = 0x4000_0000
    vm.writeGuest(at: dtbAddr, data: dtb)
    print("  DTB loaded at 0x\(String(dtbAddr, radix: 16)) (\(dtb.count) bytes)")
    print("")

    // ── Boot ────────────────────────────────────────────────────────────

    if let window = appWindow {
        // GPU mode: main thread = NSApplication, VM on background thread
        print("── Booting kernel (Metal GPU mode) ──")
        print("")

        // Boot VM on background thread
        let vmThread = Thread {
            do {
                try vm.run(entryPoint: entry, dtbAddress: dtbAddr, cpuCount: 4)
            } catch {
                print("VM error: \(error)")
                exit(1)
            }
        }
        vmThread.name = "VM-Boot"
        vmThread.qualityOfService = .userInteractive
        vmThread.start()

        let app = NSApplication.shared
        app.delegate = window

        // Wire input event forwarding
        window.onKeyboardEvent = { type, code, value in
            guard let kbTransport = vm.virtioDevices[1] else { return }
            let kbBackend = kbTransport.backend as! VirtioInputBackend
            kbBackend.injectEvent(
                type: type, code: code, value: value,
                state: kbTransport.currentQueueState(queue: 0),
                vm: vm
            )
        }

        window.onTabletEvent = { type, code, value in
            guard let tabTransport = vm.virtioDevices[2] else { return }
            let tabBackend = tabTransport.backend as! VirtioInputBackend
            tabBackend.injectEvent(
                type: type, code: code, value: value,
                state: tabTransport.currentQueueState(queue: 0),
                vm: vm
            )
        }

        // No onDisplay callback needed — VirtioMetal presents directly via Metal

        // Activate and run the application
        app.activate(ignoringOtherApps: true)
        app.run()
    } else {
        // No GPU: run VM directly on main thread (serial-only mode)
        print("── Booting kernel (serial mode) ──")
        print("")
        try vm.run(entryPoint: entry, dtbAddress: dtbAddr, cpuCount: 4)
    }
}

try main()
