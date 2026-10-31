//! Port of `src/dcimgui/main.zig`.
//!
//! STATUS: ported
//!
//! Dear ImGui (v1.92.5, docking branch) with the Dear Bindings (v0.17) C
//! API, used by the terminal inspector. Only compiled with the `inspector`
//! Cargo feature.
//!
//! Like the Zig package, the C++ sources are compiled into a static library
//! by the build script: they are vendored in `vendor/dcimgui` (`imgui/` from
//! ocornut/imgui `v1.92.5-docking`, `bindings/` from the Dear Bindings
//! release tarball the Zig build downloads), plus this package's `ext.cpp`.
//! [`c`] is generated from `dcimgui.h` with bindgen (the Zig package uses
//! translate-c), using the same defines as the build
//! (`IMGUI_HAS_DOCK`, `IMGUI_USE_WCHAR32`, `IMGUI_DISABLE_OBSOLETE_FUNCTIONS`).

#![cfg(feature = "inspector")]

use std::ffi::{c_char, c_void};

/// The Zig package's `build_options` for the Ghostty build on the current
/// target (`src/build/SharedDeps.zig`).
pub mod build_options {
    pub const FREETYPE: bool = true;
    pub const BACKEND_OPENGL3: bool = !cfg!(target_vendor = "apple");
    pub const BACKEND_METAL: bool = cfg!(target_vendor = "apple");
    pub const BACKEND_OSX: bool = cfg!(target_os = "macos");
}

/// The Dear Bindings C API (`dcimgui.h`).
#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code,
    clippy::all
)]
pub mod c {
    include!("c_bindings.rs");
}

// OpenGL3 backend
#[cfg(not(target_vendor = "apple"))]
extern "C" {
    pub fn ImGui_ImplOpenGL3_Init(glsl_version: *const c_char) -> bool;
    pub fn ImGui_ImplOpenGL3_Shutdown();
    pub fn ImGui_ImplOpenGL3_NewFrame();
    pub fn ImGui_ImplOpenGL3_RenderDrawData(draw_data: *mut c::ImDrawData);

    /// Extension: shutdown the OpenGL3 backend and zero out the imgl3w
    /// function pointer table so a subsequent Init can re-initialize the
    /// loader.
    pub fn ImGui_ImplOpenGL3_ShutdownWithLoaderCleanup();
}

// Metal backend
#[cfg(target_vendor = "apple")]
extern "C" {
    pub fn ImGui_ImplMetal_Init(device: *mut c_void) -> bool;
    pub fn ImGui_ImplMetal_Shutdown();
    pub fn ImGui_ImplMetal_NewFrame(render_pass_descriptor: *mut c_void);
    pub fn ImGui_ImplMetal_RenderDrawData(
        draw_data: *mut c::ImDrawData,
        command_buffer: *mut c_void,
        command_encoder: *mut c_void,
    );
}

// OSX
#[cfg(target_os = "macos")]
extern "C" {
    pub fn ImGui_ImplOSX_Init(view: *mut c_void) -> bool;
    pub fn ImGui_ImplOSX_Shutdown();
    pub fn ImGui_ImplOSX_NewFrame(view: *mut c_void);
}

/// Internal API types and functions from dcimgui_internal.h. We declare
/// these manually because the internal header contains bitfields that
/// translate-c has trouble with.
#[allow(non_upper_case_globals, non_snake_case)]
pub mod ImGuiDockNodeFlagsPrivate {
    use super::c::ImGuiDockNodeFlags;

    pub const DockSpace: ImGuiDockNodeFlags = 1 << 10;
    pub const CentralNode: ImGuiDockNodeFlags = 1 << 11;
    pub const NoTabBar: ImGuiDockNodeFlags = 1 << 12;
    pub const HiddenTabBar: ImGuiDockNodeFlags = 1 << 13;
    pub const NoWindowMenuButton: ImGuiDockNodeFlags = 1 << 14;
    pub const NoCloseButton: ImGuiDockNodeFlags = 1 << 15;
    pub const NoResizeX: ImGuiDockNodeFlags = 1 << 16;
    pub const NoResizeY: ImGuiDockNodeFlags = 1 << 17;
    pub const DockedWindowsInFocusRoute: ImGuiDockNodeFlags = 1 << 18;
    pub const NoDockingSplitOther: ImGuiDockNodeFlags = 1 << 19;
    pub const NoDockingOverMe: ImGuiDockNodeFlags = 1 << 20;
    pub const NoDockingOverOther: ImGuiDockNodeFlags = 1 << 21;
    pub const NoDockingOverEmpty: ImGuiDockNodeFlags = 1 << 22;
}

/// `ImVector<const char*>` (from dcimgui_internal.h).
#[allow(non_camel_case_types, non_snake_case)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ImVector_const_charPtr {
    pub Size: std::ffi::c_int,
    pub Capacity: std::ffi::c_int,
    pub Data: *mut *const c_char,
}

/// `ImVector<ImGuiID>` (from dcimgui_internal.h).
#[allow(non_camel_case_types, non_snake_case)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ImVector_ImGuiID {
    pub Size: std::ffi::c_int,
    pub Capacity: std::ffi::c_int,
    pub Data: *mut c::ImGuiID,
}

#[allow(non_snake_case)]
extern "C" {
    pub fn ImGui_DockBuilderDockWindow(window_name: *const c_char, node_id: c::ImGuiID);
    pub fn ImGui_DockBuilderGetNode(node_id: c::ImGuiID) -> *mut c_void;
    pub fn ImGui_DockBuilderGetCentralNode(node_id: c::ImGuiID) -> *mut c_void;
    pub fn ImGui_DockBuilderAddNode() -> c::ImGuiID;
    pub fn ImGui_DockBuilderAddNodeEx(
        node_id: c::ImGuiID,
        flags: c::ImGuiDockNodeFlags,
    ) -> c::ImGuiID;
    pub fn ImGui_DockBuilderRemoveNode(node_id: c::ImGuiID);
    pub fn ImGui_DockBuilderRemoveNodeDockedWindows(node_id: c::ImGuiID);
    pub fn ImGui_DockBuilderRemoveNodeDockedWindowsEx(
        node_id: c::ImGuiID,
        clear_settings_refs: bool,
    );
    pub fn ImGui_DockBuilderRemoveNodeChildNodes(node_id: c::ImGuiID);
    pub fn ImGui_DockBuilderSetNodePos(node_id: c::ImGuiID, pos: c::ImVec2);
    pub fn ImGui_DockBuilderSetNodeSize(node_id: c::ImGuiID, size: c::ImVec2);
    pub fn ImGui_DockBuilderSplitNode(
        node_id: c::ImGuiID,
        split_dir: c::ImGuiDir,
        size_ratio_for_node_at_dir: f32,
        out_id_at_dir: *mut c::ImGuiID,
        out_id_at_opposite_dir: *mut c::ImGuiID,
    ) -> c::ImGuiID;
    pub fn ImGui_DockBuilderCopyDockSpace(
        src_dockspace_id: c::ImGuiID,
        dst_dockspace_id: c::ImGuiID,
        in_window_remap_pairs: *mut ImVector_const_charPtr,
    );
    pub fn ImGui_DockBuilderCopyNode(
        src_node_id: c::ImGuiID,
        dst_node_id: c::ImGuiID,
        out_node_remap_pairs: *mut ImVector_ImGuiID,
    );
    pub fn ImGui_DockBuilderCopyWindowSettings(src_name: *const c_char, dst_name: *const c_char);
    pub fn ImGui_DockBuilderFinish(node_id: c::ImGuiID);
}

/// Extension functions from ext.cpp
#[allow(non_snake_case)]
pub mod ext {
    use super::c;

    extern "C" {
        pub fn ImFontConfig_ImFontConfig(self_: *mut c::ImFontConfig);
        pub fn ImGuiStyle_ImGuiStyle(self_: *mut c::ImGuiStyle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context() {
        // SAFETY: plain ImGui API usage on a fresh context.
        unsafe {
            let ctx = c::ImGui_CreateContext(std::ptr::null_mut());
            assert!(!ctx.is_null());
            assert!(!c::ImGui_GetIO().is_null());
            c::ImGui_DestroyContext(ctx);
        }
    }

    #[test]
    fn ext_defaults() {
        // SAFETY: the ext functions initialize the structs in place.
        unsafe {
            let mut style: c::ImGuiStyle = std::mem::zeroed();
            ext::ImGuiStyle_ImGuiStyle(&mut style);
            assert_eq!(style.Alpha, 1.0);

            let mut config: c::ImFontConfig = std::mem::zeroed();
            ext::ImFontConfig_ImFontConfig(&mut config);
            assert_eq!(config.RasterizerDensity, 1.0);
            assert!(!config.PixelSnapH);
        }
    }

    #[test]
    fn dock_flags() {
        assert_eq!(ImGuiDockNodeFlagsPrivate::DockSpace, 1 << 10);
    }
}
