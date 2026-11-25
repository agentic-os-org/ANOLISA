# Prepare once from the shared builder on a Docker host, before running release jobs.
# Build with DOCKER_BUILDKIT=0 because FROM references a local image ID.
# Keep the shared image tag available for existing releases and other components.
FROM sha256:0a5c24bde830394d0fb585a4c7c2021b315e394bf3a3e3abb22401b70e4b35db

RUN set -eu; \
    glibc_release="$(rpm -q --qf '%{VERSION}-%{RELEASE}.%{ARCH}' glibc)"; \
    dnf install -y --disablerepo='*' --enablerepo=powertools "glibc-static-${glibc_release}"; \
    dnf clean all

LABEL org.anolisa.release-builder.profile="gnu2.28-x86_64-sec-core"
