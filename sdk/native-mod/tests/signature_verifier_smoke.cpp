#include <Windows.h>

#include <cstdio>

extern "C" int __cdecl wmod_verify_trusted_authenticode(const wchar_t* path) noexcept;

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) {
        ::fwprintf(stderr, L"Usage: signature_verifier_smoke.exe <unsigned-dll>\n");
        return 2;
    }

    if (wmod_verify_trusted_authenticode(argv[1]) != 0) {
        ::fwprintf(stderr,
            L"Unsigned CI fixture unexpectedly passed Authenticode trust validation: %s\n",
            argv[1]);
        return 1;
    }

    ::wprintf(L"Authenticode verifier correctly rejected the unsigned fixture.\n");
    return 0;
}
