// #718 / AC-0289 framing foundation only; these tests do not establish MCP acceptance.
//! #718 / AC-0289 framing foundation only; no server acceptance is claimed.
use super::*;
use std::io::{BufReader, Cursor};

fn cap(bytes: usize) -> NonZeroUsize {
    NonZeroUsize::new(bytes).unwrap()
}

#[test]
fn exact_boundary_and_crlf() {
    assert_eq!(
        read_frame(&mut &b"{}\n"[..], cap(2)),
        Ok(Some(serde_json::json!({})))
    );
    assert_eq!(
        read_frame(&mut &b"{}\r\n"[..], cap(3)),
        Ok(Some(serde_json::json!({})))
    );
    assert_eq!(
        read_frame(&mut &b"{}\r\n"[..], cap(2)),
        Err(FrameError::Limit)
    );
    assert_eq!(
        read_frame(&mut &b"{}\n"[..], cap(1)),
        Err(FrameError::Limit)
    );
}

#[test]
fn eof_and_empty_lines_are_explicit() {
    assert_eq!(read_frame(&mut &b""[..], cap(8)), Ok(None));
    assert_eq!(
        read_frame(&mut &b"{}"[..], cap(2)),
        Err(FrameError::Unterminated)
    );
    for bytes in [b"\n".as_slice(), b"\r\n", b"  \n"] {
        assert_eq!(
            read_frame(&mut &*bytes, cap(8)),
            Err(FrameError::InvalidJson)
        );
    }
    assert_eq!(read_frame(&mut &b"{}x"[..], cap(2)), Err(FrameError::Limit));
}

#[test]
fn short_reads_preserve_utf8_and_frame_boundaries() {
    let input = "{\"text\":\"é🙂\"}\r\n[]\n";
    for chunk in 1..=input.len() {
        let mut reader = BufReader::with_capacity(chunk, Cursor::new(input.as_bytes()));
        assert_eq!(
            read_frame(&mut reader, cap(64)),
            Ok(Some(serde_json::json!({"text": "é🙂"})))
        );
        assert_eq!(
            read_frame(&mut reader, cap(2)),
            Ok(Some(serde_json::json!([])))
        );
        assert_eq!(read_frame(&mut reader, cap(2)), Ok(None));
    }
}

#[test]
fn rejects_invalid_utf8_json_and_trailing_values() {
    for input in [
        b"\"\xff\"\n".as_slice(),
        b"{\n",
        b"{} []\n",
        b"{}garbage\n",
        b"\"a\nb\"\n",
    ] {
        assert_eq!(
            read_frame(&mut &*input, cap(64)),
            Err(FrameError::InvalidJson)
        );
    }
    let nested = format!("{}0{}\n", "[".repeat(200), "]".repeat(200));
    assert_eq!(
        read_frame(&mut nested.as_bytes(), cap(1024)),
        Err(FrameError::InvalidJson)
    );
}

#[test]
fn rejects_large_unterminated_and_delimited_input() {
    for suffix in ["", "\n"] {
        let input = format!("{}{}", "x".repeat(100_000), suffix);
        let mut reader = BufReader::with_capacity(3, Cursor::new(input));
        assert_eq!(read_frame(&mut reader, cap(16)), Err(FrameError::Limit));
    }
}

#[test]
fn writer_bounds_escaped_serialization_before_output() {
    let value = serde_json::json!("\0\n\"\\é🙂");
    let expected = serde_json::to_vec(&value).unwrap();
    let mut output = Vec::new();
    assert_eq!(
        write_frame(&mut output, &value, cap(expected.len() - 1)),
        Err(FrameError::Limit)
    );
    assert!(output.is_empty());
    write_frame(&mut output, &value, cap(expected.len())).unwrap();
    assert_eq!(&output[..expected.len()], &expected);
    assert_eq!(&output[expected.len()..], b"\n");
    assert_eq!(
        read_frame(&mut output.as_slice(), cap(expected.len())),
        Ok(Some(value))
    );
}

struct Broken;
impl io::Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("secret payload"))
    }
}
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("secret payload"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn io_errors_are_source_free() {
    let error = read_frame(&mut BufReader::new(Broken), cap(8)).unwrap_err();
    assert_eq!(error, FrameError::Io);
    assert_eq!(error.to_string(), "frame I/O failed");
    assert_eq!(
        write_frame(&mut Broken, &serde_json::json!({}), cap(2)),
        Err(FrameError::Io)
    );
}
