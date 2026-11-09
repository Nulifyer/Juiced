# syntax=docker/dockerfile:1
FROM ubuntu:24.04 AS toolchain
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake ninja-build libssl-dev libcurl4-openssl-dev libzstd-dev pkg-config libdrm-dev git python3 curl ca-certificates && rm -rf /var/lib/apt/lists/*
FROM toolchain AS manager
WORKDIR /src
COPY . .
RUN printf '2026.44.1-juiced\n' > .version && cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF -DBUILD_TAURI_APP=OFF -DBUILD_TESTING=ON && cmake --build build -j4 --target lemond lemonade test_backend_mode_contract test_model_delete_shared_repo test_process_manager test_request_identity test_memory_budget test_inference_lease test_realtime_worker_lease test_routing_helper_reconcile test_live_throughput && ctest --test-dir build --output-on-failure -R '^(BackendModeContractTest|ModelDeleteSharedRepoTest|ProcessManagerTest|RequestIdentityTest|MemoryBudgetTest|InferenceLeaseTest|RealtimeWorkerLeaseTest|RoutingHelperReconcileTest|LiveThroughputTest)$'
RUN python3 juiced/tests/test_api.py build/lemond
RUN mkdir -p /out/licenses && cd /src/build/_deps && find . -maxdepth 2 -type f \( -iname 'license*' -o -iname 'copying*' -o -iname 'notice*' \) -exec cp --parents '{}' /out/licenses/ ';'

FROM oven/bun:1.4.3 AS bun
FROM node:22-bookworm-slim AS ui
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
WORKDIR /ui
COPY juiced/ui/package.json juiced/ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY juiced/ui/ ./
RUN bun test test/ && bun run build

FROM toolchain AS futo
RUN mkdir -p /native && curl --fail --location --retry 3 'https://gitlab.futo.org/api/v4/projects/keyboard%2Ftranscribe.cpp/repository/archive.tar.gz?sha=26d75933c16b2cec228e34c69e92a6c3c154b97c' -o /tmp/transcribe.tar.gz && tar -xzf /tmp/transcribe.tar.gz --strip-components=1 -C /native && rm /tmp/transcribe.tar.gz
RUN cmake -S /native -B /native/build -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF -DTRANSCRIBE_BUILD_TESTS=OFF -DTRANSCRIBE_USE_SYSTEM_BLAS=OFF -DTRANSCRIBE_VULKAN=OFF -DTRANSCRIBE_CUDA=OFF -DTRANSCRIBE_HIP=OFF -DTRANSCRIBE_METAL=OFF -DTRANSCRIBE_BUILD_SHARED=ON && cmake --build /native/build --target transcribe-cli -j4
COPY --from=manager /src/build/_deps/httplib-src/httplib.h /include/httplib.h
COPY --from=manager /src/build/_deps/json-src/single_include/ /include/
COPY juiced/runtime/futo_server.cpp /adapter/futo_server.cpp
RUN mkdir -p /out/futo/lib /out/futo/licenses && g++ -std=c++17 -O2 -pthread -I/native/include -I/native/examples/common -I/include /adapter/futo_server.cpp -L/native/build/src -ltranscribe -Wl,-rpath-link,/native/build/ggml/src -o /out/futo/futo-server && find /native/build -name '*.so*' -exec cp -L '{}' /out/futo/lib/ ';' && cp /native/LICENSE /out/futo/licenses/transcribe.txt && cp /native/THIRD-PARTY-LICENSES.md /out/futo/licenses/ && cp /native/ggml/LICENSE /out/futo/licenses/ggml.txt
RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH=/opt/juiced/futo/lib\nexec /opt/juiced/futo/futo-server "$@"\n' > /out/futo/futo-asr-server && chmod 755 /out/futo/futo-asr-server

FROM toolchain AS parakeet
RUN git clone --recursive https://github.com/mudler/parakeet.cpp.git /native && cd /native && git checkout da8bb4c45d08e081e759849bc6ee0a38720eb0b9 && git submodule update --init --recursive
RUN cmake -S /native -B /native/build -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF -DPARAKEET_BUILD_SERVER=ON -DPARAKEET_BUILD_TESTS=ON && cmake --build /native/build -j4 && ctest --test-dir /native/build --output-on-failure -LE model && mkdir -p /out/parakeet/bin /out/parakeet/lib /out/parakeet/licenses && cp /native/build/examples/server/parakeet-server /out/parakeet/bin/ && find /native/build -name '*.so*' -exec cp -L '{}' /out/parakeet/lib/ ';' && cp /native/LICENSE /out/parakeet/licenses/parakeet.txt
RUN cd /native && find . -type f \( -iname 'license*' -o -iname 'copying*' -o -iname 'notice*' \) -not -path './build/*' -not -path './.git/*' -exec cp --parents '{}' /out/parakeet/licenses/ ';'
RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH=/opt/juiced/parakeet/lib\nexec /opt/juiced/parakeet/bin/parakeet-server "$@"\n' > /out/parakeet/parakeet-server && chmod 755 /out/parakeet/parakeet-server

# Verified upstream release, including checkpoint/draft persistence. No local engine fork.
FROM toolchain AS llama
RUN mkdir -p /out/llama/bin /out/llama/licenses && curl --fail --location --retry 3 https://github.com/ggml-org/llama.cpp/releases/download/b12032/llama-b12032-bin-ubuntu-vulkan-x64.tar.gz -o /tmp/llama.tar.gz && echo 'f75e7107f9450034c5467492c41e9ed91816e2d815fc4b802c6bd58de7d5b126  /tmp/llama.tar.gz' | sha256sum -c - && tar -xzf /tmp/llama.tar.gz --strip-components=1 -C /out/llama/bin && cp /out/llama/bin/LICENSE /out/llama/licenses/llama.txt && rm /tmp/llama.tar.gz
RUN find /out/llama/bin -maxdepth 1 \( -name 'llama-*' ! -name 'llama-server' -o -name 'llama' -o -name 'libllama-quantize-impl.so*' \) -delete
RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH=/opt/juiced/llama/bin\nexec /opt/juiced/llama/bin/llama-server "$@"\n' > /out/llama/llama-server && chmod 755 /out/llama/llama-server && printf 'b12032 dde879bc4f954b809892fd0d660515f42536394a\n' > /out/llama/source-revision

FROM toolchain AS voice
RUN apt-get update && apt-get install -y --no-install-recommends python3-venv && rm -rf /var/lib/apt/lists/*
RUN python3 -m venv /opt/juiced/voice
COPY juiced/runtime/voice-requirements.txt /tmp/voice-requirements.txt
RUN /opt/juiced/voice/bin/pip install --no-cache-dir --require-hashes --only-binary=:all: -r /tmp/voice-requirements.txt
RUN mkdir /native && curl --fail --location --retry 3 https://github.com/roryeckel/wyoming_openai/archive/8e73875b2bd3d9d396d0dc750e58e639ed7b5f6a.tar.gz | tar -xz --strip-components=1 -C /native
RUN /opt/juiced/voice/bin/pip install --no-cache-dir /native --no-deps && /opt/juiced/voice/bin/pip uninstall -y pip setuptools && cp /native/LICENSE /opt/juiced/voice/LICENSE.wyoming_openai && printf '8e73875b2bd3d9d396d0dc750e58e639ed7b5f6a\n' > /opt/juiced/voice/source-revision

FROM ubuntu:24.04 AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl libatomic1 libcurl4t64 libdrm-amdgpu1 libgomp1 \
    libreadline8t64 libssl3t64 libstdc++6 libvulkan1 libzstd1 \
    mesa-vulkan-drivers python3 tar unzip vulkan-tools xz-utils zlib1g \
    && rm -rf /var/lib/apt/lists/*
RUN groupadd --gid 10001 juiced && useradd --uid 10001 --gid 10001 \
    --home-dir /opt/lemonade --no-create-home --shell /usr/sbin/nologin juiced
ARG SOURCE_REVISION=development
ARG SERVICE_VERSION=development
ENV JUICED_SERVICE_VERSION=${SERVICE_VERSION} JUICED_SOURCE_REVISION=${SOURCE_REVISION}
LABEL org.opencontainers.image.title="Juiced" org.opencontainers.image.source="https://github.com/Nulifyer/Juiced" org.opencontainers.image.revision="${SOURCE_REVISION}" org.opencontainers.image.version="${SERVICE_VERSION}" org.opencontainers.image.description="Local multi-backend model appliance derived from Lemonade; T3-derived model library and playground"
COPY --chown=10001:10001 --from=manager /src/build/lemond /src/build/lemonade /opt/lemonade/
COPY --chown=10001:10001 --from=manager /src/build/resources/ /opt/lemonade/resources/
COPY --chown=10001:10001 --from=manager /out/licenses/ /opt/juiced/licenses/build-dependencies/
COPY --chown=10001:10001 --from=ui /web/ /opt/lemonade/resources/juiced/
COPY --chown=10001:10001 --from=futo /out/ /opt/juiced/
COPY --chown=10001:10001 --from=parakeet /out/ /opt/juiced/
COPY --chown=10001:10001 --from=llama /out/ /opt/juiced/
COPY --chown=10001:10001 --from=voice /opt/juiced/voice/ /opt/juiced/voice/
COPY --chown=10001:10001 juiced/runtime/voice.py /opt/juiced/voice/launcher.py
COPY --chown=10001:10001 LICENSE /opt/juiced/licenses/lemonade-Apache-2.0.txt
COPY --chown=10001:10001 LICENSES/ /opt/juiced/licenses/inherited/
COPY --chown=10001:10001 juiced/ui/LICENSE.t3code juiced/ui/T3-SOURCES.json /opt/juiced/licenses/
RUN mkdir -p /data/config /data/cache /data/huggingface /data/prompt-state /library /run/lemonade && chown 10001:10001 /data /data/config /data/cache /data/huggingface /data/prompt-state /opt/lemonade /run/lemonade && chmod 700 /data/config /data/prompt-state /run/lemonade
COPY --chown=10001:10001 juiced/runtime/defaults.json /opt/juiced/defaults.json
ENV HOME=/opt/lemonade HF_HOME=/data/huggingface XDG_CACHE_HOME=/data/cache/xdg XDG_RUNTIME_DIR=/run/lemonade LEMONADE_DEFAULTS_PATH=/opt/juiced/defaults.json PATH="/opt/lemonade:${PATH}"
ENV JUICED_VOICE_PYTHON=/opt/juiced/voice/bin/python3 JUICED_VOICE_SCRIPT=/opt/juiced/voice/launcher.py PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
USER 10001:10001
WORKDIR /opt/lemonade
EXPOSE 8080 10300
HEALTHCHECK --interval=15s --timeout=6s --start-period=60s --retries=6 CMD ["curl","--fail","--silent","http://127.0.0.1:8080/v1/juiced/state"]
ENTRYPOINT ["/opt/lemonade/lemond"]
CMD ["/data/cache","/data/config","--host","0.0.0.0","--port","8080"]
