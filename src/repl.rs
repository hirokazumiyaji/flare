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
use rustyline::{Config, Context, Editor, Helper};

use crate::firestore::Firestore;
use crate::lexer::{lex_spanned_with_limits, Token};
use crate::limits::ParserLimits;
use crate::runtime::Runtime;
use crate::storage::Storage;

pub const KEYWORDS: &[&str] = &[
    "fn", "return", "if", "else", "while", "try", "rescue", "nil", "true", "false",
];

pub const MODULES: &[&str] = &["Firestore", "Auth", "Storage"];

pub const BUILTINS: &[&str] = &[
    "puts",
    "raise",
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
    if open_delim > 0 {
        return false;
    }

    let last_meaningful = spanned
        .iter()
        .rev()
        .map(|st| &st.token)
        .find(|t| !matches!(t, Token::Newline | Token::Eof));
    if let Some(Token::Dot) = last_meaningful {
        return false;
    }

    open_delim <= 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplOptions {
    pub history_file: Option<PathBuf>,
    pub no_history: bool,
}

impl Default for ReplOptions {
    fn default() -> Self {
        Self::resolve()
    }
}

impl ReplOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_no_history(mut self, no_history: bool) -> Self {
        self.no_history = no_history;
        if no_history {
            self.history_file = None;
        }
        self
    }

    pub fn with_history_file(mut self, path: Option<PathBuf>) -> Self {
        if !self.no_history {
            self.history_file = path;
        }
        self
    }

    pub fn resolve() -> Self {
        Self::resolve_from(|k| std::env::var(k).ok())
    }

    pub fn resolve_from<F>(get_env: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let no_history = match get_env("FL_NO_HISTORY") {
            Some(v) => {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            }
            None => false,
        };

        if no_history {
            return Self {
                history_file: None,
                no_history: true,
            };
        }

        let history_file = if let Some(custom) = get_env("FL_HISTORY_FILE") {
            let trimmed = custom.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        } else {
            get_env("HOME")
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".fl_history"))
        };

        Self {
            history_file,
            no_history: false,
        }
    }
}

/// Check if the input contains sensitive authentication commands that should not
/// be saved to command history (e.g., auth_sign_in, auth_sign_up, Auth.sign_*).
pub fn is_sensitive_input(input: &str) -> bool {
    // 1. Try tokenizing with lexer for precise AST identifier checks
    if let Ok(spanned) = lex_spanned_with_limits(input, ParserLimits::default()) {
        let tokens: Vec<&Token> = spanned
            .iter()
            .map(|st| &st.token)
            .filter(|t| !matches!(t, Token::Newline))
            .collect();

        for (i, tok) in tokens.iter().enumerate() {
            if let Token::Ident(name) = tok {
                if name.eq_ignore_ascii_case("auth_sign_up")
                    || name.eq_ignore_ascii_case("auth_sign_in")
                    || name.eq_ignore_ascii_case("sign_up")
                    || name.eq_ignore_ascii_case("sign_in")
                {
                    return true;
                }
                if name.eq_ignore_ascii_case("Auth") && i + 2 < tokens.len() {
                    if let (Token::Dot, Token::Ident(method)) = (tokens[i + 1], tokens[i + 2]) {
                        if is_sensitive_auth_method(method) {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback in case lexing failed on partial or malformed input
    let lower = input.to_ascii_lowercase();
    if contains_ident_word(&lower, "auth_sign_up")
        || contains_ident_word(&lower, "auth_sign_in")
        || contains_ident_word(&lower, "sign_up")
        || contains_ident_word(&lower, "sign_in")
    {
        return true;
    }

    let stripped: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if stripped.contains("auth.sign_in")
        || stripped.contains("auth.sign_up")
        || stripped.contains("auth.auth_sign_in")
        || stripped.contains("auth.auth_sign_up")
    {
        return true;
    }

    false
}

fn is_sensitive_auth_method(method: &str) -> bool {
    let m = method.to_ascii_lowercase();
    m.starts_with("sign_in")
        || m.starts_with("sign_up")
        || m.starts_with("auth_sign_in")
        || m.starts_with("auth_sign_up")
}

fn contains_ident_word(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let abs_pos = start + pos;
        let before_ok = abs_pos == 0 || {
            let prev = text[..abs_pos].chars().next_back().unwrap();
            !prev.is_alphanumeric() && prev != '_'
        };
        let end_pos = abs_pos + word.len();
        let after_ok = end_pos == text.len() || {
            let next = text[end_pos..].chars().next().unwrap();
            !next.is_alphanumeric() && next != '_'
        };
        if before_ok && after_ok {
            return true;
        }
        start = abs_pos + word.len();
    }
    false
}

#[cfg(unix)]
pub fn ensure_file_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("create_dir_all failed for {}: {e}", parent.display()),
                    )
                })?;
            }
            if let Ok(meta) = std::fs::metadata(parent) {
                let mode = meta.permissions().mode();
                // If directory lacks user execute permission (e.g. created while another
                // thread temporarily set a restrictive umask), ensure user has 0700.
                if mode & 0o100 == 0 {
                    let _ = std::fs::set_permissions(
                        parent,
                        std::fs::Permissions::from_mode(mode | 0o700),
                    );
                }
            }
        }
    }
    if !path.exists() {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("open mode 0600 failed for {}: {e}", path.display()),
                )
            })?;
    } else {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("set_permissions failed for {}: {e}", path.display()),
            )
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn ensure_file_0600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn set_file_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("set_file_mode_0600 failed for {}: {e}", path.display()),
            )
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn set_file_mode_0600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
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

                // Multiline commands are buffered until complete. Continuation lines are
                // not added individually to history; instead, the entire completed input
                // is checked with `is_sensitive_input` before deciding whether to persist it.
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

                if !is_sensitive_input(trimmed_input) {
                    add_history(trimmed_input);
                }

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
    run_interactive_with_options(rt, ReplOptions::resolve())
}

/// Run the interactive REPL with custom options.
pub fn run_interactive_with_options<F: Firestore, S: Storage>(
    rt: &mut Runtime<F, S>,
    options: ReplOptions,
) -> Result<(), Box<dyn Error>> {
    rt.set_repl_mode(true);

    let config = Config::builder().auto_add_history(false).build();
    let helper = FlareHelper::new();
    let mut rl = Editor::<FlareHelper, FileHistory>::with_config(config)?;
    rl.set_helper(Some(helper));

    let history_file = if options.no_history {
        None
    } else {
        options.history_file
    };

    if let Some(ref path) = history_file {
        let _ = ensure_file_0600(path);
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
            if !is_sensitive_input(entry) {
                let _ = rl.borrow_mut().add_history_entry(entry);
            }
        },
        std::io::stdout(),
        std::io::stderr(),
    );

    let mut rl = rl.into_inner();
    if let Some(ref path) = history_file {
        let _ = ensure_file_0600(path);
        let _ = rl.save_history(path);
        let _ = set_file_mode_0600(path);
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
