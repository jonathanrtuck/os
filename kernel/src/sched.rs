//! Schedule/block/wake integration — connects syscall handlers to the
//! scheduler and context switch.
//!
//! Blocking syscalls call `block_current()` which marks the thread as Blocked,
//! picks the next runnable thread, and calls context_switch(). When the
//! blocking condition is met, the waker calls `wake()` which marks the thread
//! as Ready and enqueues it.

use crate::{
    frame::state,
    thread::ThreadRunState,
    types::{SyscallError, ThreadId},
};

/// Block the current thread and switch to the next runnable thread.
///
/// The caller must have already placed the current thread on a wait queue
/// before calling this. Returns after the thread is woken and context-switched
/// back to.
///
/// If a pending wakeup arrived (another core called `wake()` while this
/// thread was still Running), returns immediately without blocking. The
/// thread slot lock serializes the pending_wake check with `wake()`'s
/// state check, closing the missed-wakeup race window.
pub fn block_current(current: ThreadId, core_id: usize) {
    {
        let mut thread = state::threads().write(current.0).unwrap();

        if thread.take_pending_wake() {
            return;
        }

        thread.set_state(ThreadRunState::Blocked);
    }

    switch_away(current, core_id);
}

/// Block with a deadline — same as `block_current`, but arms the per-core
/// timer. If `deadline_tick` arrives before any other wake, the timer ISR
/// sets `wakeup_error = TimedOut` and wakes the thread.
///
/// `deadline_tick` is an absolute counter value from `clock_read`. If the
/// deadline has already passed, the thread is woken immediately with TimedOut.
pub fn block_with_deadline(current: ThreadId, core_id: usize, deadline_tick: u64) {
    use crate::frame::arch::timer;

    let now = timer::now();

    if now >= deadline_tick {
        let mut thread = state::threads().write(current.0).unwrap();

        thread.set_wakeup_error(SyscallError::TimedOut);

        return;
    }

    {
        let mut thread = state::threads().write(current.0).unwrap();

        if thread.take_pending_wake() {
            return;
        }

        thread.set_state(ThreadRunState::Blocked);
    }

    timer::set_deadline_thread(core_id, current);
    timer::set_deadline(core_id, deadline_tick.saturating_sub(timer::now()).max(1));

    switch_away(current, core_id);

    timer::clear_deadline_thread(core_id);
}

/// Wake a blocked thread by marking it Ready and enqueuing it.
///
/// If the thread is Running (hasn't called `block_current()` yet), sets
/// a pending wake flag instead of enqueuing. `block_current()` checks
/// this flag under the same slot lock, so the wakeup cannot be lost.
pub fn wake(thread_id: ThreadId, core_id: usize) {
    let priority = {
        let Some(mut thread) = state::threads().write(thread_id.0) else {
            return;
        };

        match thread.state() {
            ThreadRunState::Blocked => {
                let priority = thread.effective_priority();

                thread.set_state(ThreadRunState::Ready);

                priority
            }
            ThreadRunState::Running => {
                thread.set_pending_wake();

                return;
            }
            _ => return,
        }
    };

    state::schedulers()
        .core(core_id)
        .lock()
        .enqueue(thread_id, priority);
}

/// Yield the current thread — move it to the back of its run queue and
/// switch to the next runnable thread.
pub fn yield_current(current: ThreadId, core_id: usize) {
    let priority = state::threads()
        .read(current.0)
        .unwrap()
        .effective_priority();

    state::schedulers()
        .core(core_id)
        .lock()
        .rotate_current(priority);

    switch_away(current, core_id);
}

/// Exit the current thread — mark it Exited and switch away. Never returns
/// on bare metal (context switches to a different thread).
pub fn exit_current(current: ThreadId, core_id: usize, code: u32) {
    state::threads().write(current.0).unwrap().exit(code);

    switch_away(current, core_id);
}

/// Pick the next thread and switch to it. On bare metal, if no runnable
/// thread is available, saves the current thread's RegisterState (for SMP
/// direct_switch safety), switches to the idle stack, and enters a bounded
/// WFE spin before falling through to the full idle loop.
#[inline(never)]
fn switch_away(_current: ThreadId, core_id: usize) {
    let next_id = {
        let mut pcs = state::schedulers().core(core_id).lock();

        match pcs.pick_next() {
            Some(id) => Some(id),
            None => {
                pcs.set_current(None);

                None
            }
        }
    };
    let Some(next_id) = next_id else {
        #[cfg(target_os = "none")]
        crate::frame::arch::idle::park_and_wait(_current.0, core_id);

        return;
    };

    state::threads()
        .write(next_id.0)
        .unwrap()
        .set_state(ThreadRunState::Running);
    state::schedulers()
        .core(core_id)
        .lock()
        .set_current(Some(next_id));

    #[cfg(target_os = "none")]
    if _current != next_id {
        crate::frame::arch::cpu::set_current_thread(next_id.0);

        maybe_switch_page_table(next_id);
        do_context_switch(_current, next_id);
    }
}

/// Direct process switch — block the current thread and resume the target
/// without touching the run queue. The caller must have already prepared
/// the target's wakeup state (message delivery, wakeup value, etc.).
///
/// Used on the IPC call fast path: when a server is already waiting in
/// recv, the kernel switches directly caller→server instead of
/// enqueue→dequeue through the scheduler.
pub fn direct_switch(blocker: ThreadId, target: ThreadId, core_id: usize) {
    state::threads()
        .write(blocker.0)
        .unwrap()
        .set_state(ThreadRunState::Blocked);
    state::threads()
        .write(target.0)
        .unwrap()
        .set_state(ThreadRunState::Running);
    state::schedulers()
        .core(core_id)
        .lock()
        .set_current(Some(target));

    #[cfg(target_os = "none")]
    {
        crate::frame::arch::cpu::set_current_thread(target.0);

        maybe_switch_page_table(target);
        do_context_switch(blocker, target);
    }
}

/// Pre-extracted target thread data for fast IPC switching.
pub struct SwitchTarget {
    pub thread_id: ThreadId,
    pub space_id: u32,
    pub ht_ptr: usize,
    pub pt_root: usize,
    pub pt_asid: u8,
}

/// Direct process switch using pre-extracted target data. Avoids redundant
/// thread/space table lookups — the caller already holds everything needed.
///
/// Saves 6 slot lock acquisitions vs `direct_switch`: set_current_thread
/// (thread read + space get), maybe_switch_page_table (thread read +
/// space read), and separate set_state writes (2 thread writes folded
/// into switch_threads_set_states).
pub fn direct_switch_fast(blocker: ThreadId, target: &SwitchTarget, core_id: usize) {
    state::schedulers()
        .core(core_id)
        .lock()
        .set_current(Some(target.thread_id));

    #[cfg(target_os = "none")]
    {
        crate::frame::arch::cpu::set_current_thread_fast(
            target.thread_id.0,
            target.space_id,
            target.ht_ptr,
            target.pt_root,
            target.pt_asid,
        );

        switch_to_page_table_if_needed(target.pt_root, target.pt_asid);
        crate::frame::arch::context::switch_threads_set_states(
            blocker.0,
            ThreadRunState::Blocked,
            target.thread_id.0,
            ThreadRunState::Running,
        );
    }

    // Host-target fallback: still need state changes for tests.
    #[cfg(not(target_os = "none"))]
    {
        state::threads()
            .write(blocker.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);
        state::threads()
            .write(target.thread_id.0)
            .unwrap()
            .set_state(ThreadRunState::Running);
    }
}

/// Wake a blocked thread and switch directly to it, placing the current
/// thread on the run queue as Ready. Used on the IPC reply path when the
/// caller should preempt the server.
pub fn wake_and_switch(woken: ThreadId, current: ThreadId, core_id: usize) {
    let current_pri = state::threads()
        .read(current.0)
        .unwrap()
        .effective_priority();

    state::threads()
        .write(current.0)
        .unwrap()
        .set_state(ThreadRunState::Ready);
    state::threads()
        .write(woken.0)
        .unwrap()
        .set_state(ThreadRunState::Running);

    {
        let mut sched = state::schedulers().core(core_id).lock();

        sched.enqueue(current, current_pri);
        sched.set_current(Some(woken));
    }

    #[cfg(target_os = "none")]
    {
        crate::frame::arch::cpu::set_current_thread(woken.0);
        maybe_switch_page_table(woken);

        do_context_switch(current, woken);
    }
}

/// Wake and switch using pre-extracted target data and known current priority.
/// Saves 6 slot lock acquisitions vs `wake_and_switch` (same as
/// `direct_switch_fast` — see its doc comment).
pub fn wake_and_switch_fast(
    woken: &SwitchTarget,
    current: ThreadId,
    current_pri: crate::types::Priority,
    core_id: usize,
) {
    {
        let mut sched = state::schedulers().core(core_id).lock();

        sched.enqueue(current, current_pri);
        sched.set_current(Some(woken.thread_id));
    }

    #[cfg(target_os = "none")]
    {
        crate::frame::arch::cpu::set_current_thread_fast(
            woken.thread_id.0,
            woken.space_id,
            woken.ht_ptr,
            woken.pt_root,
            woken.pt_asid,
        );

        switch_to_page_table_if_needed(woken.pt_root, woken.pt_asid);
        crate::frame::arch::context::switch_threads_set_states(
            current.0,
            ThreadRunState::Ready,
            woken.thread_id.0,
            ThreadRunState::Running,
        );
    }

    #[cfg(not(target_os = "none"))]
    {
        state::threads()
            .write(current.0)
            .unwrap()
            .set_state(ThreadRunState::Ready);
        state::threads()
            .write(woken.thread_id.0)
            .unwrap()
            .set_state(ThreadRunState::Running);
    }
}

/// Bare-metal context switch — saves current thread's RegisterState,
/// loads next thread's, switches kernel stack.
#[cfg(target_os = "none")]
fn do_context_switch(old_id: ThreadId, new_id: ThreadId) {
    crate::frame::arch::context::switch_threads(old_id.0, new_id.0);
}

/// Switch TTBR0 to a known page table, skipping all table lookups.
/// Used when the caller already has the physical root and ASID.
#[allow(dead_code)]
pub(crate) fn switch_to_page_table(_pt_root: usize, _asid: u8) {
    #[cfg(target_os = "none")]
    if _pt_root != 0 {
        crate::frame::arch::page_table::switch_table(
            crate::frame::arch::page_alloc::PhysAddr(_pt_root),
            crate::frame::arch::page_table::Asid(_asid),
        );
    }
}

/// Conditional TTBR0 switch — reads TTBR0 first and skips MSR+ISB if
/// already pointing at the target page table. Saves ~10-15 cycles on
/// the IPC fast path where the STTR space switch already set TTBR0.
pub(crate) fn switch_to_page_table_if_needed(_pt_root: usize, _asid: u8) {
    #[cfg(target_os = "none")]
    if _pt_root != 0 {
        crate::frame::arch::page_table::switch_table_if_needed(
            crate::frame::arch::page_alloc::PhysAddr(_pt_root),
            crate::frame::arch::page_table::Asid(_asid),
        );
    }
}

/// Switch TTBR0 to the target thread's address space page table if it
/// differs from the currently active one. Required when context-switching
/// across address spaces (e.g., init → service). Reads current TTBR0
/// first and skips the MSR+ISB if already correct.
#[cfg(target_os = "none")]
fn maybe_switch_page_table(target: ThreadId) {
    let (pt_root, asid) = {
        let Some(t) = state::threads().read(target.0) else {
            return;
        };
        let Some(space_id) = t.address_space() else {
            return;
        };

        drop(t);

        let Some(space) = state::spaces().read(space_id.0) else {
            return;
        };

        (space.page_table_root(), space.asid())
    };

    if pt_root != 0 {
        crate::frame::arch::page_table::switch_table_if_needed(
            crate::frame::arch::page_alloc::PhysAddr(pt_root),
            crate::frame::arch::page_table::Asid(asid),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        address_space::AddressSpace,
        thread::Thread,
        types::{AddressSpaceId, Priority, ThreadId},
    };

    fn setup() {
        state::init(1);

        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        state::spaces().alloc_shared(space);
    }

    fn add_thread(priority: Priority) -> ThreadId {
        let thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(0)),
            priority,
            0x1000,
            0x2000,
            0,
        );
        let (idx, _) = state::threads().alloc_shared(thread).unwrap();

        state::threads().write(idx).unwrap().id = ThreadId(idx);
        state::threads()
            .write(idx)
            .unwrap()
            .set_state(ThreadRunState::Running);

        ThreadId(idx)
    }

    #[test]
    fn block_marks_thread_blocked() {
        setup();

        let tid = add_thread(Priority::Medium);
        let t2 = add_thread(Priority::Medium);

        state::schedulers()
            .core(0)
            .lock()
            .enqueue(t2, Priority::Medium);

        block_current(tid, 0);

        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Blocked
        );
    }

    #[test]
    fn wake_marks_thread_ready_and_enqueues() {
        setup();

        let tid = add_thread(Priority::Medium);

        state::threads()
            .write(tid.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);

        wake(tid, 0);

        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Ready
        );
        assert_eq!(state::schedulers().core(0).lock().total_ready(), 1);
    }

    #[test]
    fn wake_nonexistent_thread_is_noop() {
        setup();
        wake(ThreadId(999), 0);
    }

    #[test]
    fn wake_running_thread_sets_pending_wake() {
        setup();

        let tid = add_thread(Priority::Medium);

        wake(tid, 0);

        assert_eq!(state::schedulers().core(0).lock().total_ready(), 0);
        assert!(state::threads().write(tid.0).unwrap().take_pending_wake());
    }

    #[test]
    fn pending_wake_prevents_blocking() {
        setup();

        let tid = add_thread(Priority::Medium);

        wake(tid, 0);
        block_current(tid, 0);

        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Running,
            "thread should stay Running when pending_wake consumed"
        );
    }

    #[test]
    fn pending_wake_consumed_only_once() {
        setup();

        let tid = add_thread(Priority::Medium);
        let t2 = add_thread(Priority::Medium);

        state::schedulers()
            .core(0)
            .lock()
            .enqueue(t2, Priority::Medium);

        wake(tid, 0);
        block_current(tid, 0);

        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Running,
        );

        block_current(tid, 0);

        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Blocked,
            "second block should succeed — pending_wake was consumed"
        );
    }

    #[test]
    fn exit_marks_thread_exited() {
        setup();

        let tid = add_thread(Priority::Medium);

        exit_current(tid, 0, 42);

        let thread = state::threads().read(tid.0).unwrap();

        assert_eq!(thread.state(), ThreadRunState::Exited);
        assert_eq!(thread.exit_code(), Some(42));
    }

    #[test]
    fn block_then_wake_cycle() {
        setup();

        let t1 = add_thread(Priority::Medium);
        let t2 = add_thread(Priority::Medium);

        state::schedulers()
            .core(0)
            .lock()
            .enqueue(t2, Priority::Medium);

        block_current(t1, 0);

        assert_eq!(
            state::threads().read(t1.0).unwrap().state(),
            ThreadRunState::Blocked
        );

        wake(t1, 0);

        assert_eq!(
            state::threads().read(t1.0).unwrap().state(),
            ThreadRunState::Ready
        );
    }

    #[test]
    fn block_with_no_other_thread_clears_current() {
        setup();

        let tid = add_thread(Priority::Medium);

        state::schedulers().core(0).lock().set_current(Some(tid));

        block_current(tid, 0);

        assert_eq!(state::schedulers().core(0).lock().current(), None);
        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Blocked
        );
    }

    #[test]
    fn exit_with_no_other_thread_clears_current() {
        setup();

        let tid = add_thread(Priority::Medium);

        state::schedulers().core(0).lock().set_current(Some(tid));

        exit_current(tid, 0, 0);

        assert_eq!(state::schedulers().core(0).lock().current(), None);
    }

    #[test]
    fn yield_then_pick_returns_same_thread() {
        setup();

        let tid = add_thread(Priority::Medium);

        state::schedulers().core(0).lock().set_current(Some(tid));

        yield_current(tid, 0);

        assert_eq!(state::schedulers().core(0).lock().current(), Some(tid));
        assert_eq!(
            state::threads().read(tid.0).unwrap().state(),
            ThreadRunState::Running,
        );
    }

    #[test]
    fn yield_with_two_threads_switches() {
        setup();

        let t1 = add_thread(Priority::Medium);
        let t2 = add_thread(Priority::Medium);

        state::schedulers()
            .core(0)
            .lock()
            .enqueue(t2, Priority::Medium);
        state::schedulers().core(0).lock().set_current(Some(t1));

        yield_current(t1, 0);

        assert_eq!(state::schedulers().core(0).lock().current(), Some(t2));
        assert_eq!(
            state::threads().read(t2.0).unwrap().state(),
            ThreadRunState::Running
        );
    }

    #[test]
    fn direct_switch_blocks_caller_runs_target() {
        setup();

        let caller = add_thread(Priority::Medium);
        let server = add_thread(Priority::Medium);

        state::threads()
            .write(server.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);
        state::schedulers().core(0).lock().set_current(Some(caller));

        direct_switch(caller, server, 0);

        assert_eq!(
            state::threads().read(caller.0).unwrap().state(),
            ThreadRunState::Blocked
        );
        assert_eq!(
            state::threads().read(server.0).unwrap().state(),
            ThreadRunState::Running
        );
        assert_eq!(state::schedulers().core(0).lock().current(), Some(server));
    }

    #[test]
    fn direct_switch_does_not_touch_run_queue() {
        setup();

        let caller = add_thread(Priority::Medium);
        let server = add_thread(Priority::Medium);
        let bystander = add_thread(Priority::High);

        state::threads()
            .write(server.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);
        state::schedulers()
            .core(0)
            .lock()
            .enqueue(bystander, Priority::High);
        state::schedulers().core(0).lock().set_current(Some(caller));

        let before = state::schedulers().core(0).lock().total_ready();

        direct_switch(caller, server, 0);

        let after = state::schedulers().core(0).lock().total_ready();

        assert_eq!(before, after, "run queue should be unchanged");
    }

    #[test]
    fn wake_and_switch_swaps_threads() {
        setup();

        let server = add_thread(Priority::Medium);
        let caller = add_thread(Priority::Medium);

        state::threads()
            .write(caller.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);
        state::schedulers().core(0).lock().set_current(Some(server));

        wake_and_switch(caller, server, 0);

        assert_eq!(
            state::threads().read(caller.0).unwrap().state(),
            ThreadRunState::Running
        );
        assert_eq!(
            state::threads().read(server.0).unwrap().state(),
            ThreadRunState::Ready
        );
        assert_eq!(state::schedulers().core(0).lock().current(), Some(caller));
        assert_eq!(
            state::schedulers().core(0).lock().total_ready(),
            1,
            "server should be on run queue"
        );
    }

    #[test]
    fn wake_and_switch_with_higher_priority_server_on_queue() {
        setup();

        let server = add_thread(Priority::High);
        let caller = add_thread(Priority::Medium);

        state::threads()
            .write(caller.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);
        state::schedulers().core(0).lock().set_current(Some(server));

        wake_and_switch(caller, server, 0);

        assert_eq!(
            state::schedulers().core(0).lock().current(),
            Some(caller),
            "caller runs now; server on queue will preempt at next schedule point"
        );

        let next = state::schedulers().core(0).lock().pick_next();

        assert_eq!(next, Some(server), "server is next in run queue");
    }
}
