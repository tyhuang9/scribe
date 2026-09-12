cmake_minimum_required(VERSION 3.22.1)

# This directory is an overlay for the pinned Vulkan-Loader test build. Add it
# only after Vulkan-Loader's tests/framework subdirectory has created these
# targets. Keeping it as an overlay avoids copying or forking the upstream test
# framework.
if(NOT WIN32 OR NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
    message(FATAL_ERROR "Scribe Vulkan policy tests require Windows x64")
endif()

foreach(required_target IN ITEMS testing_dependencies gtest_main Vulkan::Headers vulkan)
    if(NOT TARGET ${required_target})
        message(FATAL_ERROR
            "Missing '${required_target}'. Add this directory from the pinned "
            "Vulkan-Loader build after add_subdirectory(tests/framework), with "
            "BUILD_TESTS=ON.")
    endif()
endforeach()

add_executable(scribe_vulkan_pack_policy_tests
    scribe_vulkan_pack_policy_tests.cpp)
target_link_libraries(scribe_vulkan_pack_policy_tests
    PRIVATE testing_dependencies gtest_main)
add_dependencies(scribe_vulkan_pack_policy_tests
    vulkan test_icd_version_2 test_layer_export_version_2)
target_compile_definitions(scribe_vulkan_pack_policy_tests
    PRIVATE VK_NO_PROTOTYPES)
set_target_properties(scribe_vulkan_pack_policy_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
    CXX_EXTENSIONS OFF)

add_executable(scribe_vulkan_live_probe
    scribe_vulkan_live_probe.cpp)
target_link_libraries(scribe_vulkan_live_probe
    PRIVATE Vulkan::Headers)
target_compile_definitions(scribe_vulkan_live_probe
    PRIVATE VK_NO_PROTOTYPES WIN32_LEAN_AND_MEAN NOMINMAX)
set_target_properties(scribe_vulkan_live_probe PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
    CXX_EXTENSIONS OFF)

if(MSVC)
    target_compile_options(scribe_vulkan_pack_policy_tests PRIVATE /W4 /EHsc)
    target_compile_options(scribe_vulkan_live_probe PRIVATE /W4 /EHsc)
endif()

include(GoogleTest)
gtest_discover_tests(scribe_vulkan_pack_policy_tests
    TEST_PREFIX "scribe.vulkan.pack.policy."
    DISCOVERY_TIMEOUT 100
    DISCOVERY_MODE PRE_TEST
    PROPERTIES
        LABELS "scribe;vulkan;policy"
        RUN_SERIAL TRUE)

# The live probe intentionally is not a CTest test. It requires an explicit
# loader path and real display-driver qualification; see README.md.
