cmake_minimum_required(VERSION 3.16)
project(NDIMon-R VERSION 1.0.0 LANGUAGES CXX C)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_BUILD_TYPE Release CACHE STRING "Build type")

# Platform-adaptive optimization flags
if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(CMAKE_CXX_FLAGS_RELEASE "-O3 -march=armv8-a+simd -mtune=native -DNDEBUG")
    set(CMAKE_C_FLAGS_RELEASE   "-O3 -march=armv8-a+simd -mtune=native -DNDEBUG")
elseif(CMAKE_SYSTEM_PROCESSOR MATCHES "armv7|armhf")
    set(CMAKE_CXX_FLAGS_RELEASE "-O3 -march=armv7-a+neon-vfpv4 -mtune=native -DNDEBUG")
    set(CMAKE_C_FLAGS_RELEASE   "-O3 -march=armv7-a+neon-vfpv4 -mtune=native -DNDEBUG")
else()
    set(CMAKE_CXX_FLAGS_RELEASE "-O3 -DNDEBUG")
    set(CMAKE_C_FLAGS_RELEASE   "-O3 -DNDEBUG")
endif()

# --- Feature options ---
option(ENABLE_MPP    "Enable Rockchip MPP hardware decoder"       ON)
option(ENABLE_V4L2   "Enable V4L2 M2M hardware decoder (RPi)"    ON)
option(ENABLE_FFMPEG "Enable software decoder fallback (FFmpeg)"  ON)

# --- Find packages ---
find_package(PkgConfig REQUIRED)

pkg_check_modules(LIBDRM REQUIRED libdrm)
pkg_check_modules(ALSA   REQUIRED alsa)

# LibRGA (optional, Rockchip only)
pkg_check_modules(LIBRGA librga)

# NDI SDK (manually specified)
set(NDI_INCLUDE_DIR "/usr/local/include")
set(NDI_LIB_DIR     "/usr/local/lib")
find_library(NDI_LIBRARY NAMES ndi PATHS ${NDI_LIB_DIR} REQUIRED)

# Rockchip MPP (optional)
if(ENABLE_MPP)
    find_library(MPP_LIBRARY NAMES rockchip_mpp PATHS
        /usr/lib/aarch64-linux-gnu
        /usr/lib/arm-linux-gnueabihf
        /usr/lib)
    if(MPP_LIBRARY)
        set(HAVE_MPP TRUE)
        set(MPP_INCLUDE_DIR "/usr/include/rockchip")
        message(STATUS "Rockchip MPP found: ${MPP_LIBRARY}")
    else()
        message(STATUS "Rockchip MPP NOT found, MPP decoder disabled")
    endif()
endif()

# V4L2 (Linux kernel header, always present on Linux)
if(ENABLE_V4L2)
    include(CheckIncludeFiles)
    check_include_files("linux/videodev2.h" HAVE_V4L2)
    if(HAVE_V4L2)
        message(STATUS "V4L2 support enabled")
    endif()
endif()

# FFmpeg (libavcodec software decoder)
if(ENABLE_FFMPEG)
    pkg_check_modules(FFMPEG libavcodec libavutil libswscale)
    if(FFMPEG_FOUND)
        set(HAVE_FFMPEG TRUE)
        message(STATUS "FFmpeg found: software decoder enabled")
    else()
        message(STATUS "FFmpeg NOT found, software decoder disabled")
    endif()
endif()

# nlohmann/json
find_package(nlohmann_json QUIET)
if(NOT nlohmann_json_FOUND)
    include(FetchContent)
    FetchContent_Declare(nlohmann_json
        URL https://github.com/nlohmann/json/releases/download/v3.11.3/json.tar.xz)
    FetchContent_MakeAvailable(nlohmann_json)
endif()

# --- Collect decoder sources ---
set(DECODER_SOURCES
    src/main.cpp
    src/Config.cpp
    src/NDIReceiver.cpp
    src/VideoDecoder.cpp
    src/PlatformDetect.cpp
    src/DRMDisplay.cpp
    src/AlsaAudio.cpp
    src/IPCServer.cpp
)

set(DECODER_DEFINES "")
set(DECODER_INCLUDES
    ${NDI_INCLUDE_DIR}
    ${LIBDRM_INCLUDE_DIRS}
    ${ALSA_INCLUDE_DIRS}
    /usr/include/libdrm
    src/
)
set(DECODER_LIBS
    ${NDI_LIBRARY}
    ${LIBDRM_LIBRARIES}
    ${ALSA_LIBRARIES}
    pthread dl atomic
    avahi-client avahi-common
)

if(HAVE_MPP)
    list(APPEND DECODER_SOURCES src/MppDecoder.cpp)
    list(APPEND DECODER_DEFINES HAVE_MPP)
    list(APPEND DECODER_INCLUDES ${MPP_INCLUDE_DIR})
    list(APPEND DECODER_LIBS ${MPP_LIBRARY})
endif()

if(HAVE_V4L2)
    list(APPEND DECODER_SOURCES src/V4L2Decoder.cpp)
    list(APPEND DECODER_DEFINES HAVE_V4L2)
endif()

if(HAVE_FFMPEG)
    list(APPEND DECODER_SOURCES src/SoftwareDecoder.cpp)
    list(APPEND DECODER_DEFINES HAVE_FFMPEG)
    list(APPEND DECODER_INCLUDES ${FFMPEG_INCLUDE_DIRS})
    list(APPEND DECODER_LIBS ${FFMPEG_LIBRARIES})
endif()

if(LIBRGA_FOUND)
    list(APPEND DECODER_DEFINES HAVE_RGA)
    list(APPEND DECODER_INCLUDES ${LIBRGA_INCLUDE_DIRS})
    list(APPEND DECODER_LIBS ${LIBRGA_LIBRARIES})
    message(STATUS "LibRGA found: hardware color conversion enabled")
else()
    message(STATUS "LibRGA NOT found: software color conversion only")
endif()

# --- Decoder executable ---
add_executable(ndimon-r ${DECODER_SOURCES})

target_include_directories(ndimon-r PRIVATE ${DECODER_INCLUDES})
target_link_libraries(ndimon-r PRIVATE ${DECODER_LIBS} nlohmann_json::nlohmann_json)

foreach(def ${DECODER_DEFINES})
    target_compile_definitions(ndimon-r PRIVATE ${def})
endforeach()

# --- Finder executable ---
add_executable(ndimon-finder finder/main.cpp)

target_include_directories(ndimon-finder PRIVATE
    ${NDI_INCLUDE_DIR}
    src/
)
target_link_libraries(ndimon-finder PRIVATE
    ${NDI_LIBRARY}
    pthread
    nlohmann_json::nlohmann_json
    avahi-client avahi-common
)

# --- Install ---
install(TARGETS ndimon-r ndimon-finder
    RUNTIME DESTINATION /usr/local/bin
)
# Config files are intentionally NOT installed by cmake.
# install.sh handles first-time config installation with "skip if exists"
# semantics so user settings are never overwritten on update.

message(STATUS "")
message(STATUS "=== NDIMon-R Build Configuration ===")
message(STATUS "  Platform: ${CMAKE_SYSTEM_PROCESSOR}")
message(STATUS "  MPP decoder:      ${HAVE_MPP}")
message(STATUS "  V4L2 decoder:     ${HAVE_V4L2}")
message(STATUS "  FFmpeg decoder:   ${HAVE_FFMPEG}")
message(STATUS "  LibRGA:           ${LIBRGA_FOUND}")
message(STATUS "=====================================")
