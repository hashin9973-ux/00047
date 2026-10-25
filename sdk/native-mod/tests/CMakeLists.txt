cmake_minimum_required(VERSION 3.21)
project(00047_native_mod_compat_tests LANGUAGES C CXX)

set(WMOD_SDK_DIR "${CMAKE_CURRENT_LIST_DIR}/..")
set(WMOD_HOOK_ENGINE_SOURCES
  "${WMOD_SDK_DIR}/windhawk_hook_compat.cpp"
  "${WMOD_SDK_DIR}/third_party/minhook/src/buffer.c"
  "${WMOD_SDK_DIR}/third_party/minhook/src/hook.c"
  "${WMOD_SDK_DIR}/third_party/minhook/src/trampoline.c"
  "${WMOD_SDK_DIR}/third_party/minhook/src/hde/hde64.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/decoder.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/distorm.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/instructions.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/insts.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/mnemonics.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/operands.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/prefix.c"
  "${WMOD_SDK_DIR}/third_party/distorm/src/textdefs.c"
)
set(WMOD_HOOK_INCLUDE_DIRS
  "${WMOD_SDK_DIR}"
  "${WMOD_SDK_DIR}/third_party/minhook/include"
  "${WMOD_SDK_DIR}/third_party/minhook/src"
  "${WMOD_SDK_DIR}/third_party/minhook/src/hde"
  "${WMOD_SDK_DIR}/third_party/distorm/include"
  "${WMOD_SDK_DIR}/third_party/distorm/src"
)


add_executable(00047_signature_verifier_smoke
  signature_verifier_smoke.cpp
  "${CMAKE_CURRENT_LIST_DIR}/../../../src-tauri/native/signature_verifier.cpp"
)
target_compile_features(00047_signature_verifier_smoke PRIVATE cxx_std_20)
target_compile_definitions(00047_signature_verifier_smoke PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_link_libraries(00047_signature_verifier_smoke PRIVATE wintrust crypt32)

add_executable(00047_native_mod_compat_test nonhook_compat_test.cpp)
target_compile_features(00047_native_mod_compat_test PRIVATE cxx_std_17)
target_compile_definitions(00047_native_mod_compat_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_native_mod_compat_test PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})
target_link_libraries(00047_native_mod_compat_test PRIVATE winhttp ws2_32)
add_dependencies(00047_native_mod_compat_test 00047_signature_verifier_smoke)

add_library(00047_windhawk_nonhook_adapter_smoke SHARED windhawk_nonhook_adapter_smoke.cpp ${WMOD_HOOK_ENGINE_SOURCES})
target_compile_features(00047_windhawk_nonhook_adapter_smoke PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_nonhook_adapter_smoke PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_windhawk_nonhook_adapter_smoke PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})


add_executable(00047_windhawk_nonhook_adapter_runtime_test windhawk_nonhook_adapter_runtime_test.cpp)
target_compile_features(00047_windhawk_nonhook_adapter_runtime_test PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_nonhook_adapter_runtime_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
add_dependencies(00047_windhawk_nonhook_adapter_runtime_test 00047_windhawk_nonhook_adapter_smoke)

add_library(00047_windhawk_hook_runtime_smoke SHARED windhawk_hook_runtime_smoke.cpp ${WMOD_HOOK_ENGINE_SOURCES})
target_compile_features(00047_windhawk_hook_runtime_smoke PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_hook_runtime_smoke PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_compile_definitions(00047_windhawk_hook_runtime_smoke PRIVATE WMOD_HOOK_RUNTIME_TESTING)
target_include_directories(00047_windhawk_hook_runtime_smoke PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})

add_executable(00047_windhawk_hook_runtime_test windhawk_hook_runtime_test.cpp)
target_compile_features(00047_windhawk_hook_runtime_test PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_hook_runtime_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
add_dependencies(00047_windhawk_hook_runtime_test 00047_windhawk_hook_runtime_smoke)

add_executable(00047_windhawk_symbol_compat_test symbol_compat_test.cpp ${WMOD_HOOK_ENGINE_SOURCES})
target_compile_features(00047_windhawk_symbol_compat_test PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_symbol_compat_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_windhawk_symbol_compat_test PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})
target_link_libraries(00047_windhawk_symbol_compat_test PRIVATE winhttp dbghelp psapi)
if(MSVC)
  target_compile_options(00047_windhawk_symbol_compat_test PRIVATE /Zi)
  target_link_options(00047_windhawk_symbol_compat_test PRIVATE /DEBUG)
endif()

add_library(00047_windhawk_settings_changed_smoke SHARED windhawk_settings_changed_smoke.cpp ${WMOD_HOOK_ENGINE_SOURCES})
target_compile_features(00047_windhawk_settings_changed_smoke PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_settings_changed_smoke PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_windhawk_settings_changed_smoke PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})

add_executable(00047_windhawk_settings_changed_runtime_test windhawk_settings_changed_runtime_test.cpp)
target_compile_features(00047_windhawk_settings_changed_runtime_test PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_settings_changed_runtime_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
add_dependencies(00047_windhawk_settings_changed_runtime_test 00047_windhawk_settings_changed_smoke)

# CI-only test double: its output basename is explorer.exe so the real
# allow-list, remote LoadLibrary and unload code paths can be exercised without
# injecting into the machine's real Explorer process.
add_executable(00047_native_mod_integration_target native_mod_integration_target.cpp)
target_compile_features(00047_native_mod_integration_target PRIVATE cxx_std_17)
target_compile_definitions(00047_native_mod_integration_target PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
set_target_properties(00047_native_mod_integration_target PROPERTIES OUTPUT_NAME "explorer")

# Actual remote detour fixture: replaces RemoteTargetFunction's result (7 -> 17).
add_library(00047_native_mod_hook_integration_smoke SHARED
  native_mod_hook_integration_smoke.cpp ${WMOD_HOOK_ENGINE_SOURCES})
target_compile_features(00047_native_mod_hook_integration_smoke PRIVATE cxx_std_17)
target_compile_definitions(00047_native_mod_hook_integration_smoke PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_native_mod_hook_integration_smoke PRIVATE ${WMOD_HOOK_INCLUDE_DIRS})

# Deliberately rejects initialization to exercise host rollback/unload.
add_library(00047_native_mod_failed_init_smoke SHARED native_mod_failed_init_smoke.cpp)
target_compile_features(00047_native_mod_failed_init_smoke PRIVATE cxx_std_17)
target_compile_definitions(00047_native_mod_failed_init_smoke PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_native_mod_failed_init_smoke PRIVATE "${WMOD_SDK_DIR}")
