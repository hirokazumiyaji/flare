use std::collections::HashMap;
use std::time::Instant;

use crate::limits::{RuntimeLimits, RuntimeUsage};
use crate::value::Value;

use super::ExecCtx;

pub(crate) struct RuntimeState {
    pub(super) scopes: Vec<HashMap<String, Value>>,
    pub(super) current_frame_base: usize,
    pub(super) exec: ExecCtx,
    pub(super) pending_rollback: bool,
    pub(super) limits: RuntimeLimits,
    pub(super) usage: RuntimeUsage,
    pub(super) call_depth: usize,
    pub(super) started_at: Option<Instant>,
    pub(super) execution_active: bool,
    pub(super) repl_mode: bool,
}

impl RuntimeState {
    pub(crate) fn new(limits: RuntimeLimits) -> Self {
        Self {
            scopes: vec![HashMap::new()],
            current_frame_base: 0,
            exec: ExecCtx::Normal,
            pending_rollback: false,
            limits,
            usage: RuntimeUsage::default(),
            call_depth: 0,
            started_at: None,
            execution_active: false,
            repl_mode: false,
        }
    }
}
