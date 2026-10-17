use std::error::Error;

use crate::auth::AuthSession;
use crate::runtime::builtins::auth::auth_session_field;
use crate::runtime::contract::CallContract;
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::ExecFlow;
use crate::runtime::expect_int;
use crate::value::{Timestamp, Value};

pub fn timestamp_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "to_unix" | "to_rfc3339" => Some(CallContract::exact(0)),
        "add_seconds" => Some(CallContract::exact(1)),
        _ => None,
    }
}

pub fn bytes_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "len" => Some(CallContract::exact(0)),
        _ => None,
    }
}

pub fn auth_session_method_contract(_name: &str) -> Option<CallContract> {
    Some(CallContract::exact(0))
}

pub fn eval_timestamp_method(
    ts: Timestamp,
    name: &str,
    args: Vec<Value>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match name {
        "to_unix" => Ok(ExecFlow::Value(Value::Int(ts.seconds))),
        "to_rfc3339" => Ok(ExecFlow::Value(Value::Str(ts.to_rfc3339()?))),
        "add_seconds" => Ok(ExecFlow::Value(Value::Timestamp(Timestamp {
            seconds: ts
                .seconds
                .checked_add(expect_int(args.first())?)
                .ok_or(RuntimeError::IntegerOverflow)?,
        }))),
        _ => Err(RuntimeError::UnknownMethod {
            receiver: "timestamp".to_string(),
            name: name.to_string(),
        }
        .into()),
    }
}

pub fn eval_bytes_method(
    bytes: Vec<u8>,
    name: &str,
    args: Vec<Value>,
) -> Result<ExecFlow, Box<dyn Error>> {
    if name == "len" && args.is_empty() {
        Ok(ExecFlow::Value(Value::Int(bytes.len() as i64)))
    } else {
        Err(RuntimeError::UnknownMethod {
            receiver: "bytes".to_string(),
            name: name.to_string(),
        }
        .into())
    }
}

pub fn eval_auth_session_method(
    session: &AuthSession,
    name: &str,
    args: Vec<Value>,
) -> Result<ExecFlow, Box<dyn Error>> {
    if !args.is_empty() {
        return Err(RuntimeError::UnknownMethod {
            receiver: "auth session".to_string(),
            name: name.to_string(),
        }
        .into());
    }
    Ok(ExecFlow::Value(auth_session_field(session, name)))
}
