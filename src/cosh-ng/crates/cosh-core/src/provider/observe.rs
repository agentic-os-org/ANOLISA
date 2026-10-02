//! Stable attach point for out-of-process LLM observability.
//!
//! An eBPF observer cannot see cosh-ng's LLM traffic on the wire: rustls
//! terminates TLS inside the process and exports no `SSL_read`/`SSL_write`, so
//! there is nothing to hook at the library boundary. Locating rustls' own
//! plaintext functions by byte pattern was tried and does not survive: the
//! patterns encode rustc's register allocation, and the release RPM is built
//! with whatever system Rust the build host happens to have (see
//! `scripts/rpm-build.sh`, which deliberately drops `rust-toolchain.toml`), so a
//! compiler bump silently breaks capture.
//!
//! This module replaces that guesswork with an explicit contract: one exported
//! symbol whose name and signature are the whole interface. An observer attaches
//! a uprobe by name and reads the buffer out of the argument registers.
//!
//! # Adding a provider
//!
//! The taps are wired per call site, because each provider builds and sends its
//! own request — there is no shared HTTP chokepoint to hook once. A new provider
//! that reaches a real endpoint therefore has to call all three:
//! [`tap_request`], [`tap_response_head`], and [`tap_response_chunk`]. Omitting
//! them is silent: the provider keeps working, no error is logged, and its LLM
//! calls simply never appear in token accounting. `ContentGenerator::generate`
//! states this as a requirement; `openai_compat` and `sysom` are the reference
//! call patterns.
//!
//! Place the calls at the network boundary, not inside a reusable stream adapter
//! — adapters are commonly driven by in-memory fixtures in tests, which would
//! then be reported as real traffic.

/// Direction marker for [`cosh_llm_plaintext_tap`]: an outbound request body.
pub const TAP_DIR_REQUEST: u32 = 1;
/// Direction marker for [`cosh_llm_plaintext_tap`]: an inbound response chunk.
pub const TAP_DIR_RESPONSE: u32 = 0;

/// Observability attach point; does nothing on its own.
///
/// The body is intentionally empty — the *symbol* is the contract, not the
/// behaviour. Callers hand over a borrowed buffer that must stay valid for the
/// duration of the call, which is trivially true for the synchronous call sites
/// in the providers.
///
/// Three attributes are load-bearing and none may be dropped:
///
/// - `#[no_mangle]` keeps the name stable for a by-name uprobe.
/// - `#[inline(never)]` keeps a real call site to attach to; `lto = true` would
///   otherwise inline an empty function away entirely.
/// - `black_box` keeps the arguments live, so the optimiser cannot decide the
///   call computes nothing and delete it along with the argument setup.
///
/// The release profile additionally needs `-C link-args=-Wl,--export-dynamic`,
/// emitted by `build.rs` (Linux targets only): `strip = true` erases `.symtab`,
/// and only `.dynsym` survives it.
///
/// # Safety
///
/// `buf` must point to at least `len` readable bytes. Passing a dangling
/// pointer is unsound even though this function never dereferences it, because
/// an attached observer will.
#[no_mangle]
#[inline(never)]
pub extern "C" fn cosh_llm_plaintext_tap(dir: u32, buf: *const u8, len: usize) {
    std::hint::black_box((dir, buf, len));
}

/// Report an outbound request to any attached observer, framed as HTTP/1.1.
///
/// The framing is what makes the payload useful. An observer identifies the
/// provider and the API being called from the request line and headers, so a
/// bare JSON body is unrecognisable to it — it arrives as opaque bytes and no
/// token usage is ever extracted. Synthesising the framing here rather than in
/// the observer keeps the wire format knowledge (paths, content type) on the
/// side that already owns it.
///
/// `url` is the absolute request URL; its path and authority are split out to
/// build the request line and `Host` header.
pub fn tap_request(method: &str, url: &str, body: &[u8]) {
    let (authority, path) = split_url(url);
    let head = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    // One allocation per LLM call, not per chunk: the observer needs the head and
    // body contiguous, because it reads a single buffer per tap invocation.
    let mut framed = Vec::with_capacity(head.len() + body.len());
    framed.extend_from_slice(head.as_bytes());
    framed.extend_from_slice(body);
    cosh_llm_plaintext_tap(TAP_DIR_REQUEST, framed.as_ptr(), framed.len());
}

/// Report the start of a response, framed as an HTTP/1.1 status line.
///
/// Emitted once per call, before any chunk, so the observer sees a response
/// begin on this connection and can pair it with the request. The chunks that
/// follow are reported raw by [`tap_response_chunk`].
pub fn tap_response_head(status: u16, content_type: &str) {
    let head = format!(
        "HTTP/1.1 {status} OK\r\n\
         Content-Type: {content_type}\r\n\r\n"
    );
    cosh_llm_plaintext_tap(TAP_DIR_RESPONSE, head.as_ptr(), head.len());
}

/// Report one inbound response chunk to any attached observer.
///
/// Chunks are passed through untouched: they are already the SSE body an
/// observer expects to follow the status line.
pub fn tap_response_chunk(chunk: &[u8]) {
    cosh_llm_plaintext_tap(TAP_DIR_RESPONSE, chunk.as_ptr(), chunk.len());
}

/// Split an absolute URL into `(authority, path_with_query)`.
///
/// Falls back to the whole input as the path when the URL has no scheme
/// separator, which keeps the framing well-formed rather than silently empty.
fn split_url(url: &str) -> (&str, &str) {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, "/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_url_separates_authority_from_path() {
        assert_eq!(
            split_url("https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"),
            (
                "dashscope.aliyuncs.com",
                "/compatible-mode/v1/chat/completions"
            )
        );
    }

    #[test]
    fn split_url_defaults_a_missing_path_to_root() {
        assert_eq!(split_url("https://example.com"), ("example.com", "/"));
    }

    #[test]
    fn split_url_tolerates_a_schemeless_input() {
        // Rather than yield an empty path, which would produce a malformed
        // request line that no observer could parse.
        assert_eq!(split_url("/v1/chat"), ("", "/v1/chat"));
    }
}

/// Guards the build configuration the tap depends on.
///
/// These assertions cannot run against the release binary from a unit test, so
/// they pin the two things a reader might otherwise "clean up": the symbol name
/// and the fact that a build script must export it. The end-to-end guarantee —
/// that the symbol reaches `.dynsym` — is asserted by CI against the built
/// binary, because only the linker can prove that.
#[cfg(test)]
mod build_contract {
    /// The build script must emit the export flag; without it `strip = true`
    /// removes the symbol and capture silently stops.
    #[test]
    fn build_script_exports_the_tap() {
        let build_rs = include_str!("../../build.rs");
        assert!(
            build_rs.contains("rustc-link-arg-bins=-Wl,--export-dynamic"),
            "build.rs must export binary symbols or the tap disappears after strip"
        );
    }

    /// Deliberately not `.cargo/config.toml`: a `RUSTFLAGS` environment variable
    /// overrides config-file rustflags outright, and packaging environments set
    /// one, which silently dropped the flag when this was tried.
    #[test]
    fn export_flag_is_not_placed_where_rustflags_can_override_it() {
        let cargo_config = include_str!("../../../../.cargo/config.toml");
        assert!(
            !cargo_config.contains("export-dynamic"),
            "keep the export flag in build.rs; RUSTFLAGS overrides config.toml rustflags"
        );
    }

    /// The GNU flag would fail every cosh-core link on macOS, whose ld64
    /// documents the single-dash `-export_dynamic` instead, so the emission must
    /// stay gated on the target OS.
    #[test]
    fn export_flag_is_gated_to_linux_targets() {
        let build_rs = include_str!("../../build.rs");
        assert!(
            build_rs.contains("CARGO_CFG_TARGET_OS"),
            "build.rs must gate --export-dynamic on the target OS: the GNU spelling \
             is rejected by Apple's ld64 and would break the macOS build"
        );
    }

    /// The response head must be announced before the success check. Reporting
    /// only 2xx responses leaves the observer with a request that never
    /// completes whenever the endpoint answers 401/429/5xx.
    #[test]
    fn response_head_is_tapped_before_the_success_check() {
        for (provider, source) in [
            ("openai_compat", include_str!("openai_compat.rs")),
            ("sysom", include_str!("sysom.rs")),
        ] {
            let head = source
                .find("tap_response_head(")
                .unwrap_or_else(|| panic!("{provider} never taps the response head"));
            let check = source
                .find("is_success()")
                .unwrap_or_else(|| panic!("{provider} has no status check"));
            assert!(
                head < check,
                "{provider} taps the response head after the success check, so \
                 error responses are never reported"
            );
        }
    }

    /// Every network-backed provider must tap, and forgetting is silent, so the
    /// wiring is asserted here rather than left to review.
    ///
    /// Deliberately source-level: the taps are `extern "C"` no-ops with no
    /// observable effect in-process, so there is nothing a runtime assertion
    /// could check. Add new network providers to this list.
    #[test]
    fn every_network_provider_taps_all_three_points() {
        for (provider, source) in [
            ("openai_compat", include_str!("openai_compat.rs")),
            ("sysom", include_str!("sysom.rs")),
        ] {
            for tap in ["tap_request(", "tap_response_head(", "tap_response_chunk("] {
                assert!(
                    source.contains(tap),
                    "provider `{provider}` never calls `{tap}`; its LLM traffic \
                     would be invisible to observers (see provider::observe)"
                );
            }
        }
    }

    /// The mock provider is fixture-driven; tapping it would publish test data as
    /// if it were real traffic.
    #[test]
    fn the_mock_provider_does_not_tap() {
        let source = include_str!("mock.rs");
        assert!(
            !source.contains("observe::tap"),
            "MockProvider must not tap: its bytes never came from a network"
        );
    }
}
