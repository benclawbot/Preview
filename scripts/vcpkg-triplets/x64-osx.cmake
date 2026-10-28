set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE static)

set(VCPKG_CMAKE_SYSTEM_NAME Darwin)
set(VCPKG_OSX_ARCHITECTURES x86_64)
set(VCPKG_OSX_DEPLOYMENT_TARGET 11.0)
set(VCPKG_BUILD_TYPE release)

# libde265 uses GCC-only CPU runtime symbols for these kernels under Apple Clang.
# Keep its SSE4 implementation enabled while avoiding the unresolved AVX symbols.
set(VCPKG_CMAKE_CONFIGURE_OPTIONS
    -DENABLE_AVX2=OFF
    -DENABLE_AVX512=OFF
)
