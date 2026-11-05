cmake_minimum_required(VERSION 3.22)

project(kcm_apple_magic_trackpad VERSION 1.0 LANGUAGES CXX)

set(QT_MIN_VERSION "6.6.0")
set(KF_MIN_VERSION "6.0.0")

find_package(ECM ${KF_MIN_VERSION} REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

find_package(Qt6 ${QT_MIN_VERSION} REQUIRED COMPONENTS Core)

# Outside Qt's own prefix ECM would put plugins in <prefix>/lib/plugins; use
# Qt's layout (lib/qt6/plugins here) so QT_PLUGIN_PATH=<prefix>/lib/qt6/plugins
# finds the module.
if(NOT KDE_INSTALL_QTPLUGINDIR AND QT6_INSTALL_PLUGINS
   AND NOT CMAKE_INSTALL_PREFIX STREQUAL QT6_INSTALL_PREFIX)
    set(KDE_INSTALL_QTPLUGINDIR "${QT6_INSTALL_PLUGINS}" CACHE PATH "Qt plugins" FORCE)
endif()

include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)
include(ECMAddTests)
include(FeatureSummary)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(Qt6 ${QT_MIN_VERSION} REQUIRED COMPONENTS Core Gui Qml Quick)
find_package(KF6 ${KF_MIN_VERSION} REQUIRED COMPONENTS KCMUtils CoreAddons I18n)

add_definitions(-DTRANSLATION_DOMAIN=\"kcm_apple_magic_trackpad\")

# Pure logic (Qt Core only), shared by the module and the unit test.
add_library(trackpadlogic STATIC trackpadlogic.cpp trackpadlogic.h)
target_link_libraries(trackpadlogic PUBLIC Qt6::Core)
target_include_directories(trackpadlogic PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
set_target_properties(trackpadlogic PROPERTIES POSITION_INDEPENDENT_CODE ON)

# plasma/kcms/systemsettings/kcm_apple_magic_trackpad.so + ui/ as resources
kcmutils_add_qml_kcm(kcm_apple_magic_trackpad SOURCES kcm.cpp kcm.h)
target_link_libraries(kcm_apple_magic_trackpad PRIVATE
    trackpadlogic
    Qt6::Quick
    KF6::KCMUtilsQuick
    KF6::CoreAddons
    KF6::I18n
)

if(BUILD_TESTING)
    find_package(Qt6 ${QT_MIN_VERSION} REQUIRED COMPONENTS Test)
    add_subdirectory(autotests)
endif()

feature_summary(WHAT ALL INCLUDE_QUIET_PACKAGES FATAL_ON_MISSING_REQUIRED_PACKAGES)
