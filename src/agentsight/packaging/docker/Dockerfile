# AgentSight runtime image.
#
# Consumes the release RPM produced by `src/agentsight/scripts/rpm-build.sh`;
# the binary (with the embedded Dashboard) is not rebuilt here. Build from the
# repository root after a release build:
#
#   docker build -f src/agentsight/packaging/docker/Dockerfile \
#     --build-arg VERSION=$(git describe --tags --always) -t agentsight:dev .
#
# The build context must contain `rpms/<arch>/agentsight-*.rpm`.
FROM alibaba-cloud-linux-4-registry.cn-hangzhou.cr.aliyuncs.com/alinux4/alinux4:latest

ARG VERSION=unknown

LABEL org.opencontainers.image.title="AgentSight" \
      org.opencontainers.image.description="eBPF-based observability for AI agents" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.source="https://github.com/alibaba/anolisa"

RUN sed -i -e "s/cloud.aliyuncs/aliyun/g" /etc/yum.repos.d/*.repo

# rpm-build.sh emits arch dirs in RPM naming (x86_64/aarch64); map Docker's
# TARGETARCH (amd64/arm64) onto it so a multi-arch build context (both
# rpms/x86_64/ and rpms/aarch64/ present) cannot match two RPMs at once.
COPY rpms/ /tmp/rpms/
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
      amd64) rpm_arch=x86_64 ;; \
      arm64) rpm_arch=aarch64 ;; \
      *) echo "unsupported TARGETARCH=${TARGETARCH}"; exit 1 ;; \
    esac && \
    dnf install -y /tmp/rpms/"${rpm_arch}"/agentsight-*.rpm && dnf clean all && rm -rf /tmp/rpms

# Dashboard API + UI. Keep it inside the cluster; reach it via port-forward.
EXPOSE 7396

# agentsight-start supervises `trace` and `serve` and forwards SIGTERM/SIGHUP,
# so the pod lifecycle (stop, ConfigMap reload via restart) maps onto it.
ENTRYPOINT ["/usr/local/bin/agentsight-start"]
