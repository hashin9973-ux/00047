# Source-level compatibility for Windhawk's compile-time metadata constants.
# The in-app builder passes this file as CMAKE_PROJECT_INCLUDE. Generated
# CMake projects include it after project() for builds outside 00047.
get_property(_wmod_metadata_configured GLOBAL PROPERTY WMOD_WINDHAWK_METADATA_CONFIGURED)
if(_wmod_metadata_configured)
  return()
endif()

if(NOT EXISTS "${CMAKE_SOURCE_DIR}/manifest.json")
  # This SDK may be used by a general CMake project with no native-mod manifest.
  return()
endif()

file(READ "${CMAKE_SOURCE_DIR}/manifest.json" _wmod_manifest_json)
string(JSON _wmod_id ERROR_VARIABLE _wmod_id_error GET "${_wmod_manifest_json}" id)
string(JSON _wmod_version ERROR_VARIABLE _wmod_version_error GET "${_wmod_manifest_json}" version)
if(NOT "${_wmod_id_error}" STREQUAL "NOTFOUND")
  message(FATAL_ERROR "Cannot read native-mod id from manifest.json: ${_wmod_id_error}")
endif()
if(NOT "${_wmod_version_error}" STREQUAL "NOTFOUND")
  message(FATAL_ERROR "Cannot read native-mod version from manifest.json: ${_wmod_version_error}")
endif()
if(NOT _wmod_id MATCHES "^[a-z0-9._-]+$")
  message(FATAL_ERROR "manifest.json id cannot be represented as a safe WH_MOD_ID literal.")
endif()
if(_wmod_version STREQUAL "")
  message(FATAL_ERROR "manifest.json version must be non-empty.")
endif()

# Escape C/C++ string-literal metacharacters before generating the header.
string(REPLACE "\\" "\\\\" _wmod_id_escaped "${_wmod_id}")
string(REPLACE "\"" "\\\"" _wmod_id_escaped "${_wmod_id_escaped}")
string(REPLACE "\\" "\\\\" _wmod_version_escaped "${_wmod_version}")
string(REPLACE "\"" "\\\"" _wmod_version_escaped "${_wmod_version_escaped}")
string(REPLACE "\r" "\\r" _wmod_version_escaped "${_wmod_version_escaped}")
string(REPLACE "\n" "\\n" _wmod_version_escaped "${_wmod_version_escaped}")
string(REPLACE "\t" "\\t" _wmod_version_escaped "${_wmod_version_escaped}")

set(_wmod_metadata_include_dir "${CMAKE_BINARY_DIR}/.00047_windhawk_metadata")
file(MAKE_DIRECTORY "${_wmod_metadata_include_dir}")
file(WRITE "${_wmod_metadata_include_dir}/windhawk_mod_metadata.h"
  "#pragma once\n"
  "#ifndef WH_MOD_ID\n#define WH_MOD_ID L\"${_wmod_id_escaped}\"\n#endif\n"
  "#ifndef WH_MOD_VERSION\n#define WH_MOD_VERSION L\"${_wmod_version_escaped}\"\n#endif\n")

# Use a basename for /FI so the forced-include path itself never needs quoting.
# CMake's Visual Studio generator is the supported 00047 build toolchain.
if(MSVC)
  include_directories(BEFORE "${_wmod_metadata_include_dir}")
  add_compile_options("$<$<COMPILE_LANGUAGE:C,CXX>:/FIwindhawk_mod_metadata.h>")
else()
  message(FATAL_ERROR "Windhawk metadata injection currently requires the MSVC toolchain.")
endif()

set_property(GLOBAL PROPERTY WMOD_WINDHAWK_METADATA_CONFIGURED TRUE)
