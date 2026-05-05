//! Kernel state invariant checker — post-condition verification for tests.
//!
//! `verify()` asserts that the entire kernel state is internally consistent.
//! It checks structural invariants that must hold after every syscall:
//! handle→object referential integrity, generation-count consistency,
//! waiter/counter agreement, thread linked-list validity, and scheduler
//! uniqueness.
//!
//! This module is `#[cfg(any(test, fuzzing))]` only — zero cost in the kernel binary.

use alloc::{collections::BTreeSet, format, string::String, vec::Vec};

use crate::{
    syscall::Kernel,
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

pub fn verify(kernel: &Kernel) -> Vec<Violation> {
    let mut violations = Vec::new();

    check_handle_referential_integrity(kernel, &mut violations);
    check_endpoint_internal_counts(kernel, &mut violations);
    check_event_internal_counts(kernel, &mut violations);
    check_thread_space_linked_lists(kernel, &mut violations);
    check_scheduler_uniqueness(kernel, &mut violations);
    check_thread_state_consistency(kernel, &mut violations);
    check_mapping_consistency(kernel, &mut violations);
    check_ipc_blocked_thread_consistency(kernel, &mut violations);
    check_event_waiter_validity(kernel, &mut violations);
    check_irq_binding_consistency(kernel, &mut violations);
    check_vmo_mapping_range_validity(kernel, &mut violations);

    violations
}

/// Extended verification that includes object reachability (leak detection).
/// Call this only when all objects should be reachable — e.g., after full
/// lifecycle tests. NOT suitable for per-syscall checking because handle_close
/// does not automatically destroy objects (space_destroy does).
pub fn verify_no_leaks(kernel: &Kernel) -> Vec<Violation> {
    let mut violations = verify(kernel);

    check_object_reachability(kernel, &mut violations);

    violations
}

fn check_handle_referential_integrity(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (space_idx, space) in kernel.spaces.iter_allocated() {
        let space_id = AddressSpaceId(space_idx);

        for (hid, handle) in space.handles().iter_handles() {
            let obj_exists = match handle.object_type {
                ObjectType::Vmo => kernel.vmos.is_allocated(handle.object_id),
                ObjectType::Endpoint => kernel.endpoints.is_allocated(handle.object_id),
                ObjectType::Event => kernel.events.is_allocated(handle.object_id),
                ObjectType::Thread => kernel.threads.is_allocated(handle.object_id),
                ObjectType::AddressSpace => kernel.spaces.is_allocated(handle.object_id),
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
                ObjectType::Vmo => kernel.vmos.generation(handle.object_id),
                ObjectType::Endpoint => kernel.endpoints.generation(handle.object_id),
                ObjectType::Event => kernel.events.generation(handle.object_id),
                ObjectType::Thread => kernel.threads.generation(handle.object_id),
                ObjectType::AddressSpace => kernel.spaces.generation(handle.object_id),
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
    }
}

fn check_endpoint_internal_counts(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (idx, ep) in kernel.endpoints.iter_allocated() {
        if let Err(msg) = ep.verify_internal_counts() {
            violations.push(Violation {
                category: "endpoint",
                detail: format!("endpoint #{}: {}", idx, msg),
            });
        }
    }
}

fn check_event_internal_counts(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (idx, evt) in kernel.events.iter_allocated() {
        if let Err(msg) = evt.verify_internal_counts() {
            violations.push(Violation {
                category: "event",
                detail: format!("event #{}: {}", idx, msg),
            });
        }
    }
}

fn check_thread_space_linked_lists(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (space_idx, space) in kernel.spaces.iter_allocated() {
        let mut visited = BTreeSet::new();
        let mut cursor = space.thread_head();

        while let Some(tid) = cursor {
            if !visited.insert(tid) {
                violations.push(Violation {
                    category: "thread-list",
                    detail: format!(
                        "space {} thread list has cycle at thread #{}",
                        space_idx, tid
                    ),
                });

                break;
            }

            let thread = match kernel.threads.get(tid) {
                Some(t) => t,
                None => {
                    violations.push(Violation {
                        category: "thread-list",
                        detail: format!(
                            "space {} thread list references deallocated thread #{}",
                            space_idx, tid
                        ),
                    });

                    break;
                }
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

            if let Some(prev) = thread.space_prev() {
                if !visited.contains(&prev) && Some(prev) != space.thread_head().filter(|_| false) {
                    // prev should either be already visited or be unreachable (head has no prev)
                }
            }

            cursor = thread.space_next();
        }
    }
}

fn check_scheduler_uniqueness(kernel: &Kernel, violations: &mut Vec<Violation>) {
    let mut seen = BTreeSet::new();

    for core_id in 0..kernel.scheduler.num_cores() {
        let rq = kernel.scheduler.core(core_id);

        if let Some(current) = rq.current() {
            if !seen.insert(current) {
                violations.push(Violation {
                    category: "scheduler",
                    detail: format!(
                        "thread {} is current on core {} but already seen",
                        current.0, core_id
                    ),
                });
            }
        }

        for tid in rq.all_queued_thread_ids() {
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

fn check_thread_state_consistency(kernel: &Kernel, violations: &mut Vec<Violation>) {
    let mut scheduler_threads = BTreeSet::new();

    for core_id in 0..kernel.scheduler.num_cores() {
        let rq = kernel.scheduler.core(core_id);

        if let Some(c) = rq.current() {
            scheduler_threads.insert(c);
        }

        for tid in rq.all_queued_thread_ids() {
            scheduler_threads.insert(tid);
        }
    }

    for (idx, thread) in kernel.threads.iter_allocated() {
        let tid = ThreadId(idx);
        let in_scheduler = scheduler_threads.contains(&tid);

        match thread.state() {
            ThreadRunState::Ready => {
                if !in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {} is Ready but not in any run queue", idx),
                    });
                }
            }
            ThreadRunState::Running => {
                if !in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {} is Running but not current on any core", idx),
                    });
                }
            }
            ThreadRunState::Blocked => {
                if in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {} is Blocked but still in a run queue", idx),
                    });
                }
            }
            ThreadRunState::Exited => {
                if in_scheduler {
                    violations.push(Violation {
                        category: "thread-state",
                        detail: format!("thread {} is Exited but still in a run queue", idx),
                    });
                }
            }
        }
    }
}

fn check_mapping_consistency(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (space_idx, space) in kernel.spaces.iter_allocated() {
        let mappings = space.mappings();

        for i in 0..mappings.len() {
            let m = &mappings[i];

            if m.size == 0 {
                violations.push(Violation {
                    category: "mapping",
                    detail: format!("space {} mapping {} has zero size", space_idx, i),
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

            if !kernel.vmos.is_allocated(m.vmo_id.0) {
                violations.push(Violation {
                    category: "mapping→vmo",
                    detail: format!(
                        "space {} mapping {} references deallocated VMO #{}",
                        space_idx, i, m.vmo_id.0
                    ),
                });
            }
        }
    }
}

fn check_ipc_blocked_thread_consistency(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (ep_idx, ep) in kernel.endpoints.iter_allocated() {
        for caller_tid in ep.all_caller_thread_ids() {
            match kernel.threads.get(caller_tid.0) {
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
            match kernel.threads.get(waiter_tid.0) {
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
    }
}

fn check_event_waiter_validity(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (evt_idx, event) in kernel.events.iter_allocated() {
        for waiter_tid in event.all_waiter_thread_ids() {
            match kernel.threads.get(waiter_tid.0) {
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
    }
}

fn check_irq_binding_consistency(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for intid in 0..crate::config::MAX_IRQS {
        if let Some(binding) = kernel.irqs.binding_at(intid) {
            if !kernel.events.is_allocated(binding.event_id.0) {
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
}

fn check_vmo_mapping_range_validity(kernel: &Kernel, violations: &mut Vec<Violation>) {
    for (space_idx, space) in kernel.spaces.iter_allocated() {
        for (i, m) in space.mappings().iter().enumerate() {
            if let Some(vmo) = kernel.vmos.get(m.vmo_id.0) {
                let aligned_vmo_size = vmo.size().next_multiple_of(crate::config::PAGE_SIZE);

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
    }
}

fn check_object_reachability(kernel: &Kernel, violations: &mut Vec<Violation>) {
    let mut vmo_refs = BTreeSet::new();
    let mut endpoint_refs = BTreeSet::new();
    let mut event_refs = BTreeSet::new();

    for (_, space) in kernel.spaces.iter_allocated() {
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
                ObjectType::Thread | ObjectType::AddressSpace => {}
            }
        }
    }

    for (idx, _) in kernel.vmos.iter_allocated() {
        if !vmo_refs.contains(&idx) {
            let is_mapped = kernel
                .spaces
                .iter_allocated()
                .any(|(_, space)| space.mappings().iter().any(|m| m.vmo_id.0 == idx));

            if !is_mapped {
                violations.push(Violation {
                    category: "orphan-vmo",
                    detail: format!("VMO #{} has no handles and no mappings (orphaned)", idx),
                });
            }
        }
    }

    for (idx, _) in kernel.endpoints.iter_allocated() {
        if !endpoint_refs.contains(&idx) {
            violations.push(Violation {
                category: "orphan-endpoint",
                detail: format!("endpoint #{} has no handles (orphaned)", idx),
            });
        }
    }

    for (idx, _) in kernel.events.iter_allocated() {
        if !event_refs.contains(&idx) {
            violations.push(Violation {
                category: "orphan-event",
                detail: format!("event #{} has no handles (orphaned)", idx),
            });
        }
    }
}

pub fn assert_valid(kernel: &Kernel) {
    let violations = verify(kernel);

    if !violations.is_empty() {
        let mut msg = String::from("KERNEL INVARIANT VIOLATIONS:\n");

        for v in &violations {
            msg.push_str(&format!("  {}\n", v));
        }

        panic!("{}", msg);
    }
}
