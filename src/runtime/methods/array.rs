use std::error::Error;

use crate::ast::Block;
use crate::runtime::contract::{check_block_arity, CallContract};
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::value::Value;

pub fn array_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "each" | "map" | "filter" | "find" => Some(CallContract::exact(0).requiring_block()),
        "len" => Some(CallContract::exact(0)),
        "contains" => Some(CallContract::exact(1)),
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
        "filter" => {
            let Some(block) = block else {
                return Err(RuntimeError::RequiresBlock("filter".to_string()).into());
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
        _ => Err(RuntimeError::UnknownMethod {
            receiver: "array".to_string(),
            name: name.to_string(),
        }
        .into()),
    }
}
