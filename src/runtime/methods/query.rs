use std::error::Error;

use crate::error::BudgetError;
use crate::firestore::{normalize_query_field_path, QueryCandidates, QueryResultLimitExceeded};
use crate::runtime::contract::CallContract;
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::Evaluator;
use crate::runtime::{backend_err, expect_int, expect_string, reject_non_document_value, wrap_doc};
use crate::value::{Direction, QueryFilter, QueryOp, QueryRef, Value};

pub fn query_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "where" => Some(CallContract::exact(3)),
        "order_by" => Some(CallContract::range(1, 2)),
        "limit" | "offset" => Some(CallContract::exact(1)),
        "count" | "get" => Some(CallContract::exact(0)),
        _ => None,
    }
}

pub fn eval_query_method(
    rt: &mut Evaluator<'_>,
    mut query: QueryRef,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, Box<dyn Error>> {
    match name {
        "where" => {
            // Field path は正規化してから保存する。in-memory の lookup と
            // emulator の wire 表現が同じ spelling を共有する。
            let field = normalize_query_field_path(expect_string(args.first())?)?;
            let op = QueryOp::parse(expect_string(args.get(1))?)?;
            let value = args.get(2).ok_or("where requires value")?;
            reject_non_document_value(&field, value)?;
            query.filters.push(QueryFilter {
                field,
                op,
                value: value.clone(),
            });
            Ok(Value::QueryRef(query))
        }
        "limit" => {
            let n = expect_int(args.first())?;
            let n = usize::try_from(n).map_err(|_| "limit must be >= 0")?;
            query.limit = Some(n);
            Ok(Value::QueryRef(query))
        }
        "offset" => {
            let n = expect_int(args.first())?;
            let n = usize::try_from(n).map_err(|_| "offset must be >= 0")?;
            query.offset = Some(n);
            Ok(Value::QueryRef(query))
        }
        "order_by" => {
            let field = normalize_query_field_path(expect_string(args.first())?)?;
            let dir = match args.get(1) {
                Some(val) => Direction::parse(expect_string(Some(val))?)?,
                None => Direction::Asc,
            };
            query.order_by.push((field, dir));
            Ok(Value::QueryRef(query))
        }
        "count" => {
            rt.forbid_query_in_batch_or_transaction()?;
            // 集計はフィルタ一致件数を数える。順序・offset・limit は無視する。
            let mut unpaged = query.clone();
            unpaged.order_by.clear();
            unpaged.offset = None;
            unpaged.limit = None;
            // `count` returns a number, not rows. Counting still walks
            // every match, so it must respect the candidate budget too.
            Ok(Value::Int(count_query_with_budget(rt, &unpaged)?))
        }
        "get" => {
            rt.forbid_query_in_batch_or_transaction()?;
            let rows = run_query_with_budget(rt, &query)?;
            let result = Value::Array(
                rows.into_iter()
                    .map(|(path, doc)| wrap_doc(&path, &doc))
                    .collect(),
            );
            // `run_query_with_budget` checks each document field, but the
            // value handed to the script also contains the row maps, their
            // `__name__` entries, and the enclosing array.
            rt.check_value(&result)?;
            Ok(result)
        }
        _ => Err(RuntimeError::UnknownMethod {
            receiver: "query".to_string(),
            name: name.to_string(),
        }
        .into()),
    }
}

/// Count query matches under the result budget.
///
/// `count_query` returns a number, so nothing is materialized; the budget
/// still applies because counting walks every match.
pub fn count_query_with_budget(
    rt: &mut Evaluator<'_>,
    query: &QueryRef,
) -> Result<i64, Box<dyn Error>> {
    rt.check_wall_clock()?;
    let max_results = rt.limits.max_query_results;
    let count = match rt.services.firestore.count_query(query) {
        Ok(count) => count,
        Err(e) => {
            if let Some(limit) = e.downcast_ref::<QueryResultLimitExceeded>() {
                rt.usage.max_query_results = rt.usage.max_query_results.max(limit.max_results);
                return Err(BudgetError::new(limit.to_string()).into());
            }
            return Err(backend_err(e));
        }
    };
    rt.check_wall_clock()?;
    let count = usize::try_from(count)
        .map_err(|_| "query result is too large to represent as an integer".to_string())?;
    if let Some(max) = max_results {
        if count > max {
            rt.usage.max_query_results = rt.usage.max_query_results.max(max);
            return Err(
                BudgetError::new(format!("query result limit exceeded (max {max})")).into(),
            );
        }
    }
    rt.usage.query_results = rt.usage.query_results.saturating_add(count as u64);
    rt.usage.max_query_results = rt.usage.max_query_results.max(count);
    Ok(count as i64)
}

/// Run a query page under the result budget.
pub fn run_query_with_budget(
    rt: &mut Evaluator<'_>,
    query: &QueryRef,
) -> Result<QueryCandidates, Box<dyn Error>> {
    rt.check_wall_clock()?;
    let max_results = rt.limits.max_query_results;
    // The bounded hook is the resource boundary: a backend that refuses to
    // materialize more than `max` rows errors instead of returning a
    // partial vector. The length check below stays as a defense for legacy
    // backends that keep the trait's compatibility fallback.
    let result = match max_results {
        Some(max) => rt.services.firestore.run_query_bounded(query, max),
        None => rt.services.firestore.run_query_page(query),
    };
    let rows = match result {
        Ok(rows) => rows,
        Err(e) => {
            // Budget consumption stays observable on the failure path, but
            // only for the dedicated marker: a genuine backend error must
            // not be recorded as a budget failure.
            if let Some(limit) = e.downcast_ref::<QueryResultLimitExceeded>() {
                rt.usage.max_query_results = rt.usage.max_query_results.max(limit.max_results);
                return Err(BudgetError::new(limit.to_string()).into());
            }
            return Err(backend_err(e));
        }
    };
    rt.check_wall_clock()?;

    let count = rows.len();
    if let Some(max) = max_results {
        if count > max {
            rt.usage.max_query_results = rt.usage.max_query_results.max(max);
            return Err(
                BudgetError::new(format!("query result limit exceeded (max {max})")).into(),
            );
        }
    }
    rt.usage.query_results = rt.usage.query_results.saturating_add(count as u64);
    rt.usage.max_query_results = rt.usage.max_query_results.max(count);
    for (_, fields) in &rows {
        for value in fields.values() {
            rt.check_value(value)?;
        }
    }
    Ok(rows)
}
