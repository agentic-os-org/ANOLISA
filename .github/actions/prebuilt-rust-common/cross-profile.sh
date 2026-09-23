#!/usr/bin/env bash
# Run one Cargo command with a prepared ANOLISA release Cross profile.
set -euo pipefail

RUST_VERSION=1.93.0
RUST_TOOLCHAIN=1.93.0-x86_64-unknown-linux-gnu
SCCACHE_VERSION=0.17.0
SCCACHE_CACHE_SIZE=20G
GNU217_X86_BASE="docker.io/dockcross/manylinux2014-x64@sha256:ab5968050aa67592ef8fde28f3d304881bf2a394f160010d0bb13e98b4ed1b3b"
GNU217_X86_IMAGE="anolisa/rust-release-builder:gnu2.17-x86_64"
GNU217_X86_IMAGE_ID="sha256:72b3599de7e2406e6d0b3d1fd803cf4f613486868972f83c73b1fa4e145ea42a"
GNU217_ARM_BASE="docker.io/dockcross/manylinux2014-aarch64@sha256:86fb00cdd7f386dd13458ad6c55699ee78586da237087b14d0f94e1ea417ff54"
GNU217_ARM_IMAGE="anolisa/rust-release-builder:gnu2.17-aarch64"
GNU217_ARM_IMAGE_ID="sha256:4cec3c91665c86a79bb02aa4940eccbd40542bb3367bbaa4e24cced78aeb4421"
GNU228_X86_BASE="docker.io/dockcross/manylinux_2_28-x64@sha256:263b776c5dc6ae7e50942c5fbb82eb24a1ec64016cd6dd10831d51c2201923cf"
GNU228_X86_IMAGE="anolisa/rust-release-builder:gnu2.28-x86_64"
GNU228_X86_IMAGE_ID="sha256:0a5c24bde830394d0fb585a4c7c2021b315e394bf3a3e3abb22401b70e4b35db"
GNU228_ARM_BASE="docker.io/dockcross/manylinux_2_28-aarch64@sha256:63d09e1726a914c85b0087aa00b966df896fac962095310b915332f0135bfd74"
GNU228_ARM_IMAGE="anolisa/rust-release-builder:gnu2.28-aarch64"
GNU228_ARM_IMAGE_ID="sha256:8c0788e130476253259a838d7ddad7d04a34778d60ab391f6fabaf007639a2dd"
DARWIN_IMAGE="anolisa/rust-release-builder:darwin11-aarch64"
DARWIN_IMAGE_ID="sha256:f43abc5ea60980fe1a452607ebc481ea0636bb6e6f0acbd03f7ed1f5064459e1"
DARWIN_X86_IMAGE="anolisa/rust-release-builder:darwin11-x86_64"
DARWIN_X86_IMAGE_ID="sha256:938ed84677d6f90b95719e3e9f69d6b4613b0b09aa47c9e7ece659ccfdd62ddc"
DARWIN_SDK_SHA256="71ebe09d97f45d48c9814e69b524ee3577dddfe7393aa4eac8615f07d6f7e0f5"
DARWIN_OSXCROSS_REF="27d21e4977c9751d01199c7a226a6faf494c3dd9"

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

profile_target() {
    case "$1" in
        gnu2.17-x86_64|gnu2.28-x86_64) printf 'x86_64-unknown-linux-gnu\n' ;;
        gnu2.17-aarch64|gnu2.28-aarch64) printf 'aarch64-unknown-linux-gnu\n' ;;
        darwin11-aarch64) printf 'aarch64-apple-darwin\n' ;;
        darwin11-x86_64) printf 'x86_64-apple-darwin\n' ;;
        *) die "unsupported release Cross profile: $1" ;;
    esac
}

profile_image() {
    case "$1" in
        gnu2.17-x86_64) printf '%s\n' "$GNU217_X86_IMAGE" ;;
        gnu2.17-aarch64) printf '%s\n' "$GNU217_ARM_IMAGE" ;;
        gnu2.28-x86_64) printf '%s\n' "$GNU228_X86_IMAGE" ;;
        gnu2.28-aarch64) printf '%s\n' "$GNU228_ARM_IMAGE" ;;
        darwin11-aarch64) printf '%s\n' "$DARWIN_IMAGE" ;;
        darwin11-x86_64) printf '%s\n' "$DARWIN_X86_IMAGE" ;;
        *) die "unsupported release Cross profile: $1" ;;
    esac
}

profile_image_id() {
    case "$1" in
        gnu2.17-x86_64) printf '%s\n' "$GNU217_X86_IMAGE_ID" ;;
        gnu2.17-aarch64) printf '%s\n' "$GNU217_ARM_IMAGE_ID" ;;
        gnu2.28-x86_64) printf '%s\n' "$GNU228_X86_IMAGE_ID" ;;
        gnu2.28-aarch64) printf '%s\n' "$GNU228_ARM_IMAGE_ID" ;;
        darwin11-aarch64) printf '%s\n' "$DARWIN_IMAGE_ID" ;;
        darwin11-x86_64) printf '%s\n' "$DARWIN_X86_IMAGE_ID" ;;
        *) die "unsupported release Cross profile: $1" ;;
    esac
}

profile_path() {
    case "$1" in
        gnu2.17-x86_64)
            printf '/opt/rh/devtoolset-10/root/usr/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n'
            ;;
        gnu2.17-aarch64)
            printf '/usr/xcc/aarch64-unknown-linux-gnu/bin:/opt/rh/devtoolset-10/root/usr/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n'
            ;;
        gnu2.28-x86_64)
            printf '/opt/rh/gcc-toolset-14/root/usr/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n'
            ;;
        gnu2.28-aarch64)
            printf '/usr/xcc/aarch64-unknown-linux-gnu/bin:/opt/rh/gcc-toolset-14/root/usr/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n'
            ;;
        darwin11-aarch64|darwin11-x86_64)
            printf '/opt/osxcross/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n'
            ;;
        *) die "unsupported release Cross profile: $1" ;;
    esac
}

image_label() {
    docker image inspect --format "{{ index .Config.Labels \"$2\" }}" "$1"
}

verify_common_tools() {
    local actual
    local target="$1"

    for command in cross docker rustc rustup; do
        command -v "$command" >/dev/null || die "missing required command: $command"
    done
    actual="$(RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN" rustc --version | awk '{print $2}')"
    [ "$actual" = "$RUST_VERSION" ] || \
        die "rustc $RUST_VERSION is required, got $actual"
    rustup target list --installed --toolchain "$RUST_TOOLCHAIN" | grep -Fxq "$target" || \
        die "Rust target $target is not installed for $RUST_TOOLCHAIN"
    actual="$(RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN" cross --version | \
        sed -nE 's/^cross[[:space:]]+([^[:space:]]+).*$/\1/p')"
    [ "$actual" = 0.2.5 ] || die "Cross 0.2.5 is required, got ${actual:-unknown}"
}

verify_linux_profile() {
    local profile="$1"
    local image="$2"
    local base glibc

    case "$profile" in
        gnu2.17-x86_64)
            base="$GNU217_X86_BASE"
            glibc=2.17
            ;;
        gnu2.17-aarch64)
            base="$GNU217_ARM_BASE"
            glibc=2.17
            ;;
        gnu2.28-x86_64)
            base="$GNU228_X86_BASE"
            glibc=2.28
            ;;
        gnu2.28-aarch64)
            base="$GNU228_ARM_BASE"
            glibc=2.28
            ;;
    esac
    [ "$(image_label "$image" org.anolisa.release-builder.profile)" = "$profile" ] || \
        die "$image does not identify profile $profile"
    [ "${base##*@}" = "$(image_label "$image" org.anolisa.release-builder.base-image | sed 's/^.*@//')" ] || \
        die "$image does not bind the pinned base image digest"
    [ "$(docker run --rm --entrypoint getconf "$image" GNU_LIBC_VERSION)" = "glibc $glibc" ] || \
        die "$image is not a glibc $glibc profile"

    if [ "$profile" = gnu2.28-x86_64 ]; then
        docker run --rm --entrypoint rpm "$image" -q \
            clang-devel llvm-devel elfutils-libelf-devel systemd-devel \
            fuse3-devel openssl-devel zlib-devel libzstd-devel \
            pkgconf-pkg-config >/dev/null
    elif [ "$profile" = gnu2.28-aarch64 ]; then
        docker run --rm --entrypoint sh "$image" -c '
            set -eu
            sysroot="$(aarch64-unknown-linux-gnu-gcc --print-sysroot)"
            test -f "$sysroot/usr/include/openssl/ssl.h"
            readelf -h "$sysroot/usr/lib64/libssl.so.1.1" | grep -Fq AArch64
            PKG_CONFIG_SYSROOT_DIR="$sysroot" \
                PKG_CONFIG_LIBDIR="$sysroot/usr/lib64/pkgconfig:$sysroot/usr/share/pkgconfig" \
                pkg-config --exists openssl
        '
    elif [ "$profile" = gnu2.17-x86_64 ]; then
        docker run --rm --entrypoint sh "$image" -c \
            'test "$(gcc -dumpmachine)" = x86_64-redhat-linux'
    else
        docker run --rm --entrypoint sh "$image" -c \
            'test -x /usr/xcc/aarch64-unknown-linux-gnu/bin/aarch64-unknown-linux-gnu-gcc'
    fi
}

verify_darwin_profile() {
    local image="$1"
    local profile="$2"
    local target
    target="$(profile_target "$profile")"

    [ "$(image_label "$image" org.anolisa.release-builder.profile)" = "$profile" ] || \
        die "$image does not identify profile $profile"
    [ "$(image_label "$image" org.anolisa.release-builder.target)" = "$target" ] || \
        die "$image target label is invalid"
    [ "$(image_label "$image" org.anolisa.release-builder.macos-deployment-target)" = 11.0 ] || \
        die "$image deployment target label is invalid"
    [ "$(image_label "$image" org.anolisa.release-builder.macos-sdk-version)" = 15.5 ] || \
        die "$image SDK version label is invalid"
    [ "$(image_label "$image" org.anolisa.release-builder.macos-sdk-sha256)" = "$DARWIN_SDK_SHA256" ] || \
        die "$image SDK checksum label is invalid"
    [ "$(image_label "$image" org.anolisa.release-builder.osxcross-ref)" = "$DARWIN_OSXCROSS_REF" ] || \
        die "$image osxcross revision label is invalid"
    docker run --rm --entrypoint sh "$image" -c '
        set -eu
        test -x "/usr/local/bin/$1-clang"
        test -x "/usr/local/bin/$1-clang++"
        test -x "/usr/local/bin/$1-ar"
        test -d /opt/osxcross/SDK/MacOSX15.5.sdk
        "/usr/local/bin/$1-clang" --version >/dev/null
    ' sh "$target"
}

verify_profile() {
    local profile="$1"
    local actual image image_id target safe_path

    target="$(profile_target "$profile")"
    image="$(profile_image "$profile")"
    image_id="$(profile_image_id "$profile")"
    safe_path="$(profile_path "$profile")"
    verify_common_tools "$target"
    docker image inspect "$image" >/dev/null 2>&1 || \
        die "Runner profile image is not prepared: $image"
    actual="$(docker image inspect --format '{{.Id}}' "$image")"
    [ "$actual" = "$image_id" ] || \
        die "$image does not match pinned image ID $image_id"
    image="$image_id"
    actual="$(docker run --rm --entrypoint /usr/local/bin/sccache "$image" --version)"
    [ "$actual" = "sccache $SCCACHE_VERSION" ] || \
        die "$image cannot run sccache $SCCACHE_VERSION"
    if [[ "$profile" == darwin11-* ]]; then
        verify_darwin_profile "$image" "$profile"
    else
        verify_linux_profile "$profile" "$image"
    fi
    if docker run --rm --entrypoint env "$image" \
        "PATH=$safe_path" sh -c 'command -v cargo' >/dev/null 2>&1; then
        die "profile PATH exposes image-provided Cargo: $profile"
    fi
}

run_profile() {
    local profile="$1"
    shift
    local image target target_env image_env safe_path linker archiver
    local cargo_home container_opts deployment encoded
    local argument requested_target separator
    local -a cargo_args rustc_args

    target="$(profile_target "$profile")"
    image="$(profile_image_id "$profile")"
    target_env="${target^^}"
    target_env="${target_env//-/_}"
    image_env="CROSS_TARGET_${target_env}_IMAGE"
    safe_path="$(profile_path "$profile")"
    cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    separator=0
    cargo_args=()
    rustc_args=()
    while [ "$#" -gt 0 ]; do
        argument="$1"
        shift
        if [ "$separator" -eq 1 ]; then
            rustc_args+=("$argument")
            continue
        fi
        case "$argument" in
            --)
                separator=1
                ;;
            --target)
                [ "$#" -gt 0 ] || die '--target requires a value'
                requested_target="$1"
                shift
                [ "$requested_target" = "$target" ] || \
                    die "Cargo target $requested_target does not match profile $profile"
                ;;
            --target=*)
                requested_target="${argument#--target=}"
                [ "$requested_target" = "$target" ] || \
                    die "Cargo target $requested_target does not match profile $profile"
                ;;
            *)
                cargo_args+=("$argument")
                ;;
        esac
    done
    cargo_args+=(--target "$target")
    if [ "$separator" -eq 1 ]; then
        cargo_args+=(-- "${rustc_args[@]}")
    fi
    install -d -m 0755 "$cargo_home/sccache"
    case "$profile" in
        gnu2.17-x86_64|gnu2.28-x86_64) linker=gcc; archiver='ar' ;;
        gnu2.17-aarch64|gnu2.28-aarch64)
            linker=aarch64-unknown-linux-gnu-gcc
            archiver=aarch64-unknown-linux-gnu-ar
            ;;
        darwin11-aarch64|darwin11-x86_64)
            linker="$target-clang"
            archiver="$target-ar"
            ;;
    esac

    container_opts="${CROSS_CONTAINER_OPTS:-}"
    [ -z "$container_opts" ] || container_opts+=' '
    container_opts+="--env=PATH=$safe_path --env=LD_LIBRARY_PATH=/rust/lib"
    container_opts+=" --env=RUSTC_WRAPPER=/usr/local/bin/sccache"
    container_opts+=" --env=SCCACHE_DIR=/cargo/sccache"
    container_opts+=" --env=SCCACHE_CACHE_SIZE=$SCCACHE_CACHE_SIZE"
    container_opts+=" --env=SCCACHE_CLIENT_SIDE=1 --env=CARGO_INCREMENTAL=0"
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
        container_opts+=" --env=SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH"
    fi
    if [[ "$profile" == darwin11-* ]]; then
        deployment=11.0
        container_opts+=" --env=MACOSX_DEPLOYMENT_TARGET=$deployment"
        container_opts+=" --env=SDKROOT=/opt/osxcross/SDK/MacOSX.sdk"
        encoded="${CARGO_ENCODED_RUSTFLAGS:-}"
        if [[ "$encoded" != *'link-arg=-Wl,-no_uuid'* ]]; then
            [ -z "$encoded" ] || encoded+=$'\x1f'
            encoded+="-C"$'\x1f'"link-arg=-Wl,-no_uuid"
        fi
        env RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN" \
            "$image_env=$image" \
            "CROSS_CONTAINER_OPTS=$container_opts" \
            "MACOSX_DEPLOYMENT_TARGET=$deployment" \
            "CARGO_ENCODED_RUSTFLAGS=$encoded" \
            "CARGO_TARGET_${target_env}_LINKER=$linker" \
            "CC_${target//-/_}=$linker" \
            "CXX_${target//-/_}=$target-clang++" \
            "AR_${target//-/_}=$archiver" \
            "CFLAGS_${target//-/_}=-mmacosx-version-min=$deployment" \
            "CXXFLAGS_${target//-/_}=-mmacosx-version-min=$deployment" \
            cross "${cargo_args[@]}"
    else
        if [ "$profile" = gnu2.17-aarch64 ] || [ "$profile" = gnu2.28-aarch64 ]; then
            local sysroot=/usr/xcc/aarch64-unknown-linux-gnu/aarch64-unknown-linux-gnu/sysroot
            container_opts+=" --env=PKG_CONFIG_SYSROOT_DIR=$sysroot"
            container_opts+=" --env=PKG_CONFIG_LIBDIR=$sysroot/usr/lib64/pkgconfig:$sysroot/usr/share/pkgconfig"
        fi
        env RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN" \
            "$image_env=$image" \
            "CROSS_CONTAINER_OPTS=$container_opts" \
            "CARGO_TARGET_${target_env}_LINKER=$linker" \
            "CC_${target//-/_}=$linker" \
            "AR_${target//-/_}=$archiver" \
            cross "${cargo_args[@]}"
    fi
}

PROFILE="${1:?usage: cross-profile.sh PROFILE --check|CROSS_ARGS...}"
shift
verify_profile "$PROFILE"
if [ "${1:-}" = --check ]; then
    [ "$#" -eq 1 ] || die '--check does not accept additional arguments'
    printf 'Cross profile verified: %s\n' "$PROFILE"
    exit 0
fi
[ "$#" -gt 0 ] || die 'a Cross command is required'
run_profile "$PROFILE" "$@"
