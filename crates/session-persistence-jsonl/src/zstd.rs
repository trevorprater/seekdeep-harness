//! Structural scanning and independent-frame Zstandard codecs.

use std::io::{Cursor, Read, Write};

const ZSTD_MAGIC: u32 = 0xFD2F_B528;

/// Byte range occupied by one structurally complete Zstandard frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZstdFrameRange {
    /// Inclusive frame start.
    pub start: usize,
    /// Exclusive frame end.
    pub end: usize,
}

/// Structural scan result for a concatenated Zstandard stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZstdFrameScan {
    /// Complete frames in file order.
    pub frames: Vec<ZstdFrameRange>,
    /// Start of an incomplete final frame, when EOF interrupts one.
    pub torn_start: Option<usize>,
}

/// Locates structurally complete standard Zstandard frames without decoding.
/// An EOF inside the last frame is returned as a torn start; invalid complete
/// structure is rejected.
///
/// # Errors
///
/// Rejects invalid magic, reserved descriptor bits, or a reserved block type.
pub fn scan_zstd_frames(source: &[u8], max_frames: Option<usize>) -> anyhow::Result<ZstdFrameScan> {
    let mut frames = Vec::new();
    let mut offset = 0;
    let limit = max_frames.unwrap_or(usize::MAX);

    while offset < source.len() {
        let start = offset;
        let Some(magic) = read_u32_le(source, offset) else {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        };
        if magic != ZSTD_MAGIC {
            anyhow::bail!("corrupt Zstandard session log: invalid frame magic at byte {offset}");
        }
        offset += 4;

        let Some(&descriptor) = source.get(offset) else {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        };
        offset += 1;
        if descriptor & 0x18 != 0 {
            anyhow::bail!(
                "corrupt Zstandard session log: reserved frame-header bit at byte {}",
                offset - 1
            );
        }

        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;
        let checksum = descriptor & 0x04 != 0;
        let dictionary_flag = descriptor & 0x03;
        let dictionary_bytes = if dictionary_flag == 3 {
            4
        } else {
            usize::from(dictionary_flag)
        };
        let content_size_bytes = if content_size_flag == 0 {
            usize::from(single_segment)
        } else {
            1_usize << content_size_flag
        };
        let remaining_header = usize::from(!single_segment) + dictionary_bytes + content_size_bytes;
        if source.len().saturating_sub(offset) < remaining_header {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        offset += remaining_header;

        loop {
            let Some(block_header) = read_u24_le(source, offset) else {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            };
            offset += 3;
            let last_block = block_header & 1 != 0;
            let block_type = (block_header >> 1) & 0x03;
            let block_size = usize::try_from(block_header >> 3)?;
            if block_type == 0x03 {
                anyhow::bail!(
                    "corrupt Zstandard session log: reserved block type at byte {}",
                    offset - 3
                );
            }
            let payload_bytes = if block_type == 0x01 { 1 } else { block_size };
            if source.len().saturating_sub(offset) < payload_bytes {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            }
            offset += payload_bytes;
            if last_block {
                break;
            }
        }

        if checksum {
            if source.len().saturating_sub(offset) < 4 {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            }
            offset += 4;
        }
        frames.push(ZstdFrameRange { start, end: offset });
        if frames.len() == limit {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: None,
            });
        }
    }

    Ok(ZstdFrameScan {
        frames,
        torn_start: None,
    })
}

/// Compresses exactly one independently decodable checksummed frame.
///
/// # Errors
///
/// Returns encoder initialization, write, or finalization failures.
pub fn compress_zstd_frame(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 0)?;
    encoder.include_checksum(true)?;
    encoder.write_all(input)?;
    Ok(encoder.finish()?)
}

/// Decodes and checksum-validates exactly one complete frame.
///
/// # Errors
///
/// Returns malformed-frame and checksum failures.
pub fn decompress_zstd_frame(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    Ok(zstd::stream::decode_all(Cursor::new(input))?)
}

/// Recovers plaintext emitted by a structurally incomplete final frame.
/// Decode failures after some output are treated as the expected torn suffix;
/// callers retain only complete newline-terminated storage records.
///
/// # Errors
///
/// Returns decoder initialization failures or a decode failure before any
/// plaintext can be recovered.
pub fn decompress_zstd_prefix(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(input))?;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match decoder.read(&mut buffer) {
            Ok(0) => return Ok(output),
            Ok(read) => output.extend_from_slice(&buffer[..read]),
            Err(error) if !output.is_empty() => return Ok(output),
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_u32_le(source: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        source.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u24_le(source: &[u8], offset: usize) -> Option<u32> {
    let bytes = source.get(offset..offset + 3)?;
    Some(u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_independent_checksummed_frames_and_marks_torn_tail() {
        let first = compress_zstd_frame(b"header\n").expect("first");
        let second = compress_zstd_frame(b"event\n").expect("second");
        let stream = [first.as_slice(), second.as_slice()].concat();
        let scan = scan_zstd_frames(&stream, None).expect("scan");
        assert_eq!(scan.frames.len(), 2);
        assert_eq!(
            scan.frames[0],
            ZstdFrameRange {
                start: 0,
                end: first.len()
            }
        );
        assert_eq!(scan.frames[1].end, stream.len());
        assert_eq!(scan.torn_start, None);
        assert_eq!(
            decompress_zstd_frame(&stream[scan.frames[1].start..scan.frames[1].end])
                .expect("decode"),
            b"event\n"
        );

        let torn_len = stream.len() - 2;
        let torn = scan_zstd_frames(&stream[..torn_len], None).expect("torn scan");
        assert_eq!(torn.frames.len(), 1);
        assert_eq!(torn.torn_start, Some(first.len()));
    }

    #[test]
    fn rejects_invalid_magic_and_reserved_structure() {
        let error = scan_zstd_frames(b"xxxx", None).expect_err("magic");
        assert!(error.to_string().contains("invalid frame magic"));
        let error = scan_zstd_frames(&[0x28, 0xB5, 0x2F, 0xFD, 0x18], None)
            .expect_err("reserved descriptor");
        assert!(error.to_string().contains("reserved frame-header bit"));
    }
}
