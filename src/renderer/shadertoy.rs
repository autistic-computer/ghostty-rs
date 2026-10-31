//! Port of `src/renderer/shadertoy.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * Writers are `std::io::Write`; the allocator parameters are dropped.
//! * The returned shader sources are `CString`s (Zig: `[:0]const u8`).
//! * SPIR-V is produced as bytes (like Zig) and re-packed into `u32`
//!   words for [`spv_cross`] (Zig: an aligned copy of the bytes).
//! * `pkg::glslang` / `pkg::spirv_cross` are naga-based ports with a
//!   Rust-shaped C API (results instead of out-parameters), see their docs.

use std::ffi::{c_void, CString};
use std::fmt;
use std::io::{self, Write};
use std::mem::{offset_of, size_of};

use crate::config::path::{Path, RepeatablePath};
use crate::lib::compat::file as compat_file;
use crate::pkg::glslang;
use crate::pkg::spirv_cross as spvcross;

const LOG: &str = "shadertoy";

/// The uniform struct used for shadertoy shaders.
///
/// Zig: an `extern struct` with explicit field alignments; the padding
/// those alignments introduce is spelled out as `_padding_*` fields.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Uniforms {
    pub resolution: [f32; 3],
    pub time: f32,
    pub time_delta: f32,
    pub frame_rate: f32,
    pub frame: i32,
    /// Padding from `channel_time: [4][4]f32 align(16)`.
    pub _padding_channel_time: [u8; 4],
    pub channel_time: [[f32; 4]; 4],
    pub channel_resolution: [[f32; 4]; 4],
    pub mouse: [f32; 4],
    pub date: [f32; 4],
    pub sample_rate: f32,
    /// Padding from `current_cursor: [4]f32 align(16)`.
    pub _padding_current_cursor: [u8; 12],
    pub current_cursor: [f32; 4],
    pub previous_cursor: [f32; 4],
    pub current_cursor_color: [f32; 4],
    pub previous_cursor_color: [f32; 4],
    pub current_cursor_style: i32,
    pub previous_cursor_style: i32,
    pub cursor_visible: i32,
    pub cursor_change_time: f32,
    pub time_focus: f32,
    pub focus: i32,
    /// Padding from `palette: [256][4]f32 align(16)`.
    pub _padding_palette: [u8; 8],
    pub palette: [[f32; 4]; 256],
    pub background_color: [f32; 4],
    pub foreground_color: [f32; 4],
    pub cursor_color: [f32; 4],
    pub cursor_text: [f32; 4],
    pub selection_foreground_color: [f32; 4],
    pub selection_background_color: [f32; 4],
}

impl Default for Uniforms {
    fn default() -> Self {
        // SAFETY: all fields are plain numbers for which zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

// Layout checks: these match the Zig `extern struct` offsets.
const _: () = {
    assert!(offset_of!(Uniforms, resolution) == 0);
    assert!(offset_of!(Uniforms, time) == 12);
    assert!(offset_of!(Uniforms, frame) == 24);
    assert!(offset_of!(Uniforms, channel_time) == 32);
    assert!(offset_of!(Uniforms, channel_resolution) == 96);
    assert!(offset_of!(Uniforms, mouse) == 160);
    assert!(offset_of!(Uniforms, date) == 176);
    assert!(offset_of!(Uniforms, sample_rate) == 192);
    assert!(offset_of!(Uniforms, current_cursor) == 208);
    assert!(offset_of!(Uniforms, current_cursor_style) == 272);
    assert!(offset_of!(Uniforms, focus) == 292);
    assert!(offset_of!(Uniforms, palette) == 304);
    assert!(offset_of!(Uniforms, background_color) == 4400);
    assert!(offset_of!(Uniforms, selection_background_color) == 4480);
    assert!(size_of::<Uniforms>() == 4496);
};

/// The target to load shaders for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Glsl,
    Msl,
}

/// Errors from loading and converting shaders (Zig: inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    FileNotFound,
    /// Any other error opening or reading the shader file.
    Io(io::ErrorKind),
    FileTooBig,
    GlslangInitFailed,
    GlslangFailed,
    OutOfMemory,
    SpirvInvalid,
    SpvcFailed,
    WriteFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(kind) => write!(f, "{kind}"),
            _ => fmt::Debug::fmt(self, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<glslang::Error> for Error {
    fn from(e: glslang::Error) -> Self {
        match e {
            glslang::Error::GlslangInitFailed => Error::GlslangInitFailed,
            glslang::Error::GlslangFailed => Error::GlslangFailed,
            glslang::Error::OutOfMemory => Error::OutOfMemory,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Error::FileNotFound,
            io::ErrorKind::OutOfMemory => Error::OutOfMemory,
            kind => Error::Io(kind),
        }
    }
}

impl From<compat_file::ReadToEndAllocError> for Error {
    fn from(e: compat_file::ReadToEndAllocError) -> Self {
        match e {
            compat_file::ReadToEndAllocError::FileTooBig => Error::FileTooBig,
            compat_file::ReadToEndAllocError::Io(e) => e.into(),
        }
    }
}

/// Load a set of shaders from files and convert them to the target
/// format. The shader order is preserved.
pub fn load_from_files(paths: &RepeatablePath, target: Target) -> Result<Vec<CString>, Error> {
    let mut list: Vec<CString> = Vec::new();

    for item in &paths.value {
        let (path, optional) = match item {
            Path::Optional(path) => (path, true),
            Path::Required(path) => (path, false),
        };

        let shader = match load_from_file(path.as_str(), target) {
            Ok(shader) => shader,
            Err(Error::FileNotFound) if optional => continue,
            Err(err) => return Err(err),
        };
        log::info!(target: LOG, "loaded custom shader path={}", path.as_str());
        list.push(shader);
    }

    Ok(list)
}

/// Load a single shader from a file and convert it to the target language
/// ready to be used with renderers.
pub fn load_from_file(path: &str, target: Target) -> Result<CString, Error> {
    // Read it all into memory -- we don't expect shaders to be large.
    let src = {
        // Load the shader file
        let mut file = std::fs::File::open(path)?;
        compat_file::read_to_end_alloc(
            &mut file,
            4 * 1024 * 1024, // 4MB
        )?
    };

    // Convert to full GLSL
    let glsl: CString = {
        let mut stream: Vec<u8> = Vec::new();
        glsl_from_shader(&mut stream, &src)?;
        // Zig writes a terminating 0; a NUL inside the source would
        // truncate the C string there, which we treat as invalid input.
        CString::new(stream).map_err(|_| Error::GlslangFailed)?
    };

    // Convert to SPIR-V
    let spirv: Vec<u8> = {
        let mut stream: Vec<u8> = Vec::new();
        let mut errlog = SpirvLog::default();
        if let Err(err) = spirv_from_glsl(&mut stream, Some(&mut errlog), &glsl) {
            if !errlog.info.is_empty() || !errlog.debug.is_empty() {
                log::warn!(
                    target: LOG,
                    "spirv error path={} info={} debug={}",
                    path,
                    errlog.info,
                    errlog.debug,
                );
            }

            return Err(err);
        }
        stream
    };

    // Convert to MSL
    match target {
        Target::Glsl => glsl_from_spv(&spirv),
        Target::Msl => msl_from_spv(&spirv),
    }
}

/// Convert a ShaderToy shader into valid GLSL.
///
/// ShaderToy shaders aren't full shaders, they're just implementing a
/// mainImage function and don't define any of the uniforms. This function
/// will convert the ShaderToy shader into a valid GLSL shader that can be
/// compiled and linked.
pub fn glsl_from_shader<W: Write + ?Sized>(writer: &mut W, src: &[u8]) -> Result<(), Error> {
    const PREFIX: &str = include_str!("shaders/shadertoy_prefix.glsl");
    let r = (|| {
        writer.write_all(PREFIX.as_bytes())?;
        writer.write_all(b"\n\n")?;
        writer.write_all(src)
    })();
    r.map_err(|_| Error::WriteFailed)
}

/// Convert a GLSL shader into SPIR-V assembly.
pub fn spirv_from_glsl<W: Write + ?Sized>(
    writer: &mut W,
    errlog: Option<&mut SpirvLog>,
    src: &std::ffi::CStr,
) -> Result<(), Error> {
    // So we can run unit tests without fear.
    if cfg!(test) {
        glslang::testing::ensure_init()?;
    }

    // The naga-based glslang port takes UTF-8 source.
    let code = src.to_str().map_err(|_| Error::GlslangFailed)?;

    use glslang::c;
    let input = c::glslang_input_t {
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
    };

    let mut shader = glslang::Shader::create(&input)?;

    let mut errlog = errlog;
    if let Err(err) = shader.preprocess(&input) {
        if let Some(ptr) = errlog.as_deref_mut() {
            ptr.from_shader(&shader);
        }
        return Err(err.into());
    }
    if let Err(err) = shader.parse(&input) {
        if let Some(ptr) = errlog.as_deref_mut() {
            ptr.from_shader(&shader);
        }
        return Err(err.into());
    }

    let mut program = glslang::Program::create()?;
    program.add_shader(&shader);
    if let Err(err) = program.link(c::GLSLANG_MSG_SPV_RULES_BIT | c::GLSLANG_MSG_VULKAN_RULES_BIT) {
        if let Some(ptr) = errlog.as_deref_mut() {
            ptr.from_program(&program);
        }
        return Err(err.into());
    }
    program.spirv_generate(c::GLSLANG_STAGE_FRAGMENT);
    let words = program.spirv_get_ptr()?;
    debug_assert_eq!(words.len(), program.spirv_get_size());
    let mut bytes: Vec<u8> = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_ne_bytes());
    }
    writer.write_all(&bytes).map_err(|_| Error::WriteFailed)
}

/// Retrieve errors from spirv compilation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpirvLog {
    pub info: String,
    pub debug: String,
}

impl SpirvLog {
    pub fn deinit(&self) {}

    fn from_shader(&mut self, shader: &glslang::Shader) {
        self.info = shader.get_info_log().to_owned();
        self.debug = shader.get_debug_info_log().to_owned();
    }

    fn from_program(&mut self, program: &glslang::Program) {
        self.info = program.get_info_log().unwrap_or("").to_owned();
        self.debug = program.get_debug_info_log().unwrap_or("").to_owned();
    }
}

/// Convert SPIR-V binary to MSL.
pub fn msl_from_spv(spv: &[u8]) -> Result<CString, Error> {
    use spvcross::c;
    spv_cross(
        c::SPVC_BACKEND_MSL,
        spv,
        Some(|options: &mut c::spvc_compiler_options| {
            // We enable decoration binding, because we need this
            // to properly locate the uniform block to index 1.
            if c::spvc_compiler_options_set_bool(
                options,
                c::SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING,
                c::SPVC_TRUE,
            ) != c::SPVC_SUCCESS
            {
                return Err(Error::SpvcFailed);
            }
            Ok(())
        }),
    )
}

/// Convert SPIR-V binary to GLSL.
pub fn glsl_from_spv(spv: &[u8]) -> Result<CString, Error> {
    const GLSL_VERSION: u32 = 430;

    use spvcross::c;
    spv_cross(
        c::SPVC_BACKEND_GLSL,
        spv,
        Some(|options: &mut c::spvc_compiler_options| {
            if c::spvc_compiler_options_set_uint(
                options,
                c::SPVC_COMPILER_OPTION_GLSL_VERSION,
                GLSL_VERSION,
            ) != c::SPVC_SUCCESS
            {
                return Err(Error::SpvcFailed);
            }
            Ok(())
        }),
    )
}

/// Re-pack SPIR-V bytes into native-endian words.
fn spv_words(spv: &[u8]) -> Vec<u32> {
    spv.chunks_exact(4)
        .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
        .collect()
}

fn spv_cross<F>(
    backend: spvcross::c::spvc_backend,
    spv: &[u8],
    options_fn: Option<F>,
) -> Result<CString, Error>
where
    F: FnOnce(&mut spvcross::c::spvc_compiler_options) -> Result<(), Error>,
{
    // Spir-V is always a multiple of 4 because it is written as a series of words
    if spv.len() % 4 != 0 {
        return Err(Error::SpirvInvalid);
    }

    // Compiler context
    use spvcross::c;
    let ctx = c::spvc_context_create().map_err(|_| Error::SpvcFailed)?;
    struct CtxGuard(Option<c::spvc_context>);
    impl Drop for CtxGuard {
        fn drop(&mut self) {
            if let Some(ctx) = self.0.take() {
                c::spvc_context_destroy(ctx);
            }
        }
    }
    let guard = CtxGuard(Some(ctx));
    let ctx = guard.0.as_ref().unwrap();

    // It would be better to get this out into an output parameter to
    // show users but for now we can just log it.
    fn callback(_: *mut c_void, msg: &str) {
        log::warn!("spirv-cross error message={msg}");
    }
    c::spvc_context_set_error_callback(ctx, callback, std::ptr::null_mut());

    // Parse the Spir-V binary to an IR
    let words = spv_words(spv);
    let ir = c::spvc_context_parse_spirv(ctx, &words).map_err(|_| Error::SpvcFailed)?;

    // Build our compiler to GLSL
    let mut compiler =
        c::spvc_context_create_compiler(ctx, backend, ir, c::SPVC_CAPTURE_MODE_TAKE_OWNERSHIP)
            .map_err(|_| Error::SpvcFailed)?;

    // Setup our options if we have any
    if let Some(options_fn) = options_fn {
        let mut options =
            c::spvc_compiler_create_compiler_options(&compiler).map_err(|_| Error::SpvcFailed)?;

        options_fn(&mut options)?;

        if c::spvc_compiler_install_compiler_options(&mut compiler, options) != c::SPVC_SUCCESS {
            return Err(Error::SpvcFailed);
        }
    }

    // Compile the resulting string.
    let result = c::spvc_compiler_compile(&mut compiler).map_err(|_| Error::SpvcFailed)?;

    // A NUL in the output can't be represented as a C string.
    CString::new(result).map_err(|_| Error::SpvcFailed)
}

/// Convert ShaderToy shader to null-terminated glsl for testing.
#[cfg(test)]
fn test_glsl_z(src: &str) -> CString {
    let mut buf: Vec<u8> = Vec::new();
    glsl_from_shader(&mut buf, src.as_bytes()).unwrap();
    CString::new(buf).unwrap()
}

#[cfg(test)]
const TEST_CRT: &str = include_str!("shaders/test_shadertoy_crt.glsl");
#[cfg(test)]
const TEST_INVALID: &str = include_str!("shaders/test_shadertoy_invalid.glsl");
#[cfg(test)]
#[allow(dead_code)]
const TEST_FOCUS: &str = include_str!("shaders/test_shadertoy_focus.glsl");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::spirv_cross::c;

    #[test]
    fn custom_shader_selection_uniform_layout_matches_compiled_glsl() {
        let src = test_glsl_z(
            "void mainImage(out vec4 color, in vec2 pixel) {\n\
             \x20   color = vec4(iSelectionForegroundColor + iSelectionBackgroundColor, 1.0);\n\
             }",
        );

        let mut buf: Vec<u8> = Vec::with_capacity(4096 * 4);
        spirv_from_glsl(&mut buf, None, &src).unwrap();
        let words = spv_words(&buf);

        let ctx = c::spvc_context_create().unwrap();
        let ir = c::spvc_context_parse_spirv(&ctx, &words).unwrap();
        let compiler = c::spvc_context_create_compiler(
            &ctx,
            c::SPVC_BACKEND_NONE,
            ir,
            c::SPVC_CAPTURE_MODE_TAKE_OWNERSHIP,
        )
        .unwrap();
        let resources = c::spvc_compiler_create_shader_resources(&compiler).unwrap();
        let blocks =
            c::spvc_resources_get_resource_list_for_type(&resources, c::SPVC_RESOURCE_TYPE_UNIFORM_BUFFER)
                .unwrap();
        assert_eq!(1usize, blocks.len());
        let id = blocks[0].base_type_id;
        let typ = c::spvc_compiler_get_type_handle(&compiler, id).unwrap();

        for (offset_expected, name_expected) in [
            (offset_of!(Uniforms, selection_foreground_color), "iSelectionForegroundColor"),
            (offset_of!(Uniforms, selection_background_color), "iSelectionBackgroundColor"),
        ] {
            let mut found = false;
            for index in 0..c::spvc_type_get_num_member_types(typ) {
                let name = c::spvc_compiler_get_member_name(&compiler, id, index);
                if name != name_expected {
                    continue;
                }
                let offset = c::spvc_compiler_type_struct_member_offset(&compiler, typ, index).unwrap();
                assert_eq!(offset_expected as u32, offset);
                found = true;
                break;
            }
            assert!(found, "{name_expected}");
        }
        drop(compiler);
        c::spvc_context_destroy(ctx);
    }

    #[test]
    fn spirv() {
        let src = test_glsl_z(TEST_CRT);

        let mut buf: Vec<u8> = Vec::with_capacity(4096 * 4);
        spirv_from_glsl(&mut buf, None, &src).unwrap();
    }

    #[test]
    fn spirv_invalid() {
        let src = test_glsl_z(TEST_INVALID);

        let mut buf: Vec<u8> = Vec::with_capacity(4096 * 4);

        let mut errlog = SpirvLog::default();
        assert_eq!(
            Err(Error::GlslangFailed),
            spirv_from_glsl(&mut buf, Some(&mut errlog), &src)
        );
        assert!(!errlog.info.is_empty());
    }

    #[test]
    fn shadertoy_to_msl() {
        let src = test_glsl_z(TEST_CRT);

        let mut buf: Vec<u8> = Vec::new();
        spirv_from_glsl(&mut buf, None, &src).unwrap();

        let _msl = msl_from_spv(&buf).unwrap();
    }

    #[test]
    fn shadertoy_to_glsl() {
        let src = test_glsl_z(TEST_CRT);

        let mut buf: Vec<u8> = Vec::new();
        spirv_from_glsl(&mut buf, None, &src).unwrap();

        let _glsl = glsl_from_spv(&buf).unwrap();
    }
}
