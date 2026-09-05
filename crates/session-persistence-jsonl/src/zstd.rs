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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecoderState {
    Idle,
    Started,
    Closed,
}

/// One-shot synchronous decoder for complete frame ranges.
///
/// This is the Rust-native equivalent of the source package's interchangeable
/// public and Node-private decoder implementations. One instance owns one
/// decode traversal and has an idempotent close lifecycle.
#[derive(Debug)]
pub struct ZstdFrameDecoder {
    state: DecoderState,
}

impl Default for ZstdFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZstdFrameDecoder {
    /// Creates an idle decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: DecoderState::Idle,
        }
    }

    /// Decodes and checksum-validates complete frames in source order.
    ///
    /// # Errors
    ///
    /// Rejects reuse, use after close, invalid ranges, malformed frames, and
    /// checksum failures. A failed traversal still consumes the decoder.
    pub fn decode(
        &mut self,
        source: &[u8],
        frames: &[ZstdFrameRange],
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        match self.state {
            DecoderState::Idle => self.state = DecoderState::Started,
            DecoderState::Started => anyhow::bail!("Zstandard decoder already started"),
            DecoderState::Closed => anyhow::bail!("Zstandard decoder is closed"),
        }
        frames
            .iter()
            .map(|frame| {
                let bytes = source.get(frame.start..frame.end).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Zstandard frame range {}..{} lies outside source bytes",
                        frame.start,
                        frame.end
                    )
                })?;
                decompress_zstd_frame(bytes).map_err(|error| {
                    anyhow::anyhow!(
                        "Zstandard frame at byte {} failed validation: {error}",
                        frame.start
                    )
                })
            })
            .collect()
    }

    /// Releases decoder-owned state. Repeated calls are harmless.
    pub const fn close(&mut self) {
        self.state = DecoderState::Closed;
    }
}

/// Selects the native Rust frame decoder.
#[must_use]
pub const fn create_zstd_frame_decoder() -> ZstdFrameDecoder {
    ZstdFrameDecoder::new()
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

    const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

    fn empty_structural_frame(descriptor: u8) -> Vec<u8> {
        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;
        let dictionary_bytes = [0_usize, 1, 2, 4][usize::from(descriptor & 0x03)];
        let content_size_bytes = if content_size_flag == 0 {
            usize::from(single_segment)
        } else {
            1_usize << content_size_flag
        };
        let variable_header =
            vec![0; usize::from(!single_segment) + dictionary_bytes + content_size_bytes];
        let mut frame = MAGIC.to_vec();
        frame.push(descriptor);
        frame.extend(variable_header);
        frame.extend([1, 0, 0]);
        if descriptor & 0x04 != 0 {
            frame.extend([0; 4]);
        }
        frame
    }

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

    #[test]
    fn empty_stream_frame_limit_and_checksum_contract_are_exact() {
        assert_eq!(
            scan_zstd_frames(&[], None).expect("empty"),
            ZstdFrameScan {
                frames: Vec::new(),
                torn_start: None,
            }
        );
        let first = compress_zstd_frame(b"header\n").expect("first");
        let second = compress_zstd_frame(b"event\n").expect("second");
        assert_eq!(first[4] & 0x04, 0x04);
        assert_eq!(second[4] & 0x04, 0x04);
        let stream = [first.as_slice(), second.as_slice()].concat();
        assert_eq!(
            scan_zstd_frames(&stream, Some(1)).expect("limited").frames,
            [ZstdFrameRange {
                start: 0,
                end: first.len(),
            }]
        );
        assert_eq!(decompress_zstd_frame(&first).expect("decode"), b"header\n");

        let mut corrupt = first;
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert!(decompress_zstd_frame(&corrupt).is_err());
    }

    #[test]
    fn native_decoder_preserves_order_and_enforces_one_shot_lifecycle() {
        let first = compress_zstd_frame(b"first\n").expect("first");
        let second = compress_zstd_frame(b"second\n").expect("second");
        let stream = [first.as_slice(), second.as_slice()].concat();
        let ranges = scan_zstd_frames(&stream, None).expect("ranges").frames;
        let mut decoder = create_zstd_frame_decoder();
        assert_eq!(
            decoder.decode(&stream, &ranges).expect("decode"),
            [b"first\n".to_vec(), b"second\n".to_vec()]
        );
        assert!(
            decoder
                .decode(&stream, &ranges)
                .expect_err("reuse")
                .to_string()
                .contains("already started")
        );
        decoder.close();
        decoder.close();
        assert!(
            decoder
                .decode(&stream, &ranges)
                .expect_err("closed")
                .to_string()
                .contains("closed")
        );

        let mut corrupt = first;
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        let mut invalid = create_zstd_frame_decoder();
        let error = invalid
            .decode(
                &corrupt,
                &[ZstdFrameRange {
                    start: 0,
                    end: corrupt.len(),
                }],
            )
            .expect_err("checksum");
        assert!(
            error
                .to_string()
                .contains("Zstandard frame at byte 0 failed validation")
        );
        assert!(
            invalid
                .decode(&corrupt, &[])
                .expect_err("failed traversal consumed decoder")
                .to_string()
                .contains("already started")
        );

        let mut closed = create_zstd_frame_decoder();
        closed.close();
        assert!(closed.decode(&stream, &ranges).is_err());
    }

    #[test]
    fn distinguishes_every_incomplete_region_from_reserved_complete_structure() {
        assert_eq!(
            scan_zstd_frames(&MAGIC[..2], None).expect("partial magic"),
            ZstdFrameScan {
                frames: Vec::new(),
                torn_start: Some(0),
            }
        );
        assert_eq!(
            scan_zstd_frames(&MAGIC, None)
                .expect("magic only")
                .torn_start,
            Some(0)
        );
        assert!(scan_zstd_frames(&[0; 4], None).is_err());
        let mut reserved_header = MAGIC.to_vec();
        reserved_header.push(0x08);
        assert!(
            scan_zstd_frames(&reserved_header, None)
                .expect_err("reserved header")
                .to_string()
                .contains("reserved frame-header bit")
        );

        let mut missing_window = MAGIC.to_vec();
        missing_window.push(0x00);
        assert_eq!(
            scan_zstd_frames(&missing_window, None)
                .expect("missing window")
                .torn_start,
            Some(0)
        );
        let mut partial_block_header = MAGIC.to_vec();
        partial_block_header.extend([0x20, 0x00, 0x01, 0x00]);
        assert_eq!(
            scan_zstd_frames(&partial_block_header, None)
                .expect("partial block header")
                .torn_start,
            Some(0)
        );
        let mut partial_payload = MAGIC.to_vec();
        partial_payload.extend([0x20, 0x00, (5 << 3) | 1, 0, 0, 0x01, 0x02]);
        assert_eq!(
            scan_zstd_frames(&partial_payload, None)
                .expect("partial payload")
                .torn_start,
            Some(0)
        );
        let mut reserved_block = MAGIC.to_vec();
        reserved_block.extend([0x20, 0x00, 0x07, 0x00, 0x00]);
        assert!(
            scan_zstd_frames(&reserved_block, None)
                .expect_err("reserved block")
                .to_string()
                .contains("reserved block type")
        );
    }

    #[test]
    fn scans_header_variants_rle_multiple_blocks_and_checksums() {
        for descriptor in [0x00, 0x21, 0x42, 0x83, 0xe3] {
            let frame = empty_structural_frame(descriptor);
            assert_eq!(
                scan_zstd_frames(&frame, None).expect("variant"),
                ZstdFrameScan {
                    frames: vec![ZstdFrameRange {
                        start: 0,
                        end: frame.len(),
                    }],
                    torn_start: None,
                },
                "descriptor {descriptor:#04x}"
            );
        }

        let mut rle = MAGIC.to_vec();
        rle.extend([0x20, 0x01, (1 << 3) | (1 << 1) | 1, 0, 0, 0x41]);
        assert_eq!(
            scan_zstd_frames(&rle, None).expect("rle").frames[0].end,
            rle.len()
        );

        let mut two_blocks = MAGIC.to_vec();
        two_blocks.extend([0x20, 0x00, 0, 0, 0, 1, 0, 0]);
        assert_eq!(
            scan_zstd_frames(&two_blocks, None)
                .expect("two blocks")
                .frames[0]
                .end,
            two_blocks.len()
        );

        let checksummed = empty_structural_frame(0x24);
        assert_eq!(
            scan_zstd_frames(&checksummed[..checksummed.len() - 1], None)
                .expect("torn checksum")
                .torn_start,
            Some(0)
        );
        assert_eq!(
            scan_zstd_frames(&checksummed, None)
                .expect("complete checksum")
                .frames[0]
                .end,
            checksummed.len()
        );
    }

    #[test]
    fn torn_prefix_decoder_recovers_available_plaintext_when_blocks_have_emitted() {
        let plaintext = (0..20_000)
            .map(|index| char::from(b'!' + u8::try_from(index % 90).expect("range")))
            .collect::<String>();
        let frame = compress_zstd_frame(plaintext.as_bytes()).expect("frame");
        let mut recovered = None;
        for end in [
            frame.len().saturating_sub(1),
            frame.len().saturating_sub(4),
            frame.len() * 3 / 4,
            frame.len() / 2,
        ] {
            if scan_zstd_frames(&frame[..end], None)
                .expect("torn scan")
                .torn_start
                != Some(0)
            {
                continue;
            }
            if let Ok(prefix) = decompress_zstd_prefix(&frame[..end])
                && !prefix.is_empty()
            {
                recovered = Some(prefix);
                break;
            }
        }
        let recovered = recovered.expect("recoverable torn prefix");
        assert!(plaintext.as_bytes().starts_with(&recovered));
    }
}
