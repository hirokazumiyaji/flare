use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use firelang::firestore::{Firestore, InMemoryFirestore, WriteOp};
use firelang::value::{QueryRef, QuerySource, Value};

const DOCUMENT_COUNT: usize = 4_000;
const PAYLOAD_BYTES: usize = 4 * 1024;
const ITERATIONS: usize = 5;

fn main() {
    // `cargo test --all-targets` also builds harness-free targets. Keep the
    // benchmark setup out of that path; `cargo bench --bench query_payload`
    // runs the measurement below.
    if cfg!(test) {
        return;
    }

    let payload = "x".repeat(PAYLOAD_BYTES);
    let writes = (0..DOCUMENT_COUNT)
        .map(|index| WriteOp::Set {
            path: format!("items/{index:05}"),
            data: BTreeMap::from([
                ("active".to_string(), Value::Bool(true)),
                ("payload".to_string(), Value::Str(payload.clone())),
            ]),
            merge: false,
        })
        .collect();
    let mut firestore = InMemoryFirestore::default();
    firestore.apply(writes).unwrap();

    let count_query = QueryRef {
        source: QuerySource::Collection("items".to_string()),
        filters: vec![],
        order_by: vec![],
        offset: None,
        limit: None,
    };
    let page_query = QueryRef {
        limit: Some(1),
        ..count_query.clone()
    };

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let count = black_box(firestore.count_query(&count_query).unwrap());
        assert_eq!(count, DOCUMENT_COUNT as i64);
    }
    let optimized_count = started.elapsed();

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let rows = black_box(firestore.run_query(&count_query).unwrap());
        assert_eq!(rows.len(), DOCUMENT_COUNT);
    }
    let fallback_count = started.elapsed();

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let rows = black_box(firestore.run_query_page(&page_query).unwrap());
        assert_eq!(rows.len(), 1);
    }
    let optimized_page = started.elapsed();

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let rows = black_box(firestore.run_query(&page_query).unwrap());
        assert_eq!(rows.len(), 1);
    }
    let fallback_page = started.elapsed();

    let payload_mib = DOCUMENT_COUNT as f64 * PAYLOAD_BYTES as f64 / 1024.0 / 1024.0;
    println!("{payload_mib:.2} MiB of document payload, {ITERATIONS} iterations");
    println!("count direct:      {optimized_count:?}");
    println!("count fallback:    {fallback_count:?}");
    println!("limit(1) direct:   {optimized_page:?}");
    println!("limit(1) fallback: {fallback_page:?}");
}
