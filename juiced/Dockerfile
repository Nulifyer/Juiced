# syntax=docker/dockerfile:1
FROM ubuntu:24.04 AS manager
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake ninja-build libssl-dev libcurl4-openssl-dev libzstd-dev pkg-config libdrm-dev git python3 ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN printf '2026.44.1-juiced\n' > .version && cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF -DBUILD_TAURI_APP=OFF -DBUILD_TESTING=ON && cmake --build build -j4 --target lemond lemonade test_backend_mode_contract && ctest --test-dir build --output-on-failure -R '^BackendModeContractTest$'
RUN python3 juiced/tests/test_api.py build/lemond

FROM oven/bun:1.3.12 AS bun
FROM node:22-bookworm-slim AS ui
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
WORKDIR /ui
COPY juiced/ui/package.json juiced/ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY juiced/ui/ ./
RUN bun run build

FROM manager AS futo
RUN mkdir -p /native && curl --fail --location --retry 3 'https://gitlab.futo.org/api/v4/projects/keyboard%2Ftranscribe.cpp/repository/archive.tar.gz?sha=26d75933c16b2cec228e34c69e92a6c3c154b97c' | tar -xz --strip-components=1 -C /native
RUN cmake -S /native -B /native/build -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF -DTRANSCRIBE_BUILD_TESTS=OFF -DTRANSCRIBE_USE_SYSTEM_BLAS=OFF -DTRANSCRIBE_VULKAN=OFF -DTRANSCRIBE_CUDA=OFF -DTRANSCRIBE_HIP=OFF -DTRANSCRIBE_METAL=OFF -DTRANSCRIBE_BUILD_SHARED=ON && cmake --build /native/build --target transcribe-cli -j4
RUN mkdir -p /out/futo/lib /out/futo/licenses && g++ -std=c++17 -O2 -pthread -I/native/include -I/native/examples/common -I/src/build/_deps/httplib-src -I/src/build/_deps/json-src/single_include /src/juiced/runtime/futo_server.cpp -L/native/build/src -ltranscribe -Wl,-rpath-link,/native/build/ggml/src -o /out/futo/futo-server && find /native/build -name '*.so*' -exec cp -L '{}' /out/futo/lib/ ';' && cp /native/LICENSE /out/futo/licenses/transcribe.txt && cp /native/THIRD-PARTY-LICENSES.md /out/futo/licenses/ && cp /native/ggml/LICENSE /out/futo/licenses/ggml.txt
RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH=/opt/juiced/futo/lib\nexec /opt/juiced/futo/futo-server "$@"\n' > /out/futo/futo-asr-server && chmod 755 /out/futo/futo-asr-server

FROM manager AS parakeet
RUN git clone --recursive https://github.com/mudler/parakeet.cpp.git /native && cd /native && git checkout da8bb4c45d08e081e759849bc6ee0a38720eb0b9 && git submodule update --init --recursive
RUN cmake -S /native -B /native/build -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF -DPARAKEET_BUILD_SERVER=ON -DPARAKEET_BUILD_TESTS=ON && cmake --build /native/build -j4 && ctest --test-dir /native/build --output-on-failure -LE model && mkdir -p /out/parakeet/bin /out/parakeet/lib /out/parakeet/licenses && cp /native/build/examples/server/parakeet-server /out/parakeet/bin/ && find /native/build -name '*.so*' -exec cp -L '{}' /out/parakeet/lib/ ';' && cp /native/LICENSE /out/parakeet/licenses/parakeet.txt
RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH=/opt/juiced/parakeet/lib\nexec /opt/juiced/parakeet/bin/parakeet-server "$@"\n' > /out/parakeet/parakeet-server && chmod 755 /out/parakeet/parakeet-server

# This pin includes checkpoint/draft persistence missing from the stock Vulkan pin.
FROM manager AS llama
RUN apt-get update && apt-get install -y --no-install-recommends libvulkan-dev glslc && rm -rf /var/lib/apt/lists/*
RUN git clone https://github.com/ggml-org/llama.cpp.git /native && cd /native && git checkout 785322d17f2d51e5185cf996a1f4e46d87640841
RUN cmake -S /native -B /native/build -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_VULKAN=ON -DGGML_NATIVE=OFF -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_SERVER=ON && cmake --build /native/build --target llama-server -j4 && mkdir -p /out/llama/bin /out/llama/lib /out/llama/licenses && cp /native/build/bin/llama-server /out/llama/bin/ && find /native/build -name '*.so*' -exec cp -L '{}' /out/llama/lib/ ';' && cp /native/LICENSE /out/llama/licenses/llama.txt
RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH=/opt/juiced/llama/lib\nexec /opt/juiced/llama/bin/llama-server "$@"\n' > /out/llama/llama-server && chmod 755 /out/llama/llama-server && printf '785322d17f2d51e5185cf996a1f4e46d87640841\n' > /out/llama/source-revision

FROM ghcr.io/lemonade-sdk/lemonade-server@sha256:4602f637446980aeb25c3b6350d4fe414770f10f8372b459d7ba32b577347185
USER root
ARG SOURCE_REVISION=development
ARG SERVICE_VERSION=development
LABEL org.opencontainers.image.title="Juiced" org.opencontainers.image.source="https://github.com/Nulifyer/Juiced" org.opencontainers.image.revision="${SOURCE_REVISION}" org.opencontainers.image.version="${SERVICE_VERSION}" org.opencontainers.image.description="Local multi-backend model appliance derived from Lemonade; T3-derived model library and playground"
COPY --from=manager /src/build/lemond /src/build/lemonade /opt/lemonade/
COPY --from=manager /src/build/resources/ /opt/lemonade/resources/
COPY --from=ui /web/ /opt/lemonade/resources/juiced/
COPY --from=futo /out/ /opt/juiced/
COPY --from=parakeet /out/ /opt/juiced/
COPY --from=llama /out/ /opt/juiced/
COPY LICENSE /opt/juiced/licenses/lemonade-Apache-2.0.txt
COPY juiced/ui/LICENSE.t3code juiced/ui/T3-SOURCES.json /opt/juiced/licenses/
RUN mkdir -p /data/config /data/cache /data/huggingface /data/prompt-state /library && chown -R 10001:10001 /data /opt/juiced && chmod 700 /data/config /data/prompt-state
COPY juiced/runtime/defaults.json /opt/juiced/defaults.json
ENV HOME=/opt/lemonade HF_HOME=/data/huggingface LEMONADE_DEFAULTS_PATH=/opt/juiced/defaults.json
USER 10001:10001
WORKDIR /opt/lemonade
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=6s --start-period=60s --retries=6 CMD ["curl","--fail","--silent","http://127.0.0.1:8080/v1/juiced/state"]
ENTRYPOINT ["/opt/lemonade/lemond"]
CMD ["/data/cache","/data/config","--host","0.0.0.0","--port","8080"]
