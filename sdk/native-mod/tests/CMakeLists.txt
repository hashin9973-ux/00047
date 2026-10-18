cmake_minimum_required(VERSION 3.21)
project(00047_native_mod_compat_tests LANGUAGES CXX)

set(WMOD_SDK_DIR "${CMAKE_CURRENT_LIST_DIR}/..")
add_executable(00047_native_mod_compat_test nonhook_compat_test.cpp)
target_compile_features(00047_native_mod_compat_test PRIVATE cxx_std_17)
target_compile_definitions(00047_native_mod_compat_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_native_mod_compat_test PRIVATE "${WMOD_SDK_DIR}")
target_link_libraries(00047_native_mod_compat_test PRIVATE winhttp ws2_32)

add_library(00047_windhawk_nonhook_adapter_smoke SHARED windhawk_nonhook_adapter_smoke.cpp)
target_compile_features(00047_windhawk_nonhook_adapter_smoke PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_nonhook_adapter_smoke PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_windhawk_nonhook_adapter_smoke PRIVATE "${WMOD_SDK_DIR}")


# Compile and link a local PDB for deterministic symbol-lookup tests. The
# tests use an empty symbol path so they never download external PDBs.
add_executable(00047_windhawk_symbol_compat_test symbol_compat_test.cpp)
target_compile_features(00047_windhawk_symbol_compat_test PRIVATE cxx_std_17)
target_compile_definitions(00047_windhawk_symbol_compat_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_windhawk_symbol_compat_test PRIVATE "${WMOD_SDK_DIR}")
target_link_libraries(00047_windhawk_symbol_compat_test PRIVATE dbghelp psapi)
if(MSVC)
  target_compile_options(00047_windhawk_symbol_compat_test PRIVATE /Zi)
  target_link_options(00047_windhawk_symbol_compat_test PRIVATE /DEBUG)
endif()
