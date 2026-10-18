#include "00047_native_extension.h"
#include <stdint.h>

static int32_t EXTENSION_00047_CALL module_start(
    const extension_00047_host_api_v1 *host,
    uintptr_t *user_data)
{
    if (!host || host->abi_version != EXTENSION_00047_NATIVE_ABI_V1 || !user_data)
        return 1;

    *user_data = 470047u;
    if (host->log)
        host->log(host->context, 1u, "Example native module started.");
    return 0;
}

static int32_t EXTENSION_00047_CALL module_stop(
    const extension_00047_host_api_v1 *host,
    uintptr_t user_data)
{
    (void)user_data;
    if (host && host->log)
        host->log(host->context, 1u, "Example native module stopped.");
    return 0;
}

static const extension_00047_native_module_api_v1 MODULE_API = {
    EXTENSION_00047_NATIVE_ABI_V1,
    sizeof(extension_00047_native_module_api_v1),
    "org.example.native-hello",
    "Native Hello",
    "1.0.0",
    "Small C++ DLL demonstrating the 00047 in-process native extension ABI.",
    module_start,
    module_stop
};

EXTENSION_00047_EXPORT const extension_00047_native_module_api_v1 *
EXTENSION_00047_CALL extension_00047_get_api_v1(uint32_t requested_abi)
{
    return requested_abi == EXTENSION_00047_NATIVE_ABI_V1 ? &MODULE_API : 0;
}
