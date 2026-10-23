#include <Windows.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>
#include <thread>

namespace {
using InitializeFn = BOOL (WINAPI*)();
using ShutdownFn = void (WINAPI*)();
using IsInitializedFn = BOOL (WINAPI*)();
using CanUnloadFn = BOOL (WINAPI*)();
using CountFn = unsigned long (WINAPI*)();
using ReloadRequestedFn = BOOL (WINAPI*)();
using BlockCallbackFn = void (WINAPI*)(BOOL);
using CallbackEnteredFn = BOOL (WINAPI*)();

bool WriteText(const std::filesystem::path& path, const char* text) {
    std::ofstream file(path, std::ios::binary | std::ios::trunc);
    if (!file) return false;
    file << text;
    file.close();
    return file.good();
}
} // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) {
        std::wcerr << L"Usage: 00047_windhawk_settings_changed_runtime_test.exe <smoke-dll>\n";
        return 2;
    }

    std::error_code ec;
    const std::filesystem::path sourceDll = std::filesystem::absolute(argv[1], ec);
    if (ec || !std::filesystem::is_regular_file(sourceDll, ec) || ec) {
        std::wcerr << L"Settings-change smoke DLL does not exist.\n";
        return 2;
    }

    const auto packageDir = std::filesystem::temp_directory_path() /
        (L"00047-settings-changed-runtime-" +
         std::to_wstring(GetCurrentProcessId()) + L"-" +
         std::to_wstring(GetTickCount64()));
    std::filesystem::create_directories(packageDir, ec);
    if (ec || !std::filesystem::is_directory(packageDir, ec) || ec) {
        std::wcerr << L"Could not create temporary mod package directory.\n";
        return 2;
    }

    const auto packageDll = packageDir / L"settings_changed_smoke.dll";
    if (!std::filesystem::copy_file(
            sourceDll, packageDll,
            std::filesystem::copy_options::overwrite_existing, ec) || ec) {
        std::wcerr << L"Could not stage settings-change smoke DLL.\n";
        std::filesystem::remove_all(packageDir, ec);
        return 2;
    }

    const auto manifest = packageDir / L"manifest.json";
    const auto settings = packageDir / L"settings.json";
    if (!WriteText(manifest,
            R"json({"manifestVersion":1,"id":"com.example.settings-watch-test","name":"Settings Watch Test","version":"1.0.0","architecture":"x64","module":"settings_changed_smoke.dll","targets":["explorer.exe"]})json")
        || !WriteText(settings, R"json({"intSetting":101})json")) {
        std::wcerr << L"Could not create temporary mod package files.\n";
        std::filesystem::remove_all(packageDir, ec);
        return 2;
    }

    HMODULE module = LoadLibraryW(packageDll.c_str());
    InitializeFn initialize = module
        ? reinterpret_cast<InitializeFn>(GetProcAddress(module, "WmodInitializeV1"))
        : nullptr;
    ShutdownFn shutdown = module
        ? reinterpret_cast<ShutdownFn>(GetProcAddress(module, "WmodShutdownV1"))
        : nullptr;
    IsInitializedFn isInitialized = module
        ? reinterpret_cast<IsInitializedFn>(GetProcAddress(module, "WmodIsInitializedV1"))
        : nullptr;
    CanUnloadFn canUnload = module
        ? reinterpret_cast<CanUnloadFn>(GetProcAddress(module, "WmodCanUnloadV1"))
        : nullptr;
    CountFn getCount = module
        ? reinterpret_cast<CountFn>(GetProcAddress(module, "AdapterSettingsChangedCount"))
        : nullptr;
    ReloadRequestedFn isReloadRequested = module
        ? reinterpret_cast<ReloadRequestedFn>(GetProcAddress(module, "WmodIsReloadRequestedV1"))
        : nullptr;
    BlockCallbackFn blockCallback = module
        ? reinterpret_cast<BlockCallbackFn>(GetProcAddress(module, "AdapterSettingsChangedBlock"))
        : nullptr;
    CallbackEnteredFn callbackEntered = module
        ? reinterpret_cast<CallbackEnteredFn>(GetProcAddress(module, "AdapterSettingsChangedCallbackEntered"))
        : nullptr;
    bool initialized = false;
    bool retainPackageDir = false;

    auto fail = [&](const wchar_t* message) -> int {
        std::wcerr << message << L"\n";
        if (blockCallback) blockCallback(FALSE);
        if (initialized && shutdown) {
            shutdown();
            initialized = false;
        }
        if (module) {
            if (canUnload && canUnload()) {
                FreeLibrary(module);
                module = nullptr;
            } else {
                // Fail closed: don't unmap a test DLL if the lifecycle query
                // cannot confirm it has no in-flight callback frames.
                retainPackageDir = true;
                module = nullptr;
            }
        }
        if (!retainPackageDir) std::filesystem::remove_all(packageDir, ec);
        return 1;
    };

    if (!module || !initialize || !shutdown || !isInitialized || !canUnload ||
        !getCount || !isReloadRequested || !blockCallback || !callbackEntered) {
        return fail(L"Settings-change smoke DLL is missing required lifecycle/query exports.");
    }
    if (!initialize()) {
        return fail(L"WmodInitializeV1 rejected the settings-change watcher.");
    }
    initialized = true;
    if (!isInitialized()) {
        return fail(L"WmodIsInitializedV1 did not report the active lifecycle.");
    }

    // WmodInitializeV1 returns only after ReadDirectoryChangesW is armed. Modify
    // the settings file; the smoke callback performs a second write itself.
    if (!WriteText(settings, R"json({"intSetting":102})json")) {
        return fail(L"Could not trigger the initial settings change.");
    }

    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    while (getCount() < 2 && std::chrono::steady_clock::now() < deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
    }
    const unsigned long callbackCount = getCount();
    if (callbackCount < 2) {
        return fail(L"Settings-change callback did not re-arm after its own settings write.");
    }
    // The request is non-consuming so the host can retry a safe unload later.
    if (isReloadRequested() != TRUE || isReloadRequested() != TRUE) {
        return fail(L"BOOL Wh_ModSettingsChanged(BOOL*) did not persist its reload request.");
    }

    shutdown();
    initialized = false;
    const unsigned long stoppedCount = getCount();
    std::this_thread::sleep_for(std::chrono::milliseconds(250));
    if (getCount() != stoppedCount) {
        return fail(L"Settings-change callback ran after WmodShutdownV1 returned.");
    }

    // Re-enable and deliberately block the worker inside mod callback code.
    // Shutdown must return promptly, retain hook/DLL state, and only finish
    // lifecycle callbacks after the worker eventually exits.
    if (!initialize() || !isInitialized()) {
        return fail(L"Settings-change adapter did not restart after a clean shutdown.");
    }
    initialized = true;
    blockCallback(TRUE);
    if (!WriteText(settings, R"json({"intSetting":104})json")) {
        return fail(L"Could not trigger the deliberately blocked settings callback.");
    }

    const auto callbackDeadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    while (callbackEntered() != TRUE &&
           std::chrono::steady_clock::now() < callbackDeadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    if (callbackEntered() != TRUE) {
        return fail(L"Settings callback did not enter its blocking test gate.");
    }

    const auto shutdownStarted = std::chrono::steady_clock::now();
    shutdown();
    const auto shutdownElapsed = std::chrono::steady_clock::now() - shutdownStarted;
    if (shutdownElapsed > std::chrono::seconds(2)) {
        blockCallback(FALSE);
        return fail(L"Shutdown blocked on the in-flight settings callback instead of timing out.");
    }
    if (canUnload() != FALSE) {
        blockCallback(FALSE);
        return fail(L"Unload was incorrectly approved while settings callback code was still executing.");
    }

    blockCallback(FALSE);
    const auto finalizeDeadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    BOOL unloadSafe = FALSE;
    while (std::chrono::steady_clock::now() < finalizeDeadline) {
        unloadSafe = canUnload();
        if (unloadSafe == TRUE) break;
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
    }
    if (unloadSafe != TRUE || isInitialized() != FALSE) {
        return fail(L"Deferred shutdown did not finalize after the settings callback exited.");
    }
    initialized = false;

    FreeLibrary(module);
    module = nullptr;
    std::filesystem::remove_all(packageDir, ec);
    std::wcout << L"Settings-change adapter runtime test passed (callbacks: "
               << stoppedCount << L", deferred shutdown recovered).\n";
    return 0;
}
