//! ECMAScript whitespace differs from Rust's Unicode whitespace (BOM and NEL).

pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
        '\u{205f}' | '\u{3000}' | '\u{feff}')
}

pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_trim_removes_bom_but_keeps_nel() {
        assert_eq!(trim("\u{feff}\u{a0} text \r\n\u{3000}"), "text");
        assert_eq!(trim("\u{0085}text\u{0085}"), "\u{0085}text\u{0085}");
    }
}
