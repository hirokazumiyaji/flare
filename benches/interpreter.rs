use std::hint::black_box;
use std::time::Instant;

use flare::firestore::InMemoryFirestore;
use flare::Runtime;

fn main() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());

    // 1. Recursion benchmark: fib(20)
    runtime
        .run(
            r#"
fn fib(n) {
    if n < 2 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}
"#,
        )
        .unwrap();

    let start = Instant::now();
    for _ in 0..5 {
        let val = black_box(runtime.run("fib(20)").unwrap());
        assert_eq!(val, flare::value::Value::Int(6765));
    }
    let recursion_elapsed = start.elapsed();

    // 2. Loop benchmark: array iteration via .each
    runtime
        .run("items = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19]")
        .unwrap();
    let start = Instant::now();
    for _ in 0..100 {
        black_box(runtime.run("items.each { |x| x + 1 }").unwrap());
    }
    let loop_elapsed = start.elapsed();

    // 3. Method chain benchmark: map -> filter -> len
    let start = Instant::now();
    for _ in 0..100 {
        let val = black_box(
            runtime
                .run("items.map { |x| x * 2 }.filter { |x| x > 10 }.len()")
                .unwrap(),
        );
        assert_eq!(val, flare::value::Value::Int(14));
    }
    let method_chain_elapsed = start.elapsed();

    // 4. While loop benchmark (in REPL mode)
    let mut repl_rt = Runtime::new(InMemoryFirestore::default());
    repl_rt.set_repl_mode(true);
    let start = Instant::now();
    for _ in 0..50 {
        let val = black_box(
            repl_rt
                .run("i = 0\nwhile i < 100 { i = i + 1 }\ni")
                .unwrap(),
        );
        assert_eq!(val, flare::value::Value::Int(100));
    }
    let while_elapsed = start.elapsed();

    println!("Interpreter Benchmarks:");
    println!("  recursion (fib(20) x5):           {recursion_elapsed:?}");
    println!("  loop (items.each x100):           {loop_elapsed:?}");
    println!("  method chain (map/filter/len x100): {method_chain_elapsed:?}");
    println!("  while loop (100 iterations x50):  {while_elapsed:?}");
}
