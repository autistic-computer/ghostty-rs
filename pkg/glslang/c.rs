//! Port of `src/glslang/c.zig`.
//!
//! STATUS: ported
//!
//! The Zig package imports glslang's C API (`glslang_c_interface.h`) with
//! translate-c. The Rust port doesn't link glslang (C++); it implements the
//! same API in Rust on top of `naga`'s GLSL frontend and SPIR-V backend (see
//! `shader.rs`/`program.rs`). This module holds the C API's types and
//! constants, with the same names and values as the C header, so code
//! written against the C API reads the same.

#![allow(non_camel_case_types)]

use std::ffi::c_int;

/// `glslang_source_t`
pub type glslang_source_t = u32;
pub const GLSLANG_SOURCE_NONE: glslang_source_t = 0;
pub const GLSLANG_SOURCE_GLSL: glslang_source_t = 1;
pub const GLSLANG_SOURCE_HLSL: glslang_source_t = 2;

/// `glslang_stage_t`
pub type glslang_stage_t = u32;
pub const GLSLANG_STAGE_VERTEX: glslang_stage_t = 0;
pub const GLSLANG_STAGE_TESSCONTROL: glslang_stage_t = 1;
pub const GLSLANG_STAGE_TESSEVALUATION: glslang_stage_t = 2;
pub const GLSLANG_STAGE_GEOMETRY: glslang_stage_t = 3;
pub const GLSLANG_STAGE_FRAGMENT: glslang_stage_t = 4;
pub const GLSLANG_STAGE_COMPUTE: glslang_stage_t = 5;

/// `glslang_client_t`
pub type glslang_client_t = u32;
pub const GLSLANG_CLIENT_NONE: glslang_client_t = 0;
pub const GLSLANG_CLIENT_VULKAN: glslang_client_t = 1;
pub const GLSLANG_CLIENT_OPENGL: glslang_client_t = 2;

/// `glslang_target_client_version_t`
pub type glslang_target_client_version_t = u32;
pub const GLSLANG_TARGET_VULKAN_1_0: glslang_target_client_version_t = 1 << 22;
pub const GLSLANG_TARGET_VULKAN_1_1: glslang_target_client_version_t = (1 << 22) | (1 << 12);
pub const GLSLANG_TARGET_VULKAN_1_2: glslang_target_client_version_t = (1 << 22) | (2 << 12);
pub const GLSLANG_TARGET_VULKAN_1_3: glslang_target_client_version_t = (1 << 22) | (3 << 12);
pub const GLSLANG_TARGET_OPENGL_450: glslang_target_client_version_t = 450;

/// `glslang_target_language_t`
pub type glslang_target_language_t = u32;
pub const GLSLANG_TARGET_NONE: glslang_target_language_t = 0;
pub const GLSLANG_TARGET_SPV: glslang_target_language_t = 1;

/// `glslang_target_language_version_t`
pub type glslang_target_language_version_t = u32;
pub const GLSLANG_TARGET_SPV_1_0: glslang_target_language_version_t = 1 << 16;
pub const GLSLANG_TARGET_SPV_1_1: glslang_target_language_version_t = (1 << 16) | (1 << 8);
pub const GLSLANG_TARGET_SPV_1_2: glslang_target_language_version_t = (1 << 16) | (2 << 8);
pub const GLSLANG_TARGET_SPV_1_3: glslang_target_language_version_t = (1 << 16) | (3 << 8);
pub const GLSLANG_TARGET_SPV_1_4: glslang_target_language_version_t = (1 << 16) | (4 << 8);
pub const GLSLANG_TARGET_SPV_1_5: glslang_target_language_version_t = (1 << 16) | (5 << 8);
pub const GLSLANG_TARGET_SPV_1_6: glslang_target_language_version_t = (1 << 16) | (6 << 8);

/// `glslang_profile_t`
pub type glslang_profile_t = u32;
pub const GLSLANG_BAD_PROFILE: glslang_profile_t = 0;
pub const GLSLANG_NO_PROFILE: glslang_profile_t = 1 << 0;
pub const GLSLANG_CORE_PROFILE: glslang_profile_t = 1 << 1;
pub const GLSLANG_COMPATIBILITY_PROFILE: glslang_profile_t = 1 << 2;
pub const GLSLANG_ES_PROFILE: glslang_profile_t = 1 << 3;

/// `glslang_messages_t`
pub type glslang_messages_t = c_int;
pub const GLSLANG_MSG_DEFAULT_BIT: glslang_messages_t = 0;
pub const GLSLANG_MSG_RELAXED_ERRORS_BIT: glslang_messages_t = 1 << 0;
pub const GLSLANG_MSG_SUPPRESS_WARNINGS_BIT: glslang_messages_t = 1 << 1;
pub const GLSLANG_MSG_AST_BIT: glslang_messages_t = 1 << 2;
pub const GLSLANG_MSG_SPV_RULES_BIT: glslang_messages_t = 1 << 3;
pub const GLSLANG_MSG_VULKAN_RULES_BIT: glslang_messages_t = 1 << 4;
pub const GLSLANG_MSG_ONLY_PREPROCESSOR_BIT: glslang_messages_t = 1 << 5;
pub const GLSLANG_MSG_READ_HLSL_BIT: glslang_messages_t = 1 << 6;
pub const GLSLANG_MSG_CASCADING_ERRORS_BIT: glslang_messages_t = 1 << 7;
pub const GLSLANG_MSG_KEEP_UNCALLED_BIT: glslang_messages_t = 1 << 8;
pub const GLSLANG_MSG_HLSL_OFFSETS_BIT: glslang_messages_t = 1 << 9;
pub const GLSLANG_MSG_DEBUG_INFO_BIT: glslang_messages_t = 1 << 10;

/// `glslang_resource_t`: the built-in resource limits.
///
/// naga doesn't take configurable resource limits, so this carries no data;
/// it exists so that inputs are built the same way as with the C API.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct glslang_resource_t {
    _private: (),
}

static DEFAULT_RESOURCE: glslang_resource_t = glslang_resource_t { _private: () };

/// `glslang_default_resource()`
pub fn glslang_default_resource() -> &'static glslang_resource_t {
    &DEFAULT_RESOURCE
}

/// `glslang_input_t`
#[derive(Debug, Clone, Copy)]
pub struct glslang_input_t<'a> {
    pub language: glslang_source_t,
    pub stage: glslang_stage_t,
    pub client: glslang_client_t,
    pub client_version: glslang_target_client_version_t,
    pub target_language: glslang_target_language_t,
    pub target_language_version: glslang_target_language_version_t,
    /// Shader source code (`const char*` in C).
    pub code: &'a str,
    pub default_version: c_int,
    pub default_profile: glslang_profile_t,
    pub force_default_version_and_profile: c_int,
    pub forward_compatible: c_int,
    pub messages: glslang_messages_t,
    pub resource: &'a glslang_resource_t,
}
