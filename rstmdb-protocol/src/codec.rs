//! Encoder and decoder for RCP frames and messages.

use crate::error::ProtocolError;
use crate::frame::Frame;
use crate::message::{Request, Response};
use bytes::{Bytes, BytesMut};

/// Encodes requests and responses into frames.
pub struct Encoder;

impl Encoder {
    /// Encodes a request into a frame.
    pub fn encode_request(request: &Request) -> Result<BytesMut, ProtocolError> {
        let frame = Frame::from_json(request)?;
        frame.encode()
    }

    /// Encodes a response into a frame.
    pub fn encode_response(response: &Response) -> Result<BytesMut, ProtocolError> {
        let frame = Frame::from_json(response)?;
        frame.encode()
    }

    /// Encodes any JSON-serializable value into a frame.
    pub fn encode_json<T: serde::Serialize>(value: &T) -> Result<BytesMut, ProtocolError> {
        let frame = Frame::from_json(value)?;
        frame.encode()
    }
}

/// Decodes frames into requests and responses.
pub struct Decoder {
    buffer: BytesMut,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(8192),
        }
    }

    /// Appends data to the internal buffer.
    pub fn extend(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Appends bytes to the internal buffer.
    pub fn extend_bytes(&mut self, data: Bytes) {
        self.buffer.extend_from_slice(&data);
    }

    /// Attempts to decode the next frame from the buffer.
    pub fn decode_frame(&mut self) -> Result<Option<Frame>, ProtocolError> {
        Frame::decode(&mut self.buffer)
    }

    /// Attempts to decode the next request from the buffer.
    pub fn decode_request(&mut self) -> Result<Option<Request>, ProtocolError> {
        match self.decode_frame()? {
            Some(frame) => {
                let payload =
                    std::str::from_utf8(&frame.payload).map_err(|_| ProtocolError::InvalidUtf8)?;
                let request: Request = serde_json::from_str(payload)?;
                Ok(Some(request))
            }
            None => Ok(None),
        }
    }

    /// Attempts to decode the next response from the buffer.
    pub fn decode_response(&mut self) -> Result<Option<Response>, ProtocolError> {
        match self.decode_frame()? {
            Some(frame) => {
                let payload =
                    std::str::from_utf8(&frame.payload).map_err(|_| ProtocolError::InvalidUtf8)?;
                let response: Response = serde_json::from_str(payload)?;
                Ok(Some(response))
            }
            None => Ok(None),
        }
    }

    /// Returns the number of bytes currently buffered.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Clears the internal buffer.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Line-delimited JSON codec for debug mode.
pub mod jsonl {
    use super::*;

    /// Encodes a value as a JSON line (no framing).
    pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Line-delimited JSON decoder.
    pub struct LineDecoder {
        buffer: Vec<u8>,
    }

    impl LineDecoder {
        pub fn new() -> Self {
            Self {
                buffer: Vec::with_capacity(4096),
            }
        }

        pub fn extend(&mut self, data: &[u8]) {
            self.buffer.extend_from_slice(data);
        }

        /// Attempts to decode the next JSON line.
        pub fn decode_line<T: serde::de::DeserializeOwned>(
            &mut self,
        ) -> Result<Option<T>, ProtocolError> {
            if let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
                let line = self.buffer.drain(..=pos).collect::<Vec<_>>();
                let json = std::str::from_utf8(&line[..line.len() - 1])
                    .map_err(|_| ProtocolError::InvalidUtf8)?;
                let value: T = serde_json::from_str(json)?;
                Ok(Some(value))
            } else {
                Ok(None)
            }
        }
    }

    impl Default for LineDecoder {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Operation;

    #[test]
    fn test_encoder_decoder_roundtrip() {
        let request = Request::new("42", Operation::Ping);
        let encoded = Encoder::encode_request(&request).unwrap();

        let mut decoder = Decoder::new();
        decoder.extend(&encoded);

        let decoded = decoder.decode_request().unwrap().unwrap();
        assert_eq!(decoded.id, "42");
        assert_eq!(decoded.op, Operation::Ping);
    }

    #[test]
    fn test_jsonl_roundtrip() {
        let request = Request::new("1", Operation::Info);
        let encoded = jsonl::encode(&request).unwrap();

        let mut decoder = jsonl::LineDecoder::new();
        decoder.extend(&encoded);

        let decoded: Request = decoder.decode_line().unwrap().unwrap();
        assert_eq!(decoded.id, "1");
        assert_eq!(decoded.op, Operation::Info);
    }

    #[test]
    fn test_partial_frame_decoding() {
        let request = Request::new("1", Operation::Ping);
        let encoded = Encoder::encode_request(&request).unwrap();

        let mut decoder = Decoder::new();

        // Feed partial data
        decoder.extend(&encoded[..10]);
        assert!(decoder.decode_request().unwrap().is_none());

        // Feed the rest
        decoder.extend(&encoded[10..]);
        let decoded = decoder.decode_request().unwrap().unwrap();
        assert_eq!(decoded.id, "1");
    }

    #[test]
    fn test_encode_response() {
        use crate::message::{Response, ResponseStatus};

        let response = Response::ok("req-1", serde_json::json!({"pong": true}));
        let encoded = Encoder::encode_response(&response).unwrap();

        let mut decoder = Decoder::new();
        decoder.extend(&encoded);
        let decoded = decoder.decode_response().unwrap().unwrap();

        assert_eq!(decoded.id, "req-1");
        assert_eq!(decoded.status, ResponseStatus::Ok);
    }

    #[test]
    fn test_decoder_buffered() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.buffered(), 0);

        decoder.extend(b"some data");
        assert_eq!(decoder.buffered(), 9);

        decoder.clear();
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn test_decoder_extend_bytes() {
        use bytes::Bytes;

        let request = Request::new("1", Operation::Ping);
        let encoded = Encoder::encode_request(&request).unwrap();

        let mut decoder = Decoder::new();
        decoder.extend_bytes(Bytes::from(encoded.to_vec()));

        let decoded = decoder.decode_request().unwrap().unwrap();
        assert_eq!(decoded.id, "1");
    }

    #[test]
    fn test_jsonl_multiple_lines() {
        let req1 = Request::new("1", Operation::Ping);
        let req2 = Request::new("2", Operation::Info);

        let mut data = jsonl::encode(&req1).unwrap();
        data.extend(jsonl::encode(&req2).unwrap());

        let mut decoder = jsonl::LineDecoder::new();
        decoder.extend(&data);

        let decoded1: Request = decoder.decode_line().unwrap().unwrap();
        assert_eq!(decoded1.id, "1");

        let decoded2: Request = decoder.decode_line().unwrap().unwrap();
        assert_eq!(decoded2.id, "2");
    }

    #[test]
    fn test_jsonl_partial_line() {
        let mut decoder = jsonl::LineDecoder::new();
        decoder.extend(b"{\"type\":\"request\"");

        // Not complete yet
        let result: Result<Option<Request>, _> = decoder.decode_line();
        assert!(result.unwrap().is_none());

        // Complete the line
        decoder.extend(b",\"id\":\"1\",\"op\":\"PING\",\"params\":{}}\n");
        let decoded: Request = decoder.decode_line().unwrap().unwrap();
        assert_eq!(decoded.id, "1");
    }

    #[test]
    fn test_encode_json_generic() {
        #[derive(serde::Serialize)]
        struct CustomMsg {
            action: String,
        }

        let msg = CustomMsg {
            action: "test".to_string(),
        };
        let encoded = Encoder::encode_json(&msg).unwrap();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_decoder_default() {
        let decoder = Decoder::default();
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn test_jsonl_decoder_default() {
        let decoder = jsonl::LineDecoder::default();
        // Just verify it creates successfully
        drop(decoder);
    }
}
