cmake_minimum_required(VERSION 3.22.1)
project(runode_terminal C)

# GHOSTTY_VT_DIR 由 terminal 模块的 Gradle 配置传进来，下面按 zig 的目标三元组分目录。
if(NOT GHOSTTY_VT_DIR)
  message(FATAL_ERROR "没有设 GHOSTTY_VT_DIR")
endif()
if(ANDROID_ABI STREQUAL "arm64-v8a")
  set(ghostty_triple aarch64-linux-android)
elseif(ANDROID_ABI STREQUAL "x86_64")
  set(ghostty_triple x86_64-linux-android)
else()
  message(FATAL_ERROR "libghostty-vt 没有编 ${ANDROID_ABI}")
endif()
set(ghostty_prefix "${GHOSTTY_VT_DIR}/${ghostty_triple}")

# 静态库只引用 libc 和 libm，不需要 libc++，所以 ANDROID_STL 用默认值。
add_library(ghostty-vt STATIC IMPORTED)
set_target_properties(ghostty-vt PROPERTIES
  IMPORTED_LOCATION "${ghostty_prefix}/lib/libghostty-vt.a"
  INTERFACE_INCLUDE_DIRECTORIES "${ghostty_prefix}/include"
  INTERFACE_COMPILE_DEFINITIONS GHOSTTY_STATIC)

add_library(runode_vt SHARED vt_jni.c)
target_compile_options(runode_vt PRIVATE -Wall -Wextra)
# jnigraphics 的 AImageDecoder 给 Kitty 图片解 PNG。
target_link_libraries(runode_vt PRIVATE ghostty-vt jnigraphics m)
# 16 KB 页的设备上只加载按 16 KB 对齐的 .so；libghostty-vt 的符号不导出，只留 JNI 入口。
target_link_options(runode_vt PRIVATE
  "-Wl,-z,max-page-size=16384"
  "-Wl,--exclude-libs,ALL"
  "-Wl,--gc-sections")
