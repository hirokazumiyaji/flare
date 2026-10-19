#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(tokens) = flare::lexer::lex_spanned(s) {
            let mut parser = flare::parser::Parser::from_spanned(tokens);
            let _ = parser.parse_program_with_spans();
        }
    }
});
