//! Port of `src/extra/vim.zig`.
//!
//! STATUS: ported
//!
//! Zig generates the syntax file at comptime; Rust generates it on first
//! use from `Config`'s fields.

use std::sync::LazyLock;

use super::config_fields;

/// This is the associated Vim file as named by the variable.
pub fn syntax() -> &'static str {
    static SYNTAX: LazyLock<String> = LazyLock::new(gen_syntax);
    &SYNTAX
}

pub const FTDETECT: &str = r#"" Vim filetype detect file
" Language: Ghostty config file
" Maintainer: Ghostty <https://github.com/ghostty-org/ghostty>
"
" THIS FILE IS AUTO-GENERATED

au BufRead,BufNewFile */ghostty/config,*/*.ghostty/config,*/ghostty/themes/*,*.ghostty setf ghostty
"#;

pub const FTPLUGIN: &str = r#"" Vim filetype plugin file
" Language: Ghostty config file
" Maintainer: Ghostty <https://github.com/ghostty-org/ghostty>
"
" THIS FILE IS AUTO-GENERATED

if exists('b:did_ftplugin')
  finish
endif
let b:did_ftplugin = 1

setlocal commentstring=#\ %s
setlocal iskeyword+=-

" Use syntax keywords for completion
setlocal omnifunc=syntaxcomplete#Complete

" Ask ghostty to explain config keywords
setlocal keywordprg=ghostty\ +explain-config

let b:undo_ftplugin = 'setl cms< isk< ofu< kp<'

if !exists('current_compiler')
  compiler ghostty
  let b:undo_ftplugin .= " makeprg< errorformat<"
endif
"#;

pub const COMPILER: &str = r#"" Vim compiler file
" Language: Ghostty config file
" Maintainer: Ghostty <https://github.com/ghostty-org/ghostty>
"
" THIS FILE IS AUTO-GENERATED

if exists("current_compiler")
  finish
endif
let current_compiler = "ghostty"

CompilerSet makeprg=ghostty\ +validate-config\ --config-file=%:S
CompilerSet errorformat=%f:%l:%m,%m
"#;

/// Generates the syntax file.
fn gen_syntax() -> String {
    let mut out = String::new();
    write_syntax(&mut out);
    out
}

/// Writes the syntax file to the given writer.
fn write_syntax(writer: &mut String) {
    writer.push_str(r#"" Vim syntax file
" Language: Ghostty config file
" Maintainer: Ghostty <https://github.com/ghostty-org/ghostty>
"
" THIS FILE IS AUTO-GENERATED

if exists('b:current_syntax')
  finish
endif

let b:current_syntax = 'ghostty'

let s:cpo_save = &cpo
set cpo&vim

syn iskeyword @,48-57,-
syn keyword ghosttyConfigKeyword"#);

    for field in config_fields() {
        if field.name.starts_with('_') {
            continue;
        }
        writer.push_str("\n\t\\ ");
        writer.push_str(field.name);
    }

    writer.push_str(r#"

syn match ghosttyConfigComment /^\s*#.*/ contains=@Spell

hi def link ghosttyConfigComment Comment
hi def link ghosttyConfigKeyword Keyword

let &cpo = s:cpo_save
unlet s:cpo_save
"#);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vim_syntax() {
        let s = syntax();
        assert!(s.starts_with("\" Vim syntax file\n"));
        assert!(s.contains("syn keyword ghosttyConfigKeyword\n\t\\ language\n\t\\ font-family\n"));
        assert!(s.ends_with("let &cpo = s:cpo_save\nunlet s:cpo_save\n"));
        assert!(FTDETECT.ends_with("setf ghostty\n"));
        assert!(FTPLUGIN.contains("setlocal keywordprg=ghostty\\ +explain-config\n"));
        assert!(COMPILER.contains("CompilerSet errorformat=%f:%l:%m,%m\n"));
    }
}
