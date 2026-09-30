//! Runtime budget tests (issue #82).
//!
//! Every hostile script here must produce a controlled `FirelangError` with a
//! `Runtime` kind and a source position -- never a panic, a stack overflow, or
//! a hang.

use std::time::Duration;

use firelang::runtime::Runtime;
use firelang::{ErrorKind, FirelangError, InMemoryFirestore, RuntimeLimits, Value};

fn runtime_with(limits: RuntimeLimits) -> Runtime<InMemoryFirestore> {
    Runtime::with_limits(InMemoryFirestore::default(), limits)
}

fn assert_budget_error(err: Box<dyn std::error::Error>, needle: &str) {
    let text = format!("{err}");
    assert!(text.contains(needle), "expected {needle:?} in {text:?}");
    let ferr = err
        .downcast_ref::<FirelangError>()
        .unwrap_or_else(|| panic!("budget failure should be a FirelangError: {text:?}"));
    assert_eq!(ferr.kind, ErrorKind::Runtime, "{text:?}");
    assert!(
        ferr.span.is_some(),
        "budget error should carry a position: {text:?}"
    );
}

#[test]
fn infinite_recursion_is_a_controlled_error() {
    let mut rt = runtime_with(RuntimeLimits::default().with_max_call_depth(32));
    let err = rt.run("fn f() { f() }\nf()").unwrap_err();
    assert_budget_error(err, "call depth limit exceeded");
    // The guard stopped exactly at the configured ceiling.
    assert_eq!(rt.usage().max_call_depth, 32);

    // The guard must be unwound even on the failure path, so the runtime stays
    // usable for a following script.
    let out = rt.run("1 + 1").unwrap();
    assert_eq!(out, Value::Int(2));
    // `usage` is per-run: the second run started from zero again.
    assert_eq!(rt.usage().steps, 1);
}

#[test]
fn default_limits_stop_runaway_recursion() {
    // No explicit configuration: the shipped default must be the guard.
    let mut rt = runtime_with(RuntimeLimits::default());
    let err = rt.run("fn f() { f() }\nf()").unwrap_err();
    assert_budget_error(err, "call depth limit exceeded");
    assert_eq!(
        rt.usage().max_call_depth,
        rt.limits().max_call_depth.unwrap()
    );
}

#[test]
fn runaway_loop_is_a_controlled_error() {
    let mut rt = runtime_with(RuntimeLimits::default().with_max_loop_iterations(64));
    let err = rt.run("while true { }").unwrap_err();
    assert_budget_error(err, "loop iteration limit exceeded");
    assert_eq!(rt.usage().loop_iterations, 64);
}

#[test]
fn runaway_each_iteration_is_a_controlled_error() {
    // A huge literal is fine; the *iteration* budget is what stops `each`.
    let items: Vec<String> = (0..500).map(|i| i.to_string()).collect();
    let src = format!("[{}].each {{ |x| x }}", items.join(","));
    let mut rt = runtime_with(RuntimeLimits::default().with_max_loop_iterations(10));
    let err = rt.run(&src).unwrap_err();
    assert_budget_error(err, "loop iteration limit exceeded");
}

#[test]
fn step_budget_bounds_statement_count() {
    let src = "a = 1\nb = 2\nc = 3\nd = 4\ne = 5";
    let mut rt = runtime_with(RuntimeLimits::default().with_max_steps(3));
    let err = rt.run(src).unwrap_err();
    assert_budget_error(err, "step limit exceeded");
    assert_eq!(rt.usage().steps, 3);
}

#[test]
fn wall_clock_budget_bounds_a_slow_script() {
    // Disable the count budgets so only the wall clock can stop the script.
    // `m` is never reassigned (variables are immutable), so the loop body is
    // legal and the wall clock is the only possible failure.
    let mut rt = runtime_with(
        RuntimeLimits::default()
            .with_max_loop_iterations(u64::MAX)
            .with_max_steps(u64::MAX)
            .with_max_wall_clock(Duration::from_nanos(1)),
    );
    let err = rt.run("m = 1\nwhile true { [1, 2, 3] }").unwrap_err();
    assert_budget_error(err, "wall-clock limit exceeded");
}

#[test]
fn normal_recursion_and_loops_still_work() {
    // Default limits must not break the existing language contract.
    let mut rt = runtime_with(RuntimeLimits::default());
    let out = rt
        .run("fn fact(n) { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfact(10)")
        .unwrap();
    assert_eq!(out, Value::Int(3_628_800));

    let out = rt.run("[1, 2, 3, 4].map { |n| n * 2 }").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(2),
            Value::Int(4),
            Value::Int(6),
            Value::Int(8)
        ])
    );
    assert!(rt.usage().loop_iterations >= 4);
}

#[test]
fn usage_is_observable_after_a_successful_run() {
    let mut rt = runtime_with(RuntimeLimits::default());
    rt.run("x = [1, 2, 3]\nx.each { |n| n }").unwrap();
    let usage = rt.usage();
    // 2 top-level statements + one `n` statement per `each` iteration.
    assert_eq!(usage.steps, 5);
    assert_eq!(usage.loop_iterations, 3);
    assert_eq!(usage.max_call_depth, 1);
    assert!(usage.max_value_depth >= 1);
}

#[test]
fn deep_value_nesting_is_a_controlled_error() {
    // Build a value deeper than the configured limit without a huge source.
    let mut rt = runtime_with(RuntimeLimits::default().with_max_value_depth(4));
    let err = rt.run("[[[[[[[1]]]]]]]").unwrap_err();
    assert_budget_error(err, "value nesting limit exceeded");
}

#[test]
fn deep_parser_nesting_is_a_controlled_error() {
    let mut rt = runtime_with(RuntimeLimits::default().with_max_parser_depth(8));
    let src = format!("{}1{}", "(".repeat(64), ")".repeat(64));
    let err = rt.run(&src).unwrap_err();
    let text = format!("{err}");
    assert!(
        text.contains("parser nesting limit exceeded") || text.contains("token limit exceeded"),
        "{text:?}"
    );
    let ferr = err.downcast_ref::<FirelangError>().expect("typed error");
    assert_eq!(ferr.kind, ErrorKind::Parse, "{text:?}");
}

#[test]
fn deep_unary_nesting_does_not_overflow_the_stack() {
    let src = format!("{}1", "!".repeat(5_000));
    let err = firelang::run_script(&src).unwrap_err();
    let text = format!("{err}");
    assert!(text.contains("parser nesting limit exceeded"), "{text:?}");
}

#[test]
fn deep_else_if_chain_does_not_overflow_the_stack() {
    let mut src = String::from("x = 1\n");
    src.push_str("if x == 0 { 0 } else ");
    for _ in 0..5_000 {
        src.push_str("if x == 0 { 0 } else ");
    }
    src.push_str("{ 1 }");
    let err = firelang::run_script(&src).unwrap_err();
    let text = format!("{err}");
    assert!(text.contains("parser nesting limit exceeded"), "{text:?}");
}

#[test]
fn query_result_materialization_is_bounded() {
    let setup: String = (0..50)
        .map(|i| format!("set(\"users/q{i}\", {{n: {i}}})"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut rt = runtime_with(RuntimeLimits::default().with_max_query_results(10));
    let err = rt
        .run(&format!("{setup}\ncol(\"users\").get()"))
        .unwrap_err();
    assert_budget_error(err, "query result limit exceeded");
    assert_eq!(rt.usage().max_query_results, 10);
}

#[test]
fn query_limit_still_wins_when_under_the_materialization_budget() {
    let setup: String = (0..20)
        .map(|i| format!("set(\"users/l{i}\", {{n: {i}}})"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut rt = runtime_with(RuntimeLimits::default().with_max_query_results(100));
    let out = rt
        .run(&format!("{setup}\ncol(\"users\").limit(3).get().len()"))
        .unwrap();
    assert_eq!(out, Value::Int(3));
    assert_eq!(rt.usage().query_results, 20);
}

#[test]
fn unlimited_budgets_are_an_explicit_opt_in() {
    // `unlimited()` removes every guard.  Recursion still consumes the native
    // Rust stack, so the depth here stays well inside a 2 MiB test-thread
    // stack; raising the limit is exactly the risk the opt-in accepts.
    let mut rt = runtime_with(RuntimeLimits::unlimited());
    let out = rt
        .run("fn f(n) { if n <= 0 { 0 } else { f(n - 1) } }\nf(40)")
        .unwrap();
    assert_eq!(out, Value::Int(0));
    assert!(rt.limits().max_call_depth.is_none());
    assert!(rt.limits().max_wall_clock.is_none());
    assert_eq!(rt.usage().max_call_depth, 41);
}

#[test]
fn default_call_depth_is_safe_on_a_small_stack() {
    // Regression guard for the shipped default: recursion must stop as a
    // controlled error, not a native stack overflow, on a 2 MiB thread.
    let handle = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut rt = runtime_with(RuntimeLimits::default());
            let err = rt.run("fn f() { f() }\nf()").unwrap_err();
            format!("{err}")
        })
        .expect("spawn");
    let text = handle.join().expect("budget error, not a crash");
    assert!(text.contains("call depth limit exceeded"), "{text:?}");
}

#[test]
fn limits_can_be_reconfigured_on_a_live_runtime() {
    let mut rt = runtime_with(RuntimeLimits::default());
    rt.set_limits(RuntimeLimits::default().with_max_call_depth(4));
    assert_eq!(rt.limits().max_call_depth, Some(4));
    let err = rt.run("fn f() { f() }\nf()").unwrap_err();
    assert_budget_error(err, "call depth limit exceeded");
}

#[test]
fn budget_failure_rolls_back_a_batch() {
    let mut rt = runtime_with(RuntimeLimits::default().with_max_steps(3));
    let err = rt
        .run("batch { set(\"users/rb\", {x: 1})\nset(\"users/rb2\", {x: 2})\nset(\"users/rb3\", {x: 3}) }")
        .unwrap_err();
    assert_budget_error(err, "step limit exceeded");
    // Nothing from the failed batch may persist.
    let out = rt.run("get(\"users/rb\")").unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn budget_failure_aborts_a_transaction() {
    let mut rt = runtime_with(RuntimeLimits::default().with_max_steps(3));
    let err = rt
        .run("transaction { set(\"users/tx_b\", {x: 1})\nset(\"users/tx_b2\", {x: 2})\nset(\"users/tx_b3\", {x: 3}) }")
        .unwrap_err();
    assert_budget_error(err, "step limit exceeded");
    let out = rt.run("get(\"users/tx_b\")").unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn embedding_entry_points_accept_limits() {
    let limits = RuntimeLimits::default().with_max_call_depth(3);
    let err = firelang::run_script_with_limits("fn f() { f() }\nf()", limits).unwrap_err();
    assert!(
        format!("{err}").contains("call depth limit exceeded"),
        "{err}"
    );

    let limits = RuntimeLimits::default().with_max_steps(1);
    let err = firelang::run_script_with_firestore_and_limits(
        "a = 1\nb = 2",
        InMemoryFirestore::default(),
        limits,
    )
    .unwrap_err();
    assert!(format!("{err}").contains("step limit exceeded"), "{err}");
}
