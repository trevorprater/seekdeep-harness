//! Output framing and offset-reader parity for E2B callbacks.

use seekdeep_subprocess::SubprocessOutputReader as _;
use seekdeep_subprocess_e2b::output::{
    E2B_OUTPUT_COMPLETE_FRAME, E2bBase64Decoder, E2bOutputReader,
};

#[test]
fn decoder_handles_arbitrary_boundaries_and_rejects_malformed_framing() {
    let mut decoder = E2bBase64Decoder::default();
    assert_eq!(decoder.push("").unwrap(), b"");
    assert_eq!(decoder.push("5").unwrap(), b"");
    assert_eq!(decoder.push("L2").unwrap(), b"");
    assert_eq!(decoder.push("g\n").unwrap(), "你".as_bytes());
    assert_eq!(decoder.push("YQ==\nYg==\n").unwrap(), b"ab");
    assert_eq!(decoder.push("AP8=\n").unwrap(), [0, 255]);
    assert_eq!(
        decoder
            .push(&format!("{E2B_OUTPUT_COMPLETE_FRAME}\n"))
            .unwrap(),
        b""
    );
    decoder.finish(true).unwrap();

    assert!(
        E2bBase64Decoder::default()
            .push("%\n")
            .unwrap_err()
            .to_string()
            .contains("invalid base64")
    );
    assert!(
        E2bBase64Decoder::default()
            .push("AB==\n")
            .unwrap_err()
            .to_string()
            .contains("invalid base64")
    );
    assert!(
        decoder
            .push(&format!("{E2B_OUTPUT_COMPLETE_FRAME}\n"))
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(
        decoder
            .push("YQ==\n")
            .unwrap_err()
            .to_string()
            .contains("continued after completion")
    );
    let mut truncated = E2bBase64Decoder::default();
    truncated.push("YQ").unwrap();
    assert!(
        truncated
            .finish(true)
            .unwrap_err()
            .to_string()
            .contains("truncated")
    );
    assert!(
        E2bBase64Decoder::default()
            .finish(true)
            .unwrap_err()
            .to_string()
            .contains("incomplete")
    );
    let mut interrupted = E2bBase64Decoder::default();
    interrupted.push("YQ").unwrap();
    interrupted.finish(false).unwrap();
}

#[test]
fn reader_keeps_a_byte_exact_tail_with_independent_offsets() {
    let reader = E2bOutputReader::new(4, Some(10), "/remote/spill");
    reader.push(b"").unwrap();
    reader.push(b"ab").unwrap();
    reader.push(b"cdef").unwrap();
    assert_eq!(reader.size(), 6);
    let from_start = reader.read_from(0);
    assert_eq!(from_start.text, "cdef");
    assert_eq!(from_start.next_offset, 6);
    assert!(from_start.lossy);
    assert_eq!(
        from_start.spill_path.unwrap(),
        std::path::PathBuf::from("/remote/spill")
    );
    assert_eq!(reader.read_from(2).text, "cdef");
    assert_eq!(reader.read_from(5).text, "f");
    assert_eq!(reader.read_from(99).text, "");
    reader.invalidate_spill();
    assert!(reader.read_from(0).spill_path.is_none());
}

#[test]
fn reader_drops_whole_head_chunks_and_withholds_absent_or_over_cap_spills() {
    let without_spill = E2bOutputReader::new(2, None, "/unused");
    without_spill.push(b"ab").unwrap();
    without_spill.push(b"cd").unwrap();
    let read = without_spill.read_from(0);
    assert_eq!(read.text, "cd");
    assert_eq!(read.next_offset, 4);
    assert!(read.lossy);
    assert!(read.spill_path.is_none());

    let over_cap = E2bOutputReader::new(2, Some(3), "/too-small");
    over_cap.push(b"abcd").unwrap();
    let read = over_cap.read_from(0);
    assert_eq!(read.text, "cd");
    assert!(read.lossy);
    assert!(read.spill_path.is_none());
}
