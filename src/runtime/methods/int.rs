use std::error::Error;

use crate::ast::Block;
use crate::runtime::contract::CallContract;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::value::Value;

pub fn int_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "times" => Some(CallContract::exact(0).requiring_block()),
        "to_i" => Some(CallContract::exact(0)),
        _ => None,
    }
}

pub fn eval_int_method(
    rt: &mut Evaluator<'_>,
    n: i64,
    name: &str,
    _args: Vec<Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match name {
        "times" => {
            let Some(block) = block else {
                return Err("times requires { ... } block".into());
            };
            if block.params.len() > 1 {
                return Err(format!(
                    "block expects 0 or 1 argument(s), got {}",
                    block.params.len()
                )
                .into());
            }
            if n > 0 {
                for i in 0..n {
                    rt.consume_loop_iteration()?;
                    let args = if block.params.is_empty() {
                        vec![]
                    } else {
                        vec![Value::Int(i)]
                    };
                    match rt.call_block(block, args)? {
                        ExecFlow::Value(_) => {}
                        ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                    }
                }
            }
            Ok(ExecFlow::Value(Value::Int(n)))
        }
        _ => Err(format!("unknown int method: {name}").into()),
    }
}
