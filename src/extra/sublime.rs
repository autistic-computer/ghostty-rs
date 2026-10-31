//! Port of `src/extra/sublime.zig`.
//!
//! STATUS: ported
//!
//! Zig builds the syntax file at comptime; Rust builds it on first use from
//! `Config`'s fields.

use std::sync::LazyLock;

use super::config_fields;

mod template {
    pub const HEADER: &str = r#"%YAML 1.2
---
# See http://www.sublimetext.com/docs/syntax.html
name: Ghostty Config
file_extensions:
  - ghostty
scope: source.ghostty

contexts:
  main:
    # Comments
    - match: '^\s*#.*$'
      scope: comment.line.number-sign.ghostty

    # Keywords
    - match: '\b("#;
    pub const FOOTER: &str = r#")\b'
      scope: keyword.other.ghostty
"#;
}

/// Check if a field is internal (starts with underscore)
fn is_internal(name: &str) -> bool {
    name.starts_with('_')
}

/// Generate keywords from Config fields
fn generate_keywords() -> String {
    let mut keywords = String::new();

    for field in config_fields() {
        if is_internal(field.name) {
            continue;
        }
        if !keywords.is_empty() {
            keywords.push('|');
        }
        keywords.push_str(field.name);
    }

    keywords
}

/// Complete Sublime syntax file content
pub fn syntax() -> &'static str {
    static SYNTAX: LazyLock<String> = LazyLock::new(|| {
        format!("{}{}{}", template::HEADER, generate_keywords(), template::FOOTER)
    });
    &SYNTAX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sublime_syntax() {
        let s = syntax();
        assert!(s.starts_with("%YAML 1.2\n---\n"));
        assert!(s.contains("    - match: '\\b(language|font-family|"));
        assert!(s.ends_with(")\\b'\n      scope: keyword.other.ghostty\n"));
        assert!(!s.contains("|_"));
        assert!(is_internal("_diagnostics"));
        assert!(!is_internal("font-family"));
    }
}
