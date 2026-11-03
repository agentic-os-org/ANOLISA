# Extend the prepared ARM64 sysroot without changing the existing runner image.
FROM anolisa/rust-release-builder:gnu2.28-aarch64@sha256:8c0788e130476253259a838d7ddad7d04a34778d60ab391f6fabaf007639a2dd

RUN set -eu; \
    mkdir /tmp/fuse3-rpms; \
    cd /tmp/fuse3-rpms; \
    for package in fuse3-libs fuse3-devel; do \
        curl -fsSLO "https://repo.almalinux.org/almalinux/8/BaseOS/aarch64/os/Packages/$package-3.3.0-19.el8.aarch64.rpm"; \
    done; \
    printf '%s\n' \
        'd1be08a85d0927f8e07825c680b0ea1db2d3b422baa1510f50d8c59474fb254f  fuse3-libs-3.3.0-19.el8.aarch64.rpm' \
        'efa2e2041f97a44a80618d0b0a79bc601fcec9f1a175b24eff95057332808810  fuse3-devel-3.3.0-19.el8.aarch64.rpm' \
        | sha256sum -c -; \
    sysroot=/usr/xcc/aarch64-unknown-linux-gnu/aarch64-unknown-linux-gnu/sysroot; \
    cd "$sysroot"; \
    for package in /tmp/fuse3-rpms/*.rpm; do \
        rpm2cpio "$package" > /tmp/fuse3.cpio; \
        cpio -idm < /tmp/fuse3.cpio; \
    done; \
    test -f usr/include/fuse3/fuse.h; \
    readelf -h usr/lib64/libfuse3.so.3 | grep -F AArch64; \
    PKG_CONFIG_PATH= PKG_CONFIG_SYSROOT_DIR="$sysroot" \
        PKG_CONFIG_LIBDIR="$sysroot/usr/lib64/pkgconfig:$sysroot/usr/share/pkgconfig" \
        pkg-config --modversion fuse3; \
    rm -rf /tmp/fuse3-rpms /tmp/fuse3.cpio
