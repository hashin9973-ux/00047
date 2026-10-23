// SPDX-License-Identifier: MIT
// Windows Authenticode verification helper used by the native-mod trust policy.
// Verification is intentionally cache-only: missing trust/revocation metadata is
// treated as failure instead of triggering background network retrieval.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <softpub.h>
#include <wintrust.h>

extern "C" int __cdecl wmod_verify_trusted_authenticode(
    const wchar_t* path) noexcept {
    if (path == nullptr || *path == L'\0') {
        return 0;
    }

    WINTRUST_FILE_INFO fileInfo{};
    fileInfo.cbStruct = sizeof(fileInfo);
    fileInfo.pcwszFilePath = path;

    WINTRUST_DATA trustData{};
    trustData.cbStruct = sizeof(trustData);
    trustData.dwUIChoice = WTD_UI_NONE;
    trustData.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
    trustData.dwUnionChoice = WTD_CHOICE_FILE;
    trustData.pFile = &fileInfo;
    trustData.dwStateAction = WTD_STATEACTION_VERIFY;
    trustData.dwProvFlags =
        WTD_CACHE_ONLY_URL_RETRIEVAL |
        WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT |
        WTD_DISABLE_MD2_MD4;

    GUID policy = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    const LONG verification = WinVerifyTrust(
        INVALID_HANDLE_VALUE, &policy, &trustData);

    trustData.dwStateAction = WTD_STATEACTION_CLOSE;
    (void)WinVerifyTrust(INVALID_HANDLE_VALUE, &policy, &trustData);

    return verification == ERROR_SUCCESS ? 1 : 0;
}
