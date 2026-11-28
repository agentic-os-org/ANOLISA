# Build for the x64 runners that run the sec-core RPM, raw, and source jobs.
# docker build --pull --no-cache --platform linux/amd64 -t alinux4-sec-core-ci:local \
#   .github/runner-images/alinux4-sec-core-ci

ARG BASE_IMAGE=alibaba-cloud-linux-4-registry.cn-hangzhou.cr.aliyuncs.com/alinux4/alinux4@sha256:e3fd398154003b60e9a1649ae6a2f209dc0466bae971ba21afcc4e5c5cb5dff8
FROM ${BASE_IMAGE}

USER root

# The V2 RPM spec requires distro RPMs for both cargo and rust at this version.
ARG RUST_TOOLCHAIN=1.93.1

# Use the public Aliyun mirror over HTTPS. Keep its metadata for builddep and
# local RPM installation. CI can use dnf --cacheonly makecache to check it.
RUN set -eux; \
    sed -i -e 's/mirrors\.cloud\.aliyuncs\.com/mirrors.aliyun.com/g' \
        -e 's#http://mirrors\.aliyun\.com/#https://mirrors.aliyun.com/#g' /etc/yum.repos.d/*.repo; \
    sed -i '/^[[:space:]]*metadata_expire[[:space:]]*=/d; /^\[main\]$/a metadata_expire=never' \
        /etc/dnf/dnf.conf; \
    rust_version="${RUST_TOOLCHAIN:-1.93.1}"; \
    dnf --refresh install -y \
        bubblewrap curl findutils git gnupg2 gzip jq make sudo tar xz \
        clang gcc gcc-c++ glibc-static libseccomp-devel llvm openssl-devel pkgconf-pkg-config \
        dnf-plugins-core rpm-build rpmdevtools systemd-rpm-macros \
        loongshield nodejs nodejs-npm python3 python3-pip systemd \
        "cargo-${rust_version}" "rust-${rust_version}"; \
    dnf clean packages; \
    dnf --cacheonly makecache

# Match the source-build job's sudo setup in its disposable root container.
RUN printf '%s\n' '#%PAM-1.0' \
        'auth       sufficient   pam_rootok.so' \
        'account    sufficient   pam_permit.so' \
        'session    sufficient   pam_permit.so' > /etc/pam.d/sudo

# Use PyPI for the pinned versions because the distro's mirror can lag.
RUN python3 -m pip install --no-cache-dir --index-url https://pypi.org/simple/ \
    uv==0.13.10 packaging==26.4

RUN test "$(rpm -q --qf '%{VERSION}' cargo)" = "${RUST_TOOLCHAIN:-1.93.1}" && cargo --version
RUN test "$(rpm -q --qf '%{VERSION}' rust)" = "${RUST_TOOLCHAIN:-1.93.1}" && rustc --version
RUN python3.11 -c 'import packaging' && uv --version
RUN rpm -q loongshield systemd bubblewrap nodejs nodejs-npm && node --version && npm --version
RUN rpmbuild --version && sudo -u nobody true
