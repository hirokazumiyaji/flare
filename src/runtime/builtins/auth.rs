use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::auth::{AuthError, AuthSession};
use crate::error::FlareError;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::expect_string;
use crate::value::Value;

pub fn builtin_auth_sign_up(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_sign_up", &args, &kwargs, block)?;
    let email = expect_string(args.first())?.to_string();
    let password = expect_string(args.get(1))?.to_string();
    let session = rt
        .services
        .auth
        .sign_up(&email, &password)
        .map_err(auth_backend_err)?;
    Ok(ExecFlow::Value(Value::AuthSession(session)))
}

pub fn builtin_auth_sign_in(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_sign_in", &args, &kwargs, block)?;
    let email = expect_string(args.first())?.to_string();
    let password = expect_string(args.get(1))?.to_string();
    let session = rt
        .services
        .auth
        .sign_in(&email, &password)
        .map_err(auth_backend_err)?;
    Ok(ExecFlow::Value(Value::AuthSession(session)))
}

pub fn builtin_auth_current_user(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_current_user", &args, &kwargs, block)?;
    Ok(ExecFlow::Value(
        rt.services
            .auth
            .current_user()
            .map_err(auth_backend_err)?
            .map(|user| user.to_value())
            .unwrap_or(Value::Nil),
    ))
}

pub fn builtin_auth_sign_out(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_sign_out", &args, &kwargs, block)?;
    rt.services.auth.sign_out().map_err(auth_backend_err)?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn reject_auth_call_args(
    name: &str,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if (name == "auth_current_user" || name == "auth_sign_out") && !args.is_empty() {
        return Err(format!("{name} does not take arguments").into());
    }
    if args.len() > 2 {
        return Err(format!("{name} takes at most 2 arguments").into());
    }
    if !kwargs.is_empty() {
        return Err(format!("{name} does not accept keyword arguments").into());
    }
    if block.is_some() {
        return Err(format!("{name} does not accept a block").into());
    }
    Ok(())
}

fn auth_backend_err(error: AuthError) -> Box<dyn Error> {
    FlareError::auth(error.kind(), error.to_string()).into()
}

pub fn auth_session_field(session: &AuthSession, name: &str) -> Value {
    match name {
        "user" => session.user_value(),
        "id_token" => Value::RedactedToken("id_token"),
        "refresh_token" => Value::RedactedToken("refresh_token"),
        "expires_at" => Value::Int(session.expires_at),
        _ => Value::Nil,
    }
}
