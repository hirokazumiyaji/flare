use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::{BudgetError, UserRaisedError};
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

pub fn builtin_raise(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected 1 argument")?;
    let msg = match val {
        Value::Str(s) => s.clone(),
        other => other.to_string(),
    };
    Err(UserRaisedError::new(msg).into())
}

pub fn builtin_to_s(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected 1 argument")?;
    let res = Value::Str(val.to_string());
    rt.check_value(&res)?;
    Ok(ExecFlow::Value(res))
}

pub fn builtin_inspect(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected 1 argument")?;
    let res = Value::Str(val.inspect());
    rt.check_value(&res)?;
    Ok(ExecFlow::Value(res))
}

pub fn builtin_type_of(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected 1 argument")?;
    let res = Value::Str(val.type_name().to_string());
    rt.check_value(&res)?;
    Ok(ExecFlow::Value(res))
}

pub fn builtin_to_i(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected 1 argument")?;
    let res = match val {
        Value::Int(i) => Value::Int(*i),
        Value::Str(s) => {
            let parsed = s
                .trim()
                .parse::<i64>()
                .map_err(|_| format!("invalid integer: {s}"))?;
            Value::Int(parsed)
        }
        _ => return Err("to_i expects string or int argument".into()),
    };
    rt.check_value(&res)?;
    Ok(ExecFlow::Value(res))
}

pub fn builtin_array(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Array(args)))
}

pub fn builtin_range(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let (start, end, step) = match args.len() {
        1 => (0i64, expect_int(args.first())?, 1i64),
        2 => (expect_int(args.first())?, expect_int(args.get(1))?, 1i64),
        3 => {
            let step = expect_int(args.get(2))?;
            if step == 0 {
                return Err("range step cannot be zero".into());
            }
            (expect_int(args.first())?, expect_int(args.get(1))?, step)
        }
        _ => return Err("range expects 1, 2, or 3 arguments".into()),
    };

    let count: usize = if step > 0 {
        if start >= end {
            0
        } else {
            let diff = (end as i128) - (start as i128);
            let steps = (diff - 1) / (step as i128) + 1;
            usize::try_from(steps).map_err(|_| "range count exceeds addressable size")?
        }
    } else if start <= end {
        0
    } else {
        let diff = (start as i128) - (end as i128);
        let pos_step = -(step as i128);
        let steps = (diff - 1) / pos_step + 1;
        usize::try_from(steps).map_err(|_| "range count exceeds addressable size")?
    };

    let total_nodes = count.saturating_add(1);
    if let Some(limit) = rt.state.limits.max_value_nodes {
        if total_nodes > limit {
            return Err(
                BudgetError::new(format!("value node limit exceeded (max {limit})")).into(),
            );
        }
    }

    let mut items = Vec::with_capacity(count);
    let mut cur = start;
    for i in 0..count {
        items.push(Value::Int(cur));
        if i + 1 < count {
            cur = cur.checked_add(step).ok_or("range overflow")?;
        }
    }
    Ok(ExecFlow::Value(Value::Array(items)))
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
    // `args.len() == 1` は上で保証されているので `next()` は必ず `Some`。
    // 取り出しを下の `match` にまとめ、到達不能な `ok_or` 分岐を作らない。
    let bytes = match args.into_iter().next() {
        Some(Value::Str(s)) => {
            if let Some(max) = rt.limits.max_bytes_size {
                if s.len() > max {
                    return Err(
                        BudgetError::new(format!("bytes size limit exceeded (max {max})")).into(),
                    );
                }
            }
            s.into_bytes()
        }
        Some(Value::Array(items)) => {
            if let Some(max) = rt.limits.max_bytes_size {
                if items.len() > max {
                    return Err(
                        BudgetError::new(format!("bytes size limit exceeded (max {max})")).into(),
                    );
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
