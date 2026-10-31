//! Port of `src/glslang/shader.zig`.
//!
//! STATUS: ported
//!
//! `Shader` mirrors glslang's `glslang_shader_t`: create it from an input,
//! [`Shader::preprocess`], then [`Shader::parse`]; errors are reported as
//! [`Error::GlslangFailed`] with details in the info log. Parsing is done by
//! naga's GLSL frontend.
//!
//! naga's frontend is stricter than glslang (it implements GLSL 4.40+ with
//! Vulkan semantics), so preprocessing applies a compatibility rewrite that
//! keeps glslang-accepted shaders (like Ghostty's shadertoy prefix) working:
//!
//! * `#version` below 440 is raised to 450 (naga only accepts 440/450/460);
//!   a missing `#version` defaults to 450.
//! * Redundant `uniform` qualifiers on interface block members are removed.
//! * Combined image samplers (`uniform sampler2D x;`, which naga doesn't
//!   support as globals) become a separate texture (same set/binding) and a
//!   sampler (set 1, same binding), plus a `#define x sampler2D(tex, smp)` so
//!   every use site keeps working.
//! * Redeclarations of built-in variables (`in vec4 gl_FragCoord;`) are
//!   dropped; naga provides the built-ins itself.
//!
//! The rewrite is line-based (one declaration per line, which is how these
//! are written in practice); lines it doesn't recognize are left untouched.

use super::c;
use super::Error;

pub struct Shader {
    stage: c::glslang_stage_t,
    target_language_version: c::glslang_target_language_version_t,
    source: String,
    preprocessed: Option<String>,
    module: Option<naga::Module>,
    info_log: String,
    debug_log: String,
}

impl Shader {
    pub fn create(input: &c::glslang_input_t<'_>) -> Result<Box<Shader>, Error> {
        Ok(Box::new(Shader {
            stage: input.stage,
            target_language_version: input.target_language_version,
            source: input.code.to_owned(),
            preprocessed: None,
            module: None,
            info_log: String::new(),
            debug_log: String::new(),
        }))
    }

    /// Destroy the shader (`glslang_shader_delete`).
    pub fn delete(self: Box<Self>) {}

    pub fn preprocess(&mut self, input: &c::glslang_input_t<'_>) -> Result<(), Error> {
        self.info_log.clear();
        self.debug_log.clear();

        if input.language != c::GLSLANG_SOURCE_GLSL {
            self.info_log
                .push_str("ERROR: only GLSL source is supported\nERROR: 1 compilation errors.  No code generated.\n\n");
            return Err(Error::GlslangFailed);
        }

        self.preprocessed = Some(naga_compat(&self.source));
        Ok(())
    }

    pub fn parse(&mut self, input: &c::glslang_input_t<'_>) -> Result<(), Error> {
        if self.preprocessed.is_none() {
            self.preprocess(input)?;
        }
        self.info_log.clear();

        let Some(stage) = naga_stage(self.stage) else {
            self.info_log.push_str(
                "ERROR: unsupported shader stage\nERROR: 1 compilation errors.  No code generated.\n\n",
            );
            return Err(Error::GlslangFailed);
        };

        let source = self.preprocessed.as_deref().unwrap_or_default();
        let mut frontend = naga::front::glsl::Frontend::default();
        match frontend.parse(&naga::front::glsl::Options::from(stage), source) {
            Ok(module) => {
                self.module = Some(module);
                Ok(())
            }
            Err(errs) => {
                self.module = None;
                for err in &errs.errors {
                    let line = if err.meta.is_defined() {
                        err.meta.location(source).line_number
                    } else {
                        0
                    };
                    self.info_log
                        .push_str(&format!("ERROR: 0:{line}: {}\n", err.kind));
                }
                self.info_log.push_str(&format!(
                    "ERROR: {} compilation errors.  No code generated.\n\n",
                    errs.errors.len()
                ));
                self.debug_log.push_str(source);
                Err(Error::GlslangFailed)
            }
        }
    }

    pub fn get_info_log(&self) -> &str {
        &self.info_log
    }

    pub fn get_debug_info_log(&self) -> &str {
        &self.debug_log
    }

    pub(crate) fn stage(&self) -> c::glslang_stage_t {
        self.stage
    }

    pub(crate) fn target_language_version(&self) -> c::glslang_target_language_version_t {
        self.target_language_version
    }

    pub(crate) fn module(&self) -> Option<&naga::Module> {
        self.module.as_ref()
    }
}

/// Map a glslang stage to a naga shader stage.
pub(crate) fn naga_stage(stage: c::glslang_stage_t) -> Option<naga::ShaderStage> {
    match stage {
        c::GLSLANG_STAGE_VERTEX => Some(naga::ShaderStage::Vertex),
        c::GLSLANG_STAGE_FRAGMENT => Some(naga::ShaderStage::Fragment),
        c::GLSLANG_STAGE_COMPUTE => Some(naga::ShaderStage::Compute),
        _ => None,
    }
}

/// A token of a (comment-free) source line: an identifier/number or a single
/// punctuation character.
fn tokenize(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
        } else if b.is_ascii_alphanumeric() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(&s[start..i]);
        } else {
            let len = s[i..].chars().next().map_or(1, char::len_utf8);
            out.push(&s[i..i + len]);
            i += len;
        }
    }
    out
}

/// Remove comments from a line, tracking `/* */` across lines.
fn strip_comments(line: &str, in_block_comment: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        if *in_block_comment {
            match rest.find("*/") {
                Some(end) => {
                    *in_block_comment = false;
                    out.push(' ');
                    rest = &rest[end + 2..];
                }
                None => return out,
            }
        }
        let line_comment = rest.find("//");
        let block_comment = rest.find("/*");
        match (line_comment, block_comment) {
            (Some(l), Some(b)) if l < b => {
                out.push_str(&rest[..l]);
                return out;
            }
            (Some(l), None) => {
                out.push_str(&rest[..l]);
                return out;
            }
            (_, Some(b)) => {
                out.push_str(&rest[..b]);
                *in_block_comment = true;
                rest = &rest[b + 2..];
            }
            (None, None) => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// Skip an optional `layout(...)` qualifier at the start of `toks`,
/// returning the remaining tokens and the layout's tokens (without the
/// `layout` keyword and parentheses).
fn split_layout<'a>(toks: &'a [&'a str]) -> Option<(&'a [&'a str], &'a [&'a str])> {
    if toks.first() != Some(&"layout") {
        return Some((toks, &[]));
    }
    if toks.get(1) != Some(&"(") {
        return None;
    }
    let close = toks.iter().position(|t| *t == ")")?;
    Some((&toks[close + 1..], &toks[2..close]))
}

/// Find `name = value` in layout tokens.
fn layout_value(layout: &[&str], name: &str) -> Option<u32> {
    let i = layout.iter().position(|t| *t == name)?;
    if layout.get(i + 1) != Some(&"=") {
        return None;
    }
    layout.get(i + 2)?.parse().ok()
}

/// The non-shadow combined image sampler types and their texture types.
fn texture_type_for_sampler(ty: &str) -> Option<String> {
    let (prefix, rest) = match ty.as_bytes().first() {
        Some(b'i') | Some(b'u') => (&ty[..1], &ty[1..]),
        _ => ("", ty),
    };
    let suffix = rest.strip_prefix("sampler")?;
    match suffix {
        "1D" | "2D" | "3D" | "Cube" | "1DArray" | "2DArray" | "CubeArray" | "2DMS"
        | "2DMSArray" => Some(format!("{prefix}texture{suffix}")),
        _ => None,
    }
}

/// Rewrite a combined image sampler declaration, if `toks` is one.
fn rewrite_sampler(toks: &[&str], indent: &str) -> Option<String> {
    let (rest, layout) = split_layout(toks)?;
    let mut rest = rest;
    if rest.first() != Some(&"uniform") {
        return None;
    }
    rest = &rest[1..];
    if matches!(
        rest.first(),
        Some(&"lowp") | Some(&"mediump") | Some(&"highp")
    ) {
        rest = &rest[1..];
    }
    let [ty, name, ";"] = rest else {
        return None;
    };
    let texture_ty = texture_type_for_sampler(ty)?;
    let binding = layout_value(layout, "binding").unwrap_or(0);
    let set = layout_value(layout, "set").unwrap_or(0);
    let tex = format!("{name}_ghostty_texture");
    let smp = format!("{name}_ghostty_sampler");
    Some(format!(
        "{indent}layout(set = {set}, binding = {binding}) uniform {texture_ty} {tex}; \
         layout(set = {}, binding = {binding}) uniform sampler {smp};\n\
         #define {name} {ty}({tex}, {smp})",
        set + 1,
    ))
}

/// Whether `toks` is a redeclaration of a built-in variable, e.g.
/// `layout(location = 0) in vec4 gl_FragCoord;`.
fn is_builtin_redeclaration(toks: &[&str]) -> bool {
    let Some((rest, _)) = split_layout(toks) else {
        return false;
    };
    let Some((last, decl)) = rest.split_last() else {
        return false;
    };
    if *last != ";" || decl.len() < 3 {
        return false;
    }
    let name = decl[decl.len() - 1];
    if !name.starts_with("gl_") {
        return false;
    }
    decl[..decl.len() - 2].iter().all(|q| {
        matches!(
            *q,
            "in" | "out" | "flat" | "smooth" | "noperspective" | "invariant" | "centroid"
        )
    }) && decl[..decl.len() - 2]
        .iter()
        .any(|q| matches!(*q, "in" | "out"))
}

/// Rewrite a `#version` line for naga.
fn rewrite_version(line: &str) -> String {
    let toks = tokenize(line);
    // ["#", "version", N, profile?]
    let Some(version) = toks.get(2).and_then(|v| v.parse::<u32>().ok()) else {
        return line.to_owned();
    };
    let profile = toks.get(3).copied();
    if profile == Some("es") || version >= 440 {
        return line.to_owned();
    }
    match profile {
        Some(p) => format!("#version 450 {p}"),
        None => "#version 450".to_owned(),
    }
}

/// Remove whole-word `uniform` qualifiers from a line.
fn strip_uniform_words(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < line.len() {
        let is_word_start =
            i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if is_word_start && line[i..].starts_with("uniform") {
            let end = i + "uniform".len();
            let is_word_end =
                end == line.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
            if is_word_end {
                i = end;
                // Also drop one following space so indentation stays tidy.
                if line[i..].starts_with(' ') || line[i..].starts_with('\t') {
                    i += 1;
                }
                continue;
            }
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The compatibility rewrite described in the module docs.
pub(crate) fn naga_compat(src: &str) -> String {
    #[derive(PartialEq)]
    enum Block {
        None,
        /// Saw `uniform Name` without the opening brace yet.
        Header,
        Inside,
    }

    let mut out = String::with_capacity(src.len() + 256);
    let mut saw_version = false;
    let mut block = Block::None;
    let mut in_block_comment = false;

    for raw in src.split_inclusive('\n') {
        let (line, nl) = match raw.strip_suffix('\n') {
            Some(l) => (l, "\n"),
            None => (raw, ""),
        };
        let starts_in_comment = in_block_comment;
        let code = strip_comments(line, &mut in_block_comment);
        let trimmed = code.trim();
        let toks = tokenize(trimmed);
        let indent = &line[..line.len() - line.trim_start().len()];

        if starts_in_comment || toks.is_empty() {
            out.push_str(raw);
            continue;
        }

        // #version
        if !saw_version && toks.len() >= 2 && toks[0] == "#" && toks[1] == "version" {
            saw_version = true;
            out.push_str(&rewrite_version(line));
            out.push_str(nl);
            continue;
        }
        if !saw_version && toks[0] != "#" {
            // A shader without #version: glslang uses the default version,
            // naga requires one.
            saw_version = true;
            out.push_str("#version 450\n");
        }

        match block {
            Block::Inside => {
                if trimmed.contains('}') {
                    block = Block::None;
                    out.push_str(raw);
                } else {
                    out.push_str(&strip_uniform_words(line));
                    out.push_str(nl);
                }
                continue;
            }
            Block::Header => {
                if trimmed.starts_with('{') {
                    block = if trimmed.contains('}') {
                        Block::None
                    } else {
                        Block::Inside
                    };
                    out.push_str(&strip_uniform_words(line));
                    out.push_str(nl);
                    continue;
                }
                block = Block::None;
            }
            Block::None => {}
        }

        if toks[0] == "#" {
            out.push_str(raw);
            continue;
        }

        if let Some(rewritten) = rewrite_sampler(&toks, indent) {
            out.push_str(&rewritten);
            out.push_str(nl);
            continue;
        }

        if is_builtin_redeclaration(&toks) {
            out.push_str(nl);
            continue;
        }

        // Interface blocks: `[layout(...)] uniform|buffer Name [{ ...`
        let is_block_header = split_layout(&toks)
            .map(|(rest, _)| {
                matches!(rest.first(), Some(&"uniform") | Some(&"buffer"))
                    && rest.len() >= 2
                    && !rest.contains(&";")
                    && !rest.contains(&"(")
            })
            .unwrap_or(false);
        if is_block_header {
            match (trimmed.find('{'), trimmed.find('}')) {
                (Some(open), Some(_)) => {
                    // Single-line block: strip inside the braces only.
                    let open_in_line = line.find('{').unwrap_or(open);
                    out.push_str(&line[..=open_in_line]);
                    out.push_str(&strip_uniform_words(&line[open_in_line + 1..]));
                }
                (Some(_), None) => {
                    block = Block::Inside;
                    out.push_str(line);
                }
                _ => {
                    block = Block::Header;
                    out.push_str(line);
                }
            }
            out.push_str(nl);
            continue;
        }

        out.push_str(raw);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::glslang::testing;

    fn input(code: &str) -> c::glslang_input_t<'_> {
        c::glslang_input_t {
            language: c::GLSLANG_SOURCE_GLSL,
            stage: c::GLSLANG_STAGE_FRAGMENT,
            client: c::GLSLANG_CLIENT_VULKAN,
            client_version: c::GLSLANG_TARGET_VULKAN_1_2,
            target_language: c::GLSLANG_TARGET_SPV,
            target_language_version: c::GLSLANG_TARGET_SPV_1_5,
            code,
            default_version: 100,
            default_profile: c::GLSLANG_NO_PROFILE,
            force_default_version_and_profile: 0,
            forward_compatible: 0,
            messages: c::GLSLANG_MSG_DEFAULT_BIT,
            resource: c::glslang_default_resource(),
        }
    }

    #[test]
    fn shader() {
        let input = input(include_str!("test/simple.frag"));

        testing::ensure_init().unwrap();
        let mut shader = Shader::create(&input).unwrap();
        shader.preprocess(&input).unwrap();
        shader.parse(&input).unwrap();
        shader.delete();
    }

    #[test]
    fn parse_error_has_info_log() {
        let input = input("#version 450\nvoid main() { undefined_thing(); }\n");
        let mut shader = Shader::create(&input).unwrap();
        shader.preprocess(&input).unwrap();
        assert_eq!(shader.parse(&input), Err(Error::GlslangFailed));
        assert!(shader.get_info_log().contains("ERROR: 0:2:"));
    }

    #[test]
    fn compat_rewrite() {
        let src = "#version 430 core\n\
                   layout(binding = 1, std140) uniform Globals {\n\
                   \x20   uniform vec3  iResolution; // comment\n\
                   };\n\
                   layout(binding = 0) uniform sampler2D iChannel0;\n\
                   layout(location = 0) in vec4 gl_FragCoord;\n\
                   void main() {}\n";
        let out = naga_compat(src);
        assert!(out.starts_with("#version 450 core\n"));
        assert!(out.contains("    vec3  iResolution; // comment\n"));
        assert!(out
            .contains("layout(set = 0, binding = 0) uniform texture2D iChannel0_ghostty_texture;"));
        assert!(
            out.contains("layout(set = 1, binding = 0) uniform sampler iChannel0_ghostty_sampler;")
        );
        assert!(out.contains(
            "#define iChannel0 sampler2D(iChannel0_ghostty_texture, iChannel0_ghostty_sampler)\n"
        ));
        assert!(!out.contains("gl_FragCoord"));
        assert!(out.ends_with("void main() {}\n"));
    }

    #[test]
    fn compat_keeps_supported_versions() {
        assert_eq!(rewrite_version("#version 460"), "#version 460");
        assert_eq!(rewrite_version("#version 330"), "#version 450");
        assert_eq!(rewrite_version("#version 300 es"), "#version 300 es");
    }
}
