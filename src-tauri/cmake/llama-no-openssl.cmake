# Turn off llama.cpp's HTTPS support. `LLAMA_OPENSSL` defaults ON upstream, which
# compiles the vendored cpp-httplib with TLS and makes `llama-common.dll` import the
# build host's libcrypto/libssl — a TLS stack linked into the offline note generator,
# and a hard dependency on two DLLs whose names change with OpenSSL's ABI.
#
# Nothing here calls llama.cpp's downloader: model weights come over `ureq` (rustls,
# §8.2) and no other llama-common HTTP path is reachable from the binding. The only
# behaviour OFF removes is `common_http_client()` accepting an `https://` URL, which
# we never hand it — it throws there instead of opening a socket.
#
# Delivered as a toolchain file because `llama-cpp-sys-4`'s build.rs owns the cmake
# invocation and exposes no define of ours; `CMAKE_TOOLCHAIN_FILE` is the only hook
# the `cmake` crate reads from the environment (see ../.cargo/config.toml). That crate
# is also the graph's only cmake consumer, so nothing else ever sees this file.
set(LLAMA_OPENSSL OFF CACHE BOOL "no TLS in llama-common: llama.cpp's downloader is unused" FORCE)

# llama-cpp-sys-4 passes GCC-style `-O3 -DNDEBUG`, which cl.exe ignores, leaving ggml
# unoptimized (prefill ~50x slower). FORCE overrides its -D; `MSVC` isn't set yet here.
if(CMAKE_HOST_WIN32)
  set(CMAKE_C_FLAGS_RELEASE "/O2 /Ob2 /DNDEBUG" CACHE STRING "MSVC release flags" FORCE)
  set(CMAKE_CXX_FLAGS_RELEASE "/O2 /Ob2 /DNDEBUG" CACHE STRING "MSVC release flags" FORCE)
endif()

# build.rs pins ggml to SSE4.2. Build one ggml-cpu-*.dll per x86 ISA level instead;
# llama_backend_init loads the best one the running CPU supports.
set(GGML_BACKEND_DL ON CACHE BOOL "load ggml backends as DLLs at runtime" FORCE)
set(GGML_CPU_ALL_VARIANTS ON CACHE BOOL "one ggml-cpu DLL per x86 ISA level" FORCE)
