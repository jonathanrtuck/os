//! Kernel state invariant checker — post-condition verification for tests.
//!
//! `verify()` asserts that the entire kernel state is internally consistent.
//! It checks structural invariants that must hold after every syscall:
//! handle→object referential integrity, generation-count consistency,
//! waiter/counter agreement, thread linked-list validity, and scheduler
//! uniqueness.
//!
//! Available in test, fuzzing, and debug builds. Zero cost in release kernel binary.

use alloc::{collections::BTreeSet, format, string::String, vec::Vec};
use core::fmt::Write;

use crate::{
    config,
    frame::state,
    thread::ThreadRunState,
    types::{AddressSpaceId, ObjectType, ThreadId},
};

#[derive(Debug)]
pub struct Violation {
    pub category: &'static str,
    pub detail: String,
}

impl core::fmt::Display for Violation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[{}] {}", self.category, self.detail)
    }
}

pub fn verify() -> Vec<Violation> {
    let mut violations = Vec::new();

    check_handle_referential_integrity(&mut violations);
    check_endpoint_internal_counts(&mut violations);
    check_event_internal_counts(&mut violations);
    check_thread_space_linked_lists(&mut violations);
    check_scheduler_uniqueness(&mut violations);
    check_thread_state_consistency(&mut violations);
    check_mapping_consistency(&mut violations);
    check_ipc_blocked_thread_consistency(&mut violations);
    check_event_waiter_validity(&mut violations);
    check_irq_binding_consistency(&mut violations);
    check_vmo_mapping_range_validity(&mut violations);
    check_refcount_consistency(&mut violations);
    check_endpoint_event_binding_bidirectionality(&mut violations);
    check_priority_inheritance_consistency(&mut violations);

    violations
}

/// Extended verification that includes object reachability (leak detection).
/// Call this only when all objects should be reachable — e.g., after full
/// lifecycle tests. NOT suitable for per-syscall checking because handle_close
/// does not automatically destroy objects (space_destroy does).
pub fn verify_no_leaks() -> Vec<Violation> {
    let mut violations = verify();

    check_exact_refcounts(&mut violations);
    check_object_reachability(&mut violations);

    violations
}

fn check_handle_referential_integrity(violations: &mut Vec<Violation>) {
    state::spaces().for_each(|space_idx, space| {
        let space_id = AddressSpaceId(space_idx);

        for (hid, handle) in space.handles().iter_handles() {
            let obj_exists = match handle.object_type {
                ObjectType::Vmo => state::vmos().read(handle.object_id).is_some(),
                ObjectType::Endpoint => state::endpoints().read(handle.object_id).is_some(),
                ObjectType::Event => state::events().read(handle.object_id).is_some(),
                ObjectType::Thread => state::threads().read(handle.object_id).is_some(),
                // Avoid deadlock: for_each holds this space's slot lock, so
                // read() on the same slot would spin forever. If the handle
                // points to the space we are currently iterating, it exists
                // by definition. For other spaces, read() acquires a
                // different slot lock and is safe.
                ObjectType::AddressSpace => {
                    handle.object_id == space_idx
                        || state::spaces().read(handle.object_id).is_some()
                }
                ObjectType::Resource => state::resources().read(handle.object_id).is_some(),
            };

            if !obj_exists {
                violations.push(Violation {
                    category: "handle→object",
                    detail: format!(
                        "space {} handle {} references deallocated {:?} #{}",
                        space_id.0, hid.0, handle.object_type, handle.object_id
                    ),
                });
            }

            let current_gen = match handle.object_type {
                ObjectType::Vmo => state::vmos().generation(handle.object_id),
                ObjectType::Endpoint => state::endpoints().generation(handle.object_id),
                ObjectType::Event => state::events().generation(handle.object_id),
                ObjectType::Thread => state::threads().generation(handle.object_id),
                ObjectType::AddressSpace => state::spaces().generation(handle.object_id),
                ObjectType::Resource => state::resources().generation(handle.object_id),
            };

            if obj_exists && handle.generation != current_gen {
                violations.push(Violation {
                    category: "handle→generation",
                    detail: format!(
                        "space {} handle {} has generation {} but {:?} #{} is at generation {}",
                        space_id.0,
                        hid.0,
                        handle.generation,
                        handle.object_type,
                        handle.object_id,
                        current_gen
                    ),
                });
            }
        }
    });
}

fn check_endpoint_internal_counts(violations: &mut Vec<Violation>) {
    state::endpoints().for_each(|idx, ep| {
        if let Err(msg) = ep.verify_internal_counts() {
            violations.push(Violation {
                category: "endpoint",
                detail: format!("endpoint #{idx}: {msg}"),
            });
        }
    });
}

fn check_event_internal_counts(violations: &mut Vec<Violation>) {
    state::events().for_each(|idx, evt| {
        if let Err(msg) = evt.verify_internal_counts() {
            violations.push(Violation {
                category: "event",
                detail: format!("event #{idx}: {msg}"),
            });
        }
    });
}

fn check_thread_space_linked_lists(violations: &mut Vec<Violation>) {
    state::spaces().for_each(|space_idx, space| {
        let mut visited = BTreeSet::new();
        let mut cursor = space.thread_head();

        while let Some(tid) = cursor {
            if !visited.insert(tid) {
                violations.push(Violation {
                    category: "thread-list",
                    detail: format!("space {space_idx} thread list has cycle at thread #{tid}"),
                });

                break;
            }

            let Some(thread) = state::threads().read(tid) else {
                violations.push(Violation {
                    category: "thread-list",
                    detail: format!(
                        "space {space_idx} thread list references deallocated thread #{tid}",
                    ),
                });

                break;
            };

            if thread.address_space() != Some(AddressSpaceId(space_idx)) {
                violations.push(Violation {
                    category: "thread-list",
                    detail: format!(
                        "thread #{} in space {} list but has address_space {:?}",
                        tid,
                        space_idx,
                        thread.address_space()
                    ),
                });
            }

            cursor = thread.space_next();
        }
    });
}

fn check_scheduler_uniqueness(violations: &mut Vec<Violation>) {
    let scheds = state::schedulers();
    let mut seen = BTreeSet::new();

    for core_id in 0..scheds.num_cores() {
        let pcs = scheds.core(core_id).lock();

        if let Some(current) = pcs.current()
            && !seen.insert(current)
        {
            violations.push(Violation {
                category: "scheduler",
                detail: format!(
                    "thread {} is current on core {} but already seen",
                    current.0, core_id
                ),
            });
        }

        for tid in pcs.all_queued() {
            if !seen.insert(tid) {
                violations.push(Violation {
                    category: "scheduler",
                    detail: format!(
                        "thread {} queued on core {} but already seen in scheduler",
                        tid.0, core_id
                    ),
                });
            }
        }
    }
}

fn check_thread_state_consistency(violations: &mut Vec<Violation>) {
    let scheds = state::schedulers();
    let mut scheduler_threads = BTreeSet::new();

    for core_id in 0..scheds.num_cores() {
        let pcs = scheds.core(core_id).lock();

        if let Some(c) = pcs.current() {
            scheduler_threads.insert(c);
        }

        for tid in pcs.all_queued() {
            scheduler_threads.insert(tid);
        }
    }

    state::threads().for_each(|idx, thread| {
        let tid = ThreadId(idx);
        let in_scheduler = scheduler_threads.contains(&tid);

        match thread.state() {
            ThreadRunState::Ready => {
                if !in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {idx} is Ready but not in any run queue"),
                    });
                }
            }
            ThreadRunState::Running => {
                if !in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {idx} is Running but not current on any core"),
                    });
                }
            }
            ThreadRunState::Blocked => {
                if in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {idx} is Blocked but still in a run queue"),
                    });
                }
            }
            ThreadRunState::Exited => {
                if in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {idx} is Exited but still in a run queue"),
                    });
                }
            }
        }
    });
}

fn check_mapping_consistency(violations: &mut Vec<Violation>) {
    state::spaces().for_each(|space_idx, space| {
        let mappings = space.mappings();

        for i in 0..mappings.len() {
            let m = &mappings[i];

            if m.size == 0 {
                violations.push(Violation {
                    category: "mapping",
                    detail: format!("space {space_idx} mapping {i} has zero size"),
                });
            }

            if i + 1 < mappings.len() && m.va_start + m.size > mappings[i + 1].va_start {
                violations.push(Violation {
                    category: "mapping",
                    detail: format!(
                        "space {} mappings {} and {} overlap: [{:#x}..{:#x}) vs [{:#x}..)",
                        space_idx,
                        i,
                        i + 1,
                        m.va_start,
                        m.va_start + m.size,
                        mappings[i + 1].va_start
                    ),
                });
            }

            if state::vmos().read(m.vmo_id.0).is_none() {
                violations.push(Violation {
                    category: "mapping→vmo",
                    detail: format!(
                        "space {} mapping {} references deallocated VMO #{}",
                        space_idx, i, m.vmo_id.0
                    ),
                });
            }
        }
    });
}

fn check_ipc_blocked_thread_consistency(violations: &mut Vec<Violation>) {
    state::endpoints().for_each(|ep_idx, ep| {
        for caller_tid in ep.all_caller_thread_ids() {
            match state::threads().read(caller_tid.0) {
                None => {
                    violations.push(Violation {
                        category: "ipc-caller",
                        detail: format!(
                            "endpoint #{} references deallocated caller thread #{}",
                            ep_idx, caller_tid.0
                        ),
                    });
                }
                Some(thread) => {
                    if thread.state() != ThreadRunState::Blocked {
                        violations.push(Violation {
                            category: "ipc-caller",
                            detail: format!(
                                "endpoint #{} has caller thread #{} in state {:?}, expected Blocked",
                                ep_idx,
                                caller_tid.0,
                                thread.state()
                            ),
                        });
                    }
                }
            }
        }

        for waiter_tid in ep.all_recv_waiter_ids() {
            match state::threads().read(waiter_tid.0) {
                None => {
                    violations.push(Violation {
                        category: "ipc-waiter",
                        detail: format!(
                            "endpoint #{} references deallocated recv waiter thread #{}",
                            ep_idx, waiter_tid.0
                        ),
                    });
                }
                Some(thread) => {
                    if thread.state() != ThreadRunState::Blocked {
                        violations.push(Violation {
                            category: "ipc-waiter",
                            detail: format!(
                                "endpoint #{} has recv waiter thread #{} in state {:?}, expected Blocked",
                                ep_idx,
                                waiter_tid.0,
                                thread.state()
                            ),
                        });
                    }
                }
            }
        }
    });
}

fn check_event_waiter_validity(violations: &mut Vec<Violation>) {
    state::events().for_each(|evt_idx, event| {
        for waiter_tid in event.all_waiter_thread_ids() {
            match state::threads().read(waiter_tid.0) {
                None => {
                    violations.push(Violation {
                        category: "event-waiter",
                        detail: format!(
                            "event #{} references deallocated waiter thread #{}",
                            evt_idx, waiter_tid.0
                        ),
                    });
                }
                Some(thread) => {
                    if thread.state() != ThreadRunState::Blocked {
                        violations.push(Violation {
                            category: "event-waiter",
                            detail: format!(
                                "event #{} has waiter thread #{} in state {:?}, expected Blocked",
                                evt_idx,
                                waiter_tid.0,
                                thread.state()
                            ),
                        });
                    }
                }
            }
        }
    });
}

fn check_irq_binding_consistency(violations: &mut Vec<Violation>) {
    let irqs = state::irqs().lock();

    for intid in 0..config::MAX_IRQS {
        if let Some(binding) = irqs.binding_at(intid)
            && state::events().read(binding.event_id.0).is_none()
        {
            violations.push(Violation {
                category: "irq-binding",
                detail: format!(
                    "IRQ #{} bound to deallocated event #{}",
                    intid, binding.event_id.0
                ),
            });
        }
    }
}

fn check_vmo_mapping_range_validity(violations: &mut Vec<Violation>) {
    state::spaces().for_each(|space_idx, space| {
        for (i, m) in space.mappings().iter().enumerate() {
            if let Some(vmo) = state::vmos().read(m.vmo_id.0) {
                let aligned_vmo_size = vmo.size().next_multiple_of(config::PAGE_SIZE);

                if m.size > aligned_vmo_size {
                    violations.push(Violation {
                        category: "mapping-range",
                        detail: format!(
                            "space {} mapping {} maps {} bytes but VMO #{} is only {} bytes \
                             (page-aligned: {})",
                            space_idx,
                            i,
                            m.size,
                            m.vmo_id.0,
                            vmo.size(),
                            aligned_vmo_size,
                        ),
                    });
                }
            }
        }
    });
}

fn check_refcount_consistency(violations: &mut Vec<Violation>) {
    use alloc::collections::BTreeMap;

    let mut vmo_handle_counts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut endpoint_handle_counts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut event_handle_counts: BTreeMap<u32, usize> = BTreeMap::new();

    state::spaces().for_each(|_, space| {
        for (_, handle) in space.handles().iter_handles() {
            match handle.object_type {
                ObjectType::Vmo => {
                    *vmo_handle_counts.entry(handle.object_id).or_insert(0) += 1;
                }
                ObjectType::Endpoint => {
                    *endpoint_handle_counts.entry(handle.object_id).or_insert(0) += 1;
                }
                ObjectType::Event => {
                    *event_handle_counts.entry(handle.object_id).or_insert(0) += 1;
                }
                ObjectType::Thread | ObjectType::AddressSpace | ObjectType::Resource => {}
            }
        }
    });

    // Lower-bound check: refcount >= handle_count. Catches dangling handles
    // (use-after-free). Does NOT catch leaks — that requires the exact check
    // in verify_no_leaks().
    state::vmos().for_each(|idx, vmo| {
        let handle_count = vmo_handle_counts.get(&idx).copied().unwrap_or(0);

        if vmo.refcount() < handle_count {
            violations.push(Violation {
                category: "refcount-vmo",
                detail: format!(
                    "VMO #{} refcount {} < {} handles (dangling)",
                    idx,
                    vmo.refcount(),
                    handle_count
                ),
            });
        }
    });

    state::endpoints().for_each(|idx, ep| {
        let handle_count = endpoint_handle_counts.get(&idx).copied().unwrap_or(0);

        if ep.refcount() < handle_count {
            violations.push(Violation {
                category: "refcount-endpoint",
                detail: format!(
                    "endpoint #{} refcount {} < {} handles (dangling)",
                    idx,
                    ep.refcount(),
                    handle_count
                ),
            });
        }
    });

    state::events().for_each(|idx, evt| {
        let handle_count = event_handle_counts.get(&idx).copied().unwrap_or(0);

        if evt.refcount() < handle_count {
            violations.push(Violation {
                category: "refcount-event",
                detail: format!(
                    "event #{} refcount {} < {} handles (dangling)",
                    idx,
                    evt.refcount(),
                    handle_count
                ),
            });
        }
    });
}

fn check_exact_refcounts(violations: &mut Vec<Violation>) {
    use alloc::collections::BTreeMap;

    let mut vmo_handle_counts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut endpoint_handle_counts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut event_handle_counts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut vmo_mapping_counts: BTreeMap<u32, usize> = BTreeMap::new();

    state::spaces().for_each(|_, space| {
        for (_, handle) in space.handles().iter_handles() {
            match handle.object_type {
                ObjectType::Vmo => {
                    *vmo_handle_counts.entry(handle.object_id).or_insert(0) += 1;
                }
                ObjectType::Endpoint => {
                    *endpoint_handle_counts.entry(handle.object_id).or_insert(0) += 1;
                }
                ObjectType::Event => {
                    *event_handle_counts.entry(handle.object_id).or_insert(0) += 1;
                }
                ObjectType::Thread | ObjectType::AddressSpace | ObjectType::Resource => {}
            }
        }

        for m in space.mappings() {
            *vmo_mapping_counts.entry(m.vmo_id.0).or_insert(0) += 1;
        }
    });

    state::vmos().for_each(|idx, vmo| {
        let handles = vmo_handle_counts.get(&idx).copied().unwrap_or(0);
        let mappings = vmo_mapping_counts.get(&idx).copied().unwrap_or(0);
        let expected = handles + mappings;

        if vmo.refcount() != expected {
            violations.push(Violation {
                category: "exact-refcount-vmo",
                detail: format!(
                    "VMO #{} refcount {} != {} handles + {} mappings = {}",
                    idx,
                    vmo.refcount(),
                    handles,
                    mappings,
                    expected
                ),
            });
        }
    });

    state::endpoints().for_each(|idx, ep| {
        let expected = endpoint_handle_counts.get(&idx).copied().unwrap_or(0);

        if ep.refcount() != expected {
            violations.push(Violation {
                category: "exact-refcount-endpoint",
                detail: format!(
                    "endpoint #{} refcount {} != {} handles",
                    idx,
                    ep.refcount(),
                    expected
                ),
            });
        }
    });

    state::events().for_each(|idx, evt| {
        let expected = event_handle_counts.get(&idx).copied().unwrap_or(0);

        if evt.refcount() != expected {
            violations.push(Violation {
                category: "exact-refcount-event",
                detail: format!(
                    "event #{} refcount {} != {} handles",
                    idx,
                    evt.refcount(),
                    expected
                ),
            });
        }
    });
}

fn check_endpoint_event_binding_bidirectionality(violations: &mut Vec<Violation>) {
    state::endpoints().for_each(|ep_idx, ep| {
        if let Some(event_id) = ep.bound_event() {
            match state::events().read(event_id.0) {
                None => {
                    violations.push(Violation {
                        category: "binding-ep→evt",
                        detail: format!(
                            "endpoint #{} bound to deallocated event #{}",
                            ep_idx, event_id.0
                        ),
                    });
                }
                Some(evt) => {
                    if evt.bound_endpoint() != Some(crate::types::EndpointId(ep_idx)) {
                        violations.push(Violation {
                            category: "binding-ep→evt",
                            detail: format!(
                                "endpoint #{} bound to event #{} but event points to {:?}",
                                ep_idx,
                                event_id.0,
                                evt.bound_endpoint()
                            ),
                        });
                    }
                }
            }
        }
    });

    state::events().for_each(|evt_idx, evt| {
        if let Some(endpoint_id) = evt.bound_endpoint() {
            match state::endpoints().read(endpoint_id.0) {
                None => {
                    violations.push(Violation {
                        category: "binding-evt→ep",
                        detail: format!(
                            "event #{} bound to deallocated endpoint #{}",
                            evt_idx, endpoint_id.0
                        ),
                    });
                }
                Some(ep) => {
                    if ep.bound_event() != Some(crate::types::EventId(evt_idx)) {
                        violations.push(Violation {
                            category: "binding-evt→ep",
                            detail: format!(
                                "event #{} bound to endpoint #{} but endpoint points to {:?}",
                                evt_idx,
                                endpoint_id.0,
                                ep.bound_event()
                            ),
                        });
                    }
                }
            }
        }
    });
}

fn check_priority_inheritance_consistency(violations: &mut Vec<Violation>) {
    state::endpoints().for_each(|ep_idx, ep| {
        if let Some(server_tid) = ep.active_server()
            && let Some(highest_caller) = ep.highest_caller_priority()
            && let Some(thread) = state::threads().read(server_tid.0)
        {
            let effective = thread.effective_priority();

            if effective < highest_caller {
                violations.push(Violation {
                    category: "priority-inheritance",
                    detail: format!(
                        "endpoint #{} active server thread #{} has effective priority {:?} \
                         but highest pending caller is {:?} (base: {:?})",
                        ep_idx,
                        server_tid.0,
                        effective,
                        highest_caller,
                        thread.priority()
                    ),
                });
            }
        }
    });
}

fn check_object_reachability(violations: &mut Vec<Violation>) {
    let mut vmo_refs = BTreeSet::new();
    let mut endpoint_refs = BTreeSet::new();
    let mut event_refs = BTreeSet::new();

    state::spaces().for_each(|_, space| {
        for (_, handle) in space.handles().iter_handles() {
            match handle.object_type {
                ObjectType::Vmo => {
                    vmo_refs.insert(handle.object_id);
                }
                ObjectType::Endpoint => {
                    endpoint_refs.insert(handle.object_id);
                }
                ObjectType::Event => {
                    event_refs.insert(handle.object_id);
                }
                ObjectType::Thread | ObjectType::AddressSpace | ObjectType::Resource => {}
            }
        }
    });

    // Use manual loop for VMOs because we need to check mappings across
    // spaces for each unreferenced VMO (nested for_each on a different
    // table is safe — different locks — but we capture the mapped flag).
    // We explicitly drop the VMO read guard before entering spaces.for_each
    // to avoid holding a VMO slot lock across a space iteration.
    for idx in 0..config::MAX_VMOS as u32 {
        let allocated = state::vmos().read(idx).is_some();

        if allocated && !vmo_refs.contains(&idx) {
            let mut is_mapped = false;

            state::spaces().for_each(|_, space| {
                if space.mappings().iter().any(|m| m.vmo_id.0 == idx) {
                    is_mapped = true;
                }
            });

            if !is_mapped {
                violations.push(Violation {
                    category: "orphan-vmo",
                    detail: format!("VMO #{idx} has no handles and no mappings (orphaned)"),
                });
            }
        }
    }

    for idx in 0..config::MAX_ENDPOINTS as u32 {
        let allocated = state::endpoints().read(idx).is_some();

        if allocated && !endpoint_refs.contains(&idx) {
            violations.push(Violation {
                category: "orphan-endpoint",
                detail: format!("endpoint #{idx} has no handles (orphaned)"),
            });
        }
    }

    for idx in 0..config::MAX_EVENTS as u32 {
        let allocated = state::events().read(idx).is_some();

        if allocated && !event_refs.contains(&idx) {
            violations.push(Violation {
                category: "orphan-event",
                detail: format!("event #{idx} has no handles (orphaned)"),
            });
        }
    }
}

pub fn assert_valid() {
    let violations = verify();

    if !violations.is_empty() {
        let mut msg = String::from("KERNEL INVARIANT VIOLATIONS:\n");

        for v in &violations {
            let _ = writeln!(msg, "  {v}");
        }

        panic!("{}", msg);
    }
}
