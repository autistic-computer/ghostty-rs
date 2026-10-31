# CMake wrapper for libghostty-vt
#
# This file delegates to `tools/build_lib_vt.sh` (cargo) to produce the
# shared library, static library, headers, and pkg-config files. It is the
# Rust port of upstream Ghostty's CMakeLists.txt, which delegates to
# `zig build -Demit-lib-vt` instead. It exists so that CMake-based projects
# can consume libghostty-vt without interacting with cargo directly.
# However, downstream users do still require `cargo` (and bash, binutils'
# `ld`/`objcopy`/`ar`, and a C compiler) on the PATH.
#
# Building within the repo
# ------------------------
#
#   cmake -B build -DCMAKE_BUILD_TYPE=Release
#   cmake --build build
#   cmake --install build --prefix /usr/local
#
# Pass extra flags to `cargo rustc` with GHOSTTY_CARGO_BUILD_FLAGS:
#
#   cmake -B build -DGHOSTTY_CARGO_BUILD_FLAGS="--locked"
#
# Integrating into a downstream CMake project
# ---------------------------------------------
#
# Option 1 — FetchContent (recommended, no manual install step):
#
#   include(FetchContent)
#   FetchContent_Declare(ghostty SOURCE_DIR /path/to/ghostty-rs)
#   FetchContent_MakeAvailable(ghostty)
#
#   target_link_libraries(myapp PRIVATE ghostty-vt)        # shared
#   target_link_libraries(myapp PRIVATE ghostty-vt-static) # static
#
# (or GIT_REPOSITORY/GIT_TAG pointing at this repository), and to use a
# local checkout for a project that fetches it:
#
#   cmake -B build -DFETCHCONTENT_SOURCE_DIR_GHOSTTY=/path/to/ghostty-rs
#
# Option 2 — find_package (after installing to a prefix):
#
#   find_package(ghostty-vt REQUIRED)
#   target_link_libraries(myapp PRIVATE ghostty-vt::ghostty-vt)        # shared
#   target_link_libraries(myapp PRIVATE ghostty-vt::ghostty-vt-static) # static
#
# Cross-compilation
# -------------------
#
# For building libghostty-vt for a non-native Rust target (Linux/ELF
# targets; the toolchain's linker tools are taken from CC/LD/OBJCOPY/AR),
# use the ghostty_vt_add_target() function after FetchContent:
#
#   FetchContent_MakeAvailable(ghostty)
#   ghostty_vt_add_target(NAME linux-arm64 RUST_TARGET aarch64-unknown-linux-gnu)
#
#   target_link_libraries(myapp PRIVATE ghostty-vt-static-linux-arm64) # static
#   target_link_libraries(myapp PRIVATE ghostty-vt-linux-arm64)        # shared
#
# This handles build-type-to-profile mapping and output path conventions
# internally. Extra flags can be forwarded to cargo with CARGO_FLAGS:
#
#   ghostty_vt_add_target(NAME linux-arm64 RUST_TARGET aarch64-unknown-linux-gnu
#                         CARGO_FLAGS --locked)
#
# See dist/cmake/README.md for more details, example/c-vt-cmake/ for a
# complete working example, and example/c-vt-cmake-cross/ for a
# ghostty_vt_add_target() example.

cmake_minimum_required(VERSION 3.19)
project(ghostty-vt VERSION 0.1.0 LANGUAGES C)

# --- Options ----------------------------------------------------------------

set(GHOSTTY_CARGO_BUILD_FLAGS "" CACHE STRING "Additional flags to pass to cargo rustc")

# Map CMake build types to cargo profiles (Zig: optimization levels). The
# result is stored in _GHOSTTY_PROFILE_FLAG so both the native build and
# ghostty_vt_add_target() can reuse it without duplicating the mapping
# logic. Like `zig build`, the default is a debug build.
set(_GHOSTTY_PROFILE_FLAG "--debug")
if(CMAKE_BUILD_TYPE)
    string(TOUPPER "${CMAKE_BUILD_TYPE}" _bt)
    if(_bt STREQUAL "RELEASE" OR _bt STREQUAL "MINSIZEREL" OR _bt STREQUAL "RELWITHDEBINFO")
        set(_GHOSTTY_PROFILE_FLAG "--release")
    endif()
    unset(_bt)
endif()

# --- Find cargo ----------------------------------------------------------------

find_program(CARGO_EXECUTABLE cargo REQUIRED)
find_program(BASH_EXECUTABLE bash REQUIRED)
message(STATUS "Found cargo: ${CARGO_EXECUTABLE}")

set(_GHOSTTY_BUILD_LIB_VT "${CMAKE_CURRENT_SOURCE_DIR}/tools/build_lib_vt.sh")
set(_GHOSTTY_CARGO_TARGET_DIR "${CMAKE_CURRENT_SOURCE_DIR}/target/lib-vt")

# --- Build via tools/build_lib_vt.sh --------------------------------------------

# The script installs into a prefix in the build tree (Zig: zig-out/ in the
# source tree).
set(GHOSTTY_VT_OUT_DIR "${CMAKE_CURRENT_BINARY_DIR}/ghostty-vt-out")

# Library version (src/build/GhosttyLibVt.zig: libghostty-vt.so.0.1.0 with
# soname libghostty-vt.so.0).
set(GHOSTTY_VT_LIBNAME "${CMAKE_SHARED_LIBRARY_PREFIX}ghostty-vt${CMAKE_SHARED_LIBRARY_SUFFIX}")
set(GHOSTTY_VT_SONAME "${CMAKE_SHARED_LIBRARY_PREFIX}ghostty-vt${CMAKE_SHARED_LIBRARY_SUFFIX}.0")
set(GHOSTTY_VT_REALNAME "${CMAKE_SHARED_LIBRARY_PREFIX}ghostty-vt${CMAKE_SHARED_LIBRARY_SUFFIX}.0.1.0")
set(GHOSTTY_VT_SHARED_LIBRARY "${GHOSTTY_VT_OUT_DIR}/lib/${GHOSTTY_VT_REALNAME}")
set(GHOSTTY_VT_STATIC_REALNAME "libghostty-vt.a")
set(GHOSTTY_VT_STATIC_LIBRARY "${GHOSTTY_VT_OUT_DIR}/lib/${GHOSTTY_VT_STATIC_REALNAME}")

# Rust's std needs these system libraries when the static library is linked
# (`rustc --print native-static-libs`; libghostty-vt-static.pc lists the
# subset the C API actually uses).
set(GHOSTTY_VT_STATIC_LINK_LIBRARIES "gcc_s;util;rt;pthread;m;dl")

# Ensure the output directories exist so CMake doesn't reject the
# INTERFACE_INCLUDE_DIRECTORIES before the cargo build has run.
file(MAKE_DIRECTORY "${GHOSTTY_VT_OUT_DIR}/include")

# Custom command: run tools/build_lib_vt.sh (produces both shared and static)
add_custom_command(
    OUTPUT "${GHOSTTY_VT_SHARED_LIBRARY}" "${GHOSTTY_VT_STATIC_LIBRARY}"
    COMMAND "${CMAKE_COMMAND}" -E env
        "CARGO=${CARGO_EXECUTABLE}"
        "GHOSTTY_LIB_VT_TARGET_DIR=${_GHOSTTY_CARGO_TARGET_DIR}"
        "GHOSTTY_LIB_VT_CARGO_FLAGS=${GHOSTTY_CARGO_BUILD_FLAGS}"
        "${BASH_EXECUTABLE}" "${_GHOSTTY_BUILD_LIB_VT}" ${_GHOSTTY_PROFILE_FLAG} "${GHOSTTY_VT_OUT_DIR}"
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    COMMENT "Building libghostty-vt via cargo..."
    USES_TERMINAL
)

add_custom_target(cargo_build_lib_vt ALL
    DEPENDS "${GHOSTTY_VT_SHARED_LIBRARY}" "${GHOSTTY_VT_STATIC_LIBRARY}"
)

# Tell CMake's clean target to also remove the output directory.
set_property(DIRECTORY APPEND PROPERTY
    ADDITIONAL_CLEAN_FILES "${GHOSTTY_VT_OUT_DIR}"
)

# --- IMPORTED library targets ------------------------------------------------

# Shared
add_library(ghostty-vt SHARED IMPORTED GLOBAL)
set_target_properties(ghostty-vt PROPERTIES
    IMPORTED_LOCATION "${GHOSTTY_VT_SHARED_LIBRARY}"
    IMPORTED_SONAME "${GHOSTTY_VT_SONAME}"
    INTERFACE_INCLUDE_DIRECTORIES "${GHOSTTY_VT_OUT_DIR}/include"
)
add_dependencies(ghostty-vt cargo_build_lib_vt)

# Static
#
# The static library is a single relocatable object holding the C API and
# the parts of the Rust standard library it uses; consumers only need to
# link libc and the system libraries Rust's std uses.
add_library(ghostty-vt-static STATIC IMPORTED GLOBAL)
set_target_properties(ghostty-vt-static PROPERTIES
    IMPORTED_LOCATION "${GHOSTTY_VT_STATIC_LIBRARY}"
    INTERFACE_INCLUDE_DIRECTORIES "${GHOSTTY_VT_OUT_DIR}/include"
    INTERFACE_COMPILE_DEFINITIONS "GHOSTTY_STATIC"
    INTERFACE_LINK_LIBRARIES "${GHOSTTY_VT_STATIC_LINK_LIBRARIES}"
)
add_dependencies(ghostty-vt-static cargo_build_lib_vt)

# --- Install ------------------------------------------------------------------

include(GNUInstallDirs)

# Install shared library
install(FILES "${GHOSTTY_VT_SHARED_LIBRARY}" TYPE LIB)
# Install symlinks
install(CODE "
    execute_process(COMMAND \${CMAKE_COMMAND} -E create_symlink
        \"${GHOSTTY_VT_REALNAME}\"
        \"\$ENV{DESTDIR}\${CMAKE_INSTALL_PREFIX}/${CMAKE_INSTALL_LIBDIR}/${GHOSTTY_VT_SONAME}\")
    execute_process(COMMAND \${CMAKE_COMMAND} -E create_symlink
        \"${GHOSTTY_VT_SONAME}\"
        \"\$ENV{DESTDIR}\${CMAKE_INSTALL_PREFIX}/${CMAKE_INSTALL_LIBDIR}/${GHOSTTY_VT_LIBNAME}\")
")

# Install static library
install(FILES "${GHOSTTY_VT_STATIC_LIBRARY}" TYPE LIB)

# Install headers
install(DIRECTORY "${GHOSTTY_VT_OUT_DIR}/include/ghostty" DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}")

# --- CMake package config for find_package() ----------------------------------

include(CMakePackageConfigHelpers)

# Generate the config file
configure_package_config_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/dist/cmake/ghostty-vt-config.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/ghostty-vt-config.cmake"
    INSTALL_DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/ghostty-vt"
)

# Generate the version file
write_basic_package_version_file(
    "${CMAKE_CURRENT_BINARY_DIR}/ghostty-vt-config-version.cmake"
    VERSION "${PROJECT_VERSION}"
    COMPATIBILITY SameMajorVersion
)

# Install the config files
install(
    FILES
        "${CMAKE_CURRENT_BINARY_DIR}/ghostty-vt-config.cmake"
        "${CMAKE_CURRENT_BINARY_DIR}/ghostty-vt-config-version.cmake"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/ghostty-vt"
)

# --- Cross-compilation helper ------------------------------------------------
#
# For downstream projects that need to build libghostty-vt for a specific
# Rust target triple. For native builds, use the IMPORTED targets above
# (ghostty-vt, ghostty-vt-static) directly.
#
# Usage (in a downstream CMakeLists.txt after FetchContent_MakeAvailable):
#
#   ghostty_vt_add_target(NAME linux-arm64 RUST_TARGET aarch64-unknown-linux-gnu)
#
# Creates:
#   ghostty-vt-static-linux-arm64   (IMPORTED STATIC library)
#   ghostty-vt-linux-arm64          (IMPORTED SHARED library)
#
# Optional CARGO_FLAGS to pass additional flags to cargo rustc:
#
#   ghostty_vt_add_target(NAME linux-arm64 RUST_TARGET aarch64-unknown-linux-gnu
#                         CARGO_FLAGS --locked)

function(ghostty_vt_add_target)
    cmake_parse_arguments(PARSE_ARGV 0 _GVT "" "NAME;RUST_TARGET" "CARGO_FLAGS")

    if(NOT _GVT_NAME)
        message(FATAL_ERROR "ghostty_vt_add_target: NAME is required")
    endif()
    if(NOT _GVT_RUST_TARGET)
        message(FATAL_ERROR "ghostty_vt_add_target: RUST_TARGET is required")
    endif()

    # Only cache variables of this file are visible here when the function
    # is called from a parent project, so derive everything else again.
    set(_src_dir "${CMAKE_CURRENT_FUNCTION_LIST_DIR}")
    set(_prefix "${CMAKE_CURRENT_BINARY_DIR}/ghostty-${_GVT_NAME}")

    # Default to a release build when no build type is set (Zig: ReleaseFast,
    # since the sanitizer runtime of its debug builds is not available for
    # every target).
    set(_profile "--release")
    if(CMAKE_BUILD_TYPE)
        string(TOUPPER "${CMAKE_BUILD_TYPE}" _bt)
        if(_bt STREQUAL "DEBUG")
            set(_profile "--debug")
        endif()
    endif()

    # Output paths
    set(_include_dir "${_prefix}/include")
    set(_static_lib "${_prefix}/lib/libghostty-vt.a")
    set(_shared_lib "${_prefix}/lib/libghostty-vt.so.0.1.0")

    file(MAKE_DIRECTORY "${_include_dir}")

    # Custom command: invoke tools/build_lib_vt.sh for the target
    add_custom_command(
        OUTPUT "${_static_lib}" "${_shared_lib}"
        COMMAND "${CMAKE_COMMAND}" -E env
            "CARGO=${CARGO_EXECUTABLE}"
            "GHOSTTY_LIB_VT_TARGET_DIR=${_src_dir}/target/lib-vt"
            "GHOSTTY_LIB_VT_CARGO_FLAGS=${_GVT_CARGO_FLAGS}"
            "${BASH_EXECUTABLE}" "${_src_dir}/tools/build_lib_vt.sh" ${_profile}
            --target "${_GVT_RUST_TARGET}" "${_prefix}"
        WORKING_DIRECTORY "${_src_dir}"
        COMMENT "Building libghostty-vt for ${_GVT_RUST_TARGET}..."
        USES_TERMINAL
    )

    set(_build_target "cargo_build_lib_vt_${_GVT_NAME}")
    add_custom_target(${_build_target} ALL
        DEPENDS "${_static_lib}" "${_shared_lib}"
    )

    # Static target
    set(_static_target "ghostty-vt-static-${_GVT_NAME}")
    add_library(${_static_target} STATIC IMPORTED GLOBAL)
    set_target_properties(${_static_target} PROPERTIES
        IMPORTED_LOCATION "${_static_lib}"
        INTERFACE_INCLUDE_DIRECTORIES "${_include_dir}"
        INTERFACE_COMPILE_DEFINITIONS "GHOSTTY_STATIC"
        INTERFACE_LINK_LIBRARIES "gcc_s;util;rt;pthread;m;dl"
    )
    add_dependencies(${_static_target} ${_build_target})

    # Shared target
    set(_shared_target "ghostty-vt-${_GVT_NAME}")
    add_library(${_shared_target} SHARED IMPORTED GLOBAL)
    set_target_properties(${_shared_target} PROPERTIES
        IMPORTED_LOCATION "${_shared_lib}"
        IMPORTED_SONAME "libghostty-vt.so.0"
        INTERFACE_INCLUDE_DIRECTORIES "${_include_dir}"
    )
    add_dependencies(${_shared_target} ${_build_target})
endfunction()
