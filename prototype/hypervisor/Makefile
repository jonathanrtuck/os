KERNEL = ../../system/target/aarch64-unknown-none/release/kernel
BINARY = .build/debug/hypervisor
ENTITLEMENTS = hypervisor.entitlements

build:
	swift build 2>&1

sign: build
	codesign --entitlements $(ENTITLEMENTS) --force -s - $(BINARY)

# Run with Metal GPU display (opens macOS window)
run: sign
	$(BINARY) $(KERNEL)

# Run with Metal GPU display + verbose logging
run-verbose: sign
	$(BINARY) $(KERNEL) --verbose

# Run without GPU (serial output only, no window)
run-serial: sign
	$(BINARY) $(KERNEL) --no-gpu

clean:
	swift package clean

.PHONY: build sign run run-verbose run-serial clean
