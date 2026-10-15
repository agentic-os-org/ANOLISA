# Supply the existing runner base image, pinned by digest, at build time.
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake pkg-config libsystemd-dev libssl-dev elfutils zlib1g-dev \
    btrfs-progs fuse3 libfuse3-dev ripgrep zsh shellcheck perl \
    && rm -rf /var/lib/apt/lists/*
COPY install-tools.sh verify-tools.sh sccache-job-started.sh sccache-job-completed.sh /opt/anolisa/
RUN bash /opt/anolisa/install-tools.sh \
    && chown -R runner:runner /opt/hostedtoolcache /opt/anolisa/ci-tools /home/runner
ENV PATH="/opt/anolisa/ci-tools/bin:/opt/hostedtoolcache/node/20.20.2/x64/bin:/home/runner/.cargo/bin:${PATH}" \
    UV_PYTHON_INSTALL_DIR="/opt/anolisa/ci-tools/uv-python" \
    SCCACHE_SKIP_CACHE_CHECK=1 \
    RUSTUP_DIST_SERVER=https://static.rust-lang.org \
    RUSTUP_UPDATE_ROOT=https://static.rust-lang.org/rustup
USER runner
RUN bash /opt/anolisa/verify-tools.sh
