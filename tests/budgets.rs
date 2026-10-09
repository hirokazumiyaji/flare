//! Runtime budget tests (issue #82).
//!
//! Every hostile script here must produce a controlled `FlareError` with a
//! `Runtime` kind and a source position -- never a panic, a stack overflow, or
//! a hang.

use std::time::Duration;

use flare::firestore::{Firestore, QueryCandidates, WriteOp};
use flare::runtime::Runtime;
use flare::value::QueryRef;
use flare::{ErrorKind, FlareError, InMemoryFirestore, RuntimeLimits, Value};

fn runtime_with(limits: RuntimeLimits) -> Runtime<InMemoryFirestore> {
    Runtime::with_limits(InMemoryFirestore::default(), limits)
}

fn assert_budget_error(err: Box<dyn std::error::Error>, needle: &str) {
    assert_budget_error_kind(err, ErrorKind::Runtime, needle)
}

/// Every budget failure carries a position and a known kind. The kind is
/// `Runtime` for evaluation-time budgets and `Parse` for the nesting budget,
/// which the parser and the external-AST guard report with one shared message.
fn assert_budget_error_kind(err: Box<dyn std::error::Error>, kind: ErrorKind, needle: &str) {
    let text = format!("{err}");
    assert!(text.contains(needle), "expected {needle:?} in {text:?}");
    let ferr = err
        .downcast_ref::<FlareError>()
        .unwrap_or_else(|| panic!("budget failure should be a FlareError: {text:?}"));
    assert_eq!(ferr.kind, kind, "{text:?}");
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
    let ferr = err.downcast_ref::<FlareError>().expect("typed error");
    assert_eq!(ferr.kind, ErrorKind::Parse, "{text:?}");
}

#[test]
fn deep_unary_nesting_does_not_overflow_the_stack() {
    let src = format!("{}1", "!".repeat(5_000));
    let err = flare::run_script(&src).unwrap_err();
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
    let err = flare::run_script(&src).unwrap_err();
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
    // `usage` counts the rows the script received, not the candidates scanned.
    assert_eq!(rt.usage().query_results, 3);
}

#[test]
fn unlimited_budgets_are_an_explicit_opt_in() {
    // `unlimited()` removes every guard.  Recursion still consumes the native
    // Rust stack, so the depth here stays well inside a 2 MiB test-thread
    // stack (see `default_call_depth_is_safe_on_a_small_stack`); raising the
    // limit past that is exactly the risk the opt-in accepts.
    let mut rt = runtime_with(RuntimeLimits::unlimited());
    let out = rt
        .run("fn f(n) { if n <= 0 { 0 } else { f(n - 1) } }\nf(12)")
        .unwrap();
    assert_eq!(out, Value::Int(0));
    assert!(rt.limits().max_call_depth.is_none());
    assert!(rt.limits().max_wall_clock.is_none());
    assert_eq!(rt.usage().max_call_depth, 13);
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
    let err = flare::run_script_with_limits("fn f() { f() }\nf()", limits).unwrap_err();
    assert!(
        format!("{err}").contains("call depth limit exceeded"),
        "{err}"
    );

    let limits = RuntimeLimits::default().with_max_steps(1);
    let err = flare::run_script_with_firestore_and_limits(
        "a = 1\nb = 2",
        InMemoryFirestore::default(),
        limits,
    )
    .unwrap_err();
    assert!(format!("{err}").contains("step limit exceeded"), "{err}");
}

#[test]
fn value_depth_limit_is_inclusive_of_the_deepest_existing_level() {
    use flare::validate_value_limits;
    let leaf = Value::Int(1);
    let one_deep = Value::Array(vec![leaf.clone()]);
    let two_deep = Value::Array(vec![one_deep.clone()]);

    // The root is depth 0, so `[1]` puts a leaf at depth 1 and is valid at a
    // limit of 1. Only a child that actually exists may push the depth over.
    assert_eq!(validate_value_limits(&one_deep, Some(1), None).unwrap(), 1);
    assert!(validate_value_limits(&two_deep, Some(1), None)
        .unwrap_err()
        .to_string()
        .contains("value nesting limit exceeded"));
    // An empty container has no children, so it is valid even at a limit of
    // zero.
    assert_eq!(
        validate_value_limits(&Value::Array(vec![]), Some(0), None).unwrap(),
        0
    );
    assert_eq!(validate_value_limits(&leaf, Some(0), None).unwrap(), 0);
}

#[test]
fn value_node_limit_is_enforced_while_enqueuing_children() {
    use flare::validate_value_limits;
    // A backend-supplied value far wider than the budget must be rejected
    // without the traversal first enqueueing every child.
    let wide = Value::Array((0..10_000).map(Value::Int).collect());
    let err = validate_value_limits(&wide, None, Some(64)).unwrap_err();
    assert!(
        format!("{err}").contains("value node limit exceeded (max 64)"),
        "{err}"
    );
    let wide_map = Value::Map(
        (0..10_000)
            .map(|index| (format!("k{index}"), Value::Int(index)))
            .collect(),
    );
    assert!(validate_value_limits(&wide_map, None, Some(64)).is_err());
    // The root counts as one node, so a zero budget rejects even a leaf.
    assert!(validate_value_limits(&Value::Int(1), None, Some(0)).is_err());
}

#[test]
fn scalar_results_count_against_the_value_node_limit() {
    // The validator treats every root as one node, so a script that only
    // produces a scalar must still be rejected at a zero node budget.
    let mut rt = runtime_with(RuntimeLimits::default().with_max_value_nodes(0));
    assert_budget_error(rt.run("1").unwrap_err(), "value node limit exceeded");
    // A composite value is rejected for the same reason.
    let mut rt = runtime_with(RuntimeLimits::default().with_max_value_nodes(0));
    assert_budget_error(rt.run("[1]").unwrap_err(), "value node limit exceeded");
}

#[test]
fn external_asts_are_depth_checked_before_evaluation() {
    use flare::ast::{Expr, Stmt, UnOp};

    // An embedder can hand the public API an AST it built itself, so the
    // parser-depth budget has to hold for ASTs that never went through the
    // parser. A unary chain far past the default depth of 128 would otherwise
    // recurse through `eval_expr` until the thread stack is gone.
    let mut expr = Expr::Int(1);
    for _ in 0..5_000 {
        expr = Expr::Unary {
            op: UnOp::Neg,
            expr: Box::new(expr),
        };
    }
    let stmt = Stmt::Expr(expr);
    let mut rt = runtime_with(RuntimeLimits::default());
    let err = rt.eval_program(std::slice::from_ref(&stmt)).unwrap_err();
    // The external-AST guard reuses the parser's own message and kind, so an
    // embedder sees the same error whether the AST came from `parse` or from
    // its own code.
    assert_budget_error_kind(err, ErrorKind::Parse, "parser nesting limit exceeded");
    // Leaking the statement keeps the recursive `Box` drop of a 5,000-deep AST
    // out of the test: that is a property of nested boxes in Rust, not of the
    // guard under test, and it would fail for the wrong reason.
    std::mem::forget(stmt);
}

/// A backend whose `apply` sleeps past the wall-clock budget and then
/// succeeds, so a post-write deadline check would report a persistent write as
/// a timeout.
struct SlowSuccessfulWrite {
    stored: std::collections::BTreeMap<String, Value>,
}

impl SlowSuccessfulWrite {
    fn new() -> Self {
        Self {
            stored: std::collections::BTreeMap::new(),
        }
    }
}

impl Firestore for SlowSuccessfulWrite {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn std::error::Error>> {
        Ok(self.stored.get(path).cloned())
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn std::error::Error>> {
        std::thread::sleep(Duration::from_millis(30));
        for op in writes {
            match op {
                WriteOp::Set { path, data, .. } => {
                    self.stored.insert(path, Value::Map(data));
                }
                WriteOp::Update { path, data, .. } => {
                    self.stored.insert(path, Value::Map(data));
                }
                WriteOp::Delete { path } => {
                    self.stored.remove(&path);
                }
            }
        }
        Ok(())
    }

    fn run_query(&self, _query: &QueryRef) -> Result<QueryCandidates, Box<dyn std::error::Error>> {
        Ok(vec![])
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Err("transactions are not used by this test".into())
    }

    fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn std::error::Error>> {
        Err("transactions are not used by this test".into())
    }

    fn commit_transaction(
        &mut self,
        _writes: Vec<WriteOp>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Err("transactions are not used by this test".into())
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

#[test]
fn a_successful_write_is_never_reported_as_a_timeout() {
    // The backend does not support a deadline, so the wall-clock budget can
    // only be checked before the call. Reporting a timeout after a write that
    // already persisted would make a retry apply `increment()` twice, so the
    // result of a completed write is returned as-is.
    let limits = RuntimeLimits::default().with_max_wall_clock(Duration::from_millis(1));
    let mut rt = Runtime::with_limits(SlowSuccessfulWrite::new(), limits);
    // The write sleeps 30ms, so the budget is long expired by the time the
    // backend returns success. The run must still report success; the write is
    // already persistent, and a timeout here would invite a retry that applies
    // `increment()` twice.
    rt.run("set(\"users/slow\", {visits: increment(1)})")
        .unwrap();
}

#[test]
fn a_transaction_that_outruns_its_budget_is_aborted_before_commit() {
    // The block finishes, but the final pre-commit check crosses the budget.
    // The open transaction has to be aborted, or the next transaction on the
    // same runtime fails with "already in transaction".
    struct SlowBegin {
        inner: InMemoryFirestore,
    }

    impl Firestore for SlowBegin {
        fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn std::error::Error>> {
            self.inner.get(path)
        }
        fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn std::error::Error>> {
            self.inner.apply(writes)
        }
        fn run_query(
            &self,
            query: &QueryRef,
        ) -> Result<QueryCandidates, Box<dyn std::error::Error>> {
            self.inner.run_query(query)
        }
        fn run_query_page(
            &self,
            query: &QueryRef,
        ) -> Result<QueryCandidates, Box<dyn std::error::Error>> {
            self.inner.run_query_page(query)
        }
        fn count_query(&self, query: &QueryRef) -> Result<i64, Box<dyn std::error::Error>> {
            self.inner.count_query(query)
        }
        fn begin_transaction(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            self.inner.begin_transaction()?;
            // A slow `begin_transaction` is the documented reason the
            // post-begin check has to abort rather than just return.
            std::thread::sleep(Duration::from_millis(20));
            Ok(())
        }
        fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn std::error::Error>> {
            self.inner.transaction_get(path)
        }
        fn commit_transaction(
            &mut self,
            writes: Vec<WriteOp>,
        ) -> Result<(), Box<dyn std::error::Error>> {
            self.inner.commit_transaction(writes)
        }
        fn abort_transaction(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            self.inner.abort_transaction()
        }
    }

    let limits = RuntimeLimits::default().with_max_wall_clock(Duration::from_millis(5));
    let mut rt = Runtime::with_limits(
        SlowBegin {
            inner: InMemoryFirestore::default(),
        },
        limits,
    );
    assert_budget_error(
        rt.run("transaction { set(\"users/tx_slow\", {x: 1}) }")
            .unwrap_err(),
        "wall-clock limit exceeded",
    );
    // Same runtime, budget restored: a left-behind transaction would make the
    // next one fail with "already in transaction", and the write from the
    // aborted transaction must not have persisted.
    rt.set_limits(RuntimeLimits::default());
    rt.run("transaction { set(\"users/tx_ok\", {x: 1}) }")
        .unwrap();
    assert_eq!(
        rt.run("get(\"users/tx_slow\")").unwrap(),
        Value::Nil,
        "the aborted transaction must not persist"
    );
}
