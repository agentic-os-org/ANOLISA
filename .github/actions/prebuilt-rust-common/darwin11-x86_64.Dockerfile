# The prepared arm64 builder pins LLVM, sccache, and the universal macOS SDK.
# Add Intel entry points without replacing the existing arm64 runner image.
FROM anolisa/rust-release-builder:darwin11-aarch64@sha256:f43abc5ea60980fe1a452607ebc481ea0636bb6e6f0acbd03f7ed1f5064459e1

# Tell Clang to emit platform_version, which the pinned Mach-O LLD requires.
RUN printf '#!/bin/sh\nexec /usr/bin/clang --target=x86_64-apple-darwin -isysroot /opt/osxcross/SDK/MacOSX.sdk -mmacosx-version-min=11.0 -mlinker-version=609 -fuse-ld=/usr/local/bin/ld64.lld "$@"\n' \
        > /usr/local/bin/x86_64-apple-darwin-clang \
    && printf '#!/bin/sh\nexec /usr/bin/clang++ --target=x86_64-apple-darwin -isysroot /opt/osxcross/SDK/MacOSX.sdk -mmacosx-version-min=11.0 -mlinker-version=609 -fuse-ld=/usr/local/bin/ld64.lld "$@"\n' \
        > /usr/local/bin/x86_64-apple-darwin-clang++ \
    && ln -s /usr/bin/llvm-ar /usr/local/bin/x86_64-apple-darwin-ar \
    && chmod 0755 /usr/local/bin/x86_64-apple-darwin-clang* \
    && printf 'int main(void) { return 0; }\n' | \
        /usr/local/bin/x86_64-apple-darwin-clang -x c - -o /tmp/intel-macos-probe \
    && file /tmp/intel-macos-probe | grep -F 'Mach-O 64-bit x86_64 executable' \
    && llvm-otool -l /tmp/intel-macos-probe | grep -F 'minos 11.0' \
    && rm /tmp/intel-macos-probe

LABEL org.anolisa.release-builder.profile="darwin11-x86_64" \
      org.anolisa.release-builder.target="x86_64-apple-darwin"
