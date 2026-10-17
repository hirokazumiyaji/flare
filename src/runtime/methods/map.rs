use std::collections::BTreeMap;
use std::error::Error;

use crate::runtime::contract::CallContract;
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::ExecFlow;
use crate::value::Value;

pub fn map_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "keys" | "values" | "len" => Some(CallContract::exact(0)),
        "has_key" => Some(CallContract::exact(1)),
        _ => Some(CallContract::exact(0)),
    }
}

pub fn eval_map_method(
    map: BTreeMap<String, Value>,
    name: &str,
    args: Vec<Value>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match name {
        "keys" => Ok(ExecFlow::Value(Value::Array(
            map.keys().cloned().map(Value::Str).collect(),
        ))),
        "values" => Ok(ExecFlow::Value(Value::Array(
            map.values().cloned().collect(),
        ))),
        "len" => Ok(ExecFlow::Value(Value::Int(map.len() as i64))),
        "has_key" => {
            let key = match args.first() {
                Some(Value::Str(k)) => k,
                _ => return Err("has_key key must be a string".into()),
            };
            Ok(ExecFlow::Value(Value::Bool(map.contains_key(key))))
        }
        _ => {
            if !args.is_empty() {
                return Err(RuntimeError::UnknownMethod {
                    receiver: "map".to_string(),
                    name: name.to_string(),
                }
                .into());
            }
            Ok(ExecFlow::Value(
                map.get(name).cloned().unwrap_or(Value::Nil),
            ))
        }
    }
}
