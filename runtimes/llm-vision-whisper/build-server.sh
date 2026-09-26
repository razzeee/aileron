#!/bin/bash
set -euo pipefail

: "${LLAMA_SERVER_REVISION:?missing immutable llama.cpp revision}"
: "${RUNTIME_VARIANT:=cpu}"
: "${LLAMA_BUILD_JOBS:=2}"

[[ "$LLAMA_SERVER_REVISION" =~ ^[0-9a-f]{40}$ ]] || { echo "llama.cpp revision must be a full commit hash" >&2; exit 1; }
git init /llama-server-src
git -C /llama-server-src remote add origin https://github.com/ggml-org/llama.cpp.git
git -C /llama-server-src fetch --depth 1 origin "$LLAMA_SERVER_REVISION"
git -C /llama-server-src checkout --detach FETCH_HEAD
test "$(git -C /llama-server-src rev-parse HEAD)" = "$LLAMA_SERVER_REVISION"
for patch in /llama-server-patches/*.patch; do
    git -C /llama-server-src apply --check --recount "$patch"
    git -C /llama-server-src apply --recount "$patch"
done
patch_hash=$(sha256sum /llama-server-patches/*.patch | sha256sum | cut -d ' ' -f 1)
printf '{"engine":"llama-server","revision":"%s","patch_sha256":"%s","embedding_recipe":"mean-raw-special-v1"}\n' \
    "$LLAMA_SERVER_REVISION" "$patch_hash" > /runtime-provenance.json

backend=()
case "$RUNTIME_VARIANT" in
    cpu) ;;
    cuda)
        backend=(-DGGML_CUDA=ON)
        # Let upstream select its portable PTX/cubin set for an all-device build.
        # CMake's literal "all" bypasses upstream's 12X -> 12Xa fix and produces
        # generic sm_120 code that cannot assemble Blackwell FP4 instructions.
        if [[ -n "${CUDA_DOCKER_ARCH:-}" && "$CUDA_DOCKER_ARCH" != all ]]; then
            backend+=("-DCMAKE_CUDA_ARCHITECTURES=$CUDA_DOCKER_ARCH")
        fi
        ;;
    rocm)
        # HIP static objects require the same non-PIE executable link used by
        # the Whisper ROCm wrapper in this image.
        backend=(-DGGML_HIP=ON "-DAMDGPU_TARGETS=${AMDGPU_TARGETS:?missing AMDGPU_TARGETS}"
                 -DCMAKE_EXE_LINKER_FLAGS=-no-pie)
        ;;
    vulkan) backend=(-DGGML_VULKAN=ON) ;;
    *) echo "unsupported RUNTIME_VARIANT=$RUNTIME_VARIANT" >&2; exit 1 ;;
esac

cmake -S /llama-server-src -B /llama-server-build -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF \
    -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_APP=OFF \
    -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF \
    -DLLAMA_OPENSSL=OFF -DLLAMA_SUBPROCESS=OFF "${backend[@]}"
cmake --build /llama-server-build --target llama-server --parallel "$LLAMA_BUILD_JOBS"
install -Dm755 /llama-server-build/bin/llama-server /runtime-bins/llama-server
install -Dm644 /llama-server-src/LICENSE /runtime-licenses/llama.cpp.LICENSE
