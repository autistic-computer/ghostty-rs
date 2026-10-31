cmake_minimum_required(VERSION 3.19)
project(c-vt-cmake LANGUAGES C)

# This checkout's CMakeLists.txt (which builds libghostty-vt with cargo).
# Override with -DFETCHCONTENT_SOURCE_DIR_GHOSTTY=..., or declare the
# dependency with GIT_REPOSITORY/GIT_TAG in your own project.
include(FetchContent)
FetchContent_Declare(ghostty
    SOURCE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../.."
)
FetchContent_MakeAvailable(ghostty)

add_executable(c_vt_cmake src/main.c)
target_link_libraries(c_vt_cmake PRIVATE ghostty-vt)
