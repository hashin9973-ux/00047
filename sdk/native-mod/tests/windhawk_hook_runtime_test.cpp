#include <Windows.h>

#include <atomic>
#include <cstdio>
#include <cstring>
#include <thread>

struct TestDisasmResult { size_t length; char text[96]; };
using TargetFn = int (WINAPI*)(int);
using DisasmFn = BOOL (WINAPI*)(TestDisasmResult*);
using DisasmAddressFn = BOOL (WINAPI*)(void*, TestDisasmResult*);
using SimpleBoolFn = BOOL (WINAPI*)();
using VoidFn = void (WINAPI*)();
using HoldFn = void (WINAPI*)(BOOL);
using InsideFn = BOOL (WINAPI*)();
using AddressFn = void* (WINAPI*)();
using ExternalInstallFn = BOOL (WINAPI*)(void*, void**);
using ExternalRemoveFn = BOOL (WINAPI*)(void*);

#define CHECK(expression) do { \
    if (!(expression)) { \
        std::fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__, __LINE__, #expression); \
        return __LINE__; \
    } \
} while (0)

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) {
        std::fwprintf(stderr, L"Usage: 00047_windhawk_hook_runtime_test.exe <hook-smoke-dll>\n");
        return 2;
    }

    HMODULE module = LoadLibraryW(argv[1]);
    CHECK(module != nullptr);
    auto target = reinterpret_cast<TargetFn>(GetProcAddress(module, "HookRuntimeTarget"));
    auto disasm = reinterpret_cast<DisasmFn>(GetProcAddress(module, "HookRuntimeDisasm"));
    auto disasmAddress = reinterpret_cast<DisasmAddressFn>(
        GetProcAddress(module, "HookRuntimeDisasmAddress"));
    auto begin = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeBegin"));
    auto install = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeInstall"));
    auto apply = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeApply"));
    auto remove = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeRemove"));
    auto shutdown = reinterpret_cast<VoidFn>(GetProcAddress(module, "HookRuntimeShutdown"));
    auto canUnload = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeCanUnload"));
    auto holdDetour = reinterpret_cast<HoldFn>(GetProcAddress(module, "HookRuntimeHoldDetour"));
    auto insideDetour = reinterpret_cast<InsideFn>(GetProcAddress(module, "HookRuntimeInsideDetour"));
    auto targetAddress = reinterpret_cast<AddressFn>(GetProcAddress(module, "HookRuntimeTargetAddress"));
    CHECK(target && disasm && disasmAddress && begin && install && apply && remove &&
          shutdown && canUnload && holdDetour && insideDetour && targetAddress);

    // Load a second copy of the same test DLL under a different path to model
    // two independently linked mod DLLs in one shell process.
    wchar_t tempDirectory[MAX_PATH]{};
    const DWORD tempLength = GetTempPathW(_countof(tempDirectory), tempDirectory);
    CHECK(tempLength > 0 && tempLength < _countof(tempDirectory));
    wchar_t duplicatePath[MAX_PATH]{};
    CHECK(swprintf_s(duplicatePath, _countof(duplicatePath),
        L"%s00047-hook-registry-%lu.dll", tempDirectory, GetCurrentProcessId()) > 0);
    DeleteFileW(duplicatePath);
    CHECK(CopyFileW(argv[1], duplicatePath, FALSE) != FALSE);
    HMODULE secondModule = LoadLibraryW(duplicatePath);
    CHECK(secondModule != nullptr);
    auto externalBegin = reinterpret_cast<SimpleBoolFn>(GetProcAddress(secondModule, "HookRuntimeExternalBegin"));
    auto externalInstall = reinterpret_cast<ExternalInstallFn>(GetProcAddress(secondModule, "HookRuntimeExternalInstall"));
    auto externalApply = reinterpret_cast<SimpleBoolFn>(GetProcAddress(secondModule, "HookRuntimeExternalApply"));
    auto externalRemove = reinterpret_cast<ExternalRemoveFn>(GetProcAddress(secondModule, "HookRuntimeExternalRemove"));
    auto externalShutdown = reinterpret_cast<VoidFn>(GetProcAddress(secondModule, "HookRuntimeExternalShutdown"));
    auto externalCanUnload = reinterpret_cast<SimpleBoolFn>(GetProcAddress(secondModule, "HookRuntimeExternalCanUnload"));
    CHECK(externalBegin && externalInstall && externalApply &&
          externalRemove && externalShutdown && externalCanUnload);

    CHECK(target(5) == 7);
    TestDisasmResult disasmResult{};
    CHECK(disasm(&disasmResult) == TRUE);
    CHECK(disasmResult.length > 0 && disasmResult.length <= 15);
    CHECK(disasmResult.text[0] != '\0');

    const unsigned char movRaxRbx[] = {0x48, 0x89, 0xD8};
    void* codePage = VirtualAlloc(nullptr, 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    CHECK(codePage != nullptr);
    std::memcpy(codePage, movRaxRbx, sizeof(movRaxRbx));
    DWORD oldProtection = 0;
    CHECK(VirtualProtect(codePage, 4096, PAGE_EXECUTE_READ, &oldProtection) != FALSE);
    TestDisasmResult formattedInstruction{};
    const BOOL decodedKnownInstruction = disasmAddress(codePage, &formattedInstruction);
    VirtualFree(codePage, 0, MEM_RELEASE);
    CHECK(decodedKnownInstruction == TRUE);
    CHECK(formattedInstruction.length == sizeof(movRaxRbx));
    CHECK(std::strstr(formattedInstruction.text, "mov") != nullptr ||
          std::strstr(formattedInstruction.text, "MOV") != nullptr);
    CHECK(std::strstr(formattedInstruction.text, "rax") != nullptr ||
          std::strstr(formattedInstruction.text, "RAX") != nullptr);
    CHECK(std::strstr(formattedInstruction.text, "rbx") != nullptr ||
          std::strstr(formattedInstruction.text, "RBX") != nullptr);
    CHECK(begin() == TRUE);
    CHECK(install() == TRUE);
    CHECK(target(5) == 7); // registering a hook is deferred until apply
    CHECK(apply() == TRUE);
    CHECK(target(5) == 17);

    // A second mod DLL must not be able to claim the same target address.
    void* targetFunction = targetAddress();
    void* externalOriginal = nullptr;
    CHECK(targetFunction != nullptr);
    CHECK(externalBegin() == TRUE);
    CHECK(externalInstall(targetFunction, &externalOriginal) == FALSE);
    CHECK(externalOriginal == nullptr);
    CHECK(target(5) == 17);

    CHECK(remove() == TRUE);
    CHECK(target(5) == 17); // removals are queued too
    CHECK(apply() == TRUE);
    CHECK(target(5) == 7);
    CHECK(canUnload() == FALSE); // lifecycle still accepts new hooks
    shutdown();
    CHECK(canUnload() == TRUE);

    // Once the first DLL releases the target claim, the second DLL can claim
    // it. The first module must remain mapped while another mod hooks its code.
    CHECK(externalInstall(targetFunction, &externalOriginal) == TRUE);
    CHECK(externalOriginal != nullptr);
    CHECK(externalApply() == TRUE);
    CHECK(target(5) == 17);
    CHECK(canUnload() == FALSE);

    CHECK(externalRemove(targetFunction) == TRUE);
    CHECK(externalApply() == TRUE);
    CHECK(target(5) == 7);
    externalShutdown();
    CHECK(externalCanUnload() == TRUE);
    CHECK(FreeLibrary(secondModule) != FALSE);
    secondModule = nullptr;
    CHECK(DeleteFileW(duplicatePath) != FALSE);

    // Re-claiming the exact target from the first DLL confirms the cross-mod
    // registry entry was released after the second DLL removed its hook.
    CHECK(begin() == TRUE);
    CHECK(install() == TRUE);
    CHECK(apply() == TRUE);
    CHECK(target(5) == 17);
    CHECK(remove() == TRUE);
    CHECK(apply() == TRUE);
    shutdown();
    CHECK(canUnload() == TRUE);

    // Re-enter an initialized hook lifecycle and hold a detour on a second
    // thread while shutdown runs. The module must not become unloadable until
    // that thread returns out of its detour and the DLL image.
    CHECK(begin() == TRUE);
    CHECK(install() == TRUE);
    CHECK(apply() == TRUE);
    holdDetour(TRUE);
    std::atomic<int> callResult{0};
    std::thread caller([&] {
        callResult.store(target(9), std::memory_order_release);
    });

    bool entered = false;
    for (int i = 0; i < 2000; ++i) {
        if (insideDetour() == TRUE) {
            entered = true;
            break;
        }
        Sleep(1);
    }
    if (!entered) {
        holdDetour(FALSE);
        caller.join();
        shutdown();
        FreeLibrary(module);
        std::fprintf(stderr, "timed out waiting for the detour thread to enter\n");
        return 30;
    }

    shutdown();
    const BOOL unsafeWhileActive = canUnload();
    holdDetour(FALSE);
    caller.join();

    if (unsafeWhileActive != FALSE) {
        FreeLibrary(module);
        std::fprintf(stderr, "quiescence check incorrectly approved unload while detour was active\n");
        return 31;
    }
    if (callResult.load(std::memory_order_acquire) != 123) {
        FreeLibrary(module);
        std::fprintf(stderr, "blocked detour did not return the expected sentinel value\n");
        return 32;
    }
    if (canUnload() != TRUE) {
        FreeLibrary(module);
        std::fprintf(stderr, "quiescence check did not recover after detour thread exited\n");
        return 33;
    }

    CHECK(FreeLibrary(module) != FALSE);
    return 0;
}
