#include <Windows.h>
#include <iostream>
#include <atomic>
#include <thread>
#include "windhawk_nonhook_compat.h"

#if defined(_MSC_VER)
#pragma optimize("", off)
#endif
extern "C" __declspec(noinline) int CompatibilityProbeSymbol() {
    return 47;
}
#if defined(_MSC_VER)
#pragma optimize("", on)
#endif

static void* g_originalProbe = nullptr;
extern "C" __declspec(noinline) int CompatibilityProbeDetour() {
    auto original = reinterpret_cast<int(__cdecl*)()>(g_originalProbe);
    return original() + 10;
}

#define CHECK(condition) do { \
    if (!(condition)) { \
        std::wcerr << L"FAILED check at line " << __LINE__ << L"\n"; \
        return 1; \
    } \
} while (false)

int wmain() {
    CHECK(CompatibilityProbeSymbol() == 47);

    WH_FIND_SYMBOL first{};
    WH_FIND_SYMBOL_OPTIONS invalidOptions{};
    invalidOptions.optionsSize = 0;
    invalidOptions.symbolServer = L"";
    CHECK(Wh_FindFirstSymbol(GetModuleHandleW(nullptr), &invalidOptions, &first) == nullptr);
    CHECK(first.address == nullptr && first.symbol == nullptr && first.symbolDecorated == nullptr);

    WH_FIND_SYMBOL_OPTIONS options{};
    options.optionsSize = sizeof(options);
    // Disable network access in this test. The target executable is built with
    // a local PDB so the test does not depend on the Microsoft symbol server.
    options.symbolServer = L"";
    options.noUndecoratedSymbols = FALSE;

    HANDLE search = Wh_FindFirstSymbol(GetModuleHandleW(nullptr), &options, &first);
    CHECK(search != nullptr);
    bool foundProbe = false;
    WH_FIND_SYMBOL current = first;
    do {
        if (current.address == reinterpret_cast<void*>(&CompatibilityProbeSymbol) ||
            (current.symbol && std::wcscmp(current.symbol, L"CompatibilityProbeSymbol") == 0) ||
            (current.symbolDecorated &&
             std::wcscmp(current.symbolDecorated, L"CompatibilityProbeSymbol") == 0)) {
            foundProbe = true;
            break;
        }
    } while (Wh_FindNextSymbol(search, &current));
    Wh_FindCloseSymbol(search);
    CHECK(foundProbe);

    // Decorated-only enumeration still returns a decorated name and address.
    options.noUndecoratedSymbols = TRUE;
    search = Wh_FindFirstSymbol(GetModuleHandleW(nullptr), &options, &first);
    CHECK(search != nullptr);
    CHECK(first.address != nullptr);
    CHECK(first.symbolDecorated != nullptr);
    CHECK(first.symbol == nullptr);
    Wh_FindCloseSymbol(search);

    // Resolve a symbol name, queue its hook, apply it and restore the target.
    struct ProbeSelector {
        PCWSTR string;
        size_t length;
    } probeSelector{L"CompatibilityProbeSymbol", wcslen(L"CompatibilityProbeSymbol")};
    void* originalProbe = nullptr;
    WH_SYMBOL_HOOK probeHook{};
    probeHook.symbols = reinterpret_cast<const decltype(probeHook.symbols)>(&probeSelector);
    probeHook.symbolsCount = 1;
    probeHook.pOriginalFunction = &originalProbe;
    probeHook.hookFunction = reinterpret_cast<void*>(&CompatibilityProbeDetour);
    probeHook.optional = false;

    WH_HOOK_SYMBOLS_OPTIONS hookOptions{};
    hookOptions.optionsSize = sizeof(hookOptions);
    hookOptions.symbolServer = L"";
    hookOptions.noUndecoratedSymbols = FALSE;
    std::wstring resolvedCacheBase;
    hookOptions.onlineCacheUrl = L"https://example.invalid/cache";
    CHECK(wmod::windhawk_symbol_compat::ResolveOnlineCacheBase(
        &hookOptions, resolvedCacheBase) == TRUE);
    CHECK(resolvedCacheBase == L"https://example.invalid/cache/");
    hookOptions.onlineCacheUrl = L"http://example.invalid/cache/";
    CHECK(wmod::windhawk_symbol_compat::ResolveOnlineCacheBase(
        &hookOptions, resolvedCacheBase) == FALSE);
    hookOptions.onlineCacheUrl = L"";
    CHECK(wmod::windhawk_symbol_compat::ResolveOnlineCacheBase(
        &hookOptions, resolvedCacheBase) == FALSE);
    // Windhawk queues hooks while Wh_ModInit is running. Begin an explicit
    // hook lifecycle in this standalone API test, just as the native adapter does.
    CHECK(wmod::windhawk_hook_compat::BeginHookOperations() == TRUE);
    CHECK(Wh_HookSymbols(GetModuleHandleW(nullptr), &probeHook, 1, &hookOptions) == TRUE);
    CHECK(originalProbe != nullptr);
    g_originalProbe = originalProbe;
    CHECK(CompatibilityProbeSymbol() == 47);
    CHECK(Wh_ApplyHookOperations() == TRUE);
    CHECK(CompatibilityProbeSymbol() == 57);
    CHECK(Wh_RemoveFunctionHook(reinterpret_cast<void*>(&CompatibilityProbeSymbol)) == TRUE);
    CHECK(Wh_ApplyHookOperations() == TRUE);
    CHECK(CompatibilityProbeSymbol() == 47);

    // A null detour requests symbol discovery only.
    void* discoveredProbe = nullptr;
    WH_SYMBOL_HOOK lookupHook{};
    lookupHook.symbols = reinterpret_cast<const decltype(lookupHook.symbols)>(&probeSelector);
    lookupHook.symbolsCount = 1;
    lookupHook.pOriginalFunction = &discoveredProbe;
    lookupHook.hookFunction = nullptr;
    lookupHook.optional = false;
    const auto cacheHitsBeforeLookup =
        wmod::windhawk_symbol_compat::HookCacheHitCount();
    CHECK(Wh_HookSymbols(GetModuleHandleW(nullptr), &lookupHook, 1, &hookOptions) == TRUE);
    CHECK(discoveredProbe == reinterpret_cast<void*>(&CompatibilityProbeSymbol));
    CHECK(wmod::windhawk_symbol_compat::HookCacheHitCount() > cacheHitsBeforeLookup);

    // Reject a cache that attempts to redirect a valid selector outside the
    // loaded PE image; no arbitrary address from cache contents is trusted.
    MODULEINFO cacheModuleInfo{};
    CHECK(GetModuleInformation(GetCurrentProcess(), GetModuleHandleW(nullptr),
        &cacheModuleInfo, sizeof(cacheModuleInfo)) != FALSE);
    wmod::windhawk_symbol_compat::HookCacheIdentity cacheIdentity{};
    CHECK(wmod::windhawk_symbol_compat::BuildHookCacheIdentity(
        GetModuleHandleW(nullptr), cacheModuleInfo, cacheIdentity));
    const auto symbolMutexName =
        wmod::windhawk_symbol_compat::HookSymbolCacheMutexName(cacheIdentity);
    CHECK(!symbolMutexName.empty());
    CHECK(symbolMutexName.find(L"Local\\00047SymbolGetOnlineCacheMutex-") == 0);
    CHECK(symbolMutexName.size() < 256);
    CHECK(symbolMutexName ==
        wmod::windhawk_symbol_compat::HookSymbolCacheMutexName(cacheIdentity));

    {
        wmod::windhawk_symbol_compat::HookSymbolCacheLoadLock ownerLock;
        CHECK(ownerLock.Acquire(cacheIdentity, 1000) == TRUE);
        std::atomic<bool> contenderAcquired{true};
        std::thread contender([&] {
            wmod::windhawk_symbol_compat::HookSymbolCacheLoadLock contenderLock;
            contenderAcquired.store(contenderLock.Acquire(cacheIdentity, 75),
                std::memory_order_release);
        });
        contender.join();
        CHECK(contenderAcquired.load(std::memory_order_acquire) == FALSE);
    }
    {
        wmod::windhawk_symbol_compat::HookSymbolCacheLoadLock releasedLock;
        CHECK(releasedLock.Acquire(cacheIdentity, 1000) == TRUE);
    }

    const std::wstring invalidOffsetCache =
        L"1#" + cacheIdentity.headerModuleName + L"#" + cacheIdentity.version +
        L"#CompatibilityProbeSymbol#" + std::to_wstring(cacheIdentity.imageSize + 1);
    std::vector<bool> cacheFound;
    std::vector<wmod::windhawk_symbol_compat::HookCachedResolution> cacheResolved;
    CHECK(wmod::windhawk_symbol_compat::ResolveHookCacheText(
        invalidOffsetCache, cacheIdentity, &lookupHook, 1, cacheFound, cacheResolved) == FALSE);

    // Windhawk registry cache interoperability uses a disposable HKCU key.
    std::wstring windhawkSubkey;
    CHECK(wmod::windhawk_symbol_compat::BuildWindhawkRegistryCacheSubkey(
        L"com.example.symbol-cache-test", windhawkSubkey) == TRUE);
    CHECK(windhawkSubkey ==
        L"SOFTWARE\\Windhawk\\Engine\\ModsWritable\\com.example.symbol-cache-test\\SymbolCache");
    CHECK(wmod::windhawk_symbol_compat::BuildWindhawkRegistryCacheSubkey(
        L"bad\\subkey", windhawkSubkey) == FALSE);

    const std::wstring registryTestKey =
        L"Software\\00047Tests\\SymbolCache\\" + std::to_wstring(GetCurrentProcessId());
    RegDeleteTreeW(HKEY_CURRENT_USER, registryTestKey.c_str());
    HKEY registryTestHandle = nullptr;
    CHECK(RegCreateKeyExW(HKEY_CURRENT_USER, registryTestKey.c_str(), 0, nullptr, 0,
        KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY, nullptr,
        &registryTestHandle, nullptr) == ERROR_SUCCESS);
    RegCloseKey(registryTestHandle);

    const std::wstring validRegistryCache =
        L"1#" + cacheIdentity.headerModuleName + L"#" + cacheIdentity.version +
        L"#CompatibilityProbeSymbol#" +
        std::to_wstring(reinterpret_cast<std::uintptr_t>(&CompatibilityProbeSymbol) -
            cacheIdentity.moduleBase);
    CHECK(wmod::windhawk_symbol_compat::WriteRegistryHookCacheValue(
        HKEY_CURRENT_USER, registryTestKey, L"pdb_test", validRegistryCache) == TRUE);
    std::wstring loadedRegistryCache;
    CHECK(wmod::windhawk_symbol_compat::ReadRegistryHookCacheValue(
        HKEY_CURRENT_USER, registryTestKey, L"pdb_test", loadedRegistryCache) == TRUE);
    CHECK(loadedRegistryCache == validRegistryCache);
    CHECK(wmod::windhawk_symbol_compat::ResolveHookCacheText(
        loadedRegistryCache, cacheIdentity, &lookupHook, 1, cacheFound, cacheResolved) == TRUE);

    const std::wstring embeddedNul(L"invalid\0cache", 13);
    CHECK(wmod::windhawk_symbol_compat::WriteRegistryHookCacheValue(
        HKEY_CURRENT_USER, registryTestKey, L"pdb_bad", embeddedNul) == FALSE);
    std::wstring oversizedRegistryCache(
        wmod::windhawk_symbol_compat::kMaxWindhawkRegistryCacheBytes / sizeof(wchar_t), L'x');
    CHECK(wmod::windhawk_symbol_compat::WriteRegistryHookCacheValue(
        HKEY_CURRENT_USER, registryTestKey, L"pdb_large", oversizedRegistryCache) == FALSE);

    DWORD badRegistryType = 7;
    CHECK(RegOpenKeyExW(HKEY_CURRENT_USER, registryTestKey.c_str(), 0,
        KEY_SET_VALUE | KEY_WOW64_64KEY, &registryTestHandle) == ERROR_SUCCESS);
    CHECK(RegSetValueExW(registryTestHandle, L"pdb_wrong_type", 0, REG_DWORD,
        reinterpret_cast<const BYTE*>(&badRegistryType), sizeof(badRegistryType)) == ERROR_SUCCESS);
    RegCloseKey(registryTestHandle);
    CHECK(wmod::windhawk_symbol_compat::ReadRegistryHookCacheValue(
        HKEY_CURRENT_USER, registryTestKey, L"pdb_wrong_type", loadedRegistryCache) == FALSE);

    std::wstring mergedRegistryCache;
    const std::wstring additionalCache =
        L"1#" + cacheIdentity.headerModuleName + L"#" + cacheIdentity.version +
        L"#AnotherSymbolForMerge#" +
        std::to_wstring(reinterpret_cast<std::uintptr_t>(&CompatibilityProbeSymbol) -
            cacheIdentity.moduleBase);
    CHECK(wmod::windhawk_symbol_compat::MergeWindhawkHookCacheText(
        cacheIdentity, validRegistryCache, additionalCache, mergedRegistryCache) == TRUE);
    CHECK(mergedRegistryCache.find(L"#CompatibilityProbeSymbol#") != std::wstring::npos);
    CHECK(mergedRegistryCache.find(L"#AnotherSymbolForMerge#") != std::wstring::npos);
    RegDeleteTreeW(HKEY_CURRENT_USER, registryTestKey.c_str());

    wmod::windhawk_symbol_compat::WindhawkErrorThrottleContext windhawkErrorContext{};
    windhawkErrorContext.processId = 3210;
    windhawkErrorContext.processCreationTime = 132500000000000000ull;
    windhawkErrorContext.modVersion = L"1.4.2";
    windhawkErrorContext.libraryFileName = L"example-mod.dll";
    windhawkErrorContext.targetModuleStamp = L"explorer.exe-123456-7654321";
    const ULONGLONG windhawkErrorTime = 132600000000000000ull;
    const auto windhawkErrorRecord =
        wmod::windhawk_symbol_compat::BuildWindhawkErrorThrottleRecord(
            windhawkErrorContext, windhawkErrorTime);
    CHECK(windhawkErrorRecord.rfind(L"error:3#", 0) == 0);
    CHECK(wmod::windhawk_symbol_compat::IsWindhawkErrorThrottleRecordActive(
        windhawkErrorRecord, windhawkErrorContext,
        windhawkErrorTime + 30'000'000ull, 60'000) == TRUE);
    CHECK(wmod::windhawk_symbol_compat::IsWindhawkErrorThrottleRecordActive(
        windhawkErrorRecord, windhawkErrorContext,
        windhawkErrorTime + 60'000ull * 10'000ull + 1, 60'000) == FALSE);
    auto wrongSession = windhawkErrorContext;
    ++wrongSession.processId;
    CHECK(wmod::windhawk_symbol_compat::IsWindhawkErrorThrottleRecordActive(
        windhawkErrorRecord, wrongSession,
        windhawkErrorTime + 30'000'000ull, 60'000) == FALSE);
    auto wrongModBuild = windhawkErrorContext;
    wrongModBuild.libraryFileName = L"rebuilt-mod.dll";
    CHECK(wmod::windhawk_symbol_compat::IsWindhawkErrorThrottleRecordActive(
        windhawkErrorRecord, wrongModBuild,
        windhawkErrorTime + 30'000'000ull, 60'000) == FALSE);
    auto wrongTarget = windhawkErrorContext;
    wrongTarget.targetModuleStamp = L"explorer.exe-123457-7654321";
    CHECK(wmod::windhawk_symbol_compat::IsWindhawkErrorThrottleRecordActive(
        windhawkErrorRecord, wrongTarget,
        windhawkErrorTime + 30'000'000ull, 60'000) == FALSE);
    CHECK(wmod::windhawk_symbol_compat::IsWindhawkErrorThrottleRecordActive(
        L"error:2#3210#bad", windhawkErrorContext,
        windhawkErrorTime + 30'000'000ull, 60'000) == FALSE);

    // Repeated required-symbol failures are throttled briefly, but expired,
    // corrupt, clock-reversed, or different-mod markers must not suppress retry.
    std::wstring currentModStamp;
    CHECK(wmod::windhawk_symbol_compat::CurrentModBinaryStamp(currentModStamp) == TRUE);
    CHECK(currentModStamp.size() == 16);

    const auto throttleDirectory = std::filesystem::temp_directory_path() /
        (L"00047-symbol-throttle-" + std::to_wstring(GetCurrentProcessId()));
    std::filesystem::create_directories(throttleDirectory);
    const auto throttlePath = throttleDirectory / L"symbol-error.txt";
    DeleteFileW(throttlePath.c_str());
    constexpr ULONGLONG kTestFileTime = 132500000000000000ull;
    constexpr ULONGLONG kTestThrottleMs = 60'000;
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        throttlePath, currentModStamp, kTestFileTime, kTestThrottleMs) == FALSE);
    CHECK(wmod::windhawk_symbol_compat::WriteHookSymbolFailureThrottle(
        throttlePath, currentModStamp, kTestFileTime) == TRUE);
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        throttlePath, currentModStamp, kTestFileTime + 10'000'000ull,
        kTestThrottleMs) == TRUE);
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        throttlePath, L"different-mod-build", kTestFileTime + 10'000'000ull,
        kTestThrottleMs) == FALSE);
    CHECK(wmod::windhawk_symbol_compat::WriteHookSymbolFailureThrottle(
        throttlePath, currentModStamp, kTestFileTime) == TRUE);
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        throttlePath, currentModStamp,
        kTestFileTime + kTestThrottleMs * 10'000ull + 1,
        kTestThrottleMs) == FALSE);
    CHECK(wmod::windhawk_symbol_compat::WriteHookSymbolFailureThrottle(
        throttlePath, currentModStamp, kTestFileTime) == TRUE);
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        throttlePath, currentModStamp, kTestFileTime - 1,
        kTestThrottleMs) == FALSE);
    CHECK(wmod::windhawk_symbol_compat::WriteLocalHookCache(
        throttlePath, L"corrupt-throttle-marker") == TRUE);
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        throttlePath, currentModStamp, kTestFileTime + 10'000'000ull,
        kTestThrottleMs) == FALSE);
    std::filesystem::remove_all(throttleDirectory);

    // Missing required names fail without publishing partial symbol results.
    struct MissingSelector {
        PCWSTR string;
        size_t length;
    } missingSelector{L"00047DefinitelyMissingSymbol", wcslen(L"00047DefinitelyMissingSymbol")};
    void* untouched = reinterpret_cast<void*>(static_cast<ULONG_PTR>(1));
    WH_SYMBOL_HOOK missingHook{};
    missingHook.symbols = reinterpret_cast<const decltype(missingHook.symbols)>(&missingSelector);
    missingHook.symbolsCount = 1;
    missingHook.pOriginalFunction = &untouched;
    missingHook.optional = false;
    auto missingFailurePath = wmod::windhawk_symbol_compat::HookCachePath(
        cacheIdentity, &missingHook, 1, hookOptions.noUndecoratedSymbols);
    missingFailurePath += L".error";
    DeleteFileW(missingFailurePath.c_str());
    CHECK(Wh_HookSymbols(GetModuleHandleW(nullptr), &missingHook, 1, &hookOptions) == FALSE);
    CHECK(untouched == reinterpret_cast<void*>(static_cast<ULONG_PTR>(1)));
    CHECK(wmod::windhawk_symbol_compat::ShouldThrottleHookSymbolFailure(
        missingFailurePath, currentModStamp,
        wmod::windhawk_symbol_compat::CurrentUtcFileTimeTicks(),
        wmod::windhawk_symbol_compat::kHookSymbolFailureThrottleMs) == TRUE);
    // The same required selector is suppressed for the short retry window.
    CHECK(Wh_HookSymbols(GetModuleHandleW(nullptr), &missingHook, 1, &hookOptions) == FALSE);

    // An absent optional hook is a success and leaves its output untouched.
    missingHook.optional = true;
    CHECK(Wh_HookSymbols(GetModuleHandleW(nullptr), &missingHook, 1, &hookOptions) == TRUE);
    CHECK(untouched == reinterpret_cast<void*>(static_cast<ULONG_PTR>(1)));

    // NULL handles are explicitly safe no-ops.
    CHECK(Wh_FindNextSymbol(nullptr, &first) == FALSE);
    Wh_FindCloseSymbol(nullptr);
    wmod::windhawk_hook_compat::ShutdownHookOperations();
    CHECK(wmod::windhawk_hook_compat::CanUnload() == TRUE);
    std::wcout << L"DbgHelp symbol compatibility tests passed.\n";
    return 0;
}
