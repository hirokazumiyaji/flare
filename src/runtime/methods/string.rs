use std::error::Error;

use crate::runtime::contract::CallContract;
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::ExecFlow;
use crate::value::Value;

pub fn str_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "len" | "trim" => Some(CallContract::exact(0)),
        "split" | "starts_with" | "ends_with" | "contains" => Some(CallContract::exact(1)),
        _ => None,
    }
}

pub fn eval_string_method(
    s: String,
    name: &str,
    args: Vec<Value>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match name {
        "len" => Ok(ExecFlow::Value(Value::Int(s.chars().count() as i64))),
        "trim" => Ok(ExecFlow::Value(Value::Str(s.trim().to_string()))),
        "split" => {
            let sep = match args.first() {
                Some(Value::Str(sep)) => sep,
                _ => return Err("split separator must be a string".into()),
            };
            let parts = if sep.is_empty() {
                s.chars().map(|c| Value::Str(c.to_string())).collect()
            } else {
                s.split(sep.as_str())
                    .map(|part| Value::Str(part.to_string()))
                    .collect()
            };
            Ok(ExecFlow::Value(Value::Array(parts)))
        }
        "starts_with" => {
            let prefix = match args.first() {
                Some(Value::Str(prefix)) => prefix,
                _ => return Err("starts_with prefix must be a string".into()),
            };
            Ok(ExecFlow::Value(Value::Bool(s.starts_with(prefix.as_str()))))
        }
        "ends_with" => {
            let suffix = match args.first() {
                Some(Value::Str(suffix)) => suffix,
                _ => return Err("ends_with suffix must be a string".into()),
            };
            Ok(ExecFlow::Value(Value::Bool(s.ends_with(suffix.as_str()))))
        }
        "contains" => {
            let substr = match args.first() {
                Some(Value::Str(substr)) => substr,
                _ => return Err("contains substring must be a string".into()),
            };
            Ok(ExecFlow::Value(Value::Bool(s.contains(substr.as_str()))))
        }
        _ => Err(RuntimeError::UnknownMethod {
            receiver: "string".to_string(),
            name: name.to_string(),
        }
        .into()),
    }
}
