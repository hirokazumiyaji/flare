use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::error::Error;

use crate::ast::{Block, CallArg, FuncParam};
use crate::runtime::error::RuntimeError;
use crate::value::Value;

/// The positional part of a call contract.
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// Runtime validation shared by builtin functions and built-in methods.
///
/// `keywords` is intentionally small: the language's builtin API is
/// positional except for `set(..., merge: true)`.  `array` and `puts` use
/// `PositionalArity::Any` because their variable-arity behavior is part of
/// the API.
#[derive(Clone, Copy)]
pub struct CallContract {
    pub positional: PositionalArity,
    pub keywords: &'static [&'static str],
    pub block: BlockPolicy,
    /// Keywords that occupy the same slot as a positional argument.
    pub positional_keyword_conflicts: &'static [(&'static str, usize)],
    /// Optional compatibility/detail hint for a missing argument.
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

/// Hand-built ASTs can reach `eval_program` without passing the parser.
pub fn validate_func_params(params: &[FuncParam], kind: &str) -> Result<(), Box<dyn Error>> {
    let mut seen = HashSet::with_capacity(params.len());
    let mut seen_default = false;
    for param in params {
        if !seen.insert(param.name.as_str()) {
            return Err(RuntimeError::DuplicateParam {
                kind: kind.to_string(),
                name: param.name.clone(),
            }
            .into());
        }
        if param.default.is_some() {
            seen_default = true;
        } else if seen_default {
            return Err("parameter without default cannot follow parameter with default".into());
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

pub fn validate_user_call_contract(
    name: &str,
    params: &[FuncParam],
    args: &[CallArg],
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if block.is_some() {
        return Err(RuntimeError::BlockNotAllowed(name.to_string()).into());
    }
    let has_keywords = args
        .iter()
        .any(|arg| matches!(arg, CallArg::Keyword { .. }));
    if !has_keywords {
        let pos_len = args.len();
        if pos_len > params.len() {
            return Err(RuntimeError::TooManyPositionalArgs.into());
        }
        for param in &params[pos_len..] {
            if param.default.is_none() {
                return Err(RuntimeError::MissingArgument(param.name.clone()).into());
            }
        }
        return Ok(());
    }
    let positional_count = positional_arg_count(args);
    if positional_count > params.len() {
        return Err(RuntimeError::TooManyPositionalArgs.into());
    }
    let mut kwargs = keyword_names(args);
    let mut idx = 0;
    for param in params {
        if idx < positional_count {
            if kwargs.contains_key(&param.name) {
                return Err(RuntimeError::DuplicateArgument(param.name.clone()).into());
            }
            idx += 1;
            continue;
        }
        if kwargs.remove(&param.name).is_some() {
            continue;
        }
        if param.default.is_some() {
            continue;
        }
        return Err(RuntimeError::MissingArgument(param.name.clone()).into());
    }
    if !kwargs.is_empty() {
        let keys: Vec<String> = kwargs.keys().cloned().collect();
        return Err(RuntimeError::UnknownKeywordArguments(keys).into());
    }
    Ok(())
}

pub fn validate_call_contract(
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
    contract: CallContract,
) -> Result<(), Box<dyn Error>> {
    // Check this before arity/type checks so a call which supplies the same
    // option in two ways always gets the actionable duplicate-argument error.
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

    let unknown: Vec<&str> = kwargs
        .keys()
        .filter(|key| {
            !contract
                .keywords
                .iter()
                .any(|allowed| allowed == &key.as_str())
        })
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(RuntimeError::UnknownKeywordArguments(
            unknown.iter().map(|k| k.to_string()).collect(),
        )
        .into());
    }

    match (contract.block, block) {
        (BlockPolicy::Forbidden, Some(_)) => {
            Err(RuntimeError::BlockNotAllowed(name.to_string()).into())
        }
        (BlockPolicy::Required, None) => Err(RuntimeError::RequiresBlock(name.to_string()).into()),
        _ => Ok(()),
    }
}

pub fn unknown_keyword_arguments(kwargs: &BTreeMap<String, Value>) -> Box<dyn Error> {
    let keys: Vec<String> = kwargs.keys().cloned().collect();
    RuntimeError::UnknownKeywordArguments(keys).into()
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
