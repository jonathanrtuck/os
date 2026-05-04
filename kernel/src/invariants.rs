//! Kernel state invariant checker — post-condition verification for tests.
//!
//! `verify()` asserts that the entire kernel state is internally consistent.
//! It checks structural invariants that must hold after every syscall:
//! handle→object referential integrity, generation-count consistency,
//! waiter/counter agreement, thread linked-list validity, and scheduler
//! uniqueness.
//!
//! This module is `#[cfg(test)]` only — zero cost in the kernel binary.

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
