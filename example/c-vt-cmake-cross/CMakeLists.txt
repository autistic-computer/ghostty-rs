cmake_minimum_required(VERSION 3.19)

# --- Determine the target ------------------------------------------------------
#
# The Rust target triple to build libghostty-vt for, e.g.
# -DRUST_TARGET=aarch64-unknown-linux-gnu together with a matching cross C
# toolchain (-DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc, and CC/LD/OBJCOPY/AR
# in the environment for tools/build_lib_vt.sh). Upstream picks a different
# OS than the host and uses `zig cc` as the cross compiler; the Rust port's
# library build supports Linux/ELF targets, so by default this example
# builds for the host triple, through the same ghostty_vt_add_target() path.
if(NOT RUST_TARGET)
    execute_process(
        COMMAND rustc -vV
        OUTPUT_VARIABLE _rustc_vv
        OUTPUT_STRIP_TRAILING_WHITESPACE
    )
    string(REGEX MATCH "host: ([^\n]+)" _host_line "${_rustc_vv}")
    set(RUST_TARGET "${CMAKE_MATCH_1}")
    if(NOT RUST_TARGET)
        message(FATAL_ERROR "Cannot derive RUST_TARGET. Pass -DRUST_TARGET=... manually.")
    endif()
    message(STATUS "Building libghostty-vt for RUST_TARGET: ${RUST_TARGET}")
endif()

project(c-vt-cmake-cross LANGUAGES C)

# This checkout's CMakeLists.txt (which builds libghostty-vt with cargo).
include(FetchContent)
FetchContent_Declare(ghostty
    SOURCE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../.."
)
FetchContent_MakeAvailable(ghostty)

ghostty_vt_add_target(NAME cross RUST_TARGET "${RUST_TARGET}")

add_executable(c_vt_cmake_cross src/main.c)
target_link_libraries(c_vt_cmake_cross PRIVATE ghostty-vt-static-cross)
