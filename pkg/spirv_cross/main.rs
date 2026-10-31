//! Port of `src/spirv-cross/main.zig`.
//!
//! STATUS: ported
//!
//! Deviation: the Zig package links SPIRV-Cross (C++) and exposes its C API
//! as `c`. The Rust `c` module implements the subset of that API Ghostty
//! uses on top of `naga` (see `c.rs` for the differences).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "c.rs"]
pub mod c;
// @generated-mods-end

#[cfg(test)]
mod tests {
    use super::c;
    use crate::pkg::glslang;

    /// Compile a GLSL fragment shader to SPIR-V with the glslang wrapper.
    fn spirv(src: &str) -> Vec<u32> {
        let gc = &glslang::c::glslang_input_t {
            language: glslang::c::GLSLANG_SOURCE_GLSL,
            stage: glslang::c::GLSLANG_STAGE_FRAGMENT,
            client: glslang::c::GLSLANG_CLIENT_VULKAN,
            client_version: glslang::c::GLSLANG_TARGET_VULKAN_1_2,
            target_language: glslang::c::GLSLANG_TARGET_SPV,
            target_language_version: glslang::c::GLSLANG_TARGET_SPV_1_5,
            code: src,
            default_version: 100,
            default_profile: glslang::c::GLSLANG_NO_PROFILE,
            force_default_version_and_profile: 0,
            forward_compatible: 0,
            messages: glslang::c::GLSLANG_MSG_DEFAULT_BIT,
            resource: glslang::c::glslang_default_resource(),
        };
        glslang::testing::ensure_init().unwrap();
        let mut shader = glslang::Shader::create(gc).unwrap();
        shader.preprocess(gc).unwrap();
        shader.parse(gc).unwrap();
        let mut program = glslang::Program::create().unwrap();
        program.add_shader(&shader);
        program
            .link(glslang::c::GLSLANG_MSG_SPV_RULES_BIT | glslang::c::GLSLANG_MSG_VULKAN_RULES_BIT)
            .unwrap();
        program.spirv_generate(glslang::c::GLSLANG_STAGE_FRAGMENT);
        program.spirv_get_ptr().unwrap().to_vec()
    }

    fn compile(backend: c::spvc_backend, spv: &[u32], opts: &[(u32, u32)]) -> String {
        let ctx = c::spvc_context_create().unwrap();
        c::spvc_context_set_error_callback(
            &ctx,
            |_, msg| panic!("spirv-cross error message={msg}"),
            std::ptr::null_mut(),
        );
        let ir = c::spvc_context_parse_spirv(&ctx, spv).unwrap();
        let mut compiler =
            c::spvc_context_create_compiler(&ctx, backend, ir, c::SPVC_CAPTURE_MODE_TAKE_OWNERSHIP)
                .unwrap();
        let mut options = c::spvc_compiler_create_compiler_options(&compiler).unwrap();
        for &(opt, value) in opts {
            assert_eq!(
                c::SPVC_SUCCESS,
                c::spvc_compiler_options_set_uint(&mut options, opt, value)
            );
        }
        assert_eq!(
            c::SPVC_SUCCESS,
            c::spvc_compiler_install_compiler_options(&mut compiler, options)
        );
        let out = c::spvc_compiler_compile(&mut compiler).unwrap();
        c::spvc_context_destroy(ctx);
        out
    }

    /// A shadertoy-style shader that samples a texture.
    const SAMPLING: &str = "#version 430 core\n\
        layout(binding = 1, std140) uniform Globals {\n\
        \x20   uniform vec3 iResolution;\n\
        };\n\
        layout(binding = 0) uniform sampler2D iChannel0;\n\
        layout(location = 0) in vec4 gl_FragCoord;\n\
        layout(location = 0) out vec4 _fragColor;\n\
        void mainImage(out vec4 fragColor, in vec2 fragCoord);\n\
        void main() { mainImage(_fragColor, gl_FragCoord.xy); }\n\
        void mainImage(out vec4 fragColor, in vec2 fragCoord) {\n\
        \x20   fragColor = texture(iChannel0, fragCoord / iResolution.xy);\n\
        }\n";

    #[test]
    fn glsl_output_keeps_bindings() {
        let spv = spirv(SAMPLING);
        let glsl = compile(
            c::SPVC_BACKEND_GLSL,
            &spv,
            &[(c::SPVC_COMPILER_OPTION_GLSL_VERSION, 430)],
        );
        assert!(glsl.starts_with("#version 430 core"), "{glsl}");
        assert!(glsl.contains("binding = 1) uniform"), "{glsl}");
        assert!(
            glsl.contains("layout(binding = 0) uniform sampler2D"),
            "{glsl}"
        );
        assert!(glsl.contains("void main()"), "{glsl}");
    }

    #[test]
    fn msl_output() {
        let spv = spirv(SAMPLING);
        let msl = compile(
            c::SPVC_BACKEND_MSL,
            &spv,
            &[(c::SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING, 1)],
        );
        assert!(msl.contains("fragment main0_Output main0("), "{msl}");
        assert!(msl.contains("[[buffer(1)]]"), "{msl}");
        assert!(msl.contains("[[texture(0)]]"), "{msl}");
        assert!(msl.contains("[[sampler(0)]]"), "{msl}");
    }

    /// Ghostty's shadertoy prefix plus its test shaders, converted the same
    /// way `renderer/shadertoy.zig` does (GLSL 430 for OpenGL, MSL with
    /// decoration bindings for Metal).
    #[test]
    fn shadertoy_test_shaders() {
        let prefix = include_str!("../../src/renderer/shaders/shadertoy_prefix.glsl");
        for user in [
            include_str!("../../src/renderer/shaders/test_shadertoy_crt.glsl"),
            include_str!("../../src/renderer/shaders/test_shadertoy_focus.glsl"),
        ] {
            let src = format!("{prefix}\n\n{user}");
            let spv = spirv(&src);
            let glsl = compile(
                c::SPVC_BACKEND_GLSL,
                &spv,
                &[(c::SPVC_COMPILER_OPTION_GLSL_VERSION, 430)],
            );
            assert!(glsl.contains("binding = 1) uniform"), "{glsl}");
            let msl = compile(
                c::SPVC_BACKEND_MSL,
                &spv,
                &[(c::SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING, 1)],
            );
            assert!(msl.contains(" main0("), "{msl}");
        }
    }

    #[test]
    fn option_for_wrong_backend() {
        let spv = spirv(include_str!("../glslang/test/simple.frag"));
        let ctx = c::spvc_context_create().unwrap();
        let ir = c::spvc_context_parse_spirv(&ctx, &spv).unwrap();
        let compiler = c::spvc_context_create_compiler(
            &ctx,
            c::SPVC_BACKEND_GLSL,
            ir,
            c::SPVC_CAPTURE_MODE_COPY,
        )
        .unwrap();
        let mut options = c::spvc_compiler_create_compiler_options(&compiler).unwrap();
        assert_eq!(
            c::SPVC_ERROR_INVALID_ARGUMENT,
            c::spvc_compiler_options_set_bool(
                &mut options,
                c::SPVC_COMPILER_OPTION_MSL_ENABLE_DECORATION_BINDING,
                c::SPVC_TRUE
            )
        );
    }

    #[test]
    fn invalid_spirv() {
        let ctx = c::spvc_context_create().unwrap();
        assert!(c::spvc_context_parse_spirv(&ctx, &[0, 1, 2, 3]).is_err());
        assert!(!c::spvc_context_get_last_error_string(&ctx).is_empty());
    }

    #[test]
    fn uniform_block_reflection() {
        let spv = spirv(include_str!("../glslang/test/simple.frag"));
        let ctx = c::spvc_context_create().unwrap();
        let ir = c::spvc_context_parse_spirv(&ctx, &spv).unwrap();
        let compiler = c::spvc_context_create_compiler(
            &ctx,
            c::SPVC_BACKEND_NONE,
            ir,
            c::SPVC_CAPTURE_MODE_TAKE_OWNERSHIP,
        )
        .unwrap();
        let resources = c::spvc_compiler_create_shader_resources(&compiler).unwrap();
        let blocks = c::spvc_resources_get_resource_list_for_type(
            &resources,
            c::SPVC_RESOURCE_TYPE_UNIFORM_BUFFER,
        )
        .unwrap();
        assert_eq!(1, blocks.len());
        let id = blocks[0].base_type_id;
        let typ = c::spvc_compiler_get_type_handle(&compiler, id).unwrap();

        // std140 offsets of the `Globals` block in simple.frag.
        let expected = [
            ("iResolution", 0),
            ("iTime", 12),
            ("iTimeDelta", 16),
            ("iFrameRate", 20),
            ("iFrame", 24),
            ("iChannelTime", 32),
            ("iChannelResolution", 96),
            ("iMouse", 160),
            ("iDate", 176),
            ("iSampleRate", 192),
        ];
        assert_eq!(
            expected.len() as u32,
            c::spvc_type_get_num_member_types(typ)
        );
        for (field, offset) in expected {
            let mut found = false;
            for i in 0..c::spvc_type_get_num_member_types(typ) {
                let name = c::spvc_compiler_get_member_name(&compiler, id, i);
                if name != field {
                    continue;
                }
                assert_eq!(
                    Ok(offset),
                    c::spvc_compiler_type_struct_member_offset(&compiler, typ, i),
                    "{field}"
                );
                found = true;
                break;
            }
            assert!(found, "{field}");
        }
    }
}
