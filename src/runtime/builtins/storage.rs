use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::BudgetError;
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::{backend_err, ExecCtx};
use crate::storage::{validate_content_type, validate_storage_path};
use crate::value::Value;

pub fn builtin_storage_upload(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_unknown_kwargs("storage_upload", &kwargs, &["content_type"])?;
    if args.len() > 3 {
        return Err("storage_upload expects path, bytes, and optional content_type".into());
    }
    let path = expect_storage_path(args.first())?;
    let content = expect_storage_bytes(args.get(1))?;
    let content_type = match (args.get(2), kwargs.get("content_type")) {
        (Some(Value::Str(value)), None) => value.as_str(),
        (None, Some(Value::Str(value))) => value.as_str(),
        (None, None) => "application/octet-stream",
        (Some(_), Some(_)) => {
            return Err(
                "storage_upload received content_type twice (positional and keyword)".into(),
            );
        }
        (Some(_), None) => return Err("storage_upload content_type must be a string".into()),
        (None, Some(_)) => return Err("storage_upload content_type must be a string".into()),
    };
    validate_storage_path(path)?;
    validate_content_type(content_type)?;
    rt.services
        .storage
        .upload(path, content, content_type)
        .map(|metadata| ExecFlow::Value(metadata.to_value()))
        .map_err(backend_err)
}

pub fn builtin_storage_download(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_storage_kwargs("storage_download", &kwargs)?;
    expect_storage_arity("storage_download", &args, 1)?;
    let path = expect_storage_path(args.first())?;
    validate_storage_path(path)?;
    let bytes = rt.services.storage.download(path).map_err(backend_err)?;
    if let Some(max) = rt.limits.max_bytes_size {
        if bytes.len() > max {
            return Err(BudgetError::new(format!("bytes size limit exceeded (max {max})")).into());
        }
    }
    Ok(ExecFlow::Value(Value::Bytes(bytes)))
}

pub fn builtin_storage_metadata(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_storage_kwargs("storage_metadata", &kwargs)?;
    expect_storage_arity("storage_metadata", &args, 1)?;
    let path = expect_storage_path(args.first())?;
    validate_storage_path(path)?;
    rt.services
        .storage
        .metadata(path)
        .map(|metadata| ExecFlow::Value(metadata.to_value()))
        .map_err(backend_err)
}

pub fn builtin_storage_delete(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_storage_kwargs("storage_delete", &kwargs)?;
    expect_storage_arity("storage_delete", &args, 1)?;
    let path = expect_storage_path(args.first())?;
    validate_storage_path(path)?;
    rt.services.storage.delete(path).map_err(backend_err)?;
    Ok(ExecFlow::Value(Value::Nil))
}

pub fn forbid_storage_in_batch_or_transaction(rt: &Evaluator<'_>) -> Result<(), Box<dyn Error>> {
    match rt.exec {
        ExecCtx::Batch { .. } => Err(RuntimeError::StorageInBatch.into()),
        ExecCtx::Transaction { .. } => Err(RuntimeError::StorageInTransaction.into()),
        ExecCtx::Normal => Ok(()),
    }
}

fn reject_storage_kwargs(
    name: &str,
    kwargs: &BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    if kwargs.is_empty() {
        Ok(())
    } else {
        Err(format!("{name} does not take keyword arguments").into())
    }
}

fn reject_unknown_kwargs(
    name: &str,
    kwargs: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), Box<dyn Error>> {
    let unknown: Vec<&str> = kwargs
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{name} received unknown keyword arguments: {}",
            unknown.join(", ")
        )
        .into())
    }
}

fn expect_storage_arity(name: &str, args: &[Value], expected: usize) -> Result<(), Box<dyn Error>> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!("{name} expects {expected} argument(s), got {}", args.len()).into())
    }
}

fn expect_storage_path(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(path)) => Ok(path),
        _ => Err("storage API expects an object path string".into()),
    }
}

fn expect_storage_bytes(v: Option<&Value>) -> Result<&[u8], Box<dyn Error>> {
    match v {
        Some(Value::Bytes(bytes)) => Ok(bytes),
        Some(_) => Err("storage_upload expects bytes(...) content".into()),
        None => Err("storage_upload requires bytes(...) content".into()),
    }
}
