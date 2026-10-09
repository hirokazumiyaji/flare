use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use flare::firestore::{Firestore, InMemoryFirestore, WriteOp};
use flare::value::{QueryRef, QuerySource, Value};

const DOCUMENT_COUNT: usize = 1_000;
const PAYLOAD_BYTES: usize = 4 * 1024;
const ITERATIONS: usize = 3;

fn main() {
    // `cargo test --all-targets` は bench target も test として実行するため、
    // 絶対時間より「全件を複製する経路」と「page に含まれる行だけ複製する
    // 経路」の比を見る。dataset は CI でも数 ms で終わる大きさに留める。
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

    // 旧経路は全件を複製してから offset/limit を本地で適用していた。複製が
    // 済んだ時点でコストは確定するので、切り詰めずに全件件数を測る。
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let rows = black_box(firestore.run_query(&page_query).unwrap());
        assert_eq!(rows.len(), DOCUMENT_COUNT);
    }
    let fallback_page = started.elapsed();

    let payload_mib = DOCUMENT_COUNT as f64 * PAYLOAD_BYTES as f64 / 1024.0 / 1024.0;
    println!("{payload_mib:.2} MiB of document payload, {ITERATIONS} iterations");
    println!("count direct:      {optimized_count:?}");
    println!("count fallback:    {fallback_count:?}");
    println!("limit(1) direct:   {optimized_page:?}");
    println!("limit(1) fallback: {fallback_page:?}");
}
