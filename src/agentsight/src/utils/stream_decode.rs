//! Incremental content decoding releases output after each captured fragment.

use super::brotli_alloc::CountingAllocator;
use brotli::Allocator;
use std::cell::Cell;
use std::io::{self, Write};
use std::rc::Rc;

type BrotliInner = brotli::DecompressorWriterCustomIo<
    io::Error,
    Sink,
    <brotli::HeapAlloc<u8> as Allocator<u8>>::AllocatedMemory,
    CountingAllocator<u8>,
    CountingAllocator<u32>,
    CountingAllocator<brotli::HuffmanCode>,
>;

struct BrotliDecoder(BrotliInner);
impl Write for BrotliDecoder {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        brotli::CustomWrite::write(&mut self.0, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        brotli::CustomWrite::flush(&mut self.0)
    }
}
impl BrotliDecoder {
    fn get_ref(&self) -> &Sink {
        self.0.get_ref()
    }
    fn get_mut(&mut self) -> &mut Sink {
        self.0.get_mut()
    }
}

struct Sink {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(data.len()) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "decoded SSE fragment exceeds byte limit",
            ));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl brotli::CustomWrite<io::Error> for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Write::write(self, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Codec {
    Identity,
    Gzip(Box<flate2::write::MultiGzDecoder<Sink>>),
    DetectDeflate {
        prefix: [u8; 2],
        len: usize,
        limit: usize,
    },
    Zlib(Box<flate2::write::ZlibDecoder<Sink>>),
    Deflate(Box<flate2::write::DeflateDecoder<Sink>>),
    Zstd(zstd::zstd_safe::DCtx<'static>),
    Brotli(Box<BrotliDecoder>, Rc<Cell<usize>>),
}

struct SingleDecoder {
    codec: Codec,
}

pub(crate) struct StreamDecoder {
    stages: Vec<SingleDecoder>,
}

impl std::fmt::Debug for StreamDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamDecoder")
            .finish_non_exhaustive()
    }
}

impl StreamDecoder {
    pub(crate) fn new(encoding: Option<&str>, limit: usize) -> io::Result<Self> {
        // HTTP lists codings in application order. Bound decoder contexts as
        // well as decoded bytes before allocating any attacker-selected stack.
        let codings: Vec<_> = encoding
            .unwrap_or("identity")
            .split(',')
            .map(str::trim)
            .filter(|coding| !coding.is_empty() && !coding.eq_ignore_ascii_case("identity"))
            .take(9)
            .collect();
        if codings.len() > 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many SSE content codings",
            ));
        }
        let stages = codings
            .into_iter()
            .rev()
            .map(|coding| SingleDecoder::new(Some(coding), limit))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self { stages })
    }

    pub(crate) fn feed(
        &mut self,
        data: &[u8],
        mut consume: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        Self::feed_stages(&mut self.stages, data, &mut consume)
    }

    fn feed_stages(
        stages: &mut [SingleDecoder],
        data: &[u8],
        consume: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        match stages.split_first_mut() {
            Some((outer, inner)) => {
                outer.feed(data, |decoded| Self::feed_stages(inner, decoded, consume))
            }
            None => consume(data),
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.stages.capacity() * std::mem::size_of::<SingleDecoder>()
            + self
                .stages
                .iter()
                .map(SingleDecoder::retained_bytes)
                .sum::<usize>()
    }
}

impl SingleDecoder {
    fn new(encoding: Option<&str>, limit: usize) -> io::Result<Self> {
        let sink = Sink {
            bytes: Vec::new(),
            limit: limit.max(256 * 1024),
        };
        let codec = match encoding
            .unwrap_or("identity")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "identity" => Codec::Identity,
            "gzip" | "x-gzip" => Codec::Gzip(Box::new(flate2::write::MultiGzDecoder::new(sink))),
            "deflate" => Codec::DetectDeflate {
                prefix: [0; 2],
                len: 0,
                limit: sink.limit,
            },
            "zstd" => {
                let mut decoder = zstd::zstd_safe::DCtx::try_create()
                    .ok_or_else(|| io::Error::other("cannot allocate zstd decoder context"))?;
                decoder
                    .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(23))
                    .map_err(|code| io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
                Codec::Zstd(decoder)
            }
            "br" => {
                let bytes = Rc::new(Cell::new(0));
                let mut alloc = CountingAllocator::<u8>::new(Rc::clone(&bytes));
                let buffer = alloc.alloc_cell(4096);
                Codec::Brotli(
                    Box::new(BrotliDecoder(brotli::DecompressorWriterCustomIo::new(
                        sink,
                        buffer,
                        alloc,
                        CountingAllocator::new(Rc::clone(&bytes)),
                        CountingAllocator::new(Rc::clone(&bytes)),
                        io::Error::new(io::ErrorKind::InvalidData, "invalid Brotli SSE stream"),
                    ))),
                    bytes,
                )
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported SSE content encoding: {other}"),
                ));
            }
        };
        Ok(Self { codec })
    }

    pub(crate) fn feed(
        &mut self,
        data: &[u8],
        mut consume: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut data = data;
        if let Codec::DetectDeflate { prefix, len, limit } = &mut self.codec {
            let take = (2 - *len).min(data.len());
            prefix[*len..*len + take].copy_from_slice(&data[..take]);
            *len += take;
            data = &data[take..];
            if *len != 2 {
                return Ok(());
            }
            let prefix = *prefix;
            let sink = Sink {
                bytes: Vec::new(),
                limit: *limit,
            };
            let zlib = prefix[0] & 0x0f == 8
                && prefix[0] >> 4 <= 7
                && u16::from_be_bytes(prefix).is_multiple_of(31);
            self.codec = if zlib {
                Codec::Zlib(Box::new(flate2::write::ZlibDecoder::new(sink)))
            } else {
                Codec::Deflate(Box::new(flate2::write::DeflateDecoder::new(sink)))
            };
            self.decode(&prefix, &mut consume)?;
        }
        self.decode(data, &mut consume)
    }

    fn decode(
        &mut self,
        data: &[u8],
        consume: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        macro_rules! decode {
            ($decoder:expr) => {{
                let decoder = $decoder;
                // Drain decoded output between compressed fragments as well:
                // a small TLS record can contain thousands of highly compressed events.
                for fragment in data.chunks(128) {
                    let result = decoder.write_all(fragment).and_then(|_| decoder.flush());
                    let sink = decoder.get_mut();
                    consume(&sink.bytes)?;
                    sink.bytes.clear();
                    if sink.bytes.capacity() > 64 * 1024 {
                        sink.bytes = Vec::new();
                    }
                    result?;
                }
                Ok(())
            }};
        }
        match &mut self.codec {
            Codec::Identity => consume(data),
            Codec::Gzip(decoder) => decode!(decoder),
            Codec::DetectDeflate { .. } => Ok(()),
            Codec::Zlib(decoder) => decode!(decoder),
            Codec::Deflate(decoder) => decode!(decoder),
            Codec::Zstd(decoder) => {
                let mut input = zstd::zstd_safe::InBuffer::around(data);
                let mut scratch = [0_u8; 16 * 1024];
                loop {
                    let before = input.pos();
                    let mut output = zstd::zstd_safe::OutBuffer::around(&mut scratch[..]);
                    decoder
                        .decompress_stream(&mut output, &mut input)
                        .map_err(|code| io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
                    let written = output.pos();
                    consume(&scratch[..written])?;
                    if (input.pos() == before && written == 0)
                        || (input.pos() == data.len() && written < scratch.len())
                    {
                        break;
                    }
                }
                Ok(())
            }
            Codec::Brotli(decoder, _) => decode!(decoder),
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        match &self.codec {
            Codec::Identity => 0,
            Codec::DetectDeflate { .. } => 0,
            Codec::Gzip(decoder) => decoder.get_ref().bytes.capacity() + 64 * 1024,
            Codec::Zlib(decoder) => decoder.get_ref().bytes.capacity() + 64 * 1024,
            Codec::Deflate(decoder) => decoder.get_ref().bytes.capacity() + 64 * 1024,
            // Native decoder contexts are opaque. Reserve working-memory headroom
            // in addition to accounting for their visible output buffers.
            Codec::Zstd(decoder) => decoder.sizeof(),
            Codec::Brotli(decoder, allocations) => {
                decoder.get_ref().bytes.capacity()
                    + allocations.get()
                    + std::mem::size_of::<BrotliDecoder>()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn coding_stack_bounds_and_unknown_layers_fail_before_consumption() {
        for encoding in [
            "gzip, unsupported",
            "unsupported, gzip",
            &["gzip"; 9].join(","),
        ] {
            assert!(
                StreamDecoder::new(Some(encoding), 1024).is_err(),
                "{encoding}"
            );
        }
        let mut decoder = StreamDecoder::new(Some(" GZip , identity, gzip "), 1024).unwrap();
        let single = StreamDecoder::new(Some("gzip"), 1024).unwrap();
        // The extra codec must be accounted for. Container spare capacity
        // is shared, so total retained memory need not be exactly doubled.
        assert!(decoder.retained_bytes() > single.retained_bytes());
        let bytes = gzip(&gzip(b"complete"));
        let mut decoded = Vec::new();
        for byte in bytes.chunks(1) {
            decoder
                .feed(byte, |chunk| {
                    decoded.extend_from_slice(chunk);
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(decoded, b"complete");
    }

    #[test]
    fn inner_codec_and_consumer_errors_propagate() {
        let mut decoder = StreamDecoder::new(Some("gzip, gzip"), 1024).unwrap();
        assert!(
            decoder
                .feed(&gzip(b"not an inner gzip member"), |_| Ok(()))
                .is_err()
        );
        let mut decoder = StreamDecoder::new(Some("gzip, gzip"), 1024).unwrap();
        let error = decoder
            .feed(&gzip(&gzip(b"decoded output")), |_| {
                Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "consumer budget exceeded",
                ))
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    }
}
