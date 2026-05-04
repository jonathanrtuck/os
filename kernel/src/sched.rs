//! Schedule/block/wake integration — connects syscall handlers to the
//! scheduler and context switch.
//!
//! Blocking syscalls call `block_current()` which marks the thread as Blocked,
//! picks the next runnable thread, and calls context_switch(). When the
//! blocking condition is met, the waker calls `wake()` which marks the thread
//! as Ready and enqueues it.

use crate::{syscall::Kernel, thread::ThreadRunState, types::ThreadId};

/// Block the current thread and switch to the next runnable thread.
///
/// The caller must have already placed the current thread on a wait queue
/// before calling this. Returns after the thread is woken and context-switched
/// back to.
pub fn block_current(kernel: &mut Kernel, current: ThreadId, core_id: usize) {
    let thread = kernel.threads.get_mut(current.0).unwrap();

    thread.set_state(ThreadRunState::Blocked);
    switch_away(kernel, current, core_id);
}

/// Wake a blocked thread by marking it Ready and enqueuing it.
pub fn wake(kernel: &mut Kernel, thread_id: ThreadId, core_id: usize) {
    let thread = match kernel.threads.get_mut(thread_id.0) {
        Some(t) => t,
        None => return,
    };

    if thread.state() != ThreadRunState::Blocked {
        return;
    }

    let priority = thread.effective_priority();

    thread.set_state(ThreadRunState::Ready);
    kernel.scheduler.enqueue(core_id, thread_id, priority);
}

/// Yield the current thread — move it to the back of its run queue and
/// switch to the next runnable thread.
pub fn yield_current(kernel: &mut Kernel, current: ThreadId, core_id: usize) {
    let thread = kernel.threads.get(current.0).unwrap();
    let priority = thread.effective_priority();

    kernel.scheduler.core_mut(core_id).rotate_current(priority);

    switch_away(kernel, current, core_id);
}

/// Exit the current thread — mark it Exited and switch away. Never returns
/// on bare metal (context switches to a different thread).
pub fn exit_current(kernel: &mut Kernel, current: ThreadId, core_id: usize, code: u32) {
    let thread = kernel.threads.get_mut(current.0).unwrap();

    thread.exit(code);

    switch_away(kernel, current, core_id);
}

/// Pick the next thread and switch to it.
///
/// On bare metal, saves current thread's registers and loads the new thread's
/// via `frame::arch::context::context_switch()`. On host (tests), this is a
/// state-machine-only operation — no actual register switching.
fn switch_away(kernel: &mut Kernel, _current: ThreadId, core_id: usize) {
    let next_id = match kernel.scheduler.pick_next(core_id) {
        Some(id) => id,
        None => return,
    };
    let next = kernel.threads.get_mut(next_id.0).unwrap();

    next.set_state(ThreadRunState::Running);

    #[cfg(target_os = "none")]
    if _current != next_id {
        do_context_switch(kernel, _current, next_id);
    }

    kernel
        .scheduler
        .core_mut(core_id)
        .set_current(Some(next_id));
}

/// Bare-metal context switch — saves current thread's RegisterState,
/// loads next thread's, switches kernel stack.
///
/// Uses `ObjectTable::get_pair_mut` for safe dual-reference extraction
/// (split_at_mut internally, zero unsafe).
#[cfg(target_os = "none")]
fn do_context_switch(kernel: &mut Kernel, old_id: ThreadId, new_id: ThreadId) {
    let (old_thread, new_thread) = kernel
        .threads
        .get_pair_mut(old_id.0, new_id.0)
        .expect("context switch: both threads must exist");
    let old_rs = old_thread.init_register_state();
    let new_rs = new_thread
        .register_state()
        .expect("new thread has no RegisterState");

    crate::frame::arch::context::context_switch(old_rs, new_rs);
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{
        address_space::AddressSpace,
        thread::Thread,
        types::{AddressSpaceId, Priority, ThreadId},
    };

    fn setup() -> Box<Kernel> {
        let mut k = Box::new(Kernel::new(1));
        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        k.spaces.alloc(space);

        k
    }

    fn add_thread(k: &mut Kernel, priority: Priority) -> ThreadId {
        let thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(0)),
            priority,
            0x1000,
            0x2000,
            0,
        );
        let (idx, _) = k.threads.alloc(thread).unwrap();

        k.threads.get_mut(idx).unwrap().id = ThreadId(idx);
        k.threads
            .get_mut(idx)
            .unwrap()
            .set_state(ThreadRunState::Running);

        ThreadId(idx)
    }

    #[test]
    fn block_marks_thread_blocked() {
        let mut k = setup();
        let tid = add_thread(&mut k, Priority::Medium);
        let t2 = add_thread(&mut k, Priority::Medium);

        k.scheduler.enqueue(0, t2, Priority::Medium);

        block_current(&mut k, tid, 0);

        assert_eq!(
            k.threads.get(tid.0).unwrap().state(),
            ThreadRunState::Blocked
        );
    }

    #[test]
    fn wake_marks_thread_ready_and_enqueues() {
        let mut k = setup();
        let tid = add_thread(&mut k, Priority::Medium);

        k.threads
            .get_mut(tid.0)
            .unwrap()
            .set_state(ThreadRunState::Blocked);

        wake(&mut k, tid, 0);

        assert_eq!(k.threads.get(tid.0).unwrap().state(), ThreadRunState::Ready);
        assert_eq!(k.scheduler.core(0).total_ready(), 1);
    }

    #[test]
    fn wake_nonexistent_thread_is_noop() {
        let mut k = setup();

        wake(&mut k, ThreadId(999), 0);
    }

    #[test]
    fn wake_non_blocked_thread_is_noop() {
        let mut k = setup();
        let tid = add_thread(&mut k, Priority::Medium);

        wake(&mut k, tid, 0);

        assert_eq!(k.scheduler.core(0).total_ready(), 0);
    }

    #[test]
    fn exit_marks_thread_exited() {
        let mut k = setup();
        let tid = add_thread(&mut k, Priority::Medium);

        exit_current(&mut k, tid, 0, 42);

        let thread = k.threads.get(tid.0).unwrap();

        assert_eq!(thread.state(), ThreadRunState::Exited);
        assert_eq!(thread.exit_code(), Some(42));
    }

    #[test]
    fn block_then_wake_cycle() {
        let mut k = setup();
        let t1 = add_thread(&mut k, Priority::Medium);
        let t2 = add_thread(&mut k, Priority::Medium);

        k.scheduler.enqueue(0, t2, Priority::Medium);

        block_current(&mut k, t1, 0);

        assert_eq!(
            k.threads.get(t1.0).unwrap().state(),
            ThreadRunState::Blocked
        );

        wake(&mut k, t1, 0);

        assert_eq!(k.threads.get(t1.0).unwrap().state(), ThreadRunState::Ready);
    }
}
