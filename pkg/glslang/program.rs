//! Port of `src/glslang/program.zig`.
//!
//! STATUS: ported
//!
//! `Program` mirrors glslang's `glslang_program_t`: add parsed shaders,
//! [`Program::link`], then generate SPIR-V. Linking validates the naga module
//! (after the fixups glslang's linker would do: ordering functions so callees
//! come before callers, and rejecting calls to functions that were declared
//! but never defined) and SPIR-V is written by naga's SPIR-V backend.

use std::ffi::c_int;

use super::c;
use super::shader::{naga_stage, Shader};
use super::Error;

struct Linked {
    module: naga::Module,
    info: naga::valid::ModuleInfo,
}

pub struct Program {
    shaders: Vec<(
        c::glslang_stage_t,
        c::glslang_target_language_version_t,
        Option<naga::Module>,
    )>,
    linked: Option<Linked>,
    spirv: Vec<u32>,
    spirv_messages: String,
    info_log: String,
    debug_log: String,
}

impl Program {
    pub fn create() -> Result<Box<Program>, Error> {
        Ok(Box::new(Program {
            shaders: Vec::new(),
            linked: None,
            spirv: Vec::new(),
            spirv_messages: String::new(),
            info_log: String::new(),
            debug_log: String::new(),
        }))
    }

    /// Destroy the program (`glslang_program_delete`).
    pub fn delete(self: Box<Self>) {}

    pub fn add_shader(&mut self, shader: &Shader) {
        self.shaders.push((
            shader.stage(),
            shader.target_language_version(),
            shader.module().cloned(),
        ));
    }

    pub fn link(&mut self, messages: c_int) -> Result<(), Error> {
        let _ = messages;
        self.info_log.clear();
        self.debug_log.clear();
        self.linked = None;

        let mut module = match self.shaders.as_slice() {
            [(_, _, Some(module))] => module.clone(),
            [(_, _, None)] => {
                self.info_log
                    .push_str("ERROR: Linking: shader was not successfully parsed\n");
                return Err(Error::GlslangFailed);
            }
            [] => {
                self.info_log.push_str("ERROR: Linking: no shaders\n");
                return Err(Error::GlslangFailed);
            }
            _ => {
                self.info_log.push_str(
                    "ERROR: Linking: multiple compilation units per program are not supported\n",
                );
                return Err(Error::GlslangFailed);
            }
        };
        let stage_name = stage_name(self.shaders[0].0);

        // Undefined functions (prototypes without a body) are link errors
        // when they are called.
        let undefined = order_functions(&mut module);
        if !undefined.is_empty() {
            for name in &undefined {
                self.info_log.push_str(&format!(
                    "ERROR: Linking {stage_name} stage: No function definition (body) found: \n    {name}\n"
                ));
            }
            return Err(Error::GlslangFailed);
        }

        if !module.entry_points.iter().any(|ep| ep.name == "main") {
            self.info_log.push_str(&format!(
                "ERROR: Linking {stage_name} stage: Missing entry point: Each stage requires one entry point\n"
            ));
            return Err(Error::GlslangFailed);
        }

        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        match validator.validate(&module) {
            Ok(info) => {
                self.linked = Some(Linked { module, info });
                Ok(())
            }
            Err(err) => {
                self.info_log.push_str(&format!(
                    "ERROR: Linking {stage_name} stage: {}\n",
                    error_chain(&err)
                ));
                self.debug_log.push_str(&format!("{err:?}\n"));
                Err(Error::GlslangFailed)
            }
        }
    }

    pub fn spirv_generate(&mut self, stage: c::glslang_stage_t) {
        self.spirv.clear();
        self.spirv_messages.clear();

        let Some(linked) = &self.linked else {
            self.spirv_messages.push_str("program is not linked\n");
            return;
        };

        let version = self
            .shaders
            .iter()
            .find(|s| s.0 == stage)
            .map_or(c::GLSLANG_TARGET_SPV_1_0, |s| s.1);
        let options = naga::back::spv::Options {
            lang_version: (
                ((version >> 16) & 0xFF) as u8,
                ((version >> 8) & 0xFF) as u8,
            ),
            // Keep debug names (like glslang does) so that reflection on the
            // SPIR-V sees variable and member names.
            flags: naga::back::spv::WriterFlags::DEBUG
                | naga::back::spv::WriterFlags::LABEL_VARYINGS,
            ..Default::default()
        };
        let pipeline = naga_stage(stage).map(|shader_stage| naga::back::spv::PipelineOptions {
            shader_stage,
            entry_point: "main".to_owned(),
        });

        match naga::back::spv::write_vec(&linked.module, &linked.info, &options, pipeline.as_ref())
        {
            Ok(words) => self.spirv = words,
            Err(err) => self.spirv_messages.push_str(&format!("error: {err}\n")),
        }
    }

    /// The size of the generated SPIR-V, in 32-bit words.
    pub fn spirv_get_size(&self) -> usize {
        self.spirv.len()
    }

    pub fn spirv_get(&self, buf: &mut [u32]) {
        buf[..self.spirv.len()].copy_from_slice(&self.spirv);
    }

    pub fn spirv_get_ptr(&self) -> Result<&[u32], Error> {
        Ok(&self.spirv)
    }

    pub fn spirv_get_messages(&self) -> Result<&str, Error> {
        Ok(&self.spirv_messages)
    }

    pub fn get_info_log(&self) -> Result<&str, Error> {
        Ok(&self.info_log)
    }

    pub fn get_debug_info_log(&self) -> Result<&str, Error> {
        Ok(&self.debug_log)
    }
}

fn stage_name(stage: c::glslang_stage_t) -> &'static str {
    match stage {
        c::GLSLANG_STAGE_VERTEX => "vertex",
        c::GLSLANG_STAGE_TESSCONTROL => "tessellation control",
        c::GLSLANG_STAGE_TESSEVALUATION => "tessellation evaluation",
        c::GLSLANG_STAGE_GEOMETRY => "geometry",
        c::GLSLANG_STAGE_FRAGMENT => "fragment",
        c::GLSLANG_STAGE_COMPUTE => "compute",
        _ => "unknown",
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut s = err.to_string();
    let mut source = err.source();
    while let Some(e) = source {
        s.push_str(": ");
        s.push_str(&e.to_string());
        source = e.source();
    }
    s
}

fn for_each_call(block: &naga::Block, f: &mut impl FnMut(naga::Handle<naga::Function>)) {
    use naga::Statement as S;
    for st in block.iter() {
        match st {
            S::Call { function, .. } => f(*function),
            S::Block(b) => for_each_call(b, f),
            S::If { accept, reject, .. } => {
                for_each_call(accept, f);
                for_each_call(reject, f);
            }
            S::Switch { cases, .. } => {
                for case in cases {
                    for_each_call(&case.body, f);
                }
            }
            S::Loop {
                body, continuing, ..
            } => {
                for_each_call(body, f);
                for_each_call(continuing, f);
            }
            _ => {}
        }
    }
}

fn remap_block(block: &mut naga::Block, map: &[Option<naga::Handle<naga::Function>>]) {
    use naga::Statement as S;
    for st in block.iter_mut() {
        match st {
            S::Call { function, .. } => {
                *function = map[function.index()].expect("called function was removed")
            }
            S::Block(b) => remap_block(b, map),
            S::If { accept, reject, .. } => {
                remap_block(accept, map);
                remap_block(reject, map);
            }
            S::Switch { cases, .. } => {
                for case in cases {
                    remap_block(&mut case.body, map);
                }
            }
            S::Loop {
                body, continuing, ..
            } => {
                remap_block(body, map);
                remap_block(continuing, map);
            }
            _ => {}
        }
    }
}

fn remap_function(f: &mut naga::Function, map: &[Option<naga::Handle<naga::Function>>]) {
    for (_, expr) in f.expressions.iter_mut() {
        if let naga::Expression::CallResult(h) = expr {
            *h = map[h.index()].expect("called function was removed");
        }
    }
    remap_block(&mut f.body, map);
}

/// Reorder the module's functions so that every function comes after the
/// functions it calls (naga's validator requires this; GLSL allows calling
/// a function that is defined later through a prototype), and drop
/// prototypes that were never defined nor called.
///
/// Returns the names of functions that are called but never defined.
fn order_functions(module: &mut naga::Module) -> Vec<String> {
    // A defined function always ends with a return (naga adds one), so an
    // empty body means only a prototype was seen.
    let mut called = vec![false; module.functions.len()];
    for (_, f) in module.functions.iter() {
        for_each_call(&f.body, &mut |h| called[h.index()] = true);
    }
    for ep in &module.entry_points {
        for_each_call(&ep.function.body, &mut |h| called[h.index()] = true);
    }
    let mut undefined = Vec::new();
    for (h, f) in module.functions.iter() {
        if f.body.is_empty() && called[h.index()] {
            undefined.push(f.name.clone().unwrap_or_default());
        }
    }
    if !undefined.is_empty() {
        return undefined;
    }

    let funcs: Vec<(naga::Function, naga::Span)> = module
        .functions
        .drain()
        .map(|(_, f, span)| (f, span))
        .collect();
    let n = funcs.len();
    let deps: Vec<Vec<usize>> = funcs
        .iter()
        .map(|(f, _)| {
            let mut d = Vec::new();
            for_each_call(&f.body, &mut |h| d.push(h.index()));
            d
        })
        .collect();

    // Depth-first post-order: callees first. (Recursion is invalid GLSL
    // and is reported by the validator.)
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &d in &deps[i] {
            visit(d, deps, state, order);
        }
        state[i] = 2;
        order.push(i);
    }
    let mut state = vec![0u8; n];
    let mut order = Vec::with_capacity(n);
    for i in 0..n {
        visit(i, &deps, &mut state, &mut order);
    }

    let mut slots: Vec<Option<(naga::Function, naga::Span)>> =
        funcs.into_iter().map(Some).collect();
    let mut map: Vec<Option<naga::Handle<naga::Function>>> = vec![None; n];
    for old in order {
        let (f, span) = slots[old].take().unwrap();
        if f.body.is_empty() && !called[old] {
            // Uncalled prototype without a definition.
            continue;
        }
        map[old] = Some(module.functions.append(f, span));
    }

    for (_, f) in module.functions.iter_mut() {
        remap_function(f, &map);
    }
    for ep in module.entry_points.iter_mut() {
        remap_function(&mut ep.function, &map);
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn compile(code: &str) -> Result<Vec<u32>, (Error, String)> {
        let input = input(code);
        let mut shader = Shader::create(&input).unwrap();
        shader
            .preprocess(&input)
            .map_err(|e| (e, shader.get_info_log().to_owned()))?;
        shader
            .parse(&input)
            .map_err(|e| (e, shader.get_info_log().to_owned()))?;
        let mut program = Program::create().unwrap();
        program.add_shader(&shader);
        program
            .link(c::GLSLANG_MSG_SPV_RULES_BIT | c::GLSLANG_MSG_VULKAN_RULES_BIT)
            .map_err(|e| (e, program.get_info_log().unwrap().to_owned()))?;
        program.spirv_generate(c::GLSLANG_STAGE_FRAGMENT);
        let size = program.spirv_get_size();
        let mut buf = vec![0u32; size];
        program.spirv_get(&mut buf);
        assert_eq!(program.spirv_get_ptr().unwrap(), &buf[..]);
        Ok(buf)
    }

    #[test]
    fn program() {
        let program = Program::create().unwrap();
        program.delete();
    }

    #[test]
    fn simple_frag_to_spirv() {
        let spv = compile(include_str!("test/simple.frag")).unwrap();
        // SPIR-V magic number.
        assert_eq!(spv[0], 0x07230203);
        // SPIR-V 1.5
        assert_eq!(spv[1], 0x00010500);
    }

    #[test]
    fn missing_definition_is_link_error() {
        let (err, log) = compile(
            "#version 450\nlayout(location = 0) out vec4 c;\n\
             void f(out vec4 x);\nvoid main() { f(c); }\n",
        )
        .unwrap_err();
        assert_eq!(err, Error::GlslangFailed);
        assert!(log.contains("No function definition"), "{log}");
    }

    /// `renderer/shadertoy.zig`'s "spirv invalid" case: the user shader is
    /// missing `mainImage`, which glslang reports at link time.
    #[test]
    fn shadertoy_missing_main_image() {
        let src = format!(
            "{}\n\n{}",
            include_str!("../../src/renderer/shaders/shadertoy_prefix.glsl"),
            include_str!("../../src/renderer/shaders/test_shadertoy_invalid.glsl"),
        );
        let (err, log) = compile(&src).unwrap_err();
        assert_eq!(err, Error::GlslangFailed);
        assert!(!log.is_empty());
    }

    #[test]
    fn forward_declared_functions_are_ordered() {
        let spv = compile(
            "#version 450\nlayout(location = 0) out vec4 c;\n\
             float g(float x);\nvoid f(out vec4 x);\n\
             void main() { f(c); }\n\
             void f(out vec4 x) { x = vec4(g(1.0)); }\n\
             float g(float x) { return x * 2.0; }\n\
             float unused_proto(float y);\n",
        )
        .unwrap();
        assert!(!spv.is_empty());
    }
}
