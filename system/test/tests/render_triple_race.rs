//! Concurrent stress test for the triple-buffered scene graph.
//!
//! Exercises the TOCTOU race in TripleReader::new(): a concurrent
//! publish() between the reader's load of latest_buf and its store
//! of reader_buf can cause the writer to acquire the reader's buffer,
//! leading to torn reads (corrupted node data).
//!
//! Detection: writer writes matching values to two fields separated
//! by >64 bytes (different cache lines): node.x and node.content_hash.
//! If the reader sees x != content_hash, it's reading a buffer the
//! writer is actively modifying.
//!
//! Expected results:
//!   Without fix: ~1 torn read per 50M iterations (3s), ~5 per 10s
//!   With fix:    0 torn reads (validated claim loop prevents the race)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use scene::*;

/// Duration of the stress test. Longer = more confident.
/// At ~1 torn read per 70M iterations (unfixed), 10s (~100M iters)
/// expects ~1.4 corruptions — sufficient for detection with >75%
/// probability per run.
const TEST_DURATION: Duration = Duration::from_secs(10);

#[test]
fn triple_buffer_no_torn_reads_under_stress() {
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let ptr = buf.as_mut_ptr();

    // Initialize with one node.
    {
        let mut tw = TripleWriter::new(&mut buf);
        {
            let mut w = tw.acquire();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.set_root(n);
            w.node_mut(n).x = 0;
            w.node_mut(n).content_hash = 0;
        }
        tw.publish();
    }

    let shared = ptr as usize;
    let done = Arc::new(AtomicBool::new(false));
    let corruptions = Arc::new(AtomicU64::new(0));
    let reader_iters = Arc::new(AtomicU64::new(0));
    let writer_iters = Arc::new(AtomicU64::new(0));

    // ── Writer thread ──────────────────────────────────────────────
    let done_w = done.clone();
    let w_iters = writer_iters.clone();
    let writer = thread::spawn(move || {
        let buf =
            unsafe { core::slice::from_raw_parts_mut(shared as *mut u8, TRIPLE_SCENE_SIZE) };
        let mut tw = TripleWriter::from_existing(buf);
        let mut gen: u32 = 1;
        while !done_w.load(Ordering::Relaxed) {
            {
                let mut w = tw.acquire_copy();
                // Write matching values to fields >64 bytes apart
                // (different cache lines) to maximize torn-read probability.
                // node.x is at offset ~4, node.content_hash at offset ~80.
                w.node_mut(0).x = gen as i32;
                // Compiler fence: prevent the compiler from merging or
                // reordering these two stores (they must be separate
                // memory operations for torn reads to be detectable).
                core::sync::atomic::compiler_fence(Ordering::SeqCst);
                w.node_mut(0).content_hash = gen;
            }
            tw.publish();
            gen = gen.wrapping_add(1);
            w_iters.fetch_add(1, Ordering::Relaxed);
        }
    });

    // ── Reader thread ──────────────────────────────────────────────
    let done_r = done.clone();
    let corr = corruptions.clone();
    let r_iters = reader_iters.clone();
    let reader = thread::spawn(move || {
        while !done_r.load(Ordering::Relaxed) {
            let tr = unsafe { TripleReader::new(shared as *mut u8, TRIPLE_SCENE_SIZE) };
            let nodes = tr.front_nodes();
            if !nodes.is_empty() {
                let x = nodes[0].x as u32;
                let hash = nodes[0].content_hash;
                // Both should be the same generation value. A mismatch
                // means the reader is seeing a buffer being modified.
                if x != hash {
                    corr.fetch_add(1, Ordering::Relaxed);
                }
            }
            r_iters.fetch_add(1, Ordering::Relaxed);
        }
    });

    thread::sleep(TEST_DURATION);
    done.store(true, Ordering::Relaxed);

    writer.join().expect("writer panicked");
    reader.join().expect("reader panicked");

    let c = corruptions.load(Ordering::Relaxed);
    let ri = reader_iters.load(Ordering::Relaxed);
    let wi = writer_iters.load(Ordering::Relaxed);

    eprintln!(
        "triple_buffer stress: {}s, writer={} reader={} corruptions={}",
        TEST_DURATION.as_secs(),
        wi,
        ri,
        c
    );

    assert_eq!(
        c, 0,
        "detected {} torn reads in {} reader iterations ({} writer publishes) — \
         triple buffer TOCTOU race present",
        c, ri, wi
    );
}
