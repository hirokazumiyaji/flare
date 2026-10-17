use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::error::Error;

use crate::ast::{Block, CallArg};
use crate::runtime::error::RuntimeError;
use crate::value::Value;

#[derive(Clone, Copy)]
pub enum PositionalArity {
    Exact(usize),
    Range(usize, usize),
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BlockPolicy {
    Forbidden,
    Required,
}

#[derive(Clone, Copy)]
pub struct CallContract {
    pub positional: PositionalArity,
    pub keywords: &'static [&'static str],
    pub block: BlockPolicy,
    pub positional_keyword_conflicts: &'static [(&'static str, usize)],
    pub missing_hint: Option<&'static str>,
}

impl CallContract {
    pub const fn exact(positional: usize) -> Self {
        Self {
            positional: PositionalArity::Exact(positional),
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    pub const fn range(min: usize, max: usize) -> Self {
        Self {
            positional: PositionalArity::Range(min, max),
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    pub const fn any() -> Self {
        Self {
            positional: PositionalArity::Any,
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    pub const fn with_keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }

    pub const fn with_positional_keyword_conflicts(
        mut self,
        conflicts: &'static [(&'static str, usize)],
    ) -> Self {
        self.positional_keyword_conflicts = conflicts;
        self
    }

    pub const fn with_missing_hint(mut self, hint: &'static str) -> Self {
        self.missing_hint = Some(hint);
        self
    }

    pub const fn requiring_block(mut self) -> Self {
        self.block = BlockPolicy::Required;
        self
    }
}

pub fn validate_unique_keywords(args: &[CallArg]) -> Result<(), Box<dyn Error>> {
    let mut seen = BTreeSet::<&str>::new();
    for arg in args {
        if let CallArg::Keyword { name, .. } = arg {
            if !seen.insert(name.as_str()) {
                return Err(RuntimeError::DuplicateKeywordArgument(name.clone()).into());
            }
        }
    }
    Ok(())
}

pub fn positional_arg_count(args: &[CallArg]) -> usize {
    args.iter()
        .filter(|arg| matches!(arg, CallArg::Positional(_)))
        .count()
}

pub fn validate_unique_params(params: &[String], kind: &str) -> Result<(), Box<dyn Error>> {
    let mut seen = HashSet::with_capacity(params.len());
    for param in params {
        if !seen.insert(param.as_str()) {
            return Err(RuntimeError::DuplicateParam {
                kind: kind.to_string(),
                name: param.clone(),
            }
            .into());
        }
    }
    Ok(())
}

pub fn keyword_names(args: &[CallArg]) -> BTreeMap<String, Value> {
    args.iter()
        .filter_map(|arg| match arg {
            CallArg::Keyword { name, .. } => Some((name.clone(), Value::Nil)),
            _ => None,
        })
        .collect()
}

pub fn validate_call_contract(
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
    contract: CallContract,
) -> Result<(), Box<dyn Error>> {
    for (keyword, position) in contract.positional_keyword_conflicts {
        if positional_len > *position && kwargs.contains_key(*keyword) {
            return Err(RuntimeError::DuplicateArgument((*keyword).to_string()).into());
        }
    }

    match contract.positional {
        PositionalArity::Any => {}
        PositionalArity::Exact(expected) => {
            if positional_len != expected {
                return Err(RuntimeError::PositionalArityMismatch {
                    name: name.to_string(),
                    expected,
                    actual: positional_len,
                    exact: true,
                    hint: contract.missing_hint,
                }
                .into());
            }
        }
        PositionalArity::Range(min, max) => {
            if positional_len < min {
                return Err(format!(
                    "{name} expects at least {min} positional arguments, got {positional_len}"
                )
                .into());
            }
            if positional_len > max {
                return Err(RuntimeError::PositionalArityMismatch {
                    name: name.to_string(),
                    expected: max,
                    actual: positional_len,
                    exact: false,
                    hint: None,
                }
                .into());
            }
        }
    }

    let unknown: Vec<String> = kwargs
        .keys()
        .filter(|key| !contract.keywords.contains(&key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(RuntimeError::UnknownKeywordArguments(unknown).into());
    }

    match (contract.block, block) {
        (BlockPolicy::Forbidden, Some(_)) => {
            Err(RuntimeError::BlockNotAllowed(name.to_string()).into())
        }
        (BlockPolicy::Required, None) => Err(RuntimeError::RequiresBlock(name.to_string()).into()),
        _ => Ok(()),
    }
}

pub fn check_block_arity(block: &Block, yielded: usize) -> Result<(), Box<dyn Error>> {
    if block.params.len() != yielded {
        Err(RuntimeError::WrongBlockArity {
            expected: block.params.len(),
            got: yielded,
        }
        .into())
    } else {
        Ok(())
    }
}

pub fn validate_user_call_contract(
    name: &str,
    params: &[String],
    args: &[CallArg],
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if block.is_some() {
        return Err(RuntimeError::BlockNotAllowed(name.to_string()).into());
    }
    bind_function_args(
        params,
        vec![Value::Nil; positional_arg_count(args)],
        keyword_names(args),
    )
    .map(|_| ())
}

pub fn bind_function_args(
    params: &[String],
    positional: Vec<Value>,
    mut kwargs: BTreeMap<String, Value>,
) -> Result<Vec<(String, Value)>, Box<dyn Error>> {
    if positional.len() > params.len() {
        return Err(RuntimeError::TooManyPositionalArgs.into());
    }
    let mut out = Vec::with_capacity(params.len());
    let mut idx = 0;
    for param in params {
        if idx < positional.len() {
            if kwargs.contains_key(param) {
                return Err(RuntimeError::DuplicateArgument(param.clone()).into());
            }
            out.push((param.clone(), positional[idx].clone()));
            idx += 1;
            continue;
        }
        if let Some(v) = kwargs.remove(param) {
            out.push((param.clone(), v));
            continue;
        }
        return Err(RuntimeError::MissingArgument(param.clone()).into());
    }
    if !kwargs.is_empty() {
        let keys: Vec<String> = kwargs.keys().cloned().collect();
        return Err(RuntimeError::UnknownKeywordArguments(keys).into());
    }
    Ok(out)
}
