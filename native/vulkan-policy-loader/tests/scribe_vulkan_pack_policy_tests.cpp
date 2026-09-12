#include <filesystem>
#include <string>
#include <vector>

#include "test_environment.h"

namespace {

// GoogleTest's stock main does not sanitize the parent process environment.
// Clear every loader input used by this suite before FrameworkEnvironment
// loads the exact CMake-built loader. EnvVarWrapper restores values afterward.
struct ScopedCleanLoaderEnvironment {
    EnvVarWrapper test_loader_path{"VK_LOADER_TEST_LOADER_PATH"};
    EnvVarWrapper legacy_icd_files{"VK_ICD_FILENAMES"};
    EnvVarWrapper driver_files{"VK_DRIVER_FILES"};
    EnvVarWrapper add_driver_files{"VK_ADD_DRIVER_FILES"};
    EnvVarWrapper layer_path{"VK_LAYER_PATH"};
    EnvVarWrapper add_layer_path{"VK_ADD_LAYER_PATH"};
    EnvVarWrapper implicit_layer_path{"VK_IMPLICIT_LAYER_PATH"};
    EnvVarWrapper add_implicit_layer_path{"VK_ADD_IMPLICIT_LAYER_PATH"};
    EnvVarWrapper instance_layers{"VK_INSTANCE_LAYERS"};
    EnvVarWrapper drivers_select{"VK_LOADER_DRIVERS_SELECT"};
    EnvVarWrapper drivers_disable{"VK_LOADER_DRIVERS_DISABLE"};
    EnvVarWrapper layers_enable{"VK_LOADER_LAYERS_ENABLE"};
    EnvVarWrapper layers_disable{"VK_LOADER_LAYERS_DISABLE"};
    EnvVarWrapper layers_allow{"VK_LOADER_LAYERS_ALLOW"};
    EnvVarWrapper loader_debug{"VK_LOADER_DEBUG"};
    EnvVarWrapper disable_instance_extension_filter{"VK_LOADER_DISABLE_INST_EXT_FILTER"};
    EnvVarWrapper disable_select{"VK_LOADER_DISABLE_SELECT"};
};

class VulkanPackPolicyTest : public ::testing::Test {
  protected:
    ScopedCleanLoaderEnvironment clean_environment;
};

ManifestLayer MakeExplicitLayer(const char* name) {
    ManifestLayer manifest;
    manifest.add_layer(ManifestLayer::LayerDescription{}.set_name(name).set_lib_path(TEST_LAYER_PATH_EXPORT_VERSION_2));
    return manifest;
}

ManifestLayer MakeImplicitLayer(const char* name, const char* disable_environment) {
    ManifestLayer manifest;
    manifest.add_layer(ManifestLayer::LayerDescription{}
                           .set_name(name)
                           .set_lib_path(TEST_LAYER_PATH_EXPORT_VERSION_2)
                           .set_disable_environment(disable_environment));
    return manifest;
}

void ExpectNoEnumeratedLayers(FrameworkEnvironment& env) {
    uint32_t count = 0;
    ASSERT_EQ(VK_SUCCESS, env.vulkan_functions.vkEnumerateInstanceLayerProperties(&count, nullptr));
    EXPECT_EQ(0U, count);
}

void ExpectNoLayerWasActivated(FrameworkEnvironment& env) {
    for (size_t index = 0; index < env.layers.size(); ++index) {
        EXPECT_EQ(VK_NULL_HANDLE, env.get_test_layer(index).instance_handle) << "layer index " << index;
    }
}

void ExpectOneNamedPhysicalDevice(FrameworkEnvironment& env, InstWrapper& instance, const char* expected_name) {
    const auto physical_devices = instance.GetPhysDevs();
    ASSERT_EQ(1U, physical_devices.size());
    ASSERT_NE(VK_NULL_HANDLE, physical_devices.front());

    VkPhysicalDeviceProperties properties{};
    env.vulkan_functions.vkGetPhysicalDeviceProperties(physical_devices.front(), &properties);
    EXPECT_TRUE(string_eq(expected_name, properties.deviceName));
}

void AssertMockSettingsRegistration(FrameworkEnvironment& env, bool secure) {
    const auto location = secure ? ManifestLocation::settings_location : ManifestLocation::unsecured_settings;
    const auto expected_path = env.get_folder(location).location() / "vk_loader_settings.json";
    ASSERT_TRUE(std::filesystem::exists(expected_path));

#if defined(_WIN32)
    const auto& entries =
        secure ? env.platform_shim->hkey_local_machine_settings : env.platform_shim->hkey_current_user_settings;
    ASSERT_EQ(1U, entries.size());
    EXPECT_EQ(expected_path, entries.front().name);
#else
    (void)secure;
#endif
}

TEST_F(VulkanPackPolicyTest, LayerDiscoveryAndEnvironmentAllowancesRemainInert) {
    FrameworkEnvironment env{};
    constexpr char kPhysicalDeviceName[] = "scribe-policy-device";
    env.add_icd(TEST_ICD_PATH_VERSION_2).add_physical_device(PhysicalDevice{kPhysicalDeviceName});

    env.add_explicit_layer({}, MakeExplicitLayer("VK_LAYER_SCRIBE_registry_explicit"));
    env.add_implicit_layer({}, MakeImplicitLayer("VK_LAYER_SCRIBE_registry_implicit", "SCRIBE_DISABLE_REGISTRY_IMPLICIT"));
    env.add_explicit_layer(ManifestOptions{}.set_discovery_type(ManifestDiscoveryType::env_var),
                           MakeExplicitLayer("VK_LAYER_SCRIBE_environment_explicit"));
    env.add_implicit_layer(ManifestOptions{}.set_discovery_type(ManifestDiscoveryType::env_var),
                           MakeImplicitLayer("VK_LAYER_SCRIBE_environment_implicit", "SCRIBE_DISABLE_ENVIRONMENT_IMPLICIT"));
    env.add_explicit_layer(ManifestOptions{}.set_discovery_type(ManifestDiscoveryType::add_env_var),
                           MakeExplicitLayer("VK_LAYER_SCRIBE_add_environment_explicit"));
    env.add_implicit_layer(ManifestOptions{}.set_discovery_type(ManifestDiscoveryType::add_env_var),
                           MakeImplicitLayer("VK_LAYER_SCRIBE_add_environment_implicit", "SCRIBE_DISABLE_ADD_ENV_IMPLICIT"));

    // Exercise the documented allowance escape hatch as well as explicit
    // enablement. The compile-time policy must win over every runtime filter.
    EnvVarWrapper disable_all{"VK_LOADER_LAYERS_DISABLE", "~all~"};
    EnvVarWrapper allow_all{"VK_LOADER_LAYERS_ALLOW", "*"};
    EnvVarWrapper enable_all{"VK_LOADER_LAYERS_ENABLE", "*"};

    ASSERT_NO_FATAL_FAILURE(ExpectNoEnumeratedLayers(env));

    InstWrapper instance{env.vulkan_functions};
    instance.CheckCreate();
    ASSERT_NE(VK_NULL_HANDLE, instance.inst);
    ASSERT_NO_FATAL_FAILURE(ExpectOneNamedPhysicalDevice(env, instance, kPhysicalDeviceName));
    ExpectNoLayerWasActivated(env);
}

TEST_F(VulkanPackPolicyTest, ExplicitLayerRequestIsRejected) {
    FrameworkEnvironment env{};
    constexpr char kLayerName[] = "VK_LAYER_SCRIBE_explicit_request";
    env.add_icd(TEST_ICD_PATH_VERSION_2).add_physical_device({});
    env.add_explicit_layer({}, MakeExplicitLayer(kLayerName));

    InstWrapper instance{env.vulkan_functions};
    instance.create_info.add_layer(kLayerName);
    instance.CheckCreate(VK_ERROR_LAYER_NOT_PRESENT);

    EXPECT_EQ(VK_NULL_HANDLE, instance.inst);
    ExpectNoLayerWasActivated(env);
}

enum class SettingsHive { current_user, local_machine };

class VulkanPackLoaderSettingsPolicyTest : public VulkanPackPolicyTest,
                                           public ::testing::WithParamInterface<SettingsHive> {};

TEST_P(VulkanPackLoaderSettingsPolicyTest, ForceOnLayerCannotBypassPolicy) {
    FrameworkEnvironment env{};
    constexpr char kLayerName[] = "VK_LAYER_SCRIBE_settings_force_on";
    constexpr char kPhysicalDeviceName[] = "scribe-settings-baseline-device";
    env.add_icd(TEST_ICD_PATH_VERSION_2).add_physical_device(PhysicalDevice{kPhysicalDeviceName});

    // override_folder is deliberately not a normal discovery location. In an
    // unmodified loader, the mocked settings registry entry below is the only
    // route by which this manifest becomes visible.
    env.add_explicit_layer(ManifestOptions{}.set_discovery_type(ManifestDiscoveryType::override_folder),
                           MakeExplicitLayer(kLayerName));
    const auto layer_manifest_path = env.get_layer_manifest_path(0);
    ASSERT_TRUE(std::filesystem::exists(layer_manifest_path));

    env.loader_settings.set_file_format_version({1, 0, 0})
        .add_app_specific_setting(AppSpecificSettings{}.add_layer_configuration(LoaderSettingsLayerConfiguration{}
                                                                                   .set_name(kLayerName)
                                                                                   .set_path(layer_manifest_path)
                                                                                   .set_control("on")));

    const bool secure = GetParam() == SettingsHive::local_machine;
    env.update_loader_settings(env.loader_settings, secure);
    ASSERT_NO_FATAL_FAILURE(AssertMockSettingsRegistration(env, secure));

    // This is the exact bypass condition in the stock loader: settings "on"
    // used to bypass VK_LOADER_LAYERS_DISABLE. It must remain inert here.
    EnvVarWrapper disable_all{"VK_LOADER_LAYERS_DISABLE", "~all~"};
    ASSERT_NO_FATAL_FAILURE(ExpectNoEnumeratedLayers(env));

    InstWrapper instance{env.vulkan_functions};
    instance.CheckCreate();
    ASSERT_NE(VK_NULL_HANDLE, instance.inst);
    ASSERT_NO_FATAL_FAILURE(ExpectOneNamedPhysicalDevice(env, instance, kPhysicalDeviceName));
    ExpectNoLayerWasActivated(env);
}

INSTANTIATE_TEST_SUITE_P(
    MockedWindowsRegistryHives, VulkanPackLoaderSettingsPolicyTest,
    ::testing::Values(SettingsHive::current_user, SettingsHive::local_machine),
    [](const ::testing::TestParamInfo<SettingsHive>& info) {
        return info.param == SettingsHive::current_user ? std::string{"CurrentUser"} : std::string{"LocalMachine"};
    });

TEST_F(VulkanPackPolicyTest, SettingsCannotAddOrReplaceDrivers) {
    FrameworkEnvironment env{};
    constexpr char kBaselineDeviceName[] = "scribe-baseline-driver-device";
    constexpr char kSettingsDeviceName[] = "scribe-settings-driver-device";

    env.add_icd(TEST_ICD_PATH_VERSION_2).add_physical_device(PhysicalDevice{kBaselineDeviceName});
    env.add_icd(TEST_ICD_PATH_VERSION_2,
                ManifestOptions{}.set_discovery_type(ManifestDiscoveryType::override_folder))
        .add_physical_device(PhysicalDevice{kSettingsDeviceName});

    const auto settings_driver_manifest_path = env.get_icd_manifest_path(1);
    ASSERT_TRUE(std::filesystem::exists(settings_driver_manifest_path));
    env.loader_settings.set_file_format_version({1, 0, 0})
        .add_app_specific_setting(AppSpecificSettings{}
                                      .set_additional_drivers_use_exclusively(true)
                                      .add_driver_configuration(LoaderSettingsDriverConfiguration{}.set_path(
                                          settings_driver_manifest_path)));
    env.update_loader_settings(env.loader_settings);
    ASSERT_NO_FATAL_FAILURE(AssertMockSettingsRegistration(env, true));

    InstWrapper instance{env.vulkan_functions};
    instance.CheckCreate();
    ASSERT_NE(VK_NULL_HANDLE, instance.inst);
    ASSERT_NO_FATAL_FAILURE(ExpectOneNamedPhysicalDevice(env, instance, kBaselineDeviceName));
}

}  // namespace
