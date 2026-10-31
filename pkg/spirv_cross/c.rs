//! Port of `src/spirv-cross/c.zig`.
//!
//! STATUS: ported
//!
//! The Zig package imports SPIRV-Cross's C API (`spirv_cross_c.h`) with
//! translate-c. The Rust port doesn't link SPIRV-Cross (C++); this module
//! implements the subset of the C API that Ghostty uses (context, parsing,
//! compilers for GLSL/MSL, compiler options and resource reflection) in Rust
//! on top of `naga` (SPIR-V frontend, GLSL and MSL backends), keeping the C
//! names so code written against the C API reads the same.
//!
//! Differences from the C API:
//! * Functions return `Result<T, spvc_result>` instead of out-parameters.
//! * Handles are owned Rust values; "destroy" functions consume them.
//! * `spvc_compiler_compile` returns an owned `String`.
//! * Compiler option values are names only (not ABI-compatible numbers).
//! * Like SPIRV-Cross, the MSL backend names a `main` entry point `main0`
//!   and, with `SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING`, uses the
//!   SPIR-V binding decorations as Metal buffer/texture/sampler indices. The
//!   GLSL backend keeps the SPIR-V bindings as explicit `layout(binding=)`.
//! * Reflection: naga's SPIR-V writer wraps uniform blocks in a one-member
//!   struct; the reflected `base_type_id` of a uniform buffer is the inner
//!   (user-declared) block struct, which is what SPIRV-Cross reports for
//!   glslang-produced SPIR-V.

#![allow(non_camel_case_types)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::rc::Rc;

pub type SpvId = u32;
pub type spvc_bool = bool;
pub const SPVC_TRUE: spvc_bool = true;
pub const SPVC_FALSE: spvc_bool = false;

pub type spvc_type_id = SpvId;
pub type spvc_variable_id = SpvId;

/// `spvc_result`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum spvc_result {
    SPVC_SUCCESS = 0,
    SPVC_ERROR_INVALID_SPIRV = -1,
    SPVC_ERROR_UNSUPPORTED_SPIRV = -2,
    SPVC_ERROR_OUT_OF_MEMORY = -3,
    SPVC_ERROR_INVALID_ARGUMENT = -4,
}
pub use spvc_result::*;

/// `spvc_backend`
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum spvc_backend {
    SPVC_BACKEND_NONE = 0,
    SPVC_BACKEND_GLSL = 1,
    SPVC_BACKEND_HLSL = 2,
    SPVC_BACKEND_MSL = 3,
    SPVC_BACKEND_CPP = 4,
    SPVC_BACKEND_JSON = 5,
}
pub use spvc_backend::*;

/// `spvc_capture_mode`
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum spvc_capture_mode {
    SPVC_CAPTURE_MODE_COPY = 0,
    SPVC_CAPTURE_MODE_TAKE_OWNERSHIP = 1,
}
pub use spvc_capture_mode::*;

/// `spvc_resource_type`
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum spvc_resource_type {
    SPVC_RESOURCE_TYPE_UNKNOWN = 0,
    SPVC_RESOURCE_TYPE_UNIFORM_BUFFER = 1,
    SPVC_RESOURCE_TYPE_STORAGE_BUFFER = 2,
    SPVC_RESOURCE_TYPE_STAGE_INPUT = 3,
    SPVC_RESOURCE_TYPE_STAGE_OUTPUT = 4,
    SPVC_RESOURCE_TYPE_SUBPASS_INPUT = 5,
    SPVC_RESOURCE_TYPE_STORAGE_IMAGE = 6,
    SPVC_RESOURCE_TYPE_SAMPLED_IMAGE = 7,
    SPVC_RESOURCE_TYPE_ATOMIC_COUNTER = 8,
    SPVC_RESOURCE_TYPE_PUSH_CONSTANT = 9,
    SPVC_RESOURCE_TYPE_SEPARATE_IMAGE = 10,
    SPVC_RESOURCE_TYPE_SEPARATE_SAMPLERS = 11,
}
pub use spvc_resource_type::*;

/// `spvc_compiler_option`
pub type spvc_compiler_option = u32;
pub const SPVC_COMPILER_OPTION_COMMON_BIT: spvc_compiler_option = 0x1000000;
pub const SPVC_COMPILER_OPTION_GLSL_BIT: spvc_compiler_option = 0x2000000;
pub const SPVC_COMPILER_OPTION_HLSL_BIT: spvc_compiler_option = 0x4000000;
pub const SPVC_COMPILER_OPTION_MSL_BIT: spvc_compiler_option = 0x8000000;
pub const SPVC_COMPILER_OPTION_LANG_BITS: spvc_compiler_option = 0x0f000000;

pub const SPVC_COMPILER_OPTION_FLIP_VERTEX_Y: spvc_compiler_option =
    4 | SPVC_COMPILER_OPTION_COMMON_BIT;
pub const SPVC_COMPILER_OPTION_GLSL_VERSION: spvc_compiler_option =
    8 | SPVC_COMPILER_OPTION_GLSL_BIT;
pub const SPVC_COMPILER_OPTION_GLSL_ES: spvc_compiler_option = 9 | SPVC_COMPILER_OPTION_GLSL_BIT;
pub const SPVC_COMPILER_OPTION_GLSL_VULKAN_SEMANTICS: spvc_compiler_option =
    10 | SPVC_COMPILER_OPTION_GLSL_BIT;
pub const SPVC_COMPILER_OPTION_MSL_VERSION: spvc_compiler_option =
    17 | SPVC_COMPILER_OPTION_MSL_BIT;
pub const SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING: spvc_compiler_option =
    53 | SPVC_COMPILER_OPTION_MSL_BIT;

/// `spvc_error_callback`, as a Rust function pointer.
pub type spvc_error_callback = fn(userdata: *mut c_void, error: &str);

struct ContextState {
    callback: Cell<Option<(spvc_error_callback, *mut c_void)>>,
    last_error: RefCell<String>,
}

/// `spvc_context`
#[derive(Clone)]
pub struct spvc_context(Rc<ContextState>);

impl spvc_context {
    fn report(&self, msg: &str) {
        *self.0.last_error.borrow_mut() = msg.to_owned();
        if let Some((cb, userdata)) = self.0.callback.get() {
            cb(userdata, msg);
        }
    }
}

/// `spvc_parsed_ir`
pub struct spvc_parsed_ir {
    module: naga::Module,
    info: naga::valid::ModuleInfo,
}

/// `spvc_compiler_options`
#[derive(Debug, Clone)]
pub struct spvc_compiler_options {
    backend: spvc_backend,
    values: BTreeMap<spvc_compiler_option, u32>,
}

impl spvc_compiler_options {
    fn get(&self, option: spvc_compiler_option) -> Option<u32> {
        self.values.get(&option).copied()
    }
}

/// `spvc_compiler`
pub struct spvc_compiler {
    ctx: spvc_context,
    backend: spvc_backend,
    module: naga::Module,
    info: naga::valid::ModuleInfo,
    options: spvc_compiler_options,
}

/// `spvc_reflected_resource`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct spvc_reflected_resource {
    pub id: spvc_variable_id,
    pub base_type_id: spvc_type_id,
    pub type_id: spvc_type_id,
    pub name: String,
}

/// `spvc_resources`
#[derive(Debug, Clone, Default)]
pub struct spvc_resources {
    lists: BTreeMap<u32, Vec<spvc_reflected_resource>>,
}

/// `spvc_type`
#[derive(Clone, Copy)]
pub struct spvc_type<'a> {
    ty: &'a naga::Type,
}

pub fn spvc_context_create() -> Result<spvc_context, spvc_result> {
    Ok(spvc_context(Rc::new(ContextState {
        callback: Cell::new(None),
        last_error: RefCell::new(String::new()),
    })))
}

pub fn spvc_context_destroy(context: spvc_context) {
    drop(context);
}

pub fn spvc_context_set_error_callback(
    context: &spvc_context,
    cb: spvc_error_callback,
    userdata: *mut c_void,
) {
    context.0.callback.set(Some((cb, userdata)));
}

pub fn spvc_context_get_last_error_string(context: &spvc_context) -> String {
    context.0.last_error.borrow().clone()
}

fn validate(
    module: &naga::Module,
) -> Result<naga::valid::ModuleInfo, naga::WithSpan<naga::valid::ValidationError>> {
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
}

pub fn spvc_context_parse_spirv(
    context: &spvc_context,
    spirv: &[SpvId],
) -> Result<spvc_parsed_ir, spvc_result> {
    let options = naga::front::spv::Options {
        adjust_coordinate_space: false,
        strict_capabilities: false,
        block_ctx_dump_prefix: None,
    };
    let module = naga::front::spv::Frontend::new(spirv.iter().copied(), &options)
        .parse()
        .map_err(|err| {
            context.report(&format!("SPIR-V parse error: {err}"));
            SPVC_ERROR_INVALID_SPIRV
        })?;
    let info = validate(&module).map_err(|err| {
        context.report(&format!("SPIR-V validation error: {err}"));
        SPVC_ERROR_INVALID_SPIRV
    })?;
    Ok(spvc_parsed_ir { module, info })
}

pub fn spvc_context_create_compiler(
    context: &spvc_context,
    backend: spvc_backend,
    parsed_ir: spvc_parsed_ir,
    mode: spvc_capture_mode,
) -> Result<spvc_compiler, spvc_result> {
    // Both capture modes are the same here: the IR is moved in.
    let _ = mode;
    let mut values = BTreeMap::new();
    if backend == SPVC_BACKEND_GLSL {
        // SPIRV-Cross's default GLSL version.
        values.insert(SPVC_COMPILER_OPTION_GLSL_VERSION, 450);
    }
    if backend == SPVC_BACKEND_MSL {
        // SPIRV-Cross's default MSL version (1.2), encoded as
        // major * 10000 + minor * 100 + patch.
        values.insert(SPVC_COMPILER_OPTION_MSL_VERSION, 10200);
    }
    Ok(spvc_compiler {
        ctx: context.clone(),
        backend,
        module: parsed_ir.module,
        info: parsed_ir.info,
        options: spvc_compiler_options { backend, values },
    })
}

pub fn spvc_compiler_create_compiler_options(
    compiler: &spvc_compiler,
) -> Result<spvc_compiler_options, spvc_result> {
    Ok(compiler.options.clone())
}

fn option_allowed(backend: spvc_backend, option: spvc_compiler_option) -> bool {
    let lang = option & SPVC_COMPILER_OPTION_LANG_BITS;
    if lang & SPVC_COMPILER_OPTION_COMMON_BIT != 0 {
        return true;
    }
    match backend {
        SPVC_BACKEND_GLSL => lang & SPVC_COMPILER_OPTION_GLSL_BIT != 0,
        SPVC_BACKEND_HLSL => lang & SPVC_COMPILER_OPTION_HLSL_BIT != 0,
        SPVC_BACKEND_MSL => lang & SPVC_COMPILER_OPTION_MSL_BIT != 0,
        _ => false,
    }
}

pub fn spvc_compiler_options_set_bool(
    options: &mut spvc_compiler_options,
    option: spvc_compiler_option,
    value: spvc_bool,
) -> spvc_result {
    spvc_compiler_options_set_uint(options, option, value as u32)
}

pub fn spvc_compiler_options_set_uint(
    options: &mut spvc_compiler_options,
    option: spvc_compiler_option,
    value: u32,
) -> spvc_result {
    if !option_allowed(options.backend, option) {
        return SPVC_ERROR_INVALID_ARGUMENT;
    }
    options.values.insert(option, value);
    SPVC_SUCCESS
}

pub fn spvc_compiler_install_compiler_options(
    compiler: &mut spvc_compiler,
    options: spvc_compiler_options,
) -> spvc_result {
    if options.backend != compiler.backend {
        return SPVC_ERROR_INVALID_ARGUMENT;
    }
    compiler.options = options;
    SPVC_SUCCESS
}

/// Compile the IR to the compiler's backend language.
pub fn spvc_compiler_compile(compiler: &mut spvc_compiler) -> Result<String, spvc_result> {
    let result = match compiler.backend {
        SPVC_BACKEND_GLSL => compile_glsl(compiler),
        SPVC_BACKEND_MSL => compile_msl(compiler),
        backend => Err(format!("backend {backend:?} is not supported")),
    };
    result.map_err(|msg| {
        compiler.ctx.report(&msg);
        SPVC_ERROR_UNSUPPORTED_SPIRV
    })
}

fn compile_glsl(compiler: &spvc_compiler) -> Result<String, String> {
    use naga::back::glsl;

    let version_num = compiler
        .options
        .get(SPVC_COMPILER_OPTION_GLSL_VERSION)
        .unwrap_or(450);
    let es = compiler
        .options
        .get(SPVC_COMPILER_OPTION_GLSL_ES)
        .unwrap_or(0)
        != 0;
    let version = if es {
        glsl::Version::Embedded {
            version: version_num as u16,
            is_webgl: false,
        }
    } else {
        glsl::Version::Desktop(version_num as u16)
    };

    let mut writer_flags = glsl::WriterFlags::empty();
    if compiler
        .options
        .get(SPVC_COMPILER_OPTION_FLIP_VERTEX_Y)
        .unwrap_or(0)
        != 0
    {
        writer_flags |= glsl::WriterFlags::ADJUST_COORDINATE_SPACE;
    }

    // Keep the SPIR-V binding decorations, like SPIRV-Cross does.
    let mut binding_map = glsl::BindingMap::default();
    for (_, global) in compiler.module.global_variables.iter() {
        if let Some(br) = &global.binding {
            if let Ok(binding) = u8::try_from(br.binding) {
                binding_map.insert(br.clone(), binding);
            }
        }
    }

    let options = glsl::Options {
        version,
        writer_flags,
        binding_map,
        zero_initialize_workgroup_memory: true,
    };

    let ep = compiler
        .module
        .entry_points
        .first()
        .ok_or_else(|| "no entry point".to_owned())?;
    let pipeline_options = glsl::PipelineOptions {
        shader_stage: ep.stage,
        entry_point: ep.name.clone(),
        multiview: None,
    };

    let mut out = String::new();
    let mut writer = glsl::Writer::new(
        &mut out,
        &compiler.module,
        &compiler.info,
        &options,
        &pipeline_options,
        naga::proc::BoundsCheckPolicies::default(),
    )
    .map_err(|e| format!("GLSL error: {e}"))?;
    writer.write().map_err(|e| format!("GLSL error: {e}"))?;
    Ok(out)
}

fn compile_msl(compiler: &spvc_compiler) -> Result<String, String> {
    use naga::back::msl;

    // SPIRV-Cross renames `main` (reserved in Metal) to `main0`.
    let mut module = compiler.module.clone();
    for ep in module.entry_points.iter_mut() {
        if ep.name == "main" {
            ep.name = "main0".to_owned();
        }
    }
    let info = validate(&module).map_err(|e| format!("MSL error: {e}"))?;

    let ep = module
        .entry_points
        .first()
        .ok_or_else(|| "no entry point".to_owned())?;

    let decoration_binding = compiler
        .options
        .get(SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING)
        .unwrap_or(0)
        != 0;

    // Resource slots: either the binding decorations or, like SPIRV-Cross
    // without decoration binding, sequential indices per resource kind.
    let mut resources = msl::BindingMap::default();
    let (mut next_buffer, mut next_texture, mut next_sampler) = (0u8, 0u8, 0u8);
    for (_, global) in module.global_variables.iter() {
        let Some(br) = &global.binding else { continue };
        let decorated = u8::try_from(br.binding).unwrap_or(u8::MAX);
        let mut target = msl::BindTarget::default();
        match module.types[global.ty].inner {
            naga::TypeInner::Image { .. } => {
                target.texture = Some(if decoration_binding {
                    decorated
                } else {
                    next_texture += 1;
                    next_texture - 1
                });
            }
            naga::TypeInner::Sampler { .. } => {
                target.sampler = Some(msl::BindSamplerTarget::Resource(if decoration_binding {
                    decorated
                } else {
                    next_sampler += 1;
                    next_sampler - 1
                }));
            }
            _ => {
                target.buffer = Some(if decoration_binding {
                    decorated
                } else {
                    next_buffer += 1;
                    next_buffer - 1
                });
                target.mutable = matches!(
                    global.space,
                    naga::AddressSpace::Storage { access } if access.contains(naga::StorageAccess::STORE)
                );
            }
        }
        resources.insert(br.clone(), target);
    }
    let mut per_entry_point_map = msl::EntryPointResourceMap::default();
    per_entry_point_map.insert(
        ep.name.clone(),
        msl::EntryPointResources {
            resources,
            ..Default::default()
        },
    );

    let version = compiler
        .options
        .get(SPVC_COMPILER_OPTION_MSL_VERSION)
        .unwrap_or(10200);
    let options = msl::Options {
        lang_version: ((version / 10000) as u8, ((version / 100) % 100) as u8),
        per_entry_point_map,
        fake_missing_bindings: false,
        ..Default::default()
    };
    let pipeline_options = msl::PipelineOptions {
        entry_point: Some((ep.stage, ep.name.clone())),
        ..Default::default()
    };

    let (out, translation) = msl::write_string(&module, &info, &options, &pipeline_options)
        .map_err(|e| format!("MSL error: {e}"))?;

    // naga's namer appends `_` to identifiers ending in a digit, so the
    // entry point comes out as `main0_`; rename it to the requested name.
    let mut out = out;
    if let Some(Ok(written)) = translation.entry_point_names.first() {
        if *written != ep.name && !contains_word(&out, &ep.name) {
            out = replace_word(&out, written, &ep.name);
        }
    }
    Ok(out)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Find whole-identifier occurrences of `word` in `s`.
fn word_positions<'a>(s: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = s.as_bytes();
    s.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        let end = i + word.len();
        (i == 0 || !is_ident_byte(bytes[i - 1]))
            && (end == bytes.len() || !is_ident_byte(bytes[end]))
    })
}

fn contains_word(s: &str, word: &str) -> bool {
    word_positions(s, word).next().is_some()
}

fn replace_word(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for i in word_positions(s, from).collect::<Vec<_>>() {
        out.push_str(&s[last..i]);
        out.push_str(to);
        last = i + from.len();
    }
    out.push_str(&s[last..]);
    out
}

/// The type of a uniform block as the user declared it: naga's SPIR-V
/// writer wraps block structs in a struct with a single unnamed member.
fn unwrap_block_type(
    module: &naga::Module,
    ty: naga::Handle<naga::Type>,
) -> naga::Handle<naga::Type> {
    if let naga::TypeInner::Struct { members, .. } = &module.types[ty].inner {
        if let [member] = members.as_slice() {
            if member.name.is_none()
                && matches!(
                    module.types[member.ty].inner,
                    naga::TypeInner::Struct { .. }
                )
            {
                return member.ty;
            }
        }
    }
    ty
}

pub fn spvc_compiler_create_shader_resources(
    compiler: &spvc_compiler,
) -> Result<spvc_resources, spvc_result> {
    let module = &compiler.module;
    let mut resources = spvc_resources::default();
    for (handle, global) in module.global_variables.iter() {
        let inner = &module.types[global.ty].inner;
        let ty = match (global.space, inner) {
            (naga::AddressSpace::Uniform, _) => SPVC_RESOURCE_TYPE_UNIFORM_BUFFER,
            (naga::AddressSpace::Storage { .. }, _) => SPVC_RESOURCE_TYPE_STORAGE_BUFFER,
            (naga::AddressSpace::Immediate, _) => SPVC_RESOURCE_TYPE_PUSH_CONSTANT,
            (_, naga::TypeInner::Image { class, .. }) => match class {
                naga::ImageClass::Storage { .. } => SPVC_RESOURCE_TYPE_STORAGE_IMAGE,
                _ => SPVC_RESOURCE_TYPE_SEPARATE_IMAGE,
            },
            (_, naga::TypeInner::Sampler { .. }) => SPVC_RESOURCE_TYPE_SEPARATE_SAMPLERS,
            _ => continue,
        };
        let base = if ty == SPVC_RESOURCE_TYPE_UNIFORM_BUFFER {
            unwrap_block_type(module, global.ty)
        } else {
            global.ty
        };
        resources
            .lists
            .entry(ty as u32)
            .or_default()
            .push(spvc_reflected_resource {
                id: handle.index() as u32,
                base_type_id: base.index() as u32,
                type_id: global.ty.index() as u32,
                name: global.name.clone().unwrap_or_default(),
            });
    }
    Ok(resources)
}

pub fn spvc_resources_get_resource_list_for_type(
    resources: &spvc_resources,
    ty: spvc_resource_type,
) -> Result<&[spvc_reflected_resource], spvc_result> {
    Ok(resources
        .lists
        .get(&(ty as u32))
        .map_or(&[][..], |v| v.as_slice()))
}

fn type_handle(compiler: &spvc_compiler, id: spvc_type_id) -> Option<naga::Handle<naga::Type>> {
    compiler
        .module
        .types
        .iter()
        .find(|(h, _)| h.index() as u32 == id)
        .map(|(h, _)| h)
}

pub fn spvc_compiler_get_type_handle(
    compiler: &spvc_compiler,
    id: spvc_type_id,
) -> Option<spvc_type<'_>> {
    let handle = type_handle(compiler, id)?;
    Some(spvc_type {
        ty: &compiler.module.types[handle],
    })
}

pub fn spvc_type_get_num_member_types(ty: spvc_type<'_>) -> u32 {
    match &ty.ty.inner {
        naga::TypeInner::Struct { members, .. } => members.len() as u32,
        _ => 0,
    }
}

pub fn spvc_compiler_get_member_name(
    compiler: &spvc_compiler,
    id: spvc_type_id,
    index: u32,
) -> &str {
    let Some(handle) = type_handle(compiler, id) else {
        return "";
    };
    match &compiler.module.types[handle].inner {
        naga::TypeInner::Struct { members, .. } => members
            .get(index as usize)
            .and_then(|m| m.name.as_deref())
            .unwrap_or(""),
        _ => "",
    }
}

pub fn spvc_compiler_type_struct_member_offset(
    compiler: &spvc_compiler,
    ty: spvc_type<'_>,
    index: u32,
) -> Result<u32, spvc_result> {
    match &ty.ty.inner {
        naga::TypeInner::Struct { members, .. } => members
            .get(index as usize)
            .map(|m| m.offset)
            .ok_or(SPVC_ERROR_INVALID_ARGUMENT),
        _ => {
            compiler.ctx.report("type is not a struct");
            Err(SPVC_ERROR_INVALID_ARGUMENT)
        }
    }
}

pub fn spvc_compiler_get_name(compiler: &spvc_compiler, id: spvc_variable_id) -> &str {
    compiler
        .module
        .global_variables
        .iter()
        .find(|(h, _)| h.index() as u32 == id)
        .and_then(|(_, g)| g.name.as_deref())
        .unwrap_or("")
}
