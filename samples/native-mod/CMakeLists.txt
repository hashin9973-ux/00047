cmake_minimum_required(VERSION 3.21)
project(00047_sample_native_mod LANGUAGES C CXX)

set(WMOD_SDK_DIR "${CMAKE_CURRENT_LIST_DIR}/../../sdk/native-mod")
set(WMOD_HOOK_ENGINE_SOURCES
  "${WMOD_SDK_DIR}/windhawk_hook_compat.cpp"
  "${WMOD_SDK_DIR}/third_party/minhook/src/buffer.c"
  "${WMOD_SDK_DIR}/third_party/minhook/src/hook.c"
  "${WMOD_SDK_DIR}/third_party/minhook/src/trampoline.c"
  "${WMOD_SDK_DIR}/third_party/minhook/src/hde/hde64.c"
)
set(WMOD_HOOK_INCLUDE_DIRS
  "${WMOD_SDK_DIR}"
  "${WMOD_SDK_DIR}/third_party/minhook/include"
  "${WMOD_SDK_DIR}/third_party/minhook/src"
  "${WMOD_SDK_DIR}/third_party/minhook/src/hde"
)


add_library(00047_sample_native_mod SHARED sample_mod.cpp ${WMOD_HOOK_ENGINE_SOURCES})
target_compile_features(00047_sample_native_mod PRIVATE cxx_std_17)
target_compile_definitions(00047_sample_native_mod PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_sample_native_mod PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})
set_target_properties(00047_sample_native_mod PROPERTIES PREFIX "" OUTPUT_NAME "00047_sample_native_mod")
