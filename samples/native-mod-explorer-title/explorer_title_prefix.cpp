#include "00047_mod_api.h"
#include "00047_mod_services.h"
#include "windhawk_nonhook_adapter.h"

#include <atomic>
#include <mutex>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace {
constexpr wchar_t kTitlePrefix[] = L"[00047] ";

using SetWindowTextWFn = BOOL(WINAPI*)(HWND, LPCWSTR);
void* g_originalSetWindowTextWAddress = nullptr;
std::mutex g_titlesMutex;
std::unordered_map<HWND, std::wstring> g_originalTitles;
std::atomic<bool> g_stopping{false};
std::atomic<unsigned long> g_activeDetours{0};

class ActiveDetour final {
public:
    ActiveDetour() noexcept {
        g_activeDetours.fetch_add(1, std::memory_order_acq_rel);
    }
    ~ActiveDetour() {
        g_activeDetours.fetch_sub(1, std::memory_order_acq_rel);
    }
    ActiveDetour(const ActiveDetour&) = delete;
    ActiveDetour& operator=(const ActiveDetour&) = delete;
};

SetWindowTextWFn OriginalSetWindowTextW() noexcept {
    return reinterpret_cast<SetWindowTextWFn>(g_originalSetWindowTextWAddress);
}

bool HasPrefix(const std::wstring& title) noexcept {
    const std::wstring prefix(kTitlePrefix);
    return title.size() >= prefix.size() &&
        title.compare(0, prefix.size(), prefix) == 0;
}

bool IsExplorerWindow(HWND hwnd) noexcept {
    if (!hwnd || !IsWindow(hwnd) || GetAncestor(hwnd, GA_ROOT) != hwnd) {
        return false;
    }

    DWORD processId = 0;
    if (GetWindowThreadProcessId(hwnd, &processId) == 0 ||
        processId != GetCurrentProcessId()) {
        return false;
    }

    wchar_t className[96]{};
    const int length = GetClassNameW(hwnd, className, static_cast<int>(_countof(className)));
    if (length <= 0 || length >= static_cast<int>(_countof(className))) {
        return false;
    }
    return wcscmp(className, L"CabinetWClass") == 0 ||
        wcscmp(className, L"ExploreWClass") == 0;
}

BOOL WINAPI SetWindowTextWDetour(HWND hwnd, LPCWSTR requestedTitle) {
    ActiveDetour active;
    const auto original = OriginalSetWindowTextW();
    if (!original) return FALSE;
    if (g_stopping.load(std::memory_order_acquire) ||
        !requestedTitle || !*requestedTitle || !IsExplorerWindow(hwnd)) {
        return original(hwnd, requestedTitle);
    }

    try {
        const std::wstring requested(requestedTitle);
        const std::wstring prefix(kTitlePrefix);
        bool passThrough = false;
        {
            std::lock_guard<std::mutex> lock(g_titlesMutex);
            const auto existing = g_originalTitles.find(hwnd);
            if (existing != g_originalTitles.end()) {
                const std::wstring alreadyPrefixed = prefix + existing->second;
                if (requested == alreadyPrefixed) {
                    passThrough = true;
                } else if (requested.size() >= prefix.size() &&
                    requested.compare(0, prefix.size(), prefix) == 0) {
                    // The caller supplied a different title that already has
                    // this prefix; it was not produced by this hook.
                    g_originalTitles.erase(existing);
                    passThrough = true;
                } else {
                    existing->second = requested;
                }
            } else if (requested.size() >= prefix.size() &&
                requested.compare(0, prefix.size(), prefix) == 0) {
                passThrough = true;
            } else {
                g_originalTitles.emplace(hwnd, requested);
            }
        }

        if (passThrough) return original(hwnd, requestedTitle);

        const std::wstring decorated = prefix + requested;
        const BOOL result = original(hwnd, decorated.c_str());
        if (result == FALSE) {
            std::lock_guard<std::mutex> lock(g_titlesMutex);
            const auto current = g_originalTitles.find(hwnd);
            if (current != g_originalTitles.end() && current->second == requested) {
                g_originalTitles.erase(current);
            }
        }
        return result;
    } catch (...) {
        // A cosmetic title decoration must not make SetWindowTextW fail merely
        // because the hook's bookkeeping could not allocate memory.
        return original(hwnd, requestedTitle);
    }
}

BOOL CALLBACK PrefixExistingWindow(HWND hwnd, LPARAM) {
    if (!IsExplorerWindow(hwnd)) return TRUE;
    try {
        const int length = GetWindowTextLengthW(hwnd);
        if (length <= 0 || length > 32768) return TRUE;

        std::vector<wchar_t> buffer(static_cast<size_t>(length) + 1, L'\0');
        const int copied = GetWindowTextW(hwnd, buffer.data(), static_cast<int>(buffer.size()));
        if (copied <= 0 || copied >= static_cast<int>(buffer.size())) return TRUE;

        const std::wstring title(buffer.data(), static_cast<size_t>(copied));
        const std::wstring prefix(kTitlePrefix);
        if (title.size() >= prefix.size() &&
            title.compare(0, prefix.size(), prefix) == 0) {
            return TRUE;
        }

        {
            std::lock_guard<std::mutex> lock(g_titlesMutex);
            if (!g_originalTitles.emplace(hwnd, title).second) return TRUE;
        }

        const std::wstring decorated = prefix + title;
        DWORD_PTR messageResult = 0;
        const LRESULT sent = SendMessageTimeoutW(hwnd, WM_SETTEXT, 0,
            reinterpret_cast<LPARAM>(decorated.c_str()),
            SMTO_ABORTIFHUNG | SMTO_BLOCK, 250, &messageResult);
        if (sent == 0 || messageResult == 0) {
            std::lock_guard<std::mutex> lock(g_titlesMutex);
            const auto current = g_originalTitles.find(hwnd);
            if (current != g_originalTitles.end() && current->second == title) {
                g_originalTitles.erase(current);
            }
            OutputDebugStringW(L"00047 Explorer title sample: skipped an unresponsive window.\n");
        }
    } catch (...) {
        OutputDebugStringW(L"00047 Explorer title sample: failed to process a window caption.\n");
    }
    return TRUE;
}

void RestoreTrackedTitles() noexcept {
    g_stopping.store(true, std::memory_order_release);
    for (unsigned int i = 0; i < 500 &&
            g_activeDetours.load(std::memory_order_acquire) != 0; ++i) {
        Sleep(1);
    }
    if (g_activeDetours.load(std::memory_order_acquire) != 0) {
        wmod::Log(L"warning", L"Skipped title restoration because a caption hook call is still active.");
        return;
    }

    std::vector<std::pair<HWND, std::wstring>> snapshot;
    try {
        std::lock_guard<std::mutex> lock(g_titlesMutex);
        snapshot.reserve(g_originalTitles.size());
        for (const auto& entry : g_originalTitles) {
            snapshot.emplace_back(entry.first, entry.second);
        }
    } catch (...) {
        wmod::Log(L"error", L"Could not allocate a title-restoration snapshot.");
        return;
    }

    std::vector<HWND> restored;
    try {
        restored.reserve(snapshot.size());
    } catch (...) {
        wmod::Log(L"error", L"Could not allocate title-restoration bookkeeping.");
        return;
    }
    for (const auto& entry : snapshot) {
        if (!IsWindow(entry.first) || !IsExplorerWindow(entry.first)) {
            restored.push_back(entry.first);
            continue;
        }

        DWORD_PTR messageResult = 0;
        const LRESULT sent = SendMessageTimeoutW(entry.first, WM_SETTEXT, 0,
            reinterpret_cast<LPARAM>(entry.second.c_str()),
            SMTO_ABORTIFHUNG | SMTO_BLOCK, 250, &messageResult);
        if (sent != 0 && messageResult != 0) {
            restored.push_back(entry.first);
        } else {
            wmod::Log(L"warning", L"Could not restore a File Explorer title within the bounded timeout.");
        }
    }

    std::lock_guard<std::mutex> lock(g_titlesMutex);
    for (const HWND hwnd : restored) g_originalTitles.erase(hwnd);
}

BOOL Wh_ModInit() {
    RestoreTrackedTitles();
    g_stopping.store(false, std::memory_order_release);
    g_originalSetWindowTextWAddress = nullptr;

    const HMODULE user32 = GetModuleHandleW(L"user32.dll");
    const auto target = user32 ? GetProcAddress(user32, "SetWindowTextW") : nullptr;
    if (!target) {
        wmod::Log(L"error", L"Could not resolve user32!SetWindowTextW.");
        return FALSE;
    }

    const BOOL queued = Wh_SetFunctionHook(
        reinterpret_cast<void*>(target),
        reinterpret_cast<void*>(&SetWindowTextWDetour),
        &g_originalSetWindowTextWAddress);
    if (!queued) {
        wmod::Log(L"error", L"Could not register the SetWindowTextW hook.");
        return FALSE;
    }
    wmod::Log(L"info", L"Registered the File Explorer title-prefix hook.");
    return TRUE;
}

void Wh_ModAfterInit() {
    EnumWindows(&PrefixExistingWindow, 0);
    wmod::Log(L"info", L"Applied the title prefix to responsive existing File Explorer windows.");
}

void Wh_ModBeforeUninit() {
    RestoreTrackedTitles();
}

void Wh_ModUninit() {
    // Retry restoration after the adapter has removed hooks. WM_SETTEXT is sent
    // directly, so this remains independent of the removed trampoline.
    RestoreTrackedTitles();
    g_originalSetWindowTextWAddress = nullptr;
}

} // namespace

WMOD_DEFINE_WINDHAWK_LIFECYCLE(
    Wh_ModInit,
    Wh_ModAfterInit,
    Wh_ModBeforeUninit,
    Wh_ModUninit);
