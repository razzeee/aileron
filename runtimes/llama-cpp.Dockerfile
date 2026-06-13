ARG BASE_IMAGE="python:3.12-slim"
FROM ${BASE_IMAGE}

ARG RUNTIME_ID
ARG ENTRYPOINT_PATH
ARG INSTALL_SOURCE="pypi"
ARG LLAMA_CPP_PYTHON_REF="b5eefc82e0fd415d5547c81367c29b159c0268d3"
ARG EXTRA_PIP_PACKAGES=""
ARG PIP_INSTALL_ARGS=""
ARG APT_PACKAGES="build-essential cmake git ninja-build"
ARG CMAKE_ARGS=""
ARG CUDA_DOCKER_ARCH=""
ARG FORCE_CMAKE=""
ARG HSA_OVERRIDE_GFX_VERSION=""
ARG ROCM_PATH="/opt/rocm"

ENV CMAKE_ARGS="${CMAKE_ARGS}"
ENV CUDA_DOCKER_ARCH="${CUDA_DOCKER_ARCH}"
ENV FORCE_CMAKE="${FORCE_CMAKE}"
ENV ROCM_PATH="${ROCM_PATH}"
ENV PATH="${ROCM_PATH}/bin:${PATH}"
ENV HSA_OVERRIDE_GFX_VERSION="${HSA_OVERRIDE_GFX_VERSION}"

RUN apt-get update && apt-get install -y --no-install-recommends ${APT_PACKAGES} \
    && rm -rf /var/lib/apt/lists/* \
    && (command -v python >/dev/null || ln -sf python3 /usr/bin/python)

RUN if [ "$INSTALL_SOURCE" = "git" ]; then \
        python -m pip install --no-cache-dir ${PIP_INSTALL_ARGS} \
            "git+https://github.com/abetlen/llama-cpp-python.git@${LLAMA_CPP_PYTHON_REF}" \
            ${EXTRA_PIP_PACKAGES}; \
    else \
        python -m pip install --no-cache-dir ${PIP_INSTALL_ARGS} \
            "llama-cpp-python>=0.2.90" \
            ${EXTRA_PIP_PACKAGES}; \
    fi

COPY runtimes/_llama_cpp_common/aileron_runtime_common.py /aileron_runtime_common.py
COPY ${ENTRYPOINT_PATH} /entrypoint.py

LABEL aileron.runtime_id="${RUNTIME_ID}"

ENTRYPOINT ["python", "/entrypoint.py"]
