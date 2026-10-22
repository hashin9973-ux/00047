#include "windhawk_hook_compat.h"

#if !defined(_M_X64) && !defined(__x86_64__)
#error The bundled Windhawk-compatible hook backend supports x64 only.
#endif

#include "third_party/minhook/include/MinHook.h"
#include "third_party/minhook/src/hde/hde64.h"

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

namespace {
struct HookRecord {
    void* target = nullptr;
    void* detour = nullptr;
    void* original = nullptr;
    bool enabled = false;
    bool removeQueued = false;
};

std::mutex g_mutex;
std::unordered_map<void*, HookRecord> g_hooks;
bool g_initialized = false;
bool g_accepting = true;

bool EnsureInitializedLocked() noexcept {
    if (g_initialized) return true;
    const MH_STATUS status = MH_Initialize();
    if (status != MH_OK && status != MH_ERROR_ALREADY_INITIALIZED) return false;
    g_initialized = true;
    return true;
}

void TryUninitializeLocked() noexcept {
    if (!g_initialized || !g_hooks.empty()) return;
    const MH_STATUS status = MH_Uninitialize();
    if (status == MH_OK || status == MH_ERROR_NOT_INITIALIZED) g_initialized = false;
}

BOOL ApplyLocked() noexcept {
    if (g_hooks.empty()) {
        TryUninitializeLocked();
        return TRUE;
    }

    std::vector<void*> newlyEnabled;
    std::vector<void*> newlyDisabled;
    std::vector<void*> removals;
    try {
        newlyEnabled.reserve(g_hooks.size());
        newlyDisabled.reserve(g_hooks.size());
        removals.reserve(g_hooks.size());

        // Enable additions first. If one fails, roll back additions enabled by
        // this batch while leaving removal requests queued for a later retry.
        for (auto& entry : g_hooks) {
            HookRecord& hook = entry.second;
            if (!hook.enabled && !hook.removeQueued) {
                if (MH_EnableHook(hook.target) != MH_OK) {
                    for (void* target : newlyEnabled) {
                        if (MH_DisableHook(target) == MH_OK) {
                            auto it = g_hooks.find(target);
                            if (it != g_hooks.end()) it->second.enabled = false;
                        }
                    }
                    return FALSE;
                }
                hook.enabled = true;
                newlyEnabled.push_back(hook.target);
            }
        }

        for (auto& entry : g_hooks) {
            if (entry.second.removeQueued) removals.push_back(entry.first);
        }

        // Removals are disabled only after all pending enables have succeeded.
        for (void* target : removals) {
            auto it = g_hooks.find(target);
            if (it == g_hooks.end() || !it->second.enabled) continue;
            if (MH_DisableHook(target) != MH_OK) {
                for (void* changed : newlyDisabled) {
                    if (MH_EnableHook(changed) == MH_OK) {
                        auto rollback = g_hooks.find(changed);
                        if (rollback != g_hooks.end()) rollback->second.enabled = true;
                    }
                }
                for (void* changed : newlyEnabled) {
                    if (MH_DisableHook(changed) == MH_OK) {
                        auto rollback = g_hooks.find(changed);
                        if (rollback != g_hooks.end()) rollback->second.enabled = false;
                    }
                }
                return FALSE;
            }
            it->second.enabled = false;
            newlyDisabled.push_back(target);
        }

        for (void* target : removals) {
            const MH_STATUS status = MH_RemoveHook(target);
            if (status != MH_OK && status != MH_ERROR_NOT_CREATED) return FALSE;
            g_hooks.erase(target);
        }
        TryUninitializeLocked();
        return TRUE;
    } catch (...) {
        for (void* target : newlyEnabled) {
            if (MH_DisableHook(target) == MH_OK) {
                auto it = g_hooks.find(target);
                if (it != g_hooks.end()) it->second.enabled = false;
            }
        }
        return FALSE;
    }
}

const char* Mnemonic(const unsigned char* bytes, size_t available, const hde64s& decoded) noexcept {
    static const char* const conditions[] = {
        "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja",
        "js", "jns", "jp", "jnp", "jl", "jge", "jle", "jg"
    };
    size_t i = 0;
    while (i < available) {
        const unsigned char b = bytes[i];
        if (b == 0x66 || b == 0x67 || b == 0xF0 || b == 0xF2 || b == 0xF3 ||
            b == 0x2E || b == 0x36 || b == 0x3E || b == 0x26 || b == 0x64 ||
            b == 0x65 || (b >= 0x40 && b <= 0x4F)) {
            ++i;
            continue;
        }
        break;
    }
    if (i >= available) return "db";
    const unsigned char op = bytes[i++];
    if (op == 0x0F) {
        if (i >= available) return "op0f";
        const unsigned char op2 = bytes[i++];
        if (op2 >= 0x80 && op2 <= 0x8F) return conditions[op2 - 0x80];
        switch (op2) {
            case 0x05: return "syscall";
            case 0x1F: return "nop";
            case 0xAF: return "imul";
            case 0xB6: case 0xB7: return "movzx";
            case 0xBE: case 0xBF: return "movsx";
            case 0x10: case 0x11: case 0x28: case 0x29: return "mov";
            case 0x2E: case 0x2F: return "comiss";
            default: return "op0f";
        }
    }
    if (op >= 0x70 && op <= 0x7F) return conditions[op - 0x70];
    if (op >= 0x50 && op <= 0x57) return "push";
    if (op >= 0x58 && op <= 0x5F) return "pop";
    switch (op) {
        case 0x90: return "nop";
        case 0xC3: case 0xC2: return "ret";
        case 0xCC: return "int3";
        case 0xE8: return "call";
        case 0xE9: case 0xEB: return "jmp";
        case 0x89: case 0x8B: case 0xC6: case 0xC7: return "mov";
        case 0x8D: return "lea";
        case 0x84: case 0x85: case 0xA8: case 0xA9: return "test";
        case 0x01: case 0x03: case 0x04: case 0x05: return "add";
        case 0x29: case 0x2B: case 0x2C: case 0x2D: return "sub";
        case 0x31: case 0x33: return "xor";
        case 0x21: case 0x23: return "and";
        case 0x09: case 0x0B: return "or";
        case 0xFF:
            if (decoded.modrm_reg == 2 || decoded.modrm_reg == 3) return "call";
            if (decoded.modrm_reg == 4 || decoded.modrm_reg == 5) return "jmp";
            if (decoded.modrm_reg == 6) return "push";
            return "groupff";
        case 0x80: case 0x81: case 0x83:
            switch (decoded.modrm_reg) {
                case 0: return "add";
                case 1: return "or";
                case 2: return "adc";
                case 3: return "sbb";
                case 4: return "and";
                case 5: return "sub";
                case 6: return "xor";
                case 7: return "cmp";
            }
            return "group1";
        case 0xF6: case 0xF7:
            if (decoded.modrm_reg == 0) return "test";
            if (decoded.modrm_reg == 2 || decoded.modrm_reg == 3) return "not/neg";
            if (decoded.modrm_reg == 4 || decoded.modrm_reg == 5) return "mul/imul";
            return "div/idiv";
        case 0xA4: case 0xA5: case 0xAA: case 0xAB: case 0xAC: case 0xAD: return "string";
        default: return "db";
    }
}
} // namespace

BOOL Wh_SetFunctionHook(void* targetFunction, void* hookFunction, void** originalFunction) {
    if (!targetFunction || !hookFunction || targetFunction == hookFunction) return FALSE;
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_accepting || g_hooks.find(targetFunction) != g_hooks.end() ||
        !EnsureInitializedLocked()) {
        if (originalFunction) *originalFunction = nullptr;
        return FALSE;
    }

    void* trampoline = nullptr;
    const MH_STATUS status = MH_CreateHook(targetFunction, hookFunction, &trampoline);
    if (status != MH_OK) {
        if (originalFunction) *originalFunction = nullptr;
        TryUninitializeLocked();
        return FALSE;
    }

    try {
        g_hooks.emplace(targetFunction, HookRecord{targetFunction, hookFunction, trampoline, false, false});
    } catch (...) {
        MH_RemoveHook(targetFunction);
        if (originalFunction) *originalFunction = nullptr;
        TryUninitializeLocked();
        return FALSE;
    }
    if (originalFunction) *originalFunction = trampoline;
    return TRUE;
}

BOOL Wh_RemoveFunctionHook(void* targetFunction) {
    if (!targetFunction) return FALSE;
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_accepting) return FALSE;
    auto it = g_hooks.find(targetFunction);
    if (it == g_hooks.end() || it->second.removeQueued) return FALSE;
    it->second.removeQueued = true;
    return TRUE;
}

BOOL Wh_ApplyHookOperations(void) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_accepting && !g_hooks.empty()) return FALSE;
    return ApplyLocked();
}

namespace wmod::windhawk_hook_compat {
BOOL BeginHookOperations() noexcept {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_hooks.empty()) return FALSE;
    g_accepting = true;
    return TRUE;
}

BOOL ApplyHookOperations() noexcept {
    try {
        std::lock_guard<std::mutex> lock(g_mutex);
        return ApplyLocked();
    } catch (...) {
        return FALSE;
    }
}

void ShutdownHookOperations() noexcept {
    std::lock_guard<std::mutex> lock(g_mutex);
    for (auto& entry : g_hooks) entry.second.removeQueued = true;
    g_accepting = false;
    if (!ApplyLocked()) {
        for (auto& entry : g_hooks) {
            MH_DisableHook(entry.first);
            MH_RemoveHook(entry.first);
        }
        g_hooks.clear();
        TryUninitializeLocked();
        OutputDebugStringW(L"00047: hook shutdown required last-chance cleanup.\n");
    }
}
} // namespace wmod::windhawk_hook_compat

BOOL Wh_Disasm(void* address, WH_DISASM_RESULT* result) {
    if (!address || !result) return FALSE;
    std::memset(result, 0, sizeof(*result));

    MEMORY_BASIC_INFORMATION region{};
    if (VirtualQuery(address, &region, sizeof(region)) != sizeof(region) ||
        region.State != MEM_COMMIT || (region.Protect & (PAGE_GUARD | PAGE_NOACCESS))) {
        return FALSE;
    }
    const DWORD protection = region.Protect & 0xFF;
    if (protection == PAGE_EXECUTE) return FALSE;
    const auto start = reinterpret_cast<std::uintptr_t>(address);
    const auto base = reinterpret_cast<std::uintptr_t>(region.BaseAddress);
    if (start < base || start - base >= region.RegionSize) return FALSE;
    const size_t available = std::min<size_t>(15, region.RegionSize - static_cast<size_t>(start - base));
    if (available == 0) return FALSE;

    unsigned char bytes[15]{};
    std::memcpy(bytes, address, available);
    hde64s decoded{};
    hde64_disasm(bytes, &decoded);
    if ((decoded.flags & F_ERROR) || decoded.len == 0 || decoded.len > available) return FALSE;

    result->length = decoded.len;
    std::string text(Mnemonic(bytes, available, decoded));
    text += " [";
    for (size_t i = 0; i < decoded.len; ++i) {
        char value[4]{};
        std::snprintf(value, sizeof(value), "%02X", bytes[i]);
        if (i) text += ' ';
        text += value;
    }
    text += ']';
    const size_t copyLength = std::min(text.size(), sizeof(result->text) - 1);
    std::memcpy(result->text, text.data(), copyLength);
    result->text[copyLength] = '\0';
    return TRUE;
}
