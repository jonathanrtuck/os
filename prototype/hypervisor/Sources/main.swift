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

/// Global flag set by SIGUSR1 signal handler. Checked by VirtioMetal on each frame.
nonisolated(unsafe) var _signalCaptureFlag: Bool = false
/// Global reference to the Metal backend for signal-triggered captures.
nonisolated(unsafe) weak var _metalBackendForCapture: VirtioMetalBackend?

func main() throws {
    let args = CommandLine.arguments
    let noGpu = args.contains("--no-gpu")

    // Parse --capture N PATH (capture frame N to PATH, then exit)
    var captureFrame: Int = -1
    var capturePath: String = "/tmp/hypervisor-capture.png"
    if let idx = args.firstIndex(of: "--capture"), idx + 2 < args.count {
        captureFrame = Int(args[idx + 1]) ?? -1
        capturePath = args[idx + 2]
    }

    guard args.filter({ !$0.hasPrefix("-") && !$0.hasPrefix("/") }).count >= 2
          || args.contains(where: { $0.hasSuffix("kernel") || $0.hasSuffix(".elf") })
          || args.count >= 2
    else {
        print("Usage: hypervisor <kernel-elf> [options]")
        print("")
        print("  kernel-elf:          Path to the OS kernel ELF binary")
        print("  --verbose:           Enable verbose logging")
        print("  --no-gpu:            Boot without GPU (serial only, no window)")
        print("  --capture N PATH:    Capture frame N as PNG to PATH, then exit")
        print("  SIGUSR1:             Capture next frame to /tmp/hypervisor-capture.png")
        exit(1)
    }

    // Find kernel path: first arg that isn't a flag or the binary itself
    let kernelPath: String = {
        for i in 1..<args.count {
            let a = args[i]
            if a.hasPrefix("--") {
                // Skip flag and its arguments
                if a == "--capture" { continue }
                continue
            }
            // Skip arguments to --capture
            if i >= 2 && args[i - 1] == "--capture" { continue }
            if i >= 3 && args[i - 2] == "--capture" { continue }
            return a
        }
        print("Error: no kernel path specified")
        exit(1)
    }()
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
        backend.captureAtFrame = captureFrame
        backend.capturePath = capturePath
        backend.exitAfterCapture = captureFrame >= 0
        metalBackend = backend
        vm.addVirtioDevice(slot: 3, backend: backend)
        print("  GPU: Metal passthrough (slot 3)")

        // SIGUSR1 triggers ad-hoc screenshot capture.
        _metalBackendForCapture = backend
        signal(SIGUSR1) { _ in
            _signalCaptureFlag = true
        }
    }

    // ── DTB ─────────────────────────────────────────────────────────────

    var dtbDevices: [DTB.DeviceInfo] = []
    for (slot, transport) in vm.virtioDevices.sorted(by: { $0.key < $1.key }) {
        dtbDevices.append(DTB.DeviceInfo(slot: slot, deviceId: transport.backend.deviceId))
    }

    let cpuCount = 4
    let dtb = DTB.minimal(ramBase: 0x4000_0000, ramSize: 256 * 1024 * 1024,
                          cpuCount: cpuCount, virtioDevices: dtbDevices)
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
                try vm.run(entryPoint: entry, dtbAddress: dtbAddr, cpuCount: cpuCount)
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
        try vm.run(entryPoint: entry, dtbAddress: dtbAddr, cpuCount: cpuCount)
    }
}

try main()
