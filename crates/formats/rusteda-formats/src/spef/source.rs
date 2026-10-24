// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bounded transport decoding (RCLI-REQ-0073). Both compressed and decoded
//! bytes are charged before parsing; no shell, Python or external decompressor.
use super::{Error, Limits};
use std::io::Read;

/// Encoding of the original transport, detected from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEncoding {
    Plain,
    Gzip,
}

impl SourceEncoding {
    /// Stable telemetry label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Gzip => "gzip",
        }
    }
}

/// The canonical UTF-8 source stored in the immutable PARASIT container.
#[derive(Debug)]
pub struct DecodedSource {
    pub text: String,
    pub encoding: SourceEncoding,
}

/// Decode plain or concatenated gzip members under the same byte cap.
/// CRC/truncation failures are errors, including an invalid trailing member.
pub fn decode_source(bytes: &[u8], limits: Limits) -> Result<DecodedSource, Error> {
    limits.validate()?;
    if bytes.len() > limits.max_bytes {
        return Err(Error::new(0, "T0072", "SPEF source byte budget exceeded"));
    }
    let (decoded, encoding) = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::bufread::MultiGzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder
            .by_ref()
            .take(limits.max_bytes as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| Error::new(0, "T0071", format!("invalid gzip SPEF: {e}")))?;
        if out.len() > limits.max_bytes {
            return Err(Error::new(0, "T0072", "decoded SPEF byte budget exceeded"));
        }
        (out, SourceEncoding::Gzip)
    } else {
        (bytes.to_vec(), SourceEncoding::Plain)
    };
    let text = String::from_utf8(decoded)
        .map_err(|_| Error::new(0, "T0071", "SPEF source is not UTF-8"))?;
    Ok(DecodedSource { text, encoding })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }
    // RCLI-REQ-0073: equivalent encodings, multiple members and corrupt transport.
    #[test]
    fn bounded_gzip_and_crc_validation() {
        let limits = Limits::default();
        let plain = b"*SPEF \"ieee 1481-1999\"\n";
        assert_eq!(
            decode_source(plain, limits).unwrap().text,
            decode_source(&gzip(plain), limits).unwrap().text
        );
        let mut multi = gzip(b"first");
        multi.extend(gzip(b"second"));
        assert_eq!(decode_source(&multi, limits).unwrap().text, "firstsecond");
        let mut corrupt = gzip(plain);
        let last = corrupt.len() - 8;
        corrupt[last] ^= 1;
        assert!(decode_source(&corrupt, limits).is_err());
        assert!(decode_source(&corrupt[..corrupt.len() - 5], limits).is_err());
        let bomb = gzip(&vec![b'x'; 4096]);
        let cap = Limits {
            max_bytes: 1024,
            ..limits
        };
        assert_eq!(decode_source(&bomb, cap).unwrap_err().code, "T0072");
        assert!(decode_source(&[0xff], limits).is_err());
    }
}
