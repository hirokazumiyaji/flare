use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::FlareError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::{
    expect_array, expect_int, parse_timestamp_arg, BYTES_ELEMENT_ERR, BYTES_USAGE,
};
use crate::value::{Timestamp, Value};

pub fn builtin_puts(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    for v in args {
        println!("{v}");
    }
    Ok(ExecFlow::Value(Value::Nil))
}

pub fn builtin_array(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Array(args)))
}

pub fn builtin_len(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match args.first() {
        Some(Value::Array(a)) => Ok(ExecFlow::Value(Value::Int(a.len() as i64))),
        Some(Value::Map(m)) => Ok(ExecFlow::Value(Value::Int(m.len() as i64))),
        Some(Value::Str(s)) => Ok(ExecFlow::Value(Value::Int(s.chars().count() as i64))),
        Some(Value::Bytes(bytes)) => Ok(ExecFlow::Value(Value::Int(bytes.len() as i64))),
        _ => Err("len expects array, map, string, or bytes".into()),
    }
}

pub fn builtin_now(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Timestamp(Timestamp::now()?)))
}

pub fn builtin_timestamp(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Timestamp(Timestamp {
        seconds: parse_timestamp_arg(args.first())?,
    })))
}

pub fn builtin_bytes(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    if args.len() != 1 || !kwargs.is_empty() || block.is_some() {
        return Err(BYTES_USAGE.into());
    }
    let bytes = match args.into_iter().next() {
        Some(Value::Str(s)) => {
            if let Some(max) = rt.limits.max_bytes_size {
                if s.len() > max {
                    return Err(FlareError::runtime(format!(
                        "bytes size limit exceeded (max {max})"
                    ))
                    .into());
                }
            }
            s.into_bytes()
        }
        Some(Value::Array(items)) => {
            if let Some(max) = rt.limits.max_bytes_size {
                if items.len() > max {
                    return Err(FlareError::runtime(format!(
                        "bytes size limit exceeded (max {max})"
                    ))
                    .into());
                }
            }
            items
                .into_iter()
                .map(|value| match value {
                    Value::Int(byte) => {
                        u8::try_from(byte).map_err(|_| BYTES_ELEMENT_ERR.to_string())
                    }
                    _ => Err(BYTES_ELEMENT_ERR.to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => return Err(BYTES_USAGE.into()),
    };
    Ok(ExecFlow::Value(Value::Bytes(bytes)))
}

pub fn builtin_server_timestamp(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ServerTimestamp))
}

pub fn builtin_array_union(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ArrayUnion(
        expect_array(args.first())?.clone(),
    )))
}

pub fn builtin_array_remove(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ArrayRemove(
        expect_array(args.first())?.clone(),
    )))
}

pub fn builtin_delete_field(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::DeleteField))
}

pub fn builtin_increment(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Increment(expect_int(args.first())?)))
}
