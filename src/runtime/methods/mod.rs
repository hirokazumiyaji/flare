use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::ServiceId;
use crate::runtime::builtins::lookup_module_builtin;
use crate::runtime::contract::{validate_call_contract, CallContract};
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::{expect_string, validate_document_id};
use crate::value::{BuiltinModule, DocRef, Value};

pub mod array;
pub mod map;
pub mod misc;
pub mod query;
pub mod string;

pub fn method_call_label(receiver: &Value, name: &str) -> String {
    match receiver {
        Value::CollectionRef(_) if name == "doc" => format!("collection method {name}"),
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            format!("query method {name}")
        }
        Value::Array(_) => format!("array method {name}"),
        Value::Str(_) => format!("string method {name}"),
        Value::Timestamp(_) => format!("timestamp method {name}"),
        Value::Bytes(_) => format!("bytes method {name}"),
        Value::AuthSession(_) => format!("auth session method {name}"),
        Value::Map(_) => match name {
            "keys" | "values" | "len" | "has_key" => format!("map method {name}"),
            _ => format!("map field {name}"),
        },
        Value::Module(module) => format!("{}.{name}", module.name()),
        _ => "unsupported method receiver".to_string(),
    }
}

pub fn method_contract(receiver: &Value, name: &str) -> Result<CallContract, Box<dyn Error>> {
    match receiver {
        Value::CollectionRef(_) if name == "doc" => Ok(CallContract::exact(1)),
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            query::query_method_contract(name).ok_or_else(|| {
                RuntimeError::UnknownMethod {
                    receiver: "query".to_string(),
                    name: name.to_string(),
                }
                .into()
            })
        }
        Value::Array(_) => array::array_method_contract(name).ok_or_else(|| {
            RuntimeError::UnknownMethod {
                receiver: "array".to_string(),
                name: name.to_string(),
            }
            .into()
        }),
        Value::Str(_) => string::str_method_contract(name).ok_or_else(|| {
            RuntimeError::UnknownMethod {
                receiver: "string".to_string(),
                name: name.to_string(),
            }
            .into()
        }),
        Value::Timestamp(_) => misc::timestamp_method_contract(name).ok_or_else(|| {
            RuntimeError::UnknownMethod {
                receiver: "timestamp".to_string(),
                name: name.to_string(),
            }
            .into()
        }),
        Value::Bytes(_) => misc::bytes_method_contract(name).ok_or_else(|| {
            RuntimeError::UnknownMethod {
                receiver: "bytes".to_string(),
                name: name.to_string(),
            }
            .into()
        }),
        Value::AuthSession(_) => misc::auth_session_method_contract(name).ok_or_else(|| {
            RuntimeError::UnknownMethod {
                receiver: "auth session".to_string(),
                name: name.to_string(),
            }
            .into()
        }),
        Value::Map(_) => map::map_method_contract(name).ok_or_else(|| {
            RuntimeError::UnknownMethod {
                receiver: "map".to_string(),
                name: name.to_string(),
            }
            .into()
        }),
        Value::Module(_) => Err("module methods use module contract".into()),
        _ => Err(RuntimeError::UnsupportedMethodReceiver.into()),
    }
}

pub fn validate_method_call(
    receiver: &Value,
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    let contract = method_contract(receiver, name)?;
    let label = method_call_label(receiver, name);
    validate_call_contract(&label, positional_len, kwargs, block, contract)
}

pub fn required_service_for_method(receiver: &Value, name: &str) -> Option<ServiceId> {
    match receiver {
        Value::CollectionRef(_)
        | Value::CollectionGroupRef(_)
        | Value::QueryRef(_)
            if matches!(name, "count" | "get") =>
        {
            Some(ServiceId::Firestore)
        }
        Value::Module(module) => match module {
            BuiltinModule::Firestore => Some(ServiceId::Firestore),
            BuiltinModule::Auth => Some(ServiceId::Auth),
            BuiltinModule::Storage => Some(ServiceId::Storage),
            BuiltinModule::IO | BuiltinModule::File => Some(ServiceId::IO),
            BuiltinModule::JSON | BuiltinModule::CSV | BuiltinModule::YAML => None,
        },
        _ => None,
    }
}

pub fn eval_method_call(
    rt: &mut Evaluator<'_>,
    receiver: Value,
    name: &str,
    args: Vec<Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match receiver {
        Value::CollectionRef(collection) if name == "doc" => {
            let id = expect_string(args.first())?;
            validate_document_id(id)?;
            Ok(ExecFlow::Value(Value::DocRef(DocRef {
                path: format!("{}/{}", collection.path, id),
            })))
        }
        Value::CollectionRef(collection) => {
            query::eval_query_method(rt, collection.to_query(), name, args).map(ExecFlow::Value)
        }
        Value::CollectionGroupRef(group) => {
            query::eval_query_method(rt, group.to_query(), name, args).map(ExecFlow::Value)
        }
        Value::QueryRef(query) => {
            query::eval_query_method(rt, query, name, args).map(ExecFlow::Value)
        }
        Value::Array(items) => array::eval_array_method(rt, items, name, args, block),
        Value::Str(s) => string::eval_string_method(s, name, args),
        Value::Map(map) => map::eval_map_method(map, name, args),
        Value::Timestamp(ts) => misc::eval_timestamp_method(ts, name, args),
        Value::Bytes(bytes) => misc::eval_bytes_method(bytes, name, args),
        Value::AuthSession(ref session) => misc::eval_auth_session_method(session, name, args),
        Value::Module(module) => {
            let spec = lookup_module_builtin(module, name)
                .ok_or_else(|| format!("unknown module method: {}.{name}", module.name()))?;
            (spec.handler)(rt, args, BTreeMap::new(), block)
        }
        _ => Err(RuntimeError::UnsupportedMethodReceiver.into()),
    }
}
