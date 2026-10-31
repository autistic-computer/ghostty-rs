//! Port of `src/config/url.zig`.
//!
//! STATUS: ported
//!
//! Default URL/path regex. This is used to detect URLs and file paths in
//! terminal output.
//!
//! This is here in the config package because one day the matchers will be
//! configurable and this will be a default.
//!
//! For scheme URLs, this regex is liberal in what it accepts after the scheme,
//! with exceptions for URLs ending with . or ). Although such URLs are
//! perfectly valid, it is common for text to contain URLs surrounded by
//! parentheses (such as in Markdown links) or at the end of sentences.
//! Therefore, this regex excludes them as follows:
//!
//! 1. Do not match regexes ending with .
//! 2. Do not match regexes ending with ), except for ones which contain a (
//!    without a subsequent )
//!
//! Rule 2 means that we handle the following two cases:
//!
//!   "https://en.wikipedia.org/wiki/Rust_(video_game)" (include parens)
//!   "(https://example.com)" (do not include the parens)
//!
//! There are many complicated cases where these heuristics break down, but
//! handling them well requires a non-regex approach.

// The pieces are macros so they can be concatenated at compile time with
// `concat!` (Zig uses comptime `++`).

macro_rules! url_schemes {
    () => {
        r"https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:"
    };
}

macro_rules! ipv6_url_pattern {
    () => {
        r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)"
    };
}

macro_rules! scheme_url_chars {
    () => {
        r"[\w\-.~:/?#@!$&*+,;=%]"
    };
}

macro_rules! path_chars {
    () => {
        r"[\w\-.~:\/?#@!$&*+;=%]"
    };
}

macro_rules! optional_bracketed_word_suffix {
    () => {
        r"(?:[\(\[]\w*[\)\]])?"
    };
}

macro_rules! no_trailing_punctuation {
    () => {
        r"(?<![,.])"
    };
}

macro_rules! no_trailing_colon {
    () => {
        r"(?<!:)"
    };
}

macro_rules! dotted_path_lookahead {
    () => {
        r"(?=[\w\-.~:\/?#@!$&*+;=%]*\.)"
    };
}

macro_rules! non_dotted_path_lookahead {
    () => {
        r"(?![\w\-.~:\/?#@!$&*+;=%]*\.)"
    };
}

macro_rules! dotted_path_space_segments {
    () => {
        r"(?:(?<!:) (?!\w+:\/\/)(?!\.{0,2}\/)(?!~\/)[\w\-.~:\/?#@!$&*+;=%]*[\/.])*"
    };
}

macro_rules! any_path_space_segments {
    () => {
        r"(?:(?<!:) (?!\w+:\/\/)(?!\.{0,2}\/)(?!~\/)[\w\-.~:\/?#@!$&*+;=%]+)*"
    };
}

// Branch 1: URLs with explicit schemes (http, mailto, ftp, etc.).
macro_rules! scheme_url_branch {
    () => {
        concat!(
            "(?:",
            url_schemes!(),
            ")",
            "(?:",
            ipv6_url_pattern!(),
            "|",
            scheme_url_chars!(),
            "+",
            optional_bracketed_word_suffix!(),
            ")+",
            no_trailing_punctuation!()
        )
    };
}

macro_rules! rooted_or_relative_path_prefix {
    () => {
        r"(?:\.\.\/|\.\/|(?<!\w)~\/|(?:[\w][\w\-.]*\/)*(?<!\w)\$[A-Za-z_]\w*\/|\.[\w][\w\-.]*\/|(?<![\w~\/])\/(?!\/))"
    };
}

// Branch 2: Absolute paths and dot-relative paths (/, ./, ../).
// A dotted segment is treated as file-like, while the undotted case stays
// broad to capture directory-like paths with spaces.
macro_rules! rooted_or_relative_path_branch {
    () => {
        concat!(
            rooted_or_relative_path_prefix!(),
            "(?:",
            dotted_path_lookahead!(),
            path_chars!(),
            "+",
            dotted_path_space_segments!(),
            no_trailing_colon!(),
            "|",
            non_dotted_path_lookahead!(),
            path_chars!(),
            "+",
            any_path_space_segments!(),
            no_trailing_colon!(),
            ")"
        )
    };
}

// Branch 3: Bare relative paths such as src/config/url.zig.
macro_rules! bare_relative_path_prefix {
    () => {
        r"(?<!\$\d*)(?<!\w)[\w][\w\-.]*\/"
    };
}

macro_rules! bare_relative_path_branch {
    () => {
        concat!(
            dotted_path_lookahead!(),
            bare_relative_path_prefix!(),
            path_chars!(),
            "+",
            no_trailing_colon!()
        )
    };
}

pub const URL_SCHEMES: &str = url_schemes!();
pub const SCHEME_URL_BRANCH: &str = scheme_url_branch!();
pub const ROOTED_OR_RELATIVE_PATH_BRANCH: &str = rooted_or_relative_path_branch!();
pub const BARE_RELATIVE_PATH_BRANCH: &str = bare_relative_path_branch!();

pub const REGEX: &str = concat!(
    scheme_url_branch!(),
    "|",
    rooted_or_relative_path_branch!(),
    "|",
    bare_relative_path_branch!()
);

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pkg::oniguruma as oni;

    struct Case {
        input: &'static str,
        expect: &'static str,
        num_matches: usize,
    }

    const fn case(input: &'static str, expect: &'static str) -> Case {
        Case {
            input,
            expect,
            num_matches: 1,
        }
    }

    #[test]
    fn url_regex() {
        oni::testing::ensure_init().unwrap();
        let mut re = oni::Regex::init(
            REGEX.as_bytes(),
            oni::types::Option::default(),
            oni::Encoding::utf8(),
            oni::Syntax::default(),
            None,
        )
        .unwrap();

        // The URL cases to test what our regex matches. Feel free to add to this
        // as we find bugs or just want more coverage.
        let cases = [
            case("hello https://example.com world", "https://example.com"),
            case("https://example.com/foo(bar) more", "https://example.com/foo(bar)"),
            case("https://example.com/foo(bar)baz more", "https://example.com/foo(bar)baz"),
            case("Link inside (https://example.com) parens", "https://example.com"),
            case("Link period https://example.com. More text.", "https://example.com"),
            case("Link trailing comma https://example.com, more text.", "https://example.com"),
            case("Link in double quotes \"https://example.com\" and more", "https://example.com"),
            case("Link in single quotes 'https://example.com' and more", "https://example.com"),
            case("some file with https://google.com https://duckduckgo.com links.", "https://google.com"),
            case("and links in it. links https://yahoo.com mailto:test@example.com ssh://1.2.3.4", "https://yahoo.com"),
            case("also match http://example.com non-secure links", "http://example.com"),
            case("match tel://+12123456789 phone numbers", "tel://+12123456789"),
            case("match with query url https://example.com?query=1&other=2 and more text.", "https://example.com?query=1&other=2"),
            case("url with dashes [mode 2027](https://github.com/contour-terminal/terminal-unicode-core) for better unicode support", "https://github.com/contour-terminal/terminal-unicode-core"),
            case("dot.http://example.com", "http://example.com"),
            // weird characters in URL
            case("weird characters https://example.com/~user/?query=1&other=2#hash and more", "https://example.com/~user/?query=1&other=2#hash"),
            // square brackets in URL
            case("square brackets https://example.com/[foo] and more", "https://example.com/[foo]"),
            // square bracket following url
            case("[13]:TooManyStatements: TempFile#assign_temp_file_to_entity has approx 7 statements [https://example.com/docs/Too-Many-Statements.md]", "https://example.com/docs/Too-Many-Statements.md"),
            // remaining URL schemes tests
            case("match ftp://example.com ftp links", "ftp://example.com"),
            case("match file://example.com file links", "file://example.com"),
            case("match ssh://example.com ssh links", "ssh://example.com"),
            case("match git://example.com git links", "git://example.com"),
            case("/tmp/test.txt http://www.google.com", "/tmp/test.txt"),
            case("match tel:+18005551234 tel links", "tel:+18005551234"),
            case("match magnet:?xt=urn:btih:1234567890 magnet links", "magnet:?xt=urn:btih:1234567890"),
            case("match ipfs://QmSomeHashValue ipfs links", "ipfs://QmSomeHashValue"),
            case("match ipns://QmSomeHashValue ipns links", "ipns://QmSomeHashValue"),
            case("match gemini://example.com gemini links", "gemini://example.com"),
            case("match gopher://example.com gopher links", "gopher://example.com"),
            case("match news:comp.infosystems.www.servers.unix news links", "news:comp.infosystems.www.servers.unix"),
            case("/Users/ghostty.user/code/example.py", "/Users/ghostty.user/code/example.py"),
            case("/Users/ghostty.user/code/../example.py", "/Users/ghostty.user/code/../example.py"),
            case("/Users/ghostty.user/code/../example.py hello world", "/Users/ghostty.user/code/../example.py"),
            case("../example.py", "../example.py"),
            case("../example.py ", "../example.py"),
            case("first time ../example.py contributor ", "../example.py"),
            case("[link](/home/user/ghostty.user/example)", "/home/user/ghostty.user/example"),
            // IPv6 URL tests - Basic tests
            case("Serving HTTP on :: port 8000 (http://[::]:8000/)", "http://[::]:8000/"),
            case("IPv6 address https://[2001:db8::1]:8080/path", "https://[2001:db8::1]:8080/path"),
            case("IPv6 localhost http://[::1]:3000", "http://[::1]:3000"),
            case("Complex IPv6 https://[2001:db8:85a3:8d3:1319:8a2e:370:7348]:443/", "https://[2001:db8:85a3:8d3:1319:8a2e:370:7348]:443/"),
            // IPv6 URL tests - URLs with paths and query parameters
            case("IPv6 with path https://[2001:db8::1]/path/to/resource", "https://[2001:db8::1]/path/to/resource"),
            case("IPv6 with query https://[2001:db8::1]:8080/api?param=value&other=123", "https://[2001:db8::1]:8080/api?param=value&other=123"),
            // IPv6 URL tests - Compressed forms
            case("IPv6 compressed http://[2001:db8::]:80/", "http://[2001:db8::]:80/"),
            case("IPv6 multiple zeros http://[2001:0:0:0:0:0:0:1]", "http://[2001:0:0:0:0:0:0:1]"),
            // IPv6 URL tests - Special cases
            case("IPv6 link-local https://[fe80::1234:5678:9abc]", "https://[fe80::1234:5678:9abc]"),
            case("IPv6 multicast http://[ff02::1]/stream", "http://[ff02::1]/stream"),
            // IPv6 URL tests - Mixed scenarios
            case("IPv6 in markdown [link](http://[2001:db8::1]/docs)", "http://[2001:db8::1]/docs"),
            // Trailing whitespace isn't part of a detected file path.
            case("./spaces-end.   ", "./spaces-end."),
            // File paths with internal spaces
            case("./space middle", "./space middle"),
            case("../test folder/file.txt", "../test folder/file.txt"),
            case("/tmp/test folder/file.txt", "/tmp/test folder/file.txt"),
            case("/tmp/test  folder/file.txt", "/tmp/test"),
            // unified diff lines
            case("diff --git a/src/font/shaper/harfbuzz.zig b/src/font/shaper/harfbuzz.zig", "a/src/font/shaper/harfbuzz.zig"),
            // Two space-separated absolute paths should match only the first
            case("/tmp/foo /tmp/bar", "/tmp/foo"),
            case("/tmp/foo.txt /tmp/bar.txt", "/tmp/foo.txt"),
            // Bare relative file paths (no ./ or ../ prefix)
            case("src/config/url.zig", "src/config/url.zig"),
            case("app/folder/file.rb:1", "app/folder/file.rb:1"),
            case("modified:   src/config/url.zig", "src/config/url.zig"),
            case("lib/ghostty/terminal.zig:42:10", "lib/ghostty/terminal.zig:42:10"),
            case("some-pkg/src/file.txt more text", "some-pkg/src/file.txt"),
            // comma should match substrings
            case("src/foo.c,baz.txt", "src/foo.c"),
            case("~/foo/bar.txt", "~/foo/bar.txt"),
            case("open ~/Documents/notes.md please", "~/Documents/notes.md"),
            case("~/.config/ghostty/config", "~/.config/ghostty/config"),
            case("directory: ~/src/ghostty-org/ghostty", "~/src/ghostty-org/ghostty"),
            case("$HOME/src/config/url.zig", "$HOME/src/config/url.zig"),
            case("project dir: $PWD/src/ghostty/main.zig", "$PWD/src/ghostty/main.zig"),
            // $VAR mid-path should match fully, not partially from the $
            case("foo/$BAR/baz", "foo/$BAR/baz"),
            case(".foo/bar/$VAR", ".foo/bar/$VAR"),
            case(".config/ghostty/config", ".config/ghostty/config"),
            case("loaded from .local/share/ghostty/state.db now", ".local/share/ghostty/state.db"),
            case("../some/where", "../some/where"),
            // comma-separated file paths
            case("  - shared/src/foo/SomeItem.m:12, shared/src/", "shared/src/foo/SomeItem.m:12"),
            // mid-string dot should not partially match but fully
            case("foo.local/share", "foo.local/share"),
            // numeric directory should match fully
            case("2024/report.txt", "2024/report.txt"),
            // comma should stop matching in spaced path segments
            case("./foo bar,baz", "./foo bar"),
            case("/tmp/foo bar,baz", "/tmp/foo bar"),
            // trailing colon should not be part of the path
            case("./.config/ghostty: Needs upstream (main)", "./.config/ghostty"),
            case("./Downloads: Operation not permitted", "./Downloads"),
        ];

        for case in &cases {
            let reg = re
                .search(case.input.as_bytes(), oni::types::Option::default())
                .unwrap_or_else(|_| panic!("no match for input: {}", case.input));
            assert_eq!(case.num_matches, reg.count(), "input: {}", case.input);
            let m = &case.input[reg.starts()[0] as usize..reg.ends()[0] as usize];
            assert_eq!(case.expect, m, "input: {}", case.input);
        }

        let no_match_cases = [
            // bare relative paths without any dot should not match as file paths
            "input/output",
            "foo/bar",
            // $-numeric character should not match
            "$10/bar",
            "$10/$20",
            "$10/bar.txt",
            // comma should not let dot detection look past it
            "foo/bar,baz.txt",
            // $VAR should not match mid-word
            "foo$BAR/baz.txt",
            // ~ should not match mid-word
            "foo~/bar.txt",
            // double-slash comments are not paths
            "// foo bar",
            "//foo",
        ];
        for input in no_match_cases {
            assert!(
                re.search(input.as_bytes(), oni::types::Option::default()).is_err(),
                "unexpected match: {}",
                input
            );
        }
    }
}
