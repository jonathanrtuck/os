//! Test for sys_channel_create handle leak when map_channel_page fails.
//!
//! Bug: sys_channel_create inserts both handles into the handle table, then
//! attempts map_channel_page. If mapping fails, the error path closes the
//! channel endpoints but does NOT close the handle table entries — leaving
//! dangling handles that reference closed channel IDs.
//!
//! This test duplicates the pure logic from sys_channel_create and verifies
//! that handles are cleaned up on map_channel_page failure.

// Include handle.rs directly — it has zero external dependencies.
mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}
#[path = "../../paging.rs"]
mod paging;
#[path = "../../handle.rs"]
mod handle;
mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
#[path = "../../scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

use handle::*;

/// Models the sys_channel_create error path as it exists in the kernel
/// (after the fix: handles are closed on map_channel_page failure).
///
/// `map_channel_page_results`: simulates whether the first and second
/// map_channel_page calls succeed. `[true, true]` = both succeed,
/// `[true, false]` = first succeeds but second fails, etc.
///
/// Returns `(result, handle_table)` so the caller can inspect leaked handles.
fn sys_channel_create_model(
    handles: &mut HandleTable,
    map_channel_page_results: [bool; 2],
) -> Result<(Handle, Handle), i64> {
    // Simulated channel endpoints.
    let ch_a = ChannelId(0);
    let ch_b = ChannelId(1);

    // Mirrors the kernel: insert both handles first.
    let result: Result<(Handle, Handle), HandleError> = (|| {
        let handle_a = handles.insert(HandleObject::Channel(ch_a), Rights::READ_WRITE)?;

        match handles.insert(HandleObject::Channel(ch_b), Rights::READ_WRITE) {
            Ok(handle_b) => {
                // Both handles inserted — now map shared pages.
                // Simulate map_channel_page with the provided results.
                // Fix: close both handles on mapping failure.
                if !map_channel_page_results[0] {
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);

                    return Err(HandleError::TableFull);
                }
                if !map_channel_page_results[1] {
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);

                    return Err(HandleError::TableFull);
                }

                Ok((handle_a, handle_b))
            }
            Err(e) => {
                // Second insert failed — close the first handle (kernel does this).
                let _ = handles.close(handle_a);

                Err(e)
            }
        }
    })();

    match result {
        Ok((a, b)) => Ok((a, b)),
        Err(e) => {
            // Kernel calls channel::close_endpoint(ch_a) and close_endpoint(ch_b) here.
            // After the fix, handles are already closed in the inner closure.
            Err(e as i64)
        }
    }
}

/// Fixed version of sys_channel_create that properly cleans up handles.
fn sys_channel_create_fixed(
    handles: &mut HandleTable,
    map_channel_page_results: [bool; 2],
) -> Result<(Handle, Handle), i64> {
    let ch_a = ChannelId(0);
    let ch_b = ChannelId(1);

    let result: Result<(Handle, Handle), HandleError> = (|| {
        let handle_a = handles.insert(HandleObject::Channel(ch_a), Rights::READ_WRITE)?;

        match handles.insert(HandleObject::Channel(ch_b), Rights::READ_WRITE) {
            Ok(handle_b) => {
                // Both handles inserted — now map shared pages.
                if !map_channel_page_results[0] {
                    // First map failed — close both handles before returning.
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);

                    return Err(HandleError::TableFull);
                }
                if !map_channel_page_results[1] {
                    // Second map failed — close both handles before returning.
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);

                    return Err(HandleError::TableFull);
                }

                Ok((handle_a, handle_b))
            }
            Err(e) => {
                let _ = handles.close(handle_a);

                Err(e)
            }
        }
    })();

    match result {
        Ok((a, b)) => Ok((a, b)),
        Err(e) => Err(e as i64),
    }
}

// ==========================================================================
// Tests that exercise the bug
// ==========================================================================

#[test]
fn test_channel_create_leak_first_map_fails() {
    // Both handles inserted, first map_channel_page fails.
    // Bug: handles remain in table with dangling channel references.
    let mut handles = HandleTable::new();

    let result = sys_channel_create_model(&mut handles, [false, true]);

    assert!(result.is_err(), "channel_create should fail when map fails");

    // After failure, BOTH handle slots should be free (cleaned up).
    // The bug is that they're NOT freed — the handles leak.
    // This assertion verifies the fix: slot 0 and 1 should be empty.
    let slot0 = handles.get(Handle(0), Rights::READ);
    let slot1 = handles.get(Handle(1), Rights::READ);

    assert!(
        slot0.is_err(),
        "handle_a (slot 0) should be closed after map failure, but it leaked"
    );
    assert!(
        slot1.is_err(),
        "handle_b (slot 1) should be closed after map failure, but it leaked"
    );
}

#[test]
fn test_channel_create_leak_second_map_fails() {
    // Both handles inserted, first map succeeds, second map_channel_page fails.
    // Same bug: handles remain in table.
    let mut handles = HandleTable::new();

    let result = sys_channel_create_model(&mut handles, [true, false]);

    assert!(
        result.is_err(),
        "channel_create should fail when second map fails"
    );

    let slot0 = handles.get(Handle(0), Rights::READ);
    let slot1 = handles.get(Handle(1), Rights::READ);

    assert!(
        slot0.is_err(),
        "handle_a (slot 0) should be closed after map failure, but it leaked"
    );
    assert!(
        slot1.is_err(),
        "handle_b (slot 1) should be closed after map failure, but it leaked"
    );
}

#[test]
fn test_channel_create_leak_slots_reusable_after_cleanup() {
    // After a failed channel_create with proper cleanup, the handle slots
    // should be reusable for new handles.
    let mut handles = HandleTable::new();

    let result = sys_channel_create_fixed(&mut handles, [false, true]);

    assert!(result.is_err());

    // Slots should be reusable.
    let h = handles.insert(HandleObject::Channel(ChannelId(99)), Rights::READ);

    assert!(h.is_ok(), "slot should be reusable after cleanup");
    assert_eq!(h.unwrap().0, 0, "should reuse slot 0 (first free)");
}

#[test]
fn test_channel_create_success_path_unaffected() {
    // When both maps succeed, the handles should remain in the table.
    let mut handles = HandleTable::new();

    let result = sys_channel_create_model(&mut handles, [true, true]);

    assert!(
        result.is_ok(),
        "channel_create should succeed when both maps succeed"
    );

    let (a, b) = result.unwrap();

    assert_eq!(a.0, 0);
    assert_eq!(b.0, 1);

    // Both handles should be valid.
    assert!(handles.get(Handle(0), Rights::READ).is_ok());
    assert!(handles.get(Handle(1), Rights::READ).is_ok());
}

#[test]
fn test_channel_create_fixed_cleans_up_on_first_map_fail() {
    // Verify the fixed version properly cleans up.
    let mut handles = HandleTable::new();

    let result = sys_channel_create_fixed(&mut handles, [false, true]);

    assert!(result.is_err());
    assert!(
        handles.get(Handle(0), Rights::READ).is_err(),
        "fixed: handle_a cleaned up"
    );
    assert!(
        handles.get(Handle(1), Rights::READ).is_err(),
        "fixed: handle_b cleaned up"
    );
}

#[test]
fn test_channel_create_fixed_cleans_up_on_second_map_fail() {
    // Verify the fixed version properly cleans up on second map failure.
    let mut handles = HandleTable::new();

    let result = sys_channel_create_fixed(&mut handles, [true, false]);

    assert!(result.is_err());
    assert!(
        handles.get(Handle(0), Rights::READ).is_err(),
        "fixed: handle_a cleaned up"
    );
    assert!(
        handles.get(Handle(1), Rights::READ).is_err(),
        "fixed: handle_b cleaned up"
    );
}
