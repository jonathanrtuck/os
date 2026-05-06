//! Schedule/block/wake integration — connects syscall handlers to the
//! scheduler and context switch.
//!
//! Blocking syscalls call `block_current()` which marks the thread as Blocked,
//! picks the next runnable thread, and calls context_switch(). When the
//! blocking condition is met, the waker calls `wake()` which marks the thread
//! as Ready and enqueues it.

use crate::{frame::state, thread::ThreadRunState, types::ThreadId};

/// Block the current thread and switch to the next runnable thread.
///
/// The caller must have already placed the current thread on a wait queue
/// before calling this. Returns after the thread is woken and context-switched
/// back to.
pub fn block_current(current: ThreadId, core_id: usize) {
    state::threads()
        .write(current.0)
        .unwrap()
        .set_state(ThreadRunState::Blocked);
    switch_away(current, core_id);
}

/// Wake a blocked thread by marking it Ready and enqueuing it.
pub fn wake(thread_id: ThreadId, core_id: usize) {
    let priority = {
        let Some(mut thread) = state::threads().write(thread_id.0) else {
            return;
        };

        if thread.state() != ThreadRunState::Blocked {
            return;
        }

        let priority = thread.effective_priority();

        thread.set_state(ThreadRunState::Ready);

        priority
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

/// Pick the next thread and switch to it.
///
/// On bare metal, saves current thread's registers and loads the new thread's
/// via `frame::arch::context::context_switch()`. On host (tests), this is a
/// state-machine-only operation — no actual register switching.
#[inline(never)]
fn switch_away(_current: ThreadId, core_id: usize) {
    let next_id = {
        let mut pcs = state::schedulers().core(core_id).lock();
        let Some(next_id) = pcs.pick_next() else {
            pcs.set_current(None);

            return;
        };

        next_id
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

        do_context_switch(_current, next_id);
    }
}

/// Bare-metal context switch — saves current thread's RegisterState,
/// loads next thread's, switches kernel stack.
#[cfg(target_os = "none")]
fn do_context_switch(old_id: ThreadId, new_id: ThreadId) {
    crate::frame::arch::context::switch_threads(old_id.0, new_id.0);
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
    fn wake_non_blocked_thread_is_noop() {
        setup();

        let tid = add_thread(Priority::Medium);

        wake(tid, 0);

        assert_eq!(state::schedulers().core(0).lock().total_ready(), 0);
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
}
