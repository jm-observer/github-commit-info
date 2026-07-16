//! Generic decrypted-flow observation callback.
//!
//! This replaces `system-prompt-show`'s capture/storage layer (which extracted
//! OpenAI/Claude/Gemini system prompts and wrote them to SQLite). The MITM engine
//! in this crate only decrypts and parses traffic — it has no opinion about what
//! the bytes mean. Implement [`FlowSink`] to observe requests/responses/WS frames;
//! do redaction, filtering, or persistence in your own implementation.

/// A single decrypted HTTP request (plaintext). This is a borrowed view — if the
/// callback needs to retain data past the call, it must copy it itself.
pub struct FlowRequest<'a> {
    pub domain: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub version: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
}

/// A single decrypted HTTP response (plaintext). Borrowed view, same caveat as
/// [`FlowRequest`].
pub struct FlowResponse<'a> {
    pub domain: &'a str,
    pub status: u16,
    pub version: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
}

/// Direction of a WebSocket frame relative to the MITM proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsDirection {
    ClientToServer,
    ServerToClient,
}

/// A single decrypted (and, if compressed via permessage-deflate, decompressed)
/// WebSocket message. Fragmented WebSocket messages are reassembled before this
/// callback fires — `payload` is always a complete message.
pub struct FlowWsFrame<'a> {
    pub domain: &'a str,
    pub direction: WsDirection,
    pub is_text: bool,
    pub payload: &'a [u8],
}

/// Callback trait for observing decrypted MITM flow data.
///
/// The MITM engine calls these unconditionally for every request / response /
/// complete WebSocket message it decrypts — it makes no judgment about the
/// content. Implementations are responsible for any redaction, filtering, or
/// persistence they need.
///
/// Implementations must be `Send + Sync` since the engine calls them from
/// concurrently-running per-connection tasks.
pub trait FlowSink: Send + Sync {
    fn on_request(&self, req: &FlowRequest);
    fn on_response(&self, resp: &FlowResponse);
    fn on_ws_frame(&self, frame: &FlowWsFrame);
}

/// A [`FlowSink`] that does nothing. Useful when the engine should only decrypt
/// and relay traffic without observing it.
pub struct NoopSink;

impl FlowSink for NoopSink {
    fn on_request(&self, _req: &FlowRequest) {}
    fn on_response(&self, _resp: &FlowResponse) {}
    fn on_ws_frame(&self, _frame: &FlowWsFrame) {}
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    use super::{FlowRequest, FlowResponse, FlowSink, FlowWsFrame, WsDirection};

    /// A simple collecting sink for tests: records how many times each callback
    /// fired and a copy of the relevant data.
    #[derive(Default)]
    #[allow(clippy::type_complexity)]
    pub(crate) struct CollectingSink {
        pub requests: Mutex<Vec<(String, String, String, Vec<u8>)>>,
        pub responses: Mutex<Vec<(String, u16, Vec<u8>)>>,
        pub ws_frames: Mutex<Vec<(String, WsDirection, bool, Vec<u8>)>>,
    }

    impl FlowSink for CollectingSink {
        fn on_request(&self, req: &FlowRequest) {
            self.requests.lock().unwrap().push((
                req.domain.to_string(),
                req.method.to_string(),
                req.path.to_string(),
                req.body.to_vec(),
            ));
        }

        fn on_response(&self, resp: &FlowResponse) {
            self.responses.lock().unwrap().push((
                resp.domain.to_string(),
                resp.status,
                resp.body.to_vec(),
            ));
        }

        fn on_ws_frame(&self, frame: &FlowWsFrame) {
            self.ws_frames.lock().unwrap().push((
                frame.domain.to_string(),
                frame.direction,
                frame.is_text,
                frame.payload.to_vec(),
            ));
        }
    }

    #[test]
    fn collecting_sink_records_all_callbacks() {
        let sink = CollectingSink::default();
        sink.on_request(&FlowRequest {
            domain: "example.com",
            method: "GET",
            path: "/",
            version: "HTTP/1.1",
            headers: &[],
            body: b"",
        });
        sink.on_response(&FlowResponse {
            domain: "example.com",
            status: 200,
            version: "HTTP/1.1",
            headers: &[],
            body: b"ok",
        });
        sink.on_ws_frame(&FlowWsFrame {
            domain: "example.com",
            direction: WsDirection::ClientToServer,
            is_text: true,
            payload: b"hi",
        });

        assert_eq!(sink.requests.lock().unwrap().len(), 1);
        assert_eq!(sink.responses.lock().unwrap().len(), 1);
        assert_eq!(sink.ws_frames.lock().unwrap().len(), 1);
    }
}
