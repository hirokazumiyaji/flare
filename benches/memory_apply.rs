use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use flare::firestore::{Firestore, InMemoryFirestore, WriteOp};
use flare::value::Value;

/// Fixed document count so HashMap entry cloning is equal on both sides.
/// Payload size is the variable: a deep clone of every doc would scale with
/// it, while Arc-shared untouched payloads should not.
const DOCUMENT_COUNT: usize = 500;
const SMALL_PAYLOAD_BYTES: usize = 64;
const LARGE_PAYLOAD_BYTES: usize = 4 * 1024;
const WARMUP: usize = 2;
const SAMPLES: usize = 5;
/// Deep-cloning 500 × 4 KiB maps is far above a single `make_mut` of a
/// tiny target doc. Keep a loose bound for CI noise.
const MAX_LARGE_OVER_SMALL: f64 = 3.0;

fn seed_unrelated(
    firestore: &mut InMemoryFirestore,
    payload_bytes: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = "x".repeat(payload_bytes);
    let mut writes: Vec<WriteOp> = (0..DOCUMENT_COUNT)
        .map(|index| WriteOp::Set {
            path: format!("items/{index:05}"),
            data: BTreeMap::from([("payload".to_string(), Value::Str(payload.clone()))]),
            merge: false,
        })
        .collect();
    // Tiny target so make_mut cost stays constant across payload sizes.
    writes.push(WriteOp::Set {
        path: "target/doc".into(),
        data: BTreeMap::from([("n".to_string(), Value::Int(0))]),
        merge: false,
    });
    firestore.apply(writes)
}

fn time_target_updates(firestore: &mut InMemoryFirestore) -> Duration {
    let path = "target/doc";
    for _ in 0..WARMUP {
        firestore
            .apply(vec![WriteOp::Update {
                path: path.into(),
                data: BTreeMap::from([("n".to_string(), Value::Int(1))]),
            }])
            .unwrap();
    }

    let mut total = Duration::ZERO;
    for sample in 0..SAMPLES {
        let write = vec![WriteOp::Update {
            path: path.into(),
            data: BTreeMap::from([("n".to_string(), Value::Int(sample as i64 + 2))]),
        }];
        let started = Instant::now();
        firestore.apply(black_box(write)).unwrap();
        total += started.elapsed();
        let Value::Map(doc) = firestore.get(path).unwrap().unwrap() else {
            panic!("expected map document");
        };
        assert_eq!(doc.get("n"), Some(&Value::Int(sample as i64 + 2)));
    }
    total / SAMPLES as u32
}

fn main() {
    // Claim: mean single-doc update of a tiny target must not track unrelated
    // payload size. A full deep clone of the store would pay ~64x more bytes
    // for LARGE vs SMALL here. Limiter after the fix is HashMap entry clone
    // plus one small make_mut.
    let mut small_store = InMemoryFirestore::default();
    seed_unrelated(&mut small_store, SMALL_PAYLOAD_BYTES).unwrap();
    let small_mean = time_target_updates(&mut small_store);

    let mut large_store = InMemoryFirestore::default();
    seed_unrelated(&mut large_store, LARGE_PAYLOAD_BYTES).unwrap();
    let large_mean = time_target_updates(&mut large_store);

    let ratio = large_mean.as_secs_f64() / small_mean.as_secs_f64().max(1e-12);
    let large_mib = DOCUMENT_COUNT as f64 * LARGE_PAYLOAD_BYTES as f64 / 1024.0 / 1024.0;
    let payload_ratio = LARGE_PAYLOAD_BYTES as f64 / SMALL_PAYLOAD_BYTES as f64;

    println!(
        "{DOCUMENT_COUNT} unrelated docs; large store {large_mib:.2} MiB payload; \
         {SAMPLES} timed samples after {WARMUP} warmup"
    );
    println!("single update, small unrelated payload: {small_mean:?} mean");
    println!("single update, large unrelated payload: {large_mean:?} mean");
    println!("time ratio (large/small): {ratio:.2}");
    println!("unrelated payload byte ratio: {payload_ratio:.1}");

    assert!(
        ratio < MAX_LARGE_OVER_SMALL,
        "single-doc update tracked unrelated payload size \
         (time ratio {ratio:.2}, payload byte ratio {payload_ratio:.1})"
    );
}
