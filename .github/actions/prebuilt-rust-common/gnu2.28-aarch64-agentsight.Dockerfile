# Prepared base IDs: ARM bb7b152b8106f726fe041e637b3722a8613c6e6298834b252d9041ac92efca78,
# host 0a5c24bde830394d0fb585a4c7c2021b315e394bf3a3e3abb22401b70e4b35db.
# cross-profile.sh pins the resulting prepared image before executing Cargo.
ARG ARM_IMAGE=anolisa/rust-release-builder:gnu2.28-aarch64-fuse3
ARG HOST_IMAGE=anolisa/rust-release-builder:gnu2.28-x86_64
FROM ${ARM_IMAGE} AS arm
RUN set -eu; \
    mkdir /tmp/elf-rpms; cd /tmp/elf-rpms; \
    for package in elfutils-libelf-0.190-2.el8.alma.1 elfutils-libelf-devel-0.190-2.el8.alma.1 libzstd-1.4.4-1.el8 xz-libs-5.2.4-4.el8_6 bzip2-libs-1.0.6-28.el8_10; do \
      curl -fsSLO "https://repo.almalinux.org/almalinux/8/BaseOS/aarch64/os/Packages/$package.aarch64.rpm"; \
    done; \
    printf '%s\n' \
      '0e2fcc2d3fbd68011453555e50d49fe8f4127c95f953fb9d76c4c3385c0cae26  bzip2-libs-1.0.6-28.el8_10.aarch64.rpm' \
      '82ec82f44e4308a411672ed64cc16d288a1f85df2ad395219dc0ad719ff8128d  libzstd-1.4.4-1.el8.aarch64.rpm' \
      'eb1bf61e0b1635d73c0f8abda8a892d3facb8112fa5dbeb0865a7bdfacb4298a  xz-libs-5.2.4-4.el8_6.aarch64.rpm' \
      'd6eb084a6c41bb61494df4b0989fca59ac62c19a287af38ad40b6f57355db33e  elfutils-libelf-0.190-2.el8.alma.1.aarch64.rpm' \
      '7eb9eb5f1eeb3bc82e70b5b5db18c31dbaa8f8d698522f6cec521b0f26fcaa18  elfutils-libelf-devel-0.190-2.el8.alma.1.aarch64.rpm' | sha256sum -c -; \
    cd /usr/xcc/aarch64-unknown-linux-gnu/aarch64-unknown-linux-gnu/sysroot; \
    for package in /tmp/elf-rpms/*.rpm; do \
      rpm2cpio "$package" > /tmp/elf.cpio; cpio -idm < /tmp/elf.cpio; \
    done; \
    readelf -h usr/lib64/libelf.so.1 | grep -F AArch64; \
    rm -rf /tmp/elf-rpms /tmp/elf.cpio

FROM ${HOST_IMAGE}
COPY --from=arm /usr/xcc /usr/xcc
LABEL org.anolisa.release-builder.profile="gnu2.28-aarch64" \
      org.anolisa.release-builder.base-image="docker.io/dockcross/manylinux_2_28-aarch64@sha256:63d09e1726a914c85b0087aa00b966df896fac962095310b915332f0135bfd74"
