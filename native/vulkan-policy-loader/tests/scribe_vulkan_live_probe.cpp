#include <Windows.h>
#include <TlHelp32.h>

#include <vulkan/vulkan.h>

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cwctype>
#include <filesystem>
#include <iostream>
#include <set>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

namespace {

constexpr uint32_t kMinimumLoaderVersion = VK_MAKE_API_VERSION(0, 1, 3, 234);
constexpr uint32_t kMaximumLayerCount = 4096;
constexpr uint32_t kMaximumPhysicalDeviceCount = 64;
constexpr char kMissingLayerSentinel[] = "VK_LAYER_SCRIBE_policy_probe_missing";

struct ModuleGuard {
    explicit ModuleGuard(const HMODULE module) : value(module) {}
    ~ModuleGuard() {
        if (value != nullptr) {
            FreeLibrary(value);
        }
    }
    ModuleGuard(const ModuleGuard&) = delete;
    ModuleGuard& operator=(const ModuleGuard&) = delete;

    HMODULE value = nullptr;
};

struct HandleGuard {
    explicit HandleGuard(const HANDLE handle) : value(handle) {}
    ~HandleGuard() {
        if (value != INVALID_HANDLE_VALUE) {
            CloseHandle(value);
        }
    }
    HandleGuard(const HandleGuard&) = delete;
    HandleGuard& operator=(const HandleGuard&) = delete;

    HANDLE value = INVALID_HANDLE_VALUE;
};

struct InstanceGuard {
    InstanceGuard(const VkInstance instance, const PFN_vkGetInstanceProcAddr get_instance_proc_addr)
        : value(instance), destroy(instance == VK_NULL_HANDLE ? nullptr :
              reinterpret_cast<PFN_vkDestroyInstance>(get_instance_proc_addr(instance, "vkDestroyInstance"))) {}
    ~InstanceGuard() {
        if (value != VK_NULL_HANDLE && destroy != nullptr) {
            destroy(value, nullptr);
        }
    }
    InstanceGuard(const InstanceGuard&) = delete;
    InstanceGuard& operator=(const InstanceGuard&) = delete;

    VkInstance value = VK_NULL_HANDLE;
    PFN_vkDestroyInstance destroy = nullptr;
};

struct Options {
    std::filesystem::path loader_path;
    std::vector<std::string> explicit_layers;
    std::vector<std::wstring> forbidden_module_names{
        L"we-graphics-hook64.dll",
        L"ow-graphics-vulkan.dll",
        L"owclient.dll",
    };
};

std::string Utf8(const std::wstring_view value) {
    if (value.empty()) {
        return {};
    }
    const int required = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
                                              nullptr, 0, nullptr, nullptr);
    if (required <= 0) {
        throw std::runtime_error("WideCharToMultiByte(size) failed: " + std::to_string(GetLastError()));
    }
    std::string output(static_cast<size_t>(required), '\0');
    const int written = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
                                             output.data(), required, nullptr, nullptr);
    if (written != required) {
        throw std::runtime_error("WideCharToMultiByte(data) failed: " + std::to_string(GetLastError()));
    }
    return output;
}

std::string Utf8(const std::filesystem::path& value) {
    const auto& native = value.native();
    return Utf8(std::wstring_view{native.data(), native.size()});
}

std::string NarrowArgument(const std::wstring_view value) {
    const auto converted = Utf8(value);
    if (converted.empty() || converted.size() >= VK_MAX_EXTENSION_NAME_SIZE) {
        throw std::runtime_error("layer name must contain 1..255 UTF-8 bytes");
    }
    return converted;
}

std::wstring Lowercase(std::wstring value) {
    std::transform(value.begin(), value.end(), value.begin(), [](const wchar_t character) {
        return static_cast<wchar_t>(towlower(character));
    });
    return value;
}

std::filesystem::path NormalizedAbsolutePath(const std::filesystem::path& input) {
    return std::filesystem::absolute(input).lexically_normal();
}

std::wstring NormalizedPathKey(const std::filesystem::path& input) {
    return Lowercase(NormalizedAbsolutePath(input).native());
}

std::wstring ModuleNameKey(const std::filesystem::path& input) {
    return Lowercase(input.filename().native());
}

std::vector<std::filesystem::path> SnapshotModules() {
    HandleGuard snapshot{CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, GetCurrentProcessId())};
    if (snapshot.value == INVALID_HANDLE_VALUE) {
        throw std::runtime_error("CreateToolhelp32Snapshot failed: " + std::to_string(GetLastError()));
    }

    MODULEENTRY32W entry{};
    entry.dwSize = static_cast<DWORD>(sizeof(entry));
    if (Module32FirstW(snapshot.value, &entry) == FALSE) {
        throw std::runtime_error("Module32FirstW failed: " + std::to_string(GetLastError()));
    }

    std::vector<std::filesystem::path> modules;
    do {
        modules.emplace_back(entry.szExePath);
        entry.dwSize = static_cast<DWORD>(sizeof(entry));
    } while (Module32NextW(snapshot.value, &entry) != FALSE);

    const DWORD last_error = GetLastError();
    if (last_error != ERROR_NO_MORE_FILES) {
        throw std::runtime_error("Module32NextW failed: " + std::to_string(last_error));
    }
    return modules;
}

std::filesystem::path LoadedModulePath(const HMODULE module) {
    std::wstring buffer(32768, L'\0');
    const DWORD length = GetModuleFileNameW(module, buffer.data(), static_cast<DWORD>(buffer.size()));
    if (length == 0 || length >= static_cast<DWORD>(buffer.size())) {
        throw std::runtime_error("GetModuleFileNameW failed: " + std::to_string(GetLastError()));
    }
    buffer.resize(length);
    return buffer;
}

Options ParseOptions(const int argc, wchar_t** argv) {
    Options options;
    for (int index = 1; index < argc; ++index) {
        const std::wstring_view argument{argv[index]};
        if (argument == L"--loader" && index + 1 < argc) {
            options.loader_path = argv[++index];
        } else if (argument == L"--explicit-layer" && index + 1 < argc) {
            options.explicit_layers.push_back(NarrowArgument(argv[++index]));
        } else if (argument == L"--forbid-module" && index + 1 < argc) {
            const std::filesystem::path module_argument{argv[++index]};
            const auto module_name = ModuleNameKey(module_argument);
            if (module_name.empty()) {
                throw std::runtime_error("--forbid-module requires a module filename");
            }
            options.forbidden_module_names.push_back(module_name);
        } else if (argument == L"--help") {
            std::cout
                << "Usage: scribe_vulkan_live_probe.exe --loader ABSOLUTE_PATH "
                   "[--explicit-layer NAME]... [--forbid-module FILENAME]...\n";
            std::exit(0);
        } else {
            throw std::runtime_error("unknown or incomplete argument: " + Utf8(argument));
        }
    }

    if (options.loader_path.empty() || !options.loader_path.is_absolute()) {
        throw std::runtime_error("--loader must name an absolute DLL path");
    }
    options.loader_path = NormalizedAbsolutePath(options.loader_path);
    if (!std::filesystem::is_regular_file(options.loader_path)) {
        throw std::runtime_error("loader is not a regular file: " + Utf8(options.loader_path));
    }

    options.explicit_layers.insert(options.explicit_layers.begin(), kMissingLayerSentinel);
    for (auto& module_name : options.forbidden_module_names) {
        module_name = Lowercase(std::filesystem::path{module_name}.filename().native());
    }
    return options;
}

template <typename Function>
Function GlobalFunction(const PFN_vkGetInstanceProcAddr get_instance_proc_addr, const char* name) {
    return reinterpret_cast<Function>(get_instance_proc_addr(VK_NULL_HANDLE, name));
}

template <typename Function>
Function InstanceFunction(const PFN_vkGetInstanceProcAddr get_instance_proc_addr, const VkInstance instance,
                          const char* name) {
    return reinterpret_cast<Function>(get_instance_proc_addr(instance, name));
}

std::string VersionString(const uint32_t version) {
    return std::to_string(VK_API_VERSION_MAJOR(version)) + "." + std::to_string(VK_API_VERSION_MINOR(version)) + "." +
           std::to_string(VK_API_VERSION_PATCH(version));
}

bool VerifyModules(const std::vector<std::filesystem::path>& baseline_modules,
                   const std::vector<std::filesystem::path>& final_modules, const Options& options,
                   const HMODULE loader_module) {
    bool passed = true;
    const auto requested_loader_key = NormalizedPathKey(options.loader_path);
    const auto loaded_loader_path = LoadedModulePath(loader_module);
    if (NormalizedPathKey(loaded_loader_path) != requested_loader_key) {
        std::cerr << "FAIL loader.path requested=" << Utf8(options.loader_path)
                  << " mapped=" << Utf8(loaded_loader_path) << '\n';
        passed = false;
    } else {
        std::cout << "loader.path=" << Utf8(loaded_loader_path) << '\n';
    }

    std::set<std::wstring> baseline_keys;
    for (const auto& module : baseline_modules) {
        baseline_keys.insert(NormalizedPathKey(module));
    }

    std::set<std::wstring> forbidden_names(options.forbidden_module_names.begin(), options.forbidden_module_names.end());
    size_t exact_loader_count = 0;
    for (const auto& module : final_modules) {
        const auto path_key = NormalizedPathKey(module);
        const auto module_name = ModuleNameKey(module);
        if (baseline_keys.count(path_key) == 0) {
            std::cout << "module.loaded=" << Utf8(module) << '\n';
        }
        if (path_key == requested_loader_key) {
            ++exact_loader_count;
        }
        if (module_name == L"vulkan-1.dll" && path_key != requested_loader_key) {
            std::cerr << "FAIL module.unexpected_vulkan_loader=" << Utf8(module) << '\n';
            passed = false;
        }
        if (forbidden_names.count(module_name) != 0) {
            std::cerr << "FAIL module.forbidden=" << Utf8(module) << '\n';
            passed = false;
        }
    }
    if (exact_loader_count != 1) {
        std::cerr << "FAIL module.exact_loader_count=" << exact_loader_count << '\n';
        passed = false;
    }
    return passed;
}

}  // namespace

int wmain(const int argc, wchar_t** argv) {
    try {
        // Match the worker's default dependency search restriction before the
        // policy loader or a vendor ICD can be loaded. The exact policy loader
        // itself additionally receives LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR below.
        if (SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) == FALSE) {
            throw std::runtime_error("SetDefaultDllDirectories failed: " + std::to_string(GetLastError()));
        }

        const Options options = ParseOptions(argc, argv);
        const auto baseline_modules = SnapshotModules();

        ModuleGuard loader{
            LoadLibraryExW(options.loader_path.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32)};
        if (loader.value == nullptr) {
            throw std::runtime_error("LoadLibraryExW failed: " + std::to_string(GetLastError()));
        }

        const auto get_instance_proc_addr =
            reinterpret_cast<PFN_vkGetInstanceProcAddr>(GetProcAddress(loader.value, "vkGetInstanceProcAddr"));
        if (get_instance_proc_addr == nullptr) {
            throw std::runtime_error("exact DLL does not export vkGetInstanceProcAddr");
        }

        const auto enumerate_instance_version =
            GlobalFunction<PFN_vkEnumerateInstanceVersion>(get_instance_proc_addr, "vkEnumerateInstanceVersion");
        const auto enumerate_instance_layers = GlobalFunction<PFN_vkEnumerateInstanceLayerProperties>(
            get_instance_proc_addr, "vkEnumerateInstanceLayerProperties");
        const auto create_instance = GlobalFunction<PFN_vkCreateInstance>(get_instance_proc_addr, "vkCreateInstance");
        if (enumerate_instance_version == nullptr || enumerate_instance_layers == nullptr || create_instance == nullptr) {
            throw std::runtime_error("exact DLL is missing required global Vulkan entry points");
        }

        bool passed = true;
        uint32_t loader_version = 0;
        VkResult result = enumerate_instance_version(&loader_version);
        if (result != VK_SUCCESS || loader_version < kMinimumLoaderVersion) {
            std::cerr << "FAIL loader.version result=" << result << " version=" << VersionString(loader_version) << '\n';
            passed = false;
        } else {
            std::cout << "loader.version=" << VersionString(loader_version) << '\n';
        }

        uint32_t layer_count = 0;
        result = enumerate_instance_layers(&layer_count, nullptr);
        if (result != VK_SUCCESS) {
            std::cerr << "FAIL layers.enumerate.result=" << result << '\n';
            passed = false;
        }
        if (layer_count > kMaximumLayerCount) {
            std::cerr << "FAIL layers.count_over_limit=" << layer_count << " limit=" << kMaximumLayerCount << '\n';
            passed = false;
        }
        std::vector<VkLayerProperties> layer_properties;
        if (result == VK_SUCCESS && layer_count > 0 && layer_count <= kMaximumLayerCount) {
            const uint32_t capacity = layer_count;
            layer_properties.resize(capacity);
            uint32_t written_count = capacity;
            result = enumerate_instance_layers(&written_count, layer_properties.data());
            if (result != VK_SUCCESS || written_count > capacity) {
                std::cerr << "FAIL layers.enumerate.details result=" << result << " capacity=" << capacity
                          << " written=" << written_count << '\n';
                passed = false;
            }
            layer_properties.resize(std::min(capacity, written_count));
        }
        std::cout << "layers.count=" << layer_count << '\n';
        for (const auto& layer : layer_properties) {
            std::cerr << "FAIL layer.visible=" << layer.layerName << '\n';
        }
        if (layer_count != 0) {
            passed = false;
        }

        const VkApplicationInfo application_info{
            VK_STRUCTURE_TYPE_APPLICATION_INFO,
            nullptr,
            "Scribe Vulkan policy live probe",
            1,
            "Scribe",
            1,
            VK_API_VERSION_1_0,
        };

        for (const auto& requested_layer : options.explicit_layers) {
            const char* requested_layer_name = requested_layer.c_str();
            const VkInstanceCreateInfo request_info{
                VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                nullptr,
                0,
                &application_info,
                1,
                &requested_layer_name,
                0,
                nullptr,
            };
            VkInstance rejected_instance = VK_NULL_HANDLE;
            result = create_instance(&request_info, nullptr, &rejected_instance);
            const InstanceGuard rejected_lifetime{
                result == VK_SUCCESS ? rejected_instance : VK_NULL_HANDLE, get_instance_proc_addr};
            if (result != VK_ERROR_LAYER_NOT_PRESENT || rejected_instance != VK_NULL_HANDLE) {
                std::cerr << "FAIL explicit_layer.name=" << requested_layer << " result=" << result << '\n';
                passed = false;
            } else {
                std::cout << "explicit_layer.rejected=" << requested_layer << '\n';
            }
        }

        const VkInstanceCreateInfo create_info{
            VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            nullptr,
            0,
            &application_info,
            0,
            nullptr,
            0,
            nullptr,
        };
        VkInstance instance = VK_NULL_HANDLE;
        result = create_instance(&create_info, nullptr, &instance);
        // This guard is destroyed before ModuleGuard, including when allocation,
        // module enumeration, or diagnostic formatting throws below.
        const InstanceGuard instance_lifetime{
            result == VK_SUCCESS ? instance : VK_NULL_HANDLE, get_instance_proc_addr};
        if (result != VK_SUCCESS || instance == VK_NULL_HANDLE) {
            std::cerr << "FAIL instance.create.result=" << result << '\n';
            passed = false;
        }

        if (result == VK_SUCCESS && instance != VK_NULL_HANDLE) {
            const auto enumerate_physical_devices = InstanceFunction<PFN_vkEnumeratePhysicalDevices>(
                get_instance_proc_addr, instance, "vkEnumeratePhysicalDevices");
            const auto get_physical_device_properties = InstanceFunction<PFN_vkGetPhysicalDeviceProperties>(
                get_instance_proc_addr, instance, "vkGetPhysicalDeviceProperties");
            if (instance_lifetime.destroy == nullptr || enumerate_physical_devices == nullptr || get_physical_device_properties == nullptr) {
                std::cerr << "FAIL instance.entry_points\n";
                passed = false;
            } else {
                uint32_t physical_device_count = 0;
                result = enumerate_physical_devices(instance, &physical_device_count, nullptr);
                if (result != VK_SUCCESS || physical_device_count == 0 ||
                    physical_device_count > kMaximumPhysicalDeviceCount) {
                    std::cerr << "FAIL physical_devices.query result=" << result << " count=" << physical_device_count << '\n';
                    passed = false;
                } else {
                    const uint32_t capacity = physical_device_count;
                    std::vector<VkPhysicalDevice> physical_devices(capacity);
                    uint32_t written_count = capacity;
                    result = enumerate_physical_devices(instance, &written_count, physical_devices.data());
                    if (result != VK_SUCCESS || written_count == 0 || written_count > capacity) {
                        std::cerr << "FAIL physical_devices.enumerate result=" << result << " capacity=" << capacity
                                  << " written=" << written_count << '\n';
                        passed = false;
                    } else {
                        physical_devices.resize(written_count);
                        std::cout << "physical_devices.count=" << written_count << '\n';
                        for (const auto physical_device : physical_devices) {
                            if (physical_device == VK_NULL_HANDLE) {
                                std::cerr << "FAIL physical_device.null_handle\n";
                                passed = false;
                                continue;
                            }
                            VkPhysicalDeviceProperties properties{};
                            get_physical_device_properties(physical_device, &properties);
                            std::cout << "physical_device.name=" << properties.deviceName << " vendor_id=" << properties.vendorID
                                      << " device_id=" << properties.deviceID << " api="
                                      << VersionString(properties.apiVersion) << '\n';
                        }
                    }
                }
            }
        }

        const auto final_modules = SnapshotModules();
        if (!VerifyModules(baseline_modules, final_modules, options, loader.value)) {
            passed = false;
        }

        std::cout << "SCRIBE_VULKAN_LIVE_PROBE=" << (passed ? "PASS" : "FAIL") << '\n';
        return passed ? 0 : 1;
    } catch (const std::exception& error) {
        std::cerr << "SCRIBE_VULKAN_LIVE_PROBE=ERROR " << error.what() << '\n';
        return 2;
    }
}
