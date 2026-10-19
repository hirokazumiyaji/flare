use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::ServiceId;
use crate::runtime::builtins::{lookup_module_builtin, universal_value_builtin, BuiltinSpec};
use crate::runtime::contract::{unknown_keyword_arguments, validate_call_contract, CallContract};
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::{expect_string, validate_document_id};
use crate::value::{BuiltinModule, DocRef, Value};

pub mod array;
pub mod int;
pub mod map;
pub mod misc;
pub mod query;
pub mod string;

/// Reject a call on a receiver that cannot carry methods.
///
/// Keeping this exhaustive check next to the dispatch match prevents a new
/// receiver from being accepted by one path but rejected by the other. The
/// three universal value methods are the one exception: they accept every
/// value. `to_i` is deliberately not among them, so `true.to_i` reports the
/// receiver type rather than the argument type.
pub fn check_method_receiver(receiver: &Value, name: &str) -> Result<(), Box<dyn Error>> {
    match receiver {
        Value::CollectionRef(_)
        | Value::CollectionGroupRef(_)
        | Value::QueryRef(_)
        | Value::Array(_)
        | Value::Timestamp(_)
        | Value::Bytes(_)
        | Value::AuthSession(_)
        | Value::Map(_)
        | Value::Module(_)
        | Value::Str(_)
        | Value::Int(_) => Ok(()),
        other => {
            if matches!(name, "to_s" | "inspect" | "type_of") {
                Ok(())
            } else if matches!(other, Value::Nil) {
                Err(RuntimeError::CannotCallMethodOnNil(name.to_string()).into())
            } else {
                Err(RuntimeError::CannotCallMethodOnType {
                    name: name.to_string(),
                    type_name: other.type_name().to_string(),
                }
                .into())
            }
        }
    }
}

pub fn method_call_label(receiver: &Value, name: &str) -> String {
    match receiver {
        Value::CollectionRef(_) if name == "doc" => format!("collection method {name}"),
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            format!("query method {name}")
        }
        Value::Array(_) => format!("array method {name}"),
        Value::Str(_) => format!("string method {name}"),
        Value::Int(_) => format!("int method {name}"),
        Value::Timestamp(_) => format!("timestamp method {name}"),
        Value::Bytes(_) => format!("bytes method {name}"),
        Value::AuthSession(_) => format!("auth session method {name}"),
        Value::Map(_) => format!("map method {name}"),
        Value::Module(module) => format!("{}.{name}", module.name()),
        other => format!("{} method {name}", other.type_name()),
    }
}

pub fn method_contract(receiver: &Value, name: &str) -> Option<CallContract> {
    if matches!(name, "to_s" | "inspect" | "type_of") {
        return Some(CallContract::exact(0));
    }
    match receiver {
        Value::CollectionRef(_) if name == "doc" => Some(CallContract::exact(1)),
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            query::query_method_contract(name)
        }
        Value::Array(_) => array::array_method_contract(name),
        Value::Str(_) => string::str_method_contract(name),
        Value::Timestamp(_) => misc::timestamp_method_contract(name),
        Value::Bytes(_) => misc::bytes_method_contract(name),
        Value::AuthSession(_) => misc::auth_session_method_contract(name),
        Value::Map(_) => map::map_method_contract(name),
        Value::Int(_) => int::int_method_contract(name),
        Value::Module(_) => None,
        _ => None,
    }
}

pub fn validate_method_call(
    receiver: &Value,
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if let Some(contract) = method_contract(receiver, name) {
        return validate_call_contract(
            &method_call_label(receiver, name),
            positional_len,
            kwargs,
            block,
            contract,
        );
    }

    // Even an unknown method must not silently accept keyword arguments or a
    // block.  The eventual dispatch error (unknown method / unsupported
    // receiver) is returned only after these contract-level mistakes.
    if !kwargs.is_empty() {
        return Err(unknown_keyword_arguments(kwargs));
    }
    if block.is_some() {
        return Err(RuntimeError::BlockNotAllowed(method_call_label(receiver, name)).into());
    }
    Err(match receiver {
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            RuntimeError::UnknownMethod {
                receiver: "query".to_string(),
                name: name.to_string(),
            }
            .into()
        }
        Value::Array(_) => RuntimeError::UnknownMethod {
            receiver: "array".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::Str(_) => RuntimeError::UnknownMethod {
            receiver: "string".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::Int(_) => RuntimeError::UnknownMethod {
            receiver: "int".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::Timestamp(_) => RuntimeError::UnknownMethod {
            receiver: "timestamp".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::Bytes(_) => RuntimeError::UnknownMethod {
            receiver: "bytes".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::AuthSession(_) => RuntimeError::UnknownMethod {
            receiver: "auth session".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::Map(_) => RuntimeError::UnknownMethod {
            receiver: "map".to_string(),
            name: name.to_string(),
        }
        .into(),
        Value::Module(module) => RuntimeError::UnknownModuleMethod {
            module: module.name().to_string(),
            name: name.to_string(),
        }
        .into(),
        _ => RuntimeError::UnsupportedMethodReceiver.into(),
    })
}

pub fn required_service_for_method(receiver: &Value, name: &str) -> Option<ServiceId> {
    match receiver {
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_)
            if name == "get" || name == "count" =>
        {
            Some(ServiceId::Firestore)
        }
        Value::Module(module) => {
            lookup_module_builtin(*module, name).and_then(|spec| spec.required_service)
        }
        _ => None,
    }
}

pub fn eval_method_call(
    rt: &mut Evaluator<'_>,
    receiver: Value,
    name: &str,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    validate_method_call(&receiver, name, args.len(), &kwargs, block)?;
    eval_method_call_unchecked(rt, receiver, name, args, block)
}

fn eval_method_call_unchecked(
    rt: &mut Evaluator<'_>,
    receiver: Value,
    name: &str,
    args: Vec<Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    if let Some(handler) = universal_value_builtin(name) {
        return handler(rt, vec![receiver], BTreeMap::new(), None);
    }

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
        Value::Timestamp(ts) => misc::eval_timestamp_method(ts, name, args),
        Value::Bytes(bytes) => misc::eval_bytes_method(bytes, name, args),
        Value::Map(map) => map::eval_map_method(map, name, args),
        Value::Str(s) => string::eval_string_method(s, name, args),
        Value::AuthSession(ref session) => misc::eval_auth_session_method(session, name, args),
        Value::Int(n) => int::eval_int_method(rt, n, name, args, block),
        Value::Module(module) => {
            let spec = module_builtin_spec(module, name)?;
            (spec.handler)(rt, args, BTreeMap::new(), block)
        }
        _ => Err(RuntimeError::UnsupportedMethodReceiver.into()),
    }
}

/// Look up a module method, reporting the module-qualified unknown-method error.
pub fn module_builtin_spec(
    module: BuiltinModule,
    name: &str,
) -> Result<BuiltinSpec, Box<dyn Error>> {
    lookup_module_builtin(module, name).ok_or_else(|| {
        RuntimeError::UnknownModuleMethod {
            module: module.name().to_string(),
            name: name.to_string(),
        }
        .into()
    })
}
