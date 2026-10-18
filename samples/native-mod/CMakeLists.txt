cmake_minimum_required(VERSION 3.21)
project(00047_sample_native_mod LANGUAGES CXX)

add_library(00047_sample_native_mod SHARED sample_mod.cpp)
target_compile_features(00047_sample_native_mod PRIVATE cxx_std_17)
target_compile_definitions(00047_sample_native_mod PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(00047_sample_native_mod PRIVATE ../../sdk/native-mod)
set_target_properties(00047_sample_native_mod PROPERTIES PREFIX "" OUTPUT_NAME "00047_sample_native_mod")
