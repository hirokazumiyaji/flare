use std::time::Duration;

/// Conservative defaults used by the library and the `fl` command line.
///
/// The limits are intentionally finite.  Embedders that run trusted scripts
/// can opt into [`RuntimeLimits::unlimited`] explicitly.
///
/// Default nesting depth for calls.
///
/// The runtime validates a call's contract before evaluating its arguments and
/// receiver, so one user-function call costs roughly 50 KiB of native stack in
/// a debug build. Measured on a 2 MiB thread, 40 nested calls overflow before
/// the budget can fire; 32 stops first with margin to spare. That keeps the
/// default safe for hosts with a small stack (libtest threads, wasm, embedded
/// threads). Embedders with a larger stack can raise it via
/// `RuntimeLimits::with_max_call_depth`.
pub const DEFAULT_MAX_CALL_DEPTH: usize = 32;
pub const DEFAULT_MAX_STEPS: u64 = 1_000_000;
pub const DEFAULT_MAX_LOOP_ITERATIONS: u64 = 100_000;
pub const DEFAULT_MAX_WALL_CLOCK_MS: u64 = 5_000;
pub const DEFAULT_MAX_PARSER_DEPTH: usize = 128;
pub const DEFAULT_MAX_VALUE_DEPTH: usize = 64;
pub const DEFAULT_MAX_QUERY_RESULTS: usize = 10_000;
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 1_048_576;
pub const DEFAULT_MAX_TOKENS: usize = 100_000;
pub const DEFAULT_MAX_INTERPOLATION_DEPTH: usize = 32;
pub const DEFAULT_MAX_VALUE_NODES: usize = 100_000;
pub const DEFAULT_MAX_STRING_BYTES: usize = 10 * 1024 * 1024;
pub const DEFAULT_MAX_BYTES_SIZE: usize = 50 * 1024 * 1024;
pub const DEFAULT_BACKEND_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_MAX_FILE_IO_BYTES: usize = 10 * 1024 * 1024;

/// Resource limits for one [`crate::runtime::Runtime`] execution.
///
/// `None` disables an individual limit.  Counts are cumulative for one call
/// to `run`/`eval_program` and are reset at the beginning of the next run.
/// A step is one evaluated statement; a loop iteration is one pass through a
/// `while`, `each`, or `map` body.
///
/// `RuntimeLimits` is the only name for this type. There are deliberately no
/// aliases (`RuntimeConfig` / `ExecutionLimits` / `Budgets`): several names
/// for one type make it unclear which is the real API surface while adding no
/// behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLimits {
    /// Maximum nested call depth.  Calls include built-ins and user functions.
    pub max_call_depth: Option<usize>,
    /// Maximum number of evaluated statements.
    pub max_steps: Option<u64>,
    /// Maximum number of loop-body iterations (shared by all loops in a run).
    pub max_loop_iterations: Option<u64>,
    /// Maximum elapsed time for parsing and evaluation.
    ///
    /// **協調的な phase 境界の deadline**: 評価ステップの開始時、lex/parse
    /// の前後、backend 呼び出しの前後など、処理の区切りでだけ評価する。
    /// したがって 1 つの巨大な配列/map リテラルを組み立てるような区間では、
    /// 実際の経過がこの予算を超えてから検出することがある。
    /// 厳密な強制が要るなら source / token / value-node の各予算を併せて
    /// 設定すること（それらは構文・評価の途中で必ず検査される）。
    /// 実際の RPC には `backend_timeout` が別途効く。
    pub max_wall_clock: Option<Duration>,
    /// Maximum parser/AST nesting depth.
    pub max_parser_depth: Option<usize>,
    /// Maximum nesting depth of runtime `Value`s (the root is depth zero).
    pub max_value_depth: Option<usize>,
    /// Maximum number of candidates materialized by one query operation.
    pub max_query_results: Option<usize>,
    /// Maximum source size accepted by the lexer, in UTF-8 bytes.
    pub max_source_bytes: Option<usize>,
    /// Maximum number of lexical tokens (including tokens in interpolations).
    pub max_tokens: Option<usize>,
    /// Maximum nested string-interpolation depth.
    pub max_interpolation_depth: Option<usize>,
    /// Maximum number of nodes in a runtime `Value`.
    pub max_value_nodes: Option<usize>,
    /// Maximum byte size of a single `Value::Str`.
    pub max_string_bytes: Option<usize>,
    /// Maximum byte size of a single `Value::Bytes`.
    pub max_bytes_size: Option<usize>,
    /// Per-Firestore-call timeout propagated through
    /// [`crate::firestore::Firestore::set_timeout`].
    ///
    /// The generic synchronous `Firestore` trait cannot forcibly interrupt an
    /// arbitrary user implementation, so its compatibility default is a
    /// no-op. The built-in emulator honors this value for every RPC; other
    /// backends should override the hook and enforce it themselves. Use
    /// [`crate::runtime::Runtime::backend_timeout_supported`] to inspect the
    /// capability when an embedding application supports multiple backends.
    pub backend_timeout: Option<Duration>,
    /// Maximum number of bytes allowed for one file read or write.
    pub max_file_io_bytes: Option<usize>,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_call_depth: Some(DEFAULT_MAX_CALL_DEPTH),
            max_steps: Some(DEFAULT_MAX_STEPS),
            max_loop_iterations: Some(DEFAULT_MAX_LOOP_ITERATIONS),
            max_wall_clock: Some(Duration::from_millis(DEFAULT_MAX_WALL_CLOCK_MS)),
            max_parser_depth: Some(DEFAULT_MAX_PARSER_DEPTH),
            max_value_depth: Some(DEFAULT_MAX_VALUE_DEPTH),
            max_query_results: Some(DEFAULT_MAX_QUERY_RESULTS),
            max_source_bytes: Some(DEFAULT_MAX_SOURCE_BYTES),
            max_tokens: Some(DEFAULT_MAX_TOKENS),
            max_interpolation_depth: Some(DEFAULT_MAX_INTERPOLATION_DEPTH),
            max_value_nodes: Some(DEFAULT_MAX_VALUE_NODES),
            max_string_bytes: Some(DEFAULT_MAX_STRING_BYTES),
            max_bytes_size: Some(DEFAULT_MAX_BYTES_SIZE),
            backend_timeout: Some(Duration::from_millis(DEFAULT_BACKEND_TIMEOUT_MS)),
            max_file_io_bytes: Some(DEFAULT_MAX_FILE_IO_BYTES),
        }
    }
}

impl RuntimeLimits {
    /// Construct limits with every guard disabled.
    ///
    /// This is an explicit opt-in for trusted input.  Prefer the finite
    /// [`Default`] limits for files, CLI input, or other untrusted sources.
    pub fn unlimited() -> Self {
        Self {
            max_call_depth: None,
            max_steps: None,
            max_loop_iterations: None,
            max_wall_clock: None,
            max_parser_depth: None,
            max_value_depth: None,
            max_query_results: None,
            max_source_bytes: None,
            max_tokens: None,
            max_interpolation_depth: None,
            max_value_nodes: None,
            max_string_bytes: None,
            max_bytes_size: None,
            backend_timeout: None,
            max_file_io_bytes: None,
        }
    }

    /// Alias for [`RuntimeLimits::unlimited`] emphasizing that this should only
    /// be used for trusted scripts.
    pub fn trusted() -> Self {
        Self::unlimited()
    }

    pub fn with_max_call_depth(mut self, limit: usize) -> Self {
        self.max_call_depth = Some(limit);
        self
    }

    pub fn with_max_steps(mut self, limit: u64) -> Self {
        self.max_steps = Some(limit);
        self
    }

    pub fn with_max_loop_iterations(mut self, limit: u64) -> Self {
        self.max_loop_iterations = Some(limit);
        self
    }

    pub fn with_max_wall_clock(mut self, limit: Duration) -> Self {
        self.max_wall_clock = Some(limit);
        self
    }

    pub fn with_max_wall_clock_ms(mut self, limit: u64) -> Self {
        self.max_wall_clock = Some(Duration::from_millis(limit));
        self
    }

    pub fn with_max_parser_depth(mut self, limit: usize) -> Self {
        self.max_parser_depth = Some(limit);
        self
    }

    pub fn with_max_value_depth(mut self, limit: usize) -> Self {
        self.max_value_depth = Some(limit);
        self
    }

    pub fn with_max_query_results(mut self, limit: usize) -> Self {
        self.max_query_results = Some(limit);
        self
    }

    pub fn with_max_source_bytes(mut self, limit: usize) -> Self {
        self.max_source_bytes = Some(limit);
        self
    }

    pub fn with_max_tokens(mut self, limit: usize) -> Self {
        self.max_tokens = Some(limit);
        self
    }

    pub fn with_max_interpolation_depth(mut self, limit: usize) -> Self {
        self.max_interpolation_depth = Some(limit);
        self
    }

    pub fn with_max_value_nodes(mut self, limit: usize) -> Self {
        self.max_value_nodes = Some(limit);
        self
    }

    pub fn with_max_string_bytes(mut self, limit: usize) -> Self {
        self.max_string_bytes = Some(limit);
        self
    }

    pub fn with_max_bytes_size(mut self, limit: usize) -> Self {
        self.max_bytes_size = Some(limit);
        self
    }

    pub fn with_backend_timeout(mut self, limit: Duration) -> Self {
        self.backend_timeout = Some(limit);
        self
    }

    pub fn with_max_file_io_bytes(mut self, limit: usize) -> Self {
        self.max_file_io_bytes = Some(limit);
        self
    }

    /// The subset passed to the lexer and parser.
    pub fn parser_limits(&self) -> ParserLimits {
        ParserLimits {
            max_depth: self.max_parser_depth,
            max_source_bytes: self.max_source_bytes,
            max_tokens: self.max_tokens,
            max_interpolation_depth: self.max_interpolation_depth,
        }
    }
}

/// Limits used by the public lexer/parser APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    pub max_depth: Option<usize>,
    pub max_source_bytes: Option<usize>,
    pub max_tokens: Option<usize>,
    pub max_interpolation_depth: Option<usize>,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_depth: Some(DEFAULT_MAX_PARSER_DEPTH),
            max_source_bytes: Some(DEFAULT_MAX_SOURCE_BYTES),
            max_tokens: Some(DEFAULT_MAX_TOKENS),
            max_interpolation_depth: Some(DEFAULT_MAX_INTERPOLATION_DEPTH),
        }
    }
}

impl ParserLimits {
    pub fn unlimited() -> Self {
        Self {
            max_depth: None,
            max_source_bytes: None,
            max_tokens: None,
            max_interpolation_depth: None,
        }
    }

    pub fn with_max_depth(mut self, limit: usize) -> Self {
        self.max_depth = Some(limit);
        self
    }

    pub fn with_max_source_bytes(mut self, limit: usize) -> Self {
        self.max_source_bytes = Some(limit);
        self
    }

    pub fn with_max_tokens(mut self, limit: usize) -> Self {
        self.max_tokens = Some(limit);
        self
    }

    pub fn with_max_interpolation_depth(mut self, limit: usize) -> Self {
        self.max_interpolation_depth = Some(limit);
        self
    }
}

/// A snapshot of budget consumption after a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeUsage {
    pub steps: u64,
    pub loop_iterations: u64,
    pub max_call_depth: usize,
    pub query_results: u64,
    pub max_query_results: usize,
    pub max_value_depth: usize,
    pub elapsed: Duration,
}

impl RuntimeUsage {
    pub fn call_depth(&self) -> usize {
        self.max_call_depth
    }

    pub fn steps_used(&self) -> u64 {
        self.steps
    }

    pub fn loop_iterations_used(&self) -> u64 {
        self.loop_iterations
    }

    pub fn query_results_used(&self) -> u64 {
        self.query_results
    }
}
