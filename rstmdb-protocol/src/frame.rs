//! Binary frame format for RCP.
//!
//! Frame layout (18 bytes header + optional header extension + payload):
//!
//! ```text
//! +--------+---------+--------+------------+-------------+--------+
//! | magic  | version | flags  | header_len | payload_len | crc32c |
//! | 4 bytes| 2 bytes |2 bytes |  2 bytes   |   4 bytes   | 4 bytes|
//! +--------+---------+--------+------------+-------------+--------+
//! | [header_ext] | payload                                        |
//! | header_len   | payload_len bytes                              |
//! +--------------+------------------------------------------------+
//! ```

use crate::error::ProtocolError;
use crate::MAX_PAYLOAD_SIZE;
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Magic bytes identifying RCP frames: "RCPX"
pub const MAGIC: [u8; 4] = *b"RCPX";

/// Size of the fixed frame header in bytes (4+2+2+2+4+4 = 18).
pub const FRAME_HEADER_SIZE: usize = 18;

/// Frame flags bitfield.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameFlags(u16);

impl FrameFlags {
    /// CRC32C checksum is present and valid.
    pub const CRC_PRESENT: u16 = 1 << 0;
    /// Payload is compressed (reserved for future use).
    pub const COMPRESSED: u16 = 1 << 1;
    /// This frame is part of a stream.
    pub const STREAM: u16 = 1 << 2;
    /// Final frame of a stream.
    pub const END_STREAM: u16 = 1 << 3;

    /// Valid flags mask for protocol version 1.
    const VALID_V1_MASK: u16 = 0x000F;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn with_crc(mut self) -> Self {
        self.0 |= Self::CRC_PRESENT;
        self
    }

    pub fn with_stream(mut self) -> Self {
        self.0 |= Self::STREAM;
        self
    }

    pub fn with_end_stream(mut self) -> Self {
        self.0 |= Self::END_STREAM;
        self
    }

    pub fn has_crc(&self) -> bool {
        self.0 & Self::CRC_PRESENT != 0
    }

    pub fn is_compressed(&self) -> bool {
        self.0 & Self::COMPRESSED != 0
    }

    pub fn is_stream(&self) -> bool {
        self.0 & Self::STREAM != 0
    }

    pub fn is_end_stream(&self) -> bool {
        self.0 & Self::END_STREAM != 0
    }

    pub fn bits(&self) -> u16 {
        self.0
    }

    pub fn from_bits(bits: u16) -> Result<Self, ProtocolError> {
        if bits & !Self::VALID_V1_MASK != 0 {
            return Err(ProtocolError::InvalidFlags(bits));
        }
        Ok(Self(bits))
    }
}

/// A parsed RCP frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Protocol version.
    pub version: u16,
    /// Frame flags.
    pub flags: FrameFlags,
    /// Optional header extension (reserved for future use).
    pub header_extension: Bytes,
    /// Frame payload (JSON data).
    pub payload: Bytes,
}

impl Frame {
    /// Creates a new frame with the given payload.
    pub fn new(payload: Bytes) -> Self {
        Self {
            version: crate::PROTOCOL_VERSION,
            flags: FrameFlags::new().with_crc(),
            header_extension: Bytes::new(),
            payload,
        }
    }

    /// Creates a new frame from a JSON-serializable value.
    pub fn from_json<T: serde::Serialize>(value: &T) -> Result<Self, ProtocolError> {
        let payload = serde_json::to_vec(value)?;
        Ok(Self::new(Bytes::from(payload)))
    }

    /// Encodes the frame into bytes.
    pub fn encode(&self) -> Result<BytesMut, ProtocolError> {
        let payload_len = self.payload.len() as u32;
        if payload_len > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::FrameTooLarge {
                size: payload_len,
                max: MAX_PAYLOAD_SIZE,
            });
        }

        let header_len = self.header_extension.len() as u16;
        let total_size = FRAME_HEADER_SIZE + header_len as usize + self.payload.len();
        let mut buf = BytesMut::with_capacity(total_size);

        // Magic (4 bytes)
        buf.put_slice(&MAGIC);

        // Version (2 bytes)
        buf.put_u16(self.version);

        // Flags (2 bytes)
        buf.put_u16(self.flags.bits());

        // Header extension length (2 bytes)
        buf.put_u16(header_len);

        // Payload length (4 bytes)
        buf.put_u32(payload_len);

        // CRC32C of payload (4 bytes)
        let crc = if self.flags.has_crc() {
            crc32c::crc32c(&self.payload)
        } else {
            0
        };
        buf.put_u32(crc);

        // Header extension (if any)
        if !self.header_extension.is_empty() {
            buf.put_slice(&self.header_extension);
        }

        // Payload
        buf.put_slice(&self.payload);

        Ok(buf)
    }

    /// Decodes a frame from bytes.
    ///
    /// Returns `Ok(Some(frame))` if a complete frame was decoded,
    /// `Ok(None)` if more data is needed, or `Err` on protocol errors.
    pub fn decode(buf: &mut BytesMut) -> Result<Option<Self>, ProtocolError> {
        if buf.len() < FRAME_HEADER_SIZE {
            return Ok(None);
        }

        // Peek at header without consuming
        let magic: [u8; 4] = buf[0..4].try_into().unwrap();
        if magic != MAGIC {
            return Err(ProtocolError::InvalidMagic(magic));
        }

        let version = u16::from_be_bytes([buf[4], buf[5]]);
        if version != crate::PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }

        let flags_bits = u16::from_be_bytes([buf[6], buf[7]]);
        let flags = FrameFlags::from_bits(flags_bits)?;

        let header_len = u16::from_be_bytes([buf[8], buf[9]]) as usize;
        let payload_len = u32::from_be_bytes([buf[10], buf[11], buf[12], buf[13]]) as usize;

        if payload_len > MAX_PAYLOAD_SIZE as usize {
            return Err(ProtocolError::FrameTooLarge {
                size: payload_len as u32,
                max: MAX_PAYLOAD_SIZE,
            });
        }

        let crc_expected = u32::from_be_bytes([buf[14], buf[15], buf[16], buf[17]]);

        let total_len = FRAME_HEADER_SIZE + header_len + payload_len;
        if buf.len() < total_len {
            return Ok(None);
        }

        // Consume header
        buf.advance(FRAME_HEADER_SIZE);

        // Read header extension
        let header_extension = buf.split_to(header_len).freeze();

        // Read payload
        let payload = buf.split_to(payload_len).freeze();

        // Validate CRC if present
        if flags.has_crc() {
            let crc_actual = crc32c::crc32c(&payload);
            if crc_actual != crc_expected {
                return Err(ProtocolError::CrcMismatch {
                    expected: crc_expected,
                    actual: crc_actual,
                });
            }
        }

        Ok(Some(Self {
            version,
            flags,
            header_extension,
            payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_roundtrip() {
        let payload = Bytes::from(r#"{"type":"request","id":"1","op":"PING","params":{}}"#);
        let frame = Frame::new(payload.clone());

        let encoded = frame.encode().unwrap();
        let mut buf = encoded;
        let decoded = Frame::decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded.version, crate::PROTOCOL_VERSION);
        assert!(decoded.flags.has_crc());
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_crc_validation() {
        let payload = Bytes::from(r#"{"test":"data"}"#);
        let frame = Frame::new(payload);
        let mut encoded = frame.encode().unwrap();

        // Corrupt the payload
        let len = encoded.len();
        encoded[len - 1] ^= 0xFF;

        let result = Frame::decode(&mut encoded);
        assert!(matches!(result, Err(ProtocolError::CrcMismatch { .. })));
    }

    #[test]
    fn test_invalid_magic() {
        // 18 bytes: 4 magic + 2 version + 2 flags + 2 header_len + 4 payload_len + 4 crc
        let mut buf =
            BytesMut::from(&b"BADX\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"[..]);
        let result = Frame::decode(&mut buf);
        assert!(matches!(result, Err(ProtocolError::InvalidMagic(_))));
    }

    #[test]
    fn test_incomplete_frame() {
        // Only 10 bytes, less than header size
        let mut buf = BytesMut::from(&b"RCPX\x00\x01\x00\x01"[..]);
        let result = Frame::decode(&mut buf);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_unsupported_version() {
        // Valid magic but wrong version (99)
        let mut buf =
            BytesMut::from(&b"RCPX\x00\x63\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"[..]);
        let result = Frame::decode(&mut buf);
        assert!(matches!(result, Err(ProtocolError::UnsupportedVersion(99))));
    }

    #[test]
    fn test_frame_flags() {
        let flags = FrameFlags::new().with_crc().with_stream().with_end_stream();

        assert!(flags.has_crc());
        assert!(flags.is_stream());
        assert!(flags.is_end_stream());
        assert!(!flags.is_compressed());
    }

    #[test]
    fn test_invalid_flags() {
        // Bit outside valid v1 mask
        let result = FrameFlags::from_bits(0x0100);
        assert!(matches!(result, Err(ProtocolError::InvalidFlags(0x0100))));
    }

    #[test]
    fn test_frame_too_large() {
        let huge_payload = vec![0u8; (MAX_PAYLOAD_SIZE + 1) as usize];
        let frame = Frame::new(Bytes::from(huge_payload));
        let result = frame.encode();
        assert!(matches!(result, Err(ProtocolError::FrameTooLarge { .. })));
    }

    #[test]
    fn test_empty_payload() {
        let payload = Bytes::from(r#"{}"#);
        let frame = Frame::new(payload.clone());

        let encoded = frame.encode().unwrap();
        let mut buf = encoded;
        let decoded = Frame::decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_frame_from_json() {
        #[derive(serde::Serialize)]
        struct TestMsg {
            value: i32,
        }
        let frame = Frame::from_json(&TestMsg { value: 42 }).unwrap();
        let payload_str = std::str::from_utf8(&frame.payload).unwrap();
        assert!(payload_str.contains("42"));
    }

    #[test]
    fn test_frame_with_header_extension() {
        let mut frame = Frame::new(Bytes::from(r#"{"test":true}"#));
        frame.header_extension = Bytes::from(&b"ext_data"[..]);

        let encoded = frame.encode().unwrap();
        let mut buf = encoded;
        let decoded = Frame::decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded.header_extension.as_ref(), b"ext_data");
    }

    #[test]
    fn test_frame_without_crc() {
        let mut frame = Frame::new(Bytes::from(r#"{"test":true}"#));
        frame.flags = FrameFlags::new(); // No CRC

        let encoded = frame.encode().unwrap();
        let mut buf = encoded;
        let decoded = Frame::decode(&mut buf).unwrap().unwrap();

        assert!(!decoded.flags.has_crc());
    }

    #[test]
    fn test_multiple_frames_in_buffer() {
        let frame1 = Frame::new(Bytes::from(r#"{"id":"1"}"#));
        let frame2 = Frame::new(Bytes::from(r#"{"id":"2"}"#));

        let mut buf = BytesMut::new();
        buf.extend_from_slice(&frame1.encode().unwrap());
        buf.extend_from_slice(&frame2.encode().unwrap());

        let decoded1 = Frame::decode(&mut buf).unwrap().unwrap();
        assert!(std::str::from_utf8(&decoded1.payload)
            .unwrap()
            .contains("\"1\""));

        let decoded2 = Frame::decode(&mut buf).unwrap().unwrap();
        assert!(std::str::from_utf8(&decoded2.payload)
            .unwrap()
            .contains("\"2\""));
    }
}
