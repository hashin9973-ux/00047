#ifndef EXTENSION_00047_NATIVE_EXTENSION_H
#define EXTENSION_00047_NATIVE_EXTENSION_H

#include <stdint.h>

#ifdef _WIN32
  #define EXTENSION_00047_EXPORT __declspec(dllexport)
  #define EXTENSION_00047_CALL __cdecl
#else
  #define EXTENSION_00047_EXPORT
  #define EXTENSION_00047_CALL
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define EXTENSION_00047_NATIVE_ABI_V1 1u

typedef void (EXTENSION_00047_CALL *extension_00047_log_fn)(
    uintptr_t context,
    uint32_t level,       /* 0=debug, 1=info, 2=warning, 3=error */
    const char *message   /* UTF-8, NUL-terminated */
);

typedef struct extension_00047_host_api_v1 {
    uint32_t abi_version;
    uint32_t struct_size;
    uintptr_t context; /* opaque; pass unchanged to host callbacks */
    extension_00047_log_fn log;
} extension_00047_host_api_v1;

typedef int32_t (EXTENSION_00047_CALL *extension_00047_start_fn)(
    const extension_00047_host_api_v1 *host,
    uintptr_t *user_data
);

typedef int32_t (EXTENSION_00047_CALL *extension_00047_stop_fn)(
    const extension_00047_host_api_v1 *host,
    uintptr_t user_data
);

typedef struct extension_00047_native_module_api_v1 {
    uint32_t abi_version;
    uint32_t struct_size;
    const char *id;          /* lowercase a-z, digits, dots, dashes, underscores */
    const char *name;        /* UTF-8 */
    const char *version;     /* UTF-8 */
    const char *description; /* UTF-8; may be NULL */
    extension_00047_start_fn start;
    extension_00047_stop_fn stop;
} extension_00047_native_module_api_v1;

/*
 * Every native module DLL must export this symbol:
 *   extension_00047_get_api_v1
 * The host loads the DLL into the 00047 process, requests ABI v1, then calls
 * start. On unload or app shutdown it calls stop before unloading the DLL.
 */
typedef const extension_00047_native_module_api_v1 *
    (EXTENSION_00047_CALL *extension_00047_get_api_v1_fn)(uint32_t requested_abi);

#ifdef __cplusplus
}
#endif

#endif
