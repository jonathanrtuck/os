//! Host-side tests for exit code storage and retrieval.
//!
//! Tests verify the exit code lifecycle: default sentinel, voluntary exit
//! stores a user code, killed processes retain the sentinel, and retrieval
//! works correctly for exited vs running processes.
//!
//! Uses minimal models (cannot import kernel modules directly on the host).

// ============================================================
// Minimal models
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessId(u32);

/// Sentinel value for involuntary termination / not yet exited.
const EXIT_CODE_SENTINEL: i64 = i64::MIN;

/// Mirrors the exit_code field on Process.
struct Process {
    exit_code: i64,
    killed: bool,
}

impl Process {
    fn new() -> Self {
        Self {
            exit_code: EXIT_CODE_SENTINEL,
            killed: false,
        }
    }
}

/// Minimal exit code store (mirrors process_exit module's exit_codes Vec).
struct ExitCodeStore {
    codes: Vec<Option<i64>>,
    exited: Vec<bool>,
}

impl ExitCodeStore {
    fn new() -> Self {
        Self {
            codes: Vec::new(),
            exited: Vec::new(),
        }
    }

    fn create(&mut self, pid: ProcessId) {
        let idx = pid.0 as usize;

        if idx >= self.codes.len() {
            self.codes.resize(idx + 1, None);
            self.exited.resize(idx + 1, false);
        }

        self.codes[idx] = None;
        self.exited[idx] = false;
    }

    fn notify_exit(&mut self, pid: ProcessId, exit_code: i64) {
        let idx = pid.0 as usize;

        if idx >= self.codes.len() {
            self.codes.resize(idx + 1, None);
            self.exited.resize(idx + 1, false);
        }

        self.codes[idx] = Some(exit_code);
        self.exited[idx] = true;
    }

    fn get_exit_code(&self, pid: ProcessId) -> Option<i64> {
        let idx = pid.0 as usize;

        self.codes.get(idx).copied().flatten()
    }

    fn check_exited(&self, pid: ProcessId) -> bool {
        let idx = pid.0 as usize;

        self.exited.get(idx).copied().unwrap_or(false)
    }

    fn destroy(&mut self, pid: ProcessId) {
        let idx = pid.0 as usize;

        if idx < self.codes.len() {
            self.codes[idx] = None;
            self.exited[idx] = false;
        }
    }
}

// ============================================================
// Tests: Process exit_code field
// ============================================================

#[test]
fn process_exit_code_default_is_sentinel() {
    let process = Process::new();

    assert_eq!(
        process.exit_code, EXIT_CODE_SENTINEL,
        "default exit_code must be i64::MIN"
    );
}

#[test]
fn process_exit_code_sentinel_is_i64_min() {
    assert_eq!(EXIT_CODE_SENTINEL, i64::MIN);
    assert_eq!(EXIT_CODE_SENTINEL, -9223372036854775808);
}

#[test]
fn process_exit_code_voluntary_exit_stores_code() {
    let mut process = Process::new();

    // Simulate sys_exit setting the code before exit.
    process.exit_code = 42;

    assert_eq!(process.exit_code, 42);
}

#[test]
fn process_exit_code_voluntary_exit_zero() {
    let mut process = Process::new();

    process.exit_code = 0;

    assert_eq!(process.exit_code, 0);
}

#[test]
fn process_exit_code_voluntary_exit_negative() {
    let mut process = Process::new();

    process.exit_code = -1;

    assert_eq!(process.exit_code, -1);
}

#[test]
fn process_exit_code_voluntary_exit_large_positive() {
    let mut process = Process::new();

    process.exit_code = i64::MAX;

    assert_eq!(process.exit_code, i64::MAX);
}

#[test]
fn process_exit_code_killed_retains_sentinel() {
    let mut process = Process::new();

    // process_kill does NOT set exit_code — it stays at the sentinel.
    process.killed = true;

    assert_eq!(
        process.exit_code, EXIT_CODE_SENTINEL,
        "killed process must retain sentinel exit code"
    );
}

#[test]
fn process_exit_code_distinguishes_voluntary_from_killed() {
    let mut voluntary = Process::new();

    voluntary.exit_code = 0;

    let mut killed = Process::new();

    killed.killed = true;
    // killed.exit_code stays at i64::MIN

    assert_ne!(
        voluntary.exit_code, killed.exit_code,
        "voluntary exit(0) must be distinguishable from killed"
    );
}

// ============================================================
// Tests: Exit code store (mirrors process_exit module)
// ============================================================

#[test]
fn exit_code_store_no_code_before_exit() {
    let mut store = ExitCodeStore::new();

    store.create(ProcessId(0));

    assert_eq!(store.get_exit_code(ProcessId(0)), None);
    assert!(!store.check_exited(ProcessId(0)));
}

#[test]
fn exit_code_store_voluntary_exit() {
    let mut store = ExitCodeStore::new();
    let pid = ProcessId(0);

    store.create(pid);
    store.notify_exit(pid, 42);

    assert!(store.check_exited(pid));
    assert_eq!(store.get_exit_code(pid), Some(42));
}

#[test]
fn exit_code_store_killed_process() {
    let mut store = ExitCodeStore::new();
    let pid = ProcessId(1);

    store.create(pid);
    // Kill path passes EXIT_CODE_SENTINEL.
    store.notify_exit(pid, EXIT_CODE_SENTINEL);

    assert!(store.check_exited(pid));
    assert_eq!(store.get_exit_code(pid), Some(EXIT_CODE_SENTINEL));
}

#[test]
fn exit_code_store_retrieval_fails_for_running() {
    let mut store = ExitCodeStore::new();
    let pid = ProcessId(0);

    store.create(pid);

    // Process hasn't exited yet.
    assert!(!store.check_exited(pid));
    assert_eq!(store.get_exit_code(pid), None);
}

#[test]
fn exit_code_store_retrieval_fails_for_unknown_pid() {
    let store = ExitCodeStore::new();

    assert_eq!(store.get_exit_code(ProcessId(99)), None);
    assert!(!store.check_exited(ProcessId(99)));
}

#[test]
fn exit_code_store_destroy_clears_code() {
    let mut store = ExitCodeStore::new();
    let pid = ProcessId(0);

    store.create(pid);
    store.notify_exit(pid, 7);

    assert_eq!(store.get_exit_code(pid), Some(7));

    store.destroy(pid);

    assert_eq!(store.get_exit_code(pid), None);
}

#[test]
fn exit_code_store_multiple_processes() {
    let mut store = ExitCodeStore::new();
    let pid_a = ProcessId(0);
    let pid_b = ProcessId(1);
    let pid_c = ProcessId(2);

    store.create(pid_a);
    store.create(pid_b);
    store.create(pid_c);

    store.notify_exit(pid_a, 0);
    store.notify_exit(pid_b, EXIT_CODE_SENTINEL); // killed
    store.notify_exit(pid_c, -1);

    assert_eq!(store.get_exit_code(pid_a), Some(0));
    assert_eq!(store.get_exit_code(pid_b), Some(EXIT_CODE_SENTINEL));
    assert_eq!(store.get_exit_code(pid_c), Some(-1));
}

#[test]
fn exit_code_store_pid_reuse_after_destroy() {
    let mut store = ExitCodeStore::new();
    let pid = ProcessId(0);

    // First lifecycle.
    store.create(pid);
    store.notify_exit(pid, 10);

    assert_eq!(store.get_exit_code(pid), Some(10));

    store.destroy(pid);

    assert_eq!(store.get_exit_code(pid), None);

    // Second lifecycle (PID reused).
    store.create(pid);

    assert_eq!(store.get_exit_code(pid), None);

    store.notify_exit(pid, 20);

    assert_eq!(store.get_exit_code(pid), Some(20));
}

#[test]
fn exit_code_survives_until_destroy() {
    let mut store = ExitCodeStore::new();
    let pid = ProcessId(0);

    store.create(pid);
    store.notify_exit(pid, 99);

    // Code remains retrievable after exit and before destroy.
    assert_eq!(store.get_exit_code(pid), Some(99));
    assert_eq!(store.get_exit_code(pid), Some(99)); // idempotent
    assert!(store.check_exited(pid));
}

#[test]
fn exit_code_cast_round_trip() {
    // Verify that the i64 → u64 → i64 cast used in the syscall ABI is lossless.
    let values: &[i64] = &[0, 1, -1, 42, -42, i64::MAX, i64::MIN, i64::MIN + 1];

    for &code in values {
        let as_u64 = code as u64;
        let back = as_u64 as i64;

        assert_eq!(
            back, code,
            "round-trip failed for {code}: u64={as_u64:#x}"
        );
    }
}
