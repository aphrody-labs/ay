//! Content-Encoding (RFC 9110 §8.4.1): gzip, deflate (zlib or raw, as browsers accept), br and zstd, of a whole body.

use bun_deflate::Output;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Gzip,
    Deflate,
    Brotli,
    Zstd,
}

impl Encoding {
    /// The coding a `Content-Encoding` value names, if it is one of these.
    pub fn parse(value: &[u8]) -> Option<Encoding> {
        let value = value.trim_ascii();
        Some(match value.to_ascii_lowercase().as_slice() {
            b"gzip" | b"x-gzip" => Encoding::Gzip,
            b"deflate" => Encoding::Deflate,
            b"br" => Encoding::Brotli,
            b"zstd" => Encoding::Zstd,
            _ => return None,
        })
    }
}

fn has_zlib_header(bytes: &[u8]) -> bool {
    matches!(bytes, [cmf, flg, ..] if cmf & 0x0f == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0)
}

pub fn decode(encoding: Encoding, input: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    match encoding {
        Encoding::Gzip => {
            let mut rest = input;
            // A body may have several members, which gunzip decodes one after the other.
            while !rest.is_empty() {
                let done = bun_deflate::gunzip(
                    rest,
                    Output::Vec {
                        vec: &mut out,
                        limit: usize::MAX,
                    },
                )
                .map_err(|_| "Decompression error: ShortRead")?;
                rest = rest.get(done.read..).unwrap_or_default();
            }
        }
        Encoding::Deflate => {
            let raw = if has_zlib_header(input) {
                input.get(2..).unwrap_or_default()
            } else {
                input
            };
            bun_deflate::inflate(
                raw,
                Output::Vec {
                    vec: &mut out,
                    limit: usize::MAX,
                },
            )
            .map_err(|_| "Decompression error: ShortRead")?;
        }
        Encoding::Brotli => {
            let mut decoder = bun_brotli::StreamingDecoder::default();
            let mut rest = input;
            loop {
                let used = decoder
                    .decompress(rest, &mut out, usize::MAX, true)
                    .map_err(|_| "Decompression error: BrotliDecompressionError")?;
                rest = rest.get(used..).unwrap_or_default();
                if used == 0 || rest.is_empty() {
                    break;
                }
            }
        }
        Encoding::Zstd => {
            out = bun_zstd::decompress_alloc(input)
                .map_err(|_| "Decompression error: ZstdDecompressionError")?;
        }
    }
    Ok(out)
}
