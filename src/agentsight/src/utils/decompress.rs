//! HTTP body decompression utility.
//!
//! Detects the `Content-Encoding` response header and decompresses the raw
//! bytes before they are converted to strings or parsed as JSON / SSE.
//! Supported codecs: `gzip` (`x-gzip`), `deflate`, `zstd`, `br` (brotli).
//! Graceful degradation: if decompression fails, the original bytes are
//! returned unchanged.

use std::io::{Read, Write};

/// Hard cap on a single decompressed body. Decompression operates on traffic
/// from observed, untrusted processes, where a crafted "compression bomb" (a
/// few KB expanding to many GB) could OOM the single privileged observer and
/// take down the whole observation plane. 32 MiB clears even a maximal
/// extended-thinking response (bounded by output tokens) while keeping peak
/// memory to a small multiple of the cap (not the GB a bomb would reach),
/// regardless of ratio. An over-cap body falls back to raw (that call's SSE
/// enrichment is dropped).
const MAX_DECOMPRESSED_LEN: usize = 32 * 1024 * 1024;

/// Decompress through `reader` with a hard output cap (bomb defense). Reads at
/// most `MAX_DECOMPRESSED_LEN + 1` bytes, so an over-cap stream is detected and
/// peak memory stays a small multiple of the cap (the read buffer grows by
/// doubling) rather than the unbounded GB a decompressed bomb would reach; on
/// over-cap or decoder error it falls back to the raw (still-compressed) `raw`
/// bytes, matching the existing graceful-degradation policy.
fn read_capped<R: Read>(mut reader: R, raw: &[u8], codec: &str) -> Vec<u8> {
    let mut decoded = Vec::new();
    match reader
        .by_ref()
        .take(MAX_DECOMPRESSED_LEN as u64 + 1)
        .read_to_end(&mut decoded)
    {
        Ok(_) if decoded.len() > MAX_DECOMPRESSED_LEN => {
            log::warn!(
                "{codec} decompressed output exceeds {MAX_DECOMPRESSED_LEN} bytes, using raw body (possible decompression bomb)"
            );
            raw.to_vec()
        }
        Ok(_) => decoded,
        Err(e) => {
            // Truncated capture of large non-LLM downloads makes UnexpectedEof
            // routine here, so log at debug to avoid journal noise.
            log::debug!("{codec} decompression failed ({e:?}), using raw body");
            raw.to_vec()
        }
    }
}

/// Decompress an HTTP body based on its `Content-Encoding` header value.
///
/// - `None` or `"identity"` → return body unchanged
/// - `"gzip"` or `"x-gzip"` → decompress with GzDecoder
/// - `"deflate"` → decompress with DeflateDecoder
/// - `"zstd"` → decompress with the zstd decoder
/// - `"br"` → decompress with the brotli decoder
/// - Unknown encoding → return body unchanged
/// - Decompression failure → return original body (graceful fallback)
///
/// Auto-detection: if `content_encoding` is None/unknown but the body starts
/// with a known magic prefix, the codec is inferred from the bytes. This
/// handles HTTP/2 responses where the HPACK stateless decoder can't resolve
/// `Content-Encoding` from the dynamic table. Detected prefixes:
/// - gzip: `1f 8b`
/// - zstd: `28 b5 2f fd`
///
/// (Brotli has no reliable magic prefix, so it is only used when the header
/// explicitly says `br`.)
pub fn decompress_body(body: &[u8], content_encoding: Option<&str>) -> Vec<u8> {
    let encoding = content_encoding.map(|e| e.trim().to_lowercase());

    // Resolve the effective encoding: trust the header for known codecs,
    // otherwise fall back to magic-byte sniffing.
    let effective_encoding = match encoding.as_deref() {
        Some("gzip") | Some("x-gzip") | Some("deflate") | Some("zstd") | Some("br") => encoding,
        _ => {
            if body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b {
                Some("gzip".to_string())
            } else if body.len() >= 4
                && body[0] == 0x28
                && body[1] == 0xb5
                && body[2] == 0x2f
                && body[3] == 0xfd
            {
                // zstd magic number (little-endian 0xFD2FB528)
                Some("zstd".to_string())
            } else {
                encoding
            }
        }
    };

    match effective_encoding.as_deref() {
        Some("gzip") | Some("x-gzip") => {
            read_capped(flate2::read::GzDecoder::new(body), body, "gzip")
        }
        Some("deflate") => read_capped(flate2::read::DeflateDecoder::new(body), body, "deflate"),
        Some("zstd") => {
            // A streaming decoder (capped via `read_capped`) replaces
            // `zstd::decode_all`, which would allocate the full output up front
            // and so could not bound a bomb. `read::Decoder` still consumes the
            // whole reader, i.e. all concatenated frames of a flushed-per-event
            // zstd SSE stream.
            match zstd::stream::read::Decoder::new(body) {
                Ok(decoder) => read_capped(decoder, body, "zstd"),
                Err(e) => {
                    // Same routine-fallback rationale as read_capped: keep at
                    // debug so partial captures don't flood the journal.
                    log::debug!("zstd decompression failed ({e:?}), using raw body");
                    body.to_vec()
                }
            }
        }
        Some("br") => read_capped(brotli::Decompressor::new(body, 4096), body, "brotli"),
        _ => body.to_vec(),
    }
}

/// Growable output sink for the incremental zstd decoder with the same hard
/// cap as `read_capped` (bomb defense): a crafted stream expanding past
/// `MAX_DECOMPRESSED_LEN` makes `write` fail, which poisons the decoder
/// instead of exhausting memory.
#[derive(Debug, Default)]
struct CappedWriter {
    buf: Vec<u8>,
}

impl Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len().saturating_add(data.len()) > MAX_DECOMPRESSED_LEN {
            return Err(std::io::Error::other(format!(
                "decompressed output exceeds {MAX_DECOMPRESSED_LEN} bytes (possible decompression bomb)"
            )));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Incremental zstd decoder for *streaming* SSE bodies (issue #2267).
///
/// Gateways that zstd-compress an SSE stream flush per event but only close
/// the frame when the response ends — or never, when the stream is finalized
/// early (HTTP/2 framing without a chunked terminator, drain paths).
/// Whole-buffer decompression needs the frame epilogue and fails with
/// `UnexpectedEof` on such open frames; feeding fragments into a push-style
/// decoder instead recovers every block the gateway flushed so far.
///
/// The first decode error poisons the decoder (subsequent fragments are
/// ignored) but the bytes already decoded stay available via
/// [`Self::decoded_output`], so a mid-stream corruption still salvages the
/// events before it.
///
/// `pub(crate)`: aggregation-internal plumbing (review F5) — no external
/// consumer, and keeping it crate-private leaves room to change the decoding
/// strategy without a lib API break.
pub(crate) struct ZstdStreamDecoder {
    /// `None` when the zstd context could not be created; the decoder then
    /// behaves as permanently failed and callers fall back to whole-buffer
    /// decoding.
    decoder: Option<zstd::stream::write::Decoder<'static, CappedWriter>>,
    /// Total compressed bytes fed, kept for failure diagnostics.
    bytes_fed: usize,
    /// Set on the first decode error so follow-up fragments neither spam the
    /// log nor corrupt the salvaged output.
    failed: bool,
}

impl std::fmt::Debug for ZstdStreamDecoder {
    // Manual impl because the inner zstd context is not `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZstdStreamDecoder")
            .field("bytes_fed", &self.bytes_fed)
            .field("failed", &self.failed)
            .finish()
    }
}

impl Default for ZstdStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZstdStreamDecoder {
    /// Create a decoder. Context-creation failure is not fatal: the decoder
    /// starts in the failed state and `decoded_output` stays empty, letting
    /// callers fall back to whole-buffer decompression.
    pub fn new() -> Self {
        match zstd::stream::write::Decoder::new(CappedWriter::default()) {
            Ok(decoder) => ZstdStreamDecoder {
                decoder: Some(decoder),
                bytes_fed: 0,
                failed: false,
            },
            Err(e) => {
                log::warn!(
                    "zstd streaming decoder init failed ({e:?}); incremental SSE decompression unavailable for this stream"
                );
                ZstdStreamDecoder {
                    decoder: None,
                    bytes_fed: 0,
                    failed: true,
                }
            }
        }
    }

    /// Feed one compressed fragment. On the first error the decoder is
    /// poisoned and the failure is logged loudly (this replaces the silent
    /// raw-bytes fallback that made zstd streams vanish into `events=0`);
    /// output decoded before the error remains available.
    pub fn feed(&mut self, data: &[u8]) {
        // Count even after failure so diagnostics reflect the full stream.
        self.bytes_fed = self.bytes_fed.saturating_add(data.len());
        if self.failed {
            return;
        }
        let Some(decoder) = self.decoder.as_mut() else {
            return;
        };
        if let Err(e) = decoder.write_all(data) {
            log::warn!(
                "zstd streaming decompression failed (encoding=zstd, compressed_bytes_fed={}, decoded_bytes={}, err={e:?}); keeping partially decoded output",
                self.bytes_fed,
                decoder.get_ref().buf.len(),
            );
            self.failed = true;
        }
    }

    /// Return everything decoded so far (all fully received blocks).
    ///
    /// Non-consuming: finalization can run on a cloned snapshot (idle/dead
    /// drain) while the live connection keeps streaming into the same shared
    /// decoder, so the internal state must survive this call. Needs `&mut`
    /// because pending block bytes are flushed out of the zstd context first.
    pub fn decoded_output(&mut self) -> Vec<u8> {
        let Some(decoder) = self.decoder.as_mut() else {
            return Vec::new();
        };
        // Flush pushes bytes of already-complete blocks into the sink without
        // requiring the frame epilogue — exactly what an open streaming frame
        // needs. Flush failure (e.g. output cap hit) poisons like `feed`.
        if !self.failed {
            if let Err(e) = decoder.flush() {
                log::warn!(
                    "zstd streaming flush failed (encoding=zstd, compressed_bytes_fed={}, decoded_bytes={}, err={e:?}); keeping partially decoded output",
                    self.bytes_fed,
                    decoder.get_ref().buf.len(),
                );
                self.failed = true;
            }
        }
        decoder.get_ref().buf.clone()
    }

    /// Whether the decoder hit a decode error (or never initialized).
    ///
    /// Diagnostic accessor, currently exercised only by tests (production
    /// callers rely on the empty-output fallback instead), hence the
    /// dead_code allowance after the review F5 visibility narrowing.
    #[allow(dead_code)]
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Total compressed bytes fed so far.
    ///
    /// Diagnostic accessor (see [`Self::is_failed`]).
    #[allow(dead_code)]
    pub fn bytes_fed(&self) -> usize {
        self.bytes_fed
    }
}

/// Strip HTTP chunked transfer-encoding framing, returning the concatenated
/// chunk data (binary-safe). Stops at the terminating zero-size chunk. On
/// malformed or incomplete input, returns whatever was decoded so far.
///
/// Needed for compressed SSE streams: the raw bytes look like
/// `<hex>\r\n<data>\r\n…0\r\n\r\n` and the compressed payload must be
/// concatenated *without* the framing before it can be decompressed.
pub fn dechunk_body(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        // The chunk-size line ends at the first CRLF.
        let mut j = i;
        while j + 1 < raw.len() && !(raw[j] == b'\r' && raw[j + 1] == b'\n') {
            j += 1;
        }
        if j + 1 >= raw.len() {
            break; // no CRLF found -> incomplete header
        }
        let size_line = &raw[i..j];
        // Chunk size is hex, up to an optional ';' chunk-extension.
        let hex_end = size_line
            .iter()
            .position(|&b| b == b';')
            .unwrap_or(size_line.len());
        let size = match std::str::from_utf8(&size_line[..hex_end])
            .ok()
            .map(|s| s.trim())
            .and_then(|s| usize::from_str_radix(s, 16).ok())
        {
            Some(s) => s,
            None => break, // malformed size -> stop
        };
        i = j + 2; // skip CRLF after the size line
        if size == 0 {
            break; // terminating zero-size chunk
        }
        if i + size > raw.len() {
            // Incomplete final chunk: salvage what is present.
            out.extend_from_slice(&raw[i..]);
            break;
        }
        out.extend_from_slice(&raw[i..i + size]);
        i += size;
        // Skip the CRLF that follows the chunk data.
        i += 2;
    }
    out
}

/// Whether a chunk-framed body contains the terminating zero-size chunk, i.e.
/// the stream is complete. Unlike scanning the raw bytes for `b"0\r\n\r\n"`
/// (which can match by chance *inside* a compressed payload and finish a stream
/// prematurely — truncating the body so decompression fails and the call is
/// silently dropped), this walks the chunk framing and only reports completion
/// at a real zero-size chunk boundary. Mirrors `dechunk_body`'s parser.
pub fn chunked_stream_complete(raw: &[u8]) -> bool {
    let mut i = 0;
    while i < raw.len() {
        // Find the CRLF terminating the chunk-size line.
        let mut j = i;
        while j + 1 < raw.len() && !(raw[j] == b'\r' && raw[j + 1] == b'\n') {
            j += 1;
        }
        if j + 1 >= raw.len() {
            return false; // incomplete chunk-size line
        }
        let size_line = &raw[i..j];
        let hex_end = size_line
            .iter()
            .position(|&b| b == b';')
            .unwrap_or(size_line.len());
        let size = match std::str::from_utf8(&size_line[..hex_end])
            .ok()
            .map(|s| s.trim())
            .and_then(|s| usize::from_str_radix(s, 16).ok())
        {
            Some(s) => s,
            None => return false, // malformed size line
        };
        i = j + 2; // skip CRLF after the size line
        if size == 0 {
            return true; // terminating zero-size chunk reached
        }
        if i + size > raw.len() {
            return false; // incomplete final chunk
        }
        i += size + 2; // skip chunk data + its trailing CRLF
    }
    false
}

/// Convenience: decompress and then convert to String.
///
/// Returns `None` if the (decompressed) body is empty or not valid UTF-8.
pub fn decompress_body_to_string(body: &[u8], content_encoding: Option<&str>) -> Option<String> {
    let decompressed = decompress_body(body, content_encoding);
    if decompressed.is_empty() {
        None
    } else {
        String::from_utf8(decompressed).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zstd_decompresses_by_header() {
        let plain = b"data: {\"type\":\"message_start\"}\n\n";
        let comp = zstd::encode_all(&plain[..], 3).unwrap();
        assert_ne!(comp.as_slice(), &plain[..]);
        assert_eq!(decompress_body(&comp, Some("zstd")), plain);
    }

    #[test]
    fn zstd_decompresses_by_magic_autodetect() {
        // Mirrors the real HTTP/2 case where Content-Encoding can't be resolved
        // from the HPACK dynamic table: rely on the magic-byte sniff.
        let plain = b"event: content_block_delta\ndata: {\"text\":\"hi\"}\n\n";
        let comp = zstd::encode_all(&plain[..], 3).unwrap();
        assert_eq!(&comp[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        assert_eq!(decompress_body(&comp, None), plain);
    }

    #[test]
    fn brotli_decompresses_by_header() {
        let plain = b"data: {\"type\":\"content_block_delta\"}\n\n";
        let mut comp = Vec::new();
        {
            let mut w = brotli::CompressorWriter::new(&mut comp, 4096, 5, 22);
            w.write_all(plain).unwrap();
        }
        assert_eq!(decompress_body(&comp, Some("br")), plain);
    }

    #[test]
    fn identity_and_unknown_pass_through() {
        let body = b"plain body";
        assert_eq!(decompress_body(body, None), body);
        assert_eq!(decompress_body(body, Some("identity")), body);
        assert_eq!(decompress_body(body, Some("weird-codec")), body);
    }

    #[test]
    fn corrupt_compressed_falls_back_to_raw() {
        // Claims zstd but is not a valid zstd stream → graceful raw fallback.
        let body = b"not really zstd";
        assert_eq!(decompress_body(body, Some("zstd")), body);
    }

    #[test]
    fn dechunk_strips_framing() {
        let chunked = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(dechunk_body(chunked), b"hello world");
    }

    #[test]
    fn brotli_corrupt_falls_back_to_raw() {
        let body = b"not really brotli";
        assert_eq!(decompress_body(body, Some("br")), body);
    }

    #[test]
    fn gzip_decompresses_and_corrupt_falls_back() {
        let plain = b"hello gzip";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(plain).unwrap();
        let compressed = enc.finish().unwrap();
        assert_eq!(decompress_body(&compressed, Some("gzip")), plain);
        assert_eq!(decompress_body(&compressed, Some("x-gzip")), plain);

        let bad = b"not gzip at all!!";
        assert_eq!(decompress_body(bad, Some("gzip")), bad);
    }

    #[test]
    fn deflate_decompresses_and_corrupt_falls_back() {
        let plain = b"hello deflate";
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(plain).unwrap();
        let compressed = enc.finish().unwrap();
        assert_eq!(decompress_body(&compressed, Some("deflate")), plain);

        let bad = b"not deflate";
        assert_eq!(decompress_body(bad, Some("deflate")), bad);
    }

    #[test]
    fn gzip_autodetected_by_magic() {
        let plain = b"auto-detect gzip";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(plain).unwrap();
        let compressed = enc.finish().unwrap();
        assert_eq!(&compressed[..2], &[0x1f, 0x8b]);
        assert_eq!(decompress_body(&compressed, None), plain);
    }

    #[test]
    fn dechunk_incomplete_final_chunk() {
        // Chunk header says 10 bytes but only 3 are present
        let raw = b"a\r\nabc";
        let result = dechunk_body(raw);
        assert_eq!(result, b"abc");
    }

    #[test]
    fn dechunk_malformed_size_stops() {
        let raw = b"zz\r\ndata\r\n0\r\n\r\n";
        let result = dechunk_body(raw);
        assert!(result.is_empty());
    }

    #[test]
    fn dechunk_no_crlf_returns_empty() {
        let raw = b"5hello";
        let result = dechunk_body(raw);
        assert!(result.is_empty());
    }

    #[test]
    fn decompress_body_to_string_works() {
        let plain = b"hello string";
        let compressed = zstd::encode_all(&plain[..], 3).unwrap();
        let result = decompress_body_to_string(&compressed, Some("zstd"));
        assert_eq!(result.as_deref(), Some("hello string"));

        assert_eq!(decompress_body_to_string(b"", None), None);
    }

    #[test]
    fn dechunk_then_zstd_recovers_sse_stream() {
        // The real failure mode: a chunk-framed, zstd-compressed SSE response.
        let sse = b"event: content_block_delta\ndata: {\"text\":\"hi\"}\n\nevent: message_stop\ndata: {}\n\n";
        let comp = zstd::encode_all(&sse[..], 3).unwrap();
        // Frame the compressed bytes across two chunks + the zero terminator.
        let mid = comp.len() / 2;
        let mut chunked = Vec::new();
        chunked.extend_from_slice(format!("{mid:x}\r\n").as_bytes());
        chunked.extend_from_slice(&comp[..mid]);
        chunked.extend_from_slice(b"\r\n");
        chunked.extend_from_slice(format!("{:x}\r\n", comp.len() - mid).as_bytes());
        chunked.extend_from_slice(&comp[mid..]);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");

        let dechunked = dechunk_body(&chunked);
        assert_eq!(dechunked, comp);
        assert_eq!(decompress_body(&dechunked, Some("zstd")), sse);
    }

    #[test]
    fn zstd_multiframe_still_decodes() {
        // Regression guard: replacing `zstd::decode_all` with a streaming capped
        // decoder must NOT drop concatenated frames — a flushed-per-event zstd
        // SSE stream is multiple frames back-to-back.
        let f1 = zstd::encode_all(&b"event: a\ndata: {\"x\":1}\n\n"[..], 3).unwrap();
        let f2 = zstd::encode_all(&b"event: b\ndata: {\"y\":2}\n\n"[..], 3).unwrap();
        let mut concat = f1;
        concat.extend_from_slice(&f2);
        let out = decompress_body(&concat, Some("zstd"));
        assert_eq!(
            out, b"event: a\ndata: {\"x\":1}\n\nevent: b\ndata: {\"y\":2}\n\n",
            "multi-frame zstd must decode all frames, not just the first"
        );
    }

    #[test]
    fn zstd_bomb_falls_back_to_raw() {
        // A high-ratio zstd stream expanding past the cap must fall back to the
        // raw (tiny) body, bounding memory — NOT allocate the full expansion.
        let huge = vec![0u8; MAX_DECOMPRESSED_LEN + 1024 * 1024];
        let bomb = zstd::encode_all(&huge[..], 3).unwrap();
        assert!(
            bomb.len() < 1024 * 1024,
            "precondition: bomb is tiny compressed"
        );
        let out = decompress_body(&bomb, Some("zstd"));
        assert_eq!(out, bomb, "over-cap zstd must fall back to raw, not expand");
        assert!(
            out.len() <= MAX_DECOMPRESSED_LEN,
            "output must be bounded by the cap"
        );
    }

    #[test]
    fn gzip_bomb_falls_back_to_raw() {
        let huge = vec![0u8; MAX_DECOMPRESSED_LEN + 1024 * 1024];
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&huge).unwrap();
        let bomb = enc.finish().unwrap();
        let out = decompress_body(&bomb, Some("gzip"));
        assert_eq!(out, bomb, "over-cap gzip must fall back to raw");
    }

    #[test]
    fn under_cap_still_decompresses_fully() {
        // Discriminating: a large-but-under-cap body must still decompress in
        // full, so the cap does not break legitimate large SSE responses.
        let big = vec![b'x'; 2 * 1024 * 1024]; // 2 MiB, well under the cap
        let comp = zstd::encode_all(&big[..], 3).unwrap();
        assert_eq!(decompress_body(&comp, Some("zstd")), big);
    }

    /// Build a multi-block open zstd frame the way a streaming gateway does:
    /// one frame, one flush per event, no epilogue. Returns (plain, compressed).
    fn open_frame_multi_block() -> (Vec<u8>, Vec<u8>) {
        let events: [&[u8]; 3] = [
            b"event: message_start\ndata: {\"id\":\"msg_01ABC\",\"usage\":{\"input_tokens\":25}}\n\n",
            b"event: content_block_delta\ndata: {\"delta\":{\"text\":\"Hello\"}}\n\n",
            b"event: message_delta\ndata: {\"stop_reason\":\"end_turn\",\"usage\":{\"output_tokens\":42}}\n\n",
        ];
        let mut plain = Vec::new();
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        for ev in events {
            plain.extend_from_slice(ev);
            enc.write_all(ev).unwrap();
            enc.flush().unwrap();
        }
        (plain, enc.get_ref().clone())
    }

    #[test]
    fn zstd_stream_decoder_one_byte_feed_recovers_all_blocks() {
        // Boundary experiment for issue #2267 review finding F1: fragment
        // boundaries never align with zstd block boundaries in the worst case
        // (1-byte fragments). The write-decoder must buffer partial block
        // bytes across feeds and emit every completed block on flush, so the
        // decoded output is byte-identical to the original plaintext even
        // though the frame epilogue never arrives.
        let (plain, compressed) = open_frame_multi_block();
        let mut dec = ZstdStreamDecoder::new();
        for b in &compressed {
            dec.feed(std::slice::from_ref(b));
        }
        assert!(!dec.is_failed());
        assert_eq!(
            dec.decoded_output(),
            plain,
            "1-byte-fragmented open frame must decode byte-exactly"
        );
    }

    #[test]
    fn zstd_stream_decoder_truncated_tail_keeps_complete_blocks() {
        // Companion experiment: cut the compressed stream inside the LAST
        // block. Only that incomplete block is undecodable; every earlier
        // block must still be recovered in full (prefix of the plaintext).
        let (plain, compressed) = open_frame_multi_block();
        let mut dec = ZstdStreamDecoder::new();
        // Drop the last 4 compressed bytes: mid-block cut, no epilogue.
        dec.feed(&compressed[..compressed.len() - 4]);
        let out = dec.decoded_output();
        assert!(!out.is_empty(), "earlier complete blocks must survive");
        assert!(
            plain.starts_with(&out),
            "truncated-tail output must be a plaintext prefix (no corruption)"
        );
        // The first two events were flushed as complete blocks well before
        // the cut, so they must be fully present.
        let second_event_end = plain
            .windows(2)
            .position(|w| w == b"\n\n")
            .map(|p| p + 2)
            .and_then(|first_end| {
                plain[first_end..]
                    .windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|p| first_end + p + 2)
            })
            .unwrap();
        assert!(
            out.len() >= second_event_end,
            "all blocks before the cut must decode: got {} of {} bytes",
            out.len(),
            second_event_end
        );
    }

    #[test]
    fn chunked_stream_complete_detects_terminator() {
        assert!(chunked_stream_complete(b"5\r\nhello\r\n0\r\n\r\n"));
        assert!(chunked_stream_complete(
            b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
        ));
    }

    #[test]
    fn chunked_stream_complete_false_until_terminator() {
        assert!(!chunked_stream_complete(b"5\r\nhello\r\n")); // mid-stream, no zero chunk
        assert!(!chunked_stream_complete(b"a\r\nabc")); // incomplete final data
        assert!(!chunked_stream_complete(b"zz\r\ndata\r\n0\r\n\r\n")); // malformed size
    }

    #[test]
    fn chunked_stream_complete_ignores_embedded_terminator() {
        // THE bug this fixes: the 5-byte terminator pattern occurs *inside* a
        // chunk's data while the stream is NOT complete. Naive `windows(5).any`
        // would wrongly report complete; framing-aware parsing must not be fooled.
        let payload = b"AB0\r\n\r\nCD"; // contains b"0\r\n\r\n" inside the data
        let mut raw = Vec::new();
        raw.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
        raw.extend_from_slice(payload);
        raw.extend_from_slice(b"\r\n"); // chunk-data CRLF, but no zero chunk yet
        assert!(
            raw.windows(5).any(|w| w == b"0\r\n\r\n"),
            "precondition: the naive scan WOULD falsely match"
        );
        assert!(
            !chunked_stream_complete(&raw),
            "framing-aware check must not be fooled by an embedded terminator"
        );
    }
}
