use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::firestore::{validate_write_data, WriteOp};
use crate::runtime::error::RuntimeError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::{
    backend_err, expect_doc_path, expect_map, expect_string, validate_collection_group_id,
    validate_collection_path, validate_doc_path, ExecCtx,
};
use crate::value::{CollectionGroupRef, CollectionRef, DocRef, Value};

pub fn builtin_doc(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_string(args.first())?;
    validate_doc_path(path)?;
    Ok(ExecFlow::Value(Value::DocRef(DocRef {
        path: path.to_string(),
    })))
}

pub fn builtin_col(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_string(args.first())?;
    validate_collection_path(path)?;
    Ok(ExecFlow::Value(Value::CollectionRef(CollectionRef {
        path: path.to_string(),
    })))
}

pub fn builtin_col_group(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let id = expect_string(args.first())?;
    validate_collection_group_id(id)?;
    Ok(ExecFlow::Value(Value::CollectionGroupRef(
        CollectionGroupRef { id: id.to_string() },
    )))
}

pub fn builtin_get(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?;
    rt.check_wall_clock()?;
    let v = match &rt.exec {
        ExecCtx::Batch { .. } => {
            return Err(RuntimeError::GetInBatch.into());
        }
        ExecCtx::Transaction { writes } if !writes.is_empty() => {
            return Err(RuntimeError::GetAfterWriteInTransaction.into());
        }
        ExecCtx::Transaction { .. } => rt
            .services
            .firestore
            .transaction_get(path)
            .map_err(backend_err)?
            .unwrap_or(Value::Nil),
        ExecCtx::Normal => rt
            .services
            .firestore
            .get(path)
            .map_err(backend_err)?
            .unwrap_or(Value::Nil),
    };
    // `get` only reads, so a deadline that fires afterwards cannot invalidate
    // work the script already observed.
    rt.check_wall_clock()?;
    Ok(ExecFlow::Value(v))
}

pub fn builtin_set(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    validate_write_data(&data)?;
    let merge = match (args.get(2), kwargs.get("merge")) {
        (_, Some(Value::Bool(v))) => *v,
        (Some(Value::Bool(v)), None) => *v,
        (None, None) => false,
        _ => return Err("set merge must be bool".into()),
    };
    rt.dispatch_write(WriteOp::Set { path, data, merge })?;
    Ok(ExecFlow::Value(Value::Nil))
}

pub fn builtin_update(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    validate_write_data(&data)?;
    rt.dispatch_write(WriteOp::Update { path, data })?;
    Ok(ExecFlow::Value(Value::Nil))
}

pub fn builtin_delete(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    rt.dispatch_write(WriteOp::Delete { path })?;
    Ok(ExecFlow::Value(Value::Nil))
}

pub fn builtin_batch(
    rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let Some(stmts) = block else {
        return Err(RuntimeError::RequiresBlock("batch".to_string()).into());
    };
    if !stmts.params.is_empty() {
        return Err(RuntimeError::BlockTakesNoParameters("batch".to_string()).into());
    }
    if !matches!(rt.exec, ExecCtx::Normal) {
        return Err("batch cannot be nested inside batch/transaction".into());
    }
    let (flow, writes) = rt.run_staged_block(ExecCtx::Batch { writes: vec![] }, &stmts.body);
    let flow = flow?;
    rt.check_wall_clock()?;
    // `apply` は永続書き込みなので、ここでは wall-clock を再検査しない。
    // 書き込み後の timeout 判定は「成功したはずの write」を失敗として返し、
    // 呼び出し側の再試行と重なって二重書き込みを招く。実際の RPC には
    // `Runtime::with_limits` が設定した backend deadline が効く。
    rt.services.firestore.apply(writes).map_err(backend_err)?;
    Ok(flow)
}

pub fn builtin_transaction(
    rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let Some(stmts) = block else {
        return Err(RuntimeError::RequiresBlock("transaction".to_string()).into());
    };
    if !stmts.params.is_empty() {
        return Err(RuntimeError::BlockTakesNoParameters("transaction".to_string()).into());
    }
    if !matches!(rt.exec, ExecCtx::Normal) {
        return Err("transaction cannot be nested inside batch/transaction".into());
    }
    // A previous transaction may have left the backend with an open
    // transaction because its rollback failed. Clear it before starting a
    // new one, otherwise the begin below fails forever.
    rt.retry_pending_rollback();
    rt.check_wall_clock()?;
    rt.services
        .firestore
        .begin_transaction()
        .map_err(backend_err)?;
    // `begin_transaction` は遅いことがあるため、budget 超過を begin 後に
    // 検出したら必ず abort する。さもないと server transaction が開き続け、
    // 以降の `begin_transaction` が "already in transaction" で失敗する。
    if let Err(e) = rt.check_wall_clock() {
        rt.abort_transaction_best_effort();
        return Err(e);
    }
    let (flow, writes) = rt.run_staged_block(ExecCtx::Transaction { writes: vec![] }, &stmts.body);
    match flow {
        Ok(flow) => {
            // commit 前の budget 超過でも transaction を開いたままにしない。
            // in-memory は tx_snapshot、emulator は transaction_id を保持し、
            // どちらの backend も以降の transaction を "already in
            // transaction" で失敗させるため、ここは abort が必要。
            if let Err(e) = rt.check_wall_clock() {
                rt.abort_transaction_best_effort();
                return Err(e);
            }
            if let Err(e) = rt.services.firestore.commit_transaction(writes) {
                rt.abort_transaction_best_effort();
                return Err(backend_err(e));
            }
            // commit 後も wall-clock を再検査しない。batch と同じく、
            // 永続化済みの操作を timeout として返すと再試行が二重書き込みに
            // つながる。commit 自体の deadline は backend timeout が担保する。
            Ok(flow)
        }
        Err(e) => {
            // Preserve the block error if rollback also fails.
            rt.abort_transaction_best_effort();
            Err(e)
        }
    }
}
