use std::collections::BTreeMap;
use std::error::Error;

use crate::runtime::contract::CallContract;
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::ExecFlow;
use crate::value::Value;

pub fn map_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "keys" | "values" | "len" | "entries" => Some(CallContract::exact(0)),
        "has_key" | "delete" | "merge" => Some(CallContract::exact(1)),
        _ => None,
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
        "entries" => {
            let entries = map
                .iter()
                .map(|(k, v)| Value::Array(vec![Value::Str(k.clone()), v.clone()]))
                .collect();
            Ok(ExecFlow::Value(Value::Array(entries)))
        }
        "delete" => {
            let key = match args.first() {
                Some(Value::Str(k)) => k,
                _ => return Err("delete key must be a string".into()),
            };
            let mut new_map = map.clone();
            new_map.remove(key);
            Ok(ExecFlow::Value(Value::Map(new_map)))
        }
        "merge" => {
            let other = match args.first() {
                Some(Value::Map(m)) => m,
                _ => return Err("merge argument must be a map".into()),
            };
            let mut new_map = map.clone();
            new_map.extend(other.clone());
            Ok(ExecFlow::Value(Value::Map(new_map)))
        }
        _ => Err(RuntimeError::UnknownMethod {
            receiver: "map".to_string(),
            name: name.to_string(),
        }
        .into()),
    }
}
