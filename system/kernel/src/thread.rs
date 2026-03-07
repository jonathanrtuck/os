//! Kernel thread representation.

use super::addr_space::AddressSpace;
use super::handle::HandleTable;
use super::Context;
use alloc::boxed::Box;
use core::alloc::Layout;

pub const STACK_SIZE: usize = 64 * 1024;
pub const KERNEL_STACK_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ThreadId(pub u64);
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Exited,
}
/// A kernel thread.
///
/// `context` MUST be the first field — `TPIDR_EL1` points at the start of
/// the Thread, and exception.S expects the Context at offset 0.
#[repr(C)]
pub struct Thread {
    pub context: Context,
    pub id: ThreadId,
    pub state: ThreadState,
    pub stack_bottom: *mut u8,
    pub address_space: Option<Box<AddressSpace>>,
    pub handles: HandleTable,
}

const _: () = assert!(core::mem::offset_of!(Thread, context) == 0);

unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    pub fn new(id: u64, entry: fn() -> !) -> Box<Self> {
        let stack_layout = Layout::from_size_align(STACK_SIZE, 16).unwrap();
        let stack_bottom = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };

        assert!(!stack_bottom.is_null());

        let stack_top = unsafe { stack_bottom.add(STACK_SIZE) } as u64;
        let mut ctx: Context = unsafe { core::mem::zeroed() };

        ctx.elr = entry as *const () as u64;
        ctx.sp = stack_top;
        ctx.spsr = 0b0101; // EL1h, DAIF clear
        ctx.x[30] = thread_exit as *const () as u64;

        Box::new(Thread {
            context: ctx,
            id: ThreadId(id),
            state: ThreadState::Ready,
            stack_bottom,
            address_space: None,
            handles: HandleTable::new(),
        })
    }
    /// Create a new EL0 (user) thread with its own address space.
    pub fn new_user(
        id: u64,
        addr_space: Box<AddressSpace>,
        entry_va: u64,
        user_stack_top: u64,
    ) -> Box<Self> {
        let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        let stack_bottom = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };

        assert!(!stack_bottom.is_null());

        let kernel_stack_top = unsafe { stack_bottom.add(KERNEL_STACK_SIZE) } as u64;
        let mut ctx: Context = unsafe { core::mem::zeroed() };

        ctx.elr = entry_va;
        ctx.sp = kernel_stack_top;
        ctx.sp_el0 = user_stack_top;
        ctx.spsr = 0b0000; // EL0t, DAIF clear

        Box::new(Thread {
            context: ctx,
            id: ThreadId(id),
            state: ThreadState::Ready,
            stack_bottom,
            address_space: Some(addr_space),
            handles: HandleTable::new(),
        })
    }
}

fn thread_exit() -> ! {
    super::scheduler::exit_current();
}
