use std::cell::RefCell;
use std::error::Error;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::FileHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

use crate::firestore::Firestore;
use crate::lexer::{lex_spanned_with_limits, Token};
use crate::limits::ParserLimits;
use crate::runtime::Runtime;
use crate::storage::Storage;

pub const KEYWORDS: &[&str] = &[
    "fn", "return", "if", "else", "while", "nil", "true", "false",
];

pub const MODULES: &[&str] = &["Firestore", "Auth", "Storage"];

pub const BUILTINS: &[&str] = &[
    "puts",
    "doc",
    "col",
    "col_group",
    "array",
    "range",
    "map",
    "timestamp",
    "server_timestamp",
    "increment",
    "array_union",
    "array_remove",
    "delete_field",
    "bytes",
    "get",
    "set",
    "update",
    "delete",
    "where",
    "order_by",
    "limit",
    "offset",
    "count",
    "batch",
    "transaction",
    "auth_sign_up",
    "auth_sign_in",
    "auth_sign_out",
    "auth_current_user",
    "storage_upload",
    "storage_download",
    "storage_delete",
    "storage_metadata",
    "len",
    "to_s",
    "inspect",
    "type_of",
    "to_i",
    "exit",
    "quit",
];

#[derive(Default)]
pub struct FlareHelper {
    candidates: RefCell<Vec<String>>,
}

impl FlareHelper {
    pub fn new() -> Self {
        let mut candidates = Vec::new();
        for k in KEYWORDS {
            candidates.push((*k).to_string());
        }
        for m in MODULES {
            candidates.push((*m).to_string());
        }
        for b in BUILTINS {
            candidates.push((*b).to_string());
        }
        candidates.sort();
        candidates.dedup();
        Self {
            candidates: RefCell::new(candidates),
        }
    }

    pub fn set_candidates(&self, candidates: Vec<String>) {
        *self.candidates.borrow_mut() = candidates;
    }
}

impl Completer for FlareHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if pos > line.len() || !line.is_char_boundary(pos) {
            return Ok((pos, Vec::new()));
        }
        let sub = &line[..pos];
        let start = sub
            .char_indices()
            .rfind(|(_, c)| !c.is_alphanumeric() && *c != '_')
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if start > pos || !line.is_char_boundary(start) {
            return Ok((pos, Vec::new()));
        }
        let prefix = &line[start..pos];
        if prefix.is_empty() {
            return Ok((pos, Vec::new()));
        }
        let candidates = self.candidates.borrow();
        let matches: Vec<Pair> = candidates
            .iter()
            .filter(|c| c.starts_with(prefix))
            .map(|c| Pair {
                display: c.clone(),
                replacement: c.clone(),
            })
            .collect();
        Ok((start, matches))
    }
}

impl Hinter for FlareHelper {
    type Hint = String;
}

impl Highlighter for FlareHelper {}

impl Validator for FlareHelper {}

impl Helper for FlareHelper {}

/// Check if the input buffer represents a complete statement or block.
///
/// Returns `false` if there are unclosed brackets (`{`, `(`, `[`) or an
/// unterminated string/interpolation, indicating continuation is needed.
pub fn is_complete_input(source: &str) -> bool {
    is_complete_input_with_limits(source, ParserLimits::default())
}

/// Check if the input buffer represents a complete statement or block under the given limits.
pub fn is_complete_input_with_limits(source: &str, limits: ParserLimits) -> bool {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return true;
    }

    if let Some(max) = limits.max_source_bytes {
        if source.len() > max {
            return true;
        }
    }

    let spanned = match lex_spanned_with_limits(source, limits) {
        Ok(tokens) => tokens,
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("source size limit exceeded") {
                return true;
            }
            if msg.contains("unterminated string")
                || msg.contains("unterminated string interpolation")
            {
                return false;
            }
            return true;
        }
    };

    let mut open_delim = 0isize;
    for token in &spanned {
        match token.token {
            Token::LBrace | Token::LParen | Token::LBracket => {
                open_delim += 1;
            }
            Token::RBrace | Token::RParen | Token::RBracket => {
                open_delim -= 1;
            }
            _ => {}
        }
    }
    open_delim <= 0
}

/// Core REPL loop parameterized by input provider, history recorder, and output streams.
/// This allows comprehensive unit testing of continuation prompts, syntax errors during continuation,
/// Ctrl-C (Interrupted), and Ctrl-D (Eof) behavior.
pub fn run_repl_loop<F, S, InputFn, HistFn, OutW, ErrW>(
    rt: &mut Runtime<F, S>,
    mut readline: InputFn,
    mut add_history: HistFn,
    mut stdout: OutW,
    mut stderr: ErrW,
) -> Result<(), Box<dyn Error>>
where
    F: Firestore,
    S: Storage,
    InputFn: FnMut(&mut Runtime<F, S>, &str) -> Result<String, ReadlineError>,
    HistFn: FnMut(&str),
    OutW: Write,
    ErrW: Write,
{
    rt.set_repl_mode(true);
    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() { "fl> " } else { "fl* " };
        let readline = readline(rt, prompt);

        match readline {
            Ok(line) => {
                let trimmed = line.trim();
                if buffer.is_empty() && (trimmed == "exit" || trimmed == "quit") {
                    break;
                }

                let parser_limits = rt.limits().parser_limits();
                if let Some(max) = parser_limits.max_source_bytes {
                    let next_len = if buffer.is_empty() {
                        line.len()
                    } else {
                        buffer.len() + 1 + line.len()
                    };
                    if next_len > max {
                        writeln!(stderr, "error: source size limit exceeded (max {max})")?;
                        buffer.clear();
                        continue;
                    }
                }

                if !buffer.is_empty() {
                    buffer.push('\n');
                }
                buffer.push_str(&line);

                if !is_complete_input_with_limits(&buffer, parser_limits) {
                    continue;
                }

                let input = std::mem::take(&mut buffer);
                let trimmed_input = input.trim();
                if trimmed_input.is_empty() {
                    continue;
                }

                add_history(trimmed_input);

                match rt.run(&input) {
                    Ok(val) => {
                        writeln!(stdout, "=> {}", val.inspect())?;
                    }
                    Err(err) => {
                        writeln!(stderr, "error: {err}")?;
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                if buffer.is_empty() {
                    writeln!(stdout, "^C")?;
                } else {
                    buffer.clear();
                    writeln!(stdout, "^C")?;
                }
            }
            Err(ReadlineError::Eof) => {
                if !buffer.is_empty() {
                    buffer.clear();
                    writeln!(stdout)?;
                } else {
                    break;
                }
            }
            Err(err) => {
                writeln!(stderr, "error: {err}")?;
                break;
            }
        }
    }

    Ok(())
}

/// Run the interactive REPL using Rustyline for terminal line-editing and history.
pub fn run_interactive<F: Firestore, S: Storage>(
    rt: &mut Runtime<F, S>,
) -> Result<(), Box<dyn Error>> {
    rt.set_repl_mode(true);

    let helper = FlareHelper::new();
    let mut rl = Editor::<FlareHelper, FileHistory>::new()?;
    rl.set_helper(Some(helper));

    let history_file = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fl_history"));
    if let Some(ref path) = history_file {
        let _ = rl.load_history(path);
    }

    println!("Flare interactive console (type exit or quit to leave)");

    let rl = RefCell::new(rl);
    let res = run_repl_loop(
        rt,
        |rt, prompt| {
            let mut rl_ref = rl.borrow_mut();
            if let Some(h) = rl_ref.helper() {
                let mut candidates = Vec::new();
                for k in KEYWORDS {
                    candidates.push((*k).to_string());
                }
                for m in MODULES {
                    candidates.push((*m).to_string());
                }
                for b in BUILTINS {
                    candidates.push((*b).to_string());
                }
                for v in rt.global_variable_names() {
                    candidates.push(v);
                }
                candidates.sort();
                candidates.dedup();
                h.set_candidates(candidates);
            }
            rl_ref.readline(prompt)
        },
        |entry| {
            let _ = rl.borrow_mut().add_history_entry(entry);
        },
        std::io::stdout(),
        std::io::stderr(),
    );

    let mut rl = rl.into_inner();
    if let Some(ref path) = history_file {
        let _ = rl.save_history(path);
    }

    res
}

/// Run REPL over a plain input reader and output writer (e.g., piped stdin or tests).
pub fn run_stream<F: Firestore, S: Storage, R: BufRead, W: Write>(
    rt: &mut Runtime<F, S>,
    reader: R,
    mut writer: W,
) -> Result<(), Box<dyn Error>> {
    rt.set_repl_mode(true);

    let mut buffer = String::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if buffer.is_empty() && (trimmed == "exit" || trimmed == "quit") {
            break;
        }

        let parser_limits = rt.limits().parser_limits();
        if let Some(max) = parser_limits.max_source_bytes {
            let next_len = if buffer.is_empty() {
                line.len()
            } else {
                buffer.len() + 1 + line.len()
            };
            if next_len > max {
                writeln!(writer, "error: source size limit exceeded (max {max})")?;
                buffer.clear();
                continue;
            }
        }

        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(&line);

        if !is_complete_input_with_limits(&buffer, parser_limits) {
            continue;
        }

        let input = std::mem::take(&mut buffer);
        if input.trim().is_empty() {
            continue;
        }

        match rt.run(&input) {
            Ok(val) => {
                writeln!(writer, "=> {}", val.inspect())?;
            }
            Err(err) => {
                writeln!(writer, "error: {err}")?;
            }
        }
    }

    Ok(())
}
