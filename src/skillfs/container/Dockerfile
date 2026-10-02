# Dedicated SkillFS FUSE sidecar image.
#
# This image runs one PID 1 supervisor around
# `skillfs mount ... --foreground --allow-other`. It deliberately does not
# include or start any other ANOLISA service, and it does not depend on systemd
# or the SkillFS managed-mount supervisor.
#
# Build context: the `src/skillfs` component directory.
#   docker build -f container/Dockerfile .

# ---------------------------------------------------------------------------
# Stage 1 — builder
# ---------------------------------------------------------------------------
# The builder and the runtime share the same base image so the produced binary
# links against the runtime glibc and fuse3 ABI. Do not swap one side only.
ARG BASE_IMAGE=debian:bookworm-slim

FROM ${BASE_IMAGE} AS builder

# Cargo.toml pins rust-version = "1.86" and edition = "2024". The toolchain is
# installed explicitly rather than taken from the distro repo so the build does
# not silently move to whatever rust version the base image ships this month.
ARG RUST_VERSION=1.86.0
ARG RUSTUP_DIST_SERVER=https://static.rust-lang.org
ARG RUSTUP_UPDATE_ROOT=https://static.rust-lang.org/rustup

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        gcc \
        make \
        binutils \
        pkg-config \
        libfuse3-dev \
        curl \
        ca-certificates; \
    rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER}"; \
    export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT}"; \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal \
            --default-toolchain "${RUST_VERSION}"; \
    rustc --version; \
    cargo --version

WORKDIR /build

# Copy the whole component. `.dockerignore` keeps `target/` and local mount
# scratch out of the context so the image does not embed host build artifacts.
COPY . /build

# `--locked` makes the dependency set exactly Cargo.lock, which is what makes
# repeated builds of the same commit produce the same dependency tree.
RUN set -eux; \
    RUSTFLAGS="-C opt-level=3 -C codegen-units=1" \
        cargo build --release --locked -p skillfs; \
    strip target/release/skillfs; \
    target/release/skillfs --version

# ---------------------------------------------------------------------------
# Stage 2 — runtime
# ---------------------------------------------------------------------------
FROM ${BASE_IMAGE} AS runtime

ARG VERSION=unknown
ARG REVISION=unknown
ARG SOURCE_URL=https://github.com/alibaba/anolisa

# fuse3 provides the fusermount3 helper and the libfuse3 runtime the fuser crate
# links against. The rest are the exact utilities the entrypoint, preflight, and
# probe scripts use; they are named explicitly rather than assumed from the base
# image, so a slimmer base cannot silently break startup:
#
#   bash                  all four scripts
#   coreutils             stat, realpath, id, timeout, wc, install, sleep, head
#   grep, gawk, sed       fuse.conf and /proc/self/mountinfo parsing
#   findutils             the example seeding step in the reference manifest
#   util-linux            umount, for residual-mount cleanup
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        fuse3 \
        bash \
        coreutils \
        grep \
        gawk \
        sed \
        findutils \
        util-linux; \
    rm -rf /var/lib/apt/lists/*

# `--allow-other` requires user_allow_other in /etc/fuse.conf. The sidecar runs
# as root today, where libfuse skips the check, but the flag is written
# unconditionally so a future non-root sidecar UID does not silently regress,
# and so the preflight check has something real to assert.
RUN set -eux; \
    touch /etc/fuse.conf; \
    if ! grep -qE '^[[:space:]]*user_allow_other[[:space:]]*$' /etc/fuse.conf; then \
        printf 'user_allow_other\n' >> /etc/fuse.conf; \
    fi; \
    chmod 0644 /etc/fuse.conf; \
    grep -qE '^[[:space:]]*user_allow_other[[:space:]]*$' /etc/fuse.conf

COPY --from=builder /build/target/release/skillfs /usr/local/bin/skillfs
COPY container/entrypoint.sh /usr/local/bin/skillfs-sidecar-entrypoint
COPY container/preflight.sh /usr/local/bin/skillfs-preflight
COPY container/mount-probe.sh /usr/local/bin/skillfs-mount-probe
COPY container/supervisor.sh /usr/local/bin/skillfs-supervisor

RUN set -eux; \
    chmod 0755 /usr/local/bin/skillfs \
        /usr/local/bin/skillfs-sidecar-entrypoint \
        /usr/local/bin/skillfs-preflight \
        /usr/local/bin/skillfs-mount-probe \
        /usr/local/bin/skillfs-supervisor; \
    skillfs --version

# Defaults match src/skillfs/deploy/kubernetes. Override per deployment.
#
# SKILLFS_MOUNTPOINT is a *subdirectory* of the shared volume, not the volume
# root: mount propagation carries a mount created inside the shared volume, and
# over-mounting the volume root itself is not the supported topology.
#
# SKILLFS_PROBE_FILE is relative to the mountpoint. SkillFS exposes the view
# under `<mountpoint>/skills/<skill-name>/`, so the probe path includes `skills/`.
ENV SKILLFS_SOURCE=/var/lib/skillfs/source \
    SKILLFS_MOUNTPOINT=/var/lib/skillfs/shared/mount \
    SKILLFS_DISCOVER_ROOT=/var/lib/skillfs/shared/mount/skills \
    SKILLFS_EXTRA_ARGS="" \
    SKILLFS_PROBE_FILE=skills/skillfs-container-example/SKILL.md \
    SKILLFS_PROBE_TIMEOUT=5 \
    SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS=2 \
    SKILLFS_SUPERVISOR_FAILURE_THRESHOLD=2 \
    SKILLFS_SUPERVISOR_STABLE_HEALTHY_PROBES=3 \
    SKILLFS_SUPERVISOR_STARTUP_TIMEOUT_SECONDS=30 \
    SKILLFS_SUPERVISOR_STOP_TIMEOUT_SECONDS=10 \
    SKILLFS_SUPERVISOR_MAX_FAILED_ATTEMPTS=5 \
    SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS=1 \
    SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS=30 \
    RUST_LOG=info

LABEL org.opencontainers.image.title="SkillFS FUSE sidecar" \
      org.opencontainers.image.description="SkillFS foreground FUSE mount sidecar for Kubernetes" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.source="${SOURCE_URL}" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.vendor="The Anolisa Authors"

# The entrypoint execs the supervisor as PID 1. It forwards termination signals
# to the foreground mount worker and performs bounded cleanup before exiting.
ENTRYPOINT ["/usr/local/bin/skillfs-sidecar-entrypoint"]
