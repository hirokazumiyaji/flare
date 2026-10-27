use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::BudgetError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::expect_string;
use crate::value::{validate_value_limits, BuiltinModule, Value};

pub fn builtin_io_file(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Module(BuiltinModule::File)))
}

/// Maps an I/O or codec error that carries Flare's internal `resource limit
/// exceeded` prefix (set by `io/*` and `BoundedWriter`) to `BudgetError` so
/// `try/rescue` cannot swallow a budget breach.
pub(super) fn promote_resource_limit(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.to_string().contains("resource limit exceeded") {
        BudgetError::new(e.to_string()).into()
    } else {
        e
    }
}

/// Validate a builtin's result against the value budgets and promote the
/// validator's plain-string errors to `BudgetError`, so `try/rescue` cannot
/// swallow an engine budget breach.
pub(super) fn validate_value_budget(
    rt: &Evaluator<'_>,
    value: &Value,
) -> Result<(), Box<dyn Error>> {
    validate_value_limits(
        value,
        rt.state.limits.max_value_depth,
        rt.state.limits.max_value_nodes,
    )
    .map_err(|e| BudgetError::new(e.to_string()))?;
    Ok(())
}

pub fn builtin_file_read(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path_str = expect_string(args.first())?;
    let path = std::path::Path::new(path_str);
    let max_bytes = rt.state.limits.max_file_io_bytes;
    let data = rt
        .services
        .io
        .read(path, max_bytes)
        .map_err(promote_resource_limit)?;
    let val = match String::from_utf8(data) {
        Ok(s) => Value::Str(s),
        Err(e) => Value::Bytes(e.into_bytes()),
    };
    validate_value_budget(rt, &val)?;
    Ok(ExecFlow::Value(val))
}

pub fn builtin_file_write(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path_str = expect_string(args.first())?;
    let path = std::path::Path::new(path_str);
    let content = args.get(1).ok_or("expected content argument")?;
    let bytes: &[u8] = match content {
        Value::Str(s) => s.as_bytes(),
        Value::Bytes(b) => b.as_slice(),
        _ => return Err("File.write expects str or bytes content".into()),
    };
    let max_bytes = rt.state.limits.max_file_io_bytes;
    rt.services
        .io
        .write(path, bytes, max_bytes)
        .map_err(promote_resource_limit)?;
    Ok(ExecFlow::Value(Value::Int(bytes.len() as i64)))
}

pub struct BoundedWriter<W> {
    inner: W,
    written: usize,
    max_bytes: Option<usize>,
}

impl<W> BoundedWriter<W> {
    pub fn new(inner: W, max_bytes: Option<usize>) -> Self {
        Self {
            inner,
            written: 0,
            max_bytes,
        }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: std::io::Write> std::io::Write for BoundedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(limit) = self.max_bytes {
            if self.written.saturating_add(buf.len()) > limit {
                return Err(std::io::Error::other(format!(
                    "resource limit exceeded: max file io bytes ({limit})"
                )));
            }
        }
        let n = self.inner.write(buf)?;
        self.written += n;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
