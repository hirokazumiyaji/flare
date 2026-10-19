use std::error::Error;

use crate::ast::Block;
use crate::runtime::contract::{check_block_arity, CallContract};
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::value::Value;

pub fn array_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "each" | "map" | "filter" | "select" | "reject" | "find" | "each_with_index"
        | "map_with_index" | "sort_by" | "any" | "all" => {
            Some(CallContract::exact(0).requiring_block())
        }
        "reduce" | "inject" => Some(CallContract::range(0, 1).requiring_block()),
        "len" | "sort" | "reverse" | "first" | "last" | "uniq" | "flatten" => {
            Some(CallContract::exact(0))
        }
        "contains" => Some(CallContract::exact(1)),
        "slice" => Some(CallContract::exact(2)),
        "join" => Some(CallContract::range(0, 1)),
        _ => None,
    }
}

pub fn eval_array_method(
    rt: &mut Evaluator<'_>,
    items: Vec<Value>,
    name: &str,
    args: Vec<Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match name {
        "each" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("each".to_string()).into());
            };
            // 空配列でも呼び出し側の引数個数違反を検出できるよう、
            // ループの前にブロック引数の個数を検証する。
            check_block_arity(block, 1)?;
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item])? {
                    ExecFlow::Value(_) => {}
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Nil))
        }
        "map" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("map".to_string()).into());
            };
            check_block_arity(block, 1)?;
            let mut out = vec![];
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item])? {
                    ExecFlow::Value(v) => out.push(v),
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Array(out)))
        }
        "filter" | "select" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock(name.to_string()).into());
            };
            check_block_arity(block, 1)?;
            let mut out = vec![];
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item.clone()])? {
                    ExecFlow::Value(v) => {
                        if v.truthy() {
                            out.push(item);
                        }
                    }
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Array(out)))
        }
        "reject" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("reject".to_string()).into());
            };
            check_block_arity(block, 1)?;
            let mut out = vec![];
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item.clone()])? {
                    ExecFlow::Value(v) => {
                        if !v.truthy() {
                            out.push(item);
                        }
                    }
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Array(out)))
        }
        "each_with_index" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("each_with_index".to_string()).into());
            };
            check_block_arity(block, 2)?;
            for (i, item) in items.into_iter().enumerate() {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item, Value::Int(i as i64)])? {
                    ExecFlow::Value(_) => {}
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Nil))
        }
        "map_with_index" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("map_with_index".to_string()).into());
            };
            check_block_arity(block, 2)?;
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item, Value::Int(i as i64)])? {
                    ExecFlow::Value(v) => out.push(v),
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Array(out)))
        }
        "reduce" | "inject" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock(name.to_string()).into());
            };
            check_block_arity(block, 2)?;
            if let Some(init) = args.into_iter().next() {
                let mut acc = init;
                for item in items {
                    rt.consume_loop_iteration()?;
                    match rt.call_block(block, vec![acc, item])? {
                        ExecFlow::Value(v) => acc = v,
                        ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                    }
                }
                Ok(ExecFlow::Value(acc))
            } else {
                let mut iter = items.into_iter();
                let Some(first) = iter.next() else {
                    return Ok(ExecFlow::Value(Value::Nil));
                };
                let mut acc = first;
                for item in iter {
                    rt.consume_loop_iteration()?;
                    match rt.call_block(block, vec![acc, item])? {
                        ExecFlow::Value(v) => acc = v,
                        ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                    }
                }
                Ok(ExecFlow::Value(acc))
            }
        }
        "find" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("find".to_string()).into());
            };
            check_block_arity(block, 1)?;
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item.clone()])? {
                    ExecFlow::Value(v) => {
                        if v.truthy() {
                            return Ok(ExecFlow::Value(item));
                        }
                    }
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Nil))
        }
        "contains" => {
            let target = args.first().ok_or("contains requires 1 argument")?;
            let found = items.iter().any(|item| item == target);
            Ok(ExecFlow::Value(Value::Bool(found)))
        }
        "join" => {
            let sep = match args.first() {
                Some(Value::Str(sep)) => sep.as_str(),
                Some(_) => return Err("join separator must be a string".into()),
                None => "",
            };
            let out = items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
                .join(sep);
            Ok(ExecFlow::Value(Value::Str(out)))
        }
        "len" => Ok(ExecFlow::Value(Value::Int(items.len() as i64))),
        "sort" => {
            let mut sorted = items;
            let mut sort_err: Option<Box<dyn Error>> = None;
            sorted.sort_by(|a, b| {
                if sort_err.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                if let Err(e) = rt.consume_loop_iteration() {
                    sort_err = Some(e);
                    return std::cmp::Ordering::Equal;
                }
                Value::cmp_for_order(a, b)
            });
            if let Some(err) = sort_err {
                return Err(err);
            }
            Ok(ExecFlow::Value(Value::Array(sorted)))
        }
        "sort_by" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("sort_by".to_string()).into());
            };
            check_block_arity(block, 1)?;
            let mut keyed = Vec::with_capacity(items.len());
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item.clone()])? {
                    ExecFlow::Value(k) => keyed.push((k, item)),
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            let mut sort_err: Option<Box<dyn Error>> = None;
            keyed.sort_by(|(k1, _), (k2, _)| {
                if sort_err.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                if let Err(e) = rt.consume_loop_iteration() {
                    sort_err = Some(e);
                    return std::cmp::Ordering::Equal;
                }
                k1.cmp_for_order(k2)
            });
            if let Some(err) = sort_err {
                return Err(err);
            }
            let sorted = keyed.into_iter().map(|(_, item)| item).collect();
            Ok(ExecFlow::Value(Value::Array(sorted)))
        }
        "reverse" => {
            let mut rev = items;
            rev.reverse();
            Ok(ExecFlow::Value(Value::Array(rev)))
        }
        "first" => Ok(ExecFlow::Value(
            items.first().cloned().unwrap_or(Value::Nil),
        )),
        "last" => Ok(ExecFlow::Value(items.last().cloned().unwrap_or(Value::Nil))),
        "slice" => {
            let start = match args.first() {
                Some(Value::Int(start)) => *start,
                _ => return Err("slice start must be an int".into()),
            };
            let len = match args.get(1) {
                Some(Value::Int(len)) => *len,
                _ => return Err("slice len must be an int".into()),
            };
            if start < 0 {
                return Err("slice start must be >= 0".into());
            }
            if len < 0 {
                return Err("slice len must be >= 0".into());
            }
            let start = (start as usize).min(items.len());
            let end = start.saturating_add(len as usize).min(items.len());
            Ok(ExecFlow::Value(Value::Array(items[start..end].to_vec())))
        }
        "uniq" => {
            let mut out = Vec::new();
            for item in items {
                rt.consume_loop_iteration()?;
                if !out.contains(&item) {
                    out.push(item);
                }
            }
            Ok(ExecFlow::Value(Value::Array(out)))
        }
        "flatten" => {
            let mut out = Vec::new();
            for item in items {
                rt.consume_loop_iteration()?;
                match item {
                    Value::Array(nested) => out.extend(nested),
                    other => out.push(other),
                }
            }
            Ok(ExecFlow::Value(Value::Array(out)))
        }
        "any" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("any".to_string()).into());
            };
            check_block_arity(block, 1)?;
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item])? {
                    ExecFlow::Value(v) => {
                        if v.truthy() {
                            return Ok(ExecFlow::Value(Value::Bool(true)));
                        }
                    }
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Bool(false)))
        }
        "all" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("all".to_string()).into());
            };
            check_block_arity(block, 1)?;
            for item in items {
                rt.consume_loop_iteration()?;
                match rt.call_block(block, vec![item])? {
                    ExecFlow::Value(v) => {
                        if !v.truthy() {
                            return Ok(ExecFlow::Value(Value::Bool(false)));
                        }
                    }
                    ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                }
            }
            Ok(ExecFlow::Value(Value::Bool(true)))
        }
        _ => Err(RuntimeError::UnknownMethod {
            receiver: "array".to_string(),
            name: name.to_string(),
        }
        .into()),
    }
}
