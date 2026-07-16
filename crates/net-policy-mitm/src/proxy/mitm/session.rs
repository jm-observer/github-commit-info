use std::sync::Arc;

use log::debug;

use crate::proxy::ProxyRuntime;
use crate::sink::{FlowRequest, FlowResponse, FlowSink, FlowWsFrame};

/// Per-MITM-connection session: identity (domain + a fresh id per connection)
/// plus the [`FlowSink`] to forward decrypted flow data to.
///
/// This replaces `system-prompt-show`'s `MitmSession`, which additionally
/// tracked `x-session-id` extraction, deferred `CreateMessage`/`CloseMessage`
/// storage commands, and "summary sent once" bookkeeping for its SQLite layer.
/// None of that is meaningful to a generic MITM engine, so this is now a thin
/// pass-through: `record_request` / `record_response` / `record_ws_frame` call
/// straight into `sink.on_request` / `on_response` / `on_ws_frame`.
#[derive(Clone)]
pub(super) struct MitmSession {
    pub domain: String,
    pub session_id: String,
    pub sink: Arc<dyn FlowSink>,
}

impl MitmSession {
    pub fn new(domain: String, runtime: &ProxyRuntime) -> Self {
        let session_id = uuid::Uuid::new_v4().to_string();
        debug!("[{domain}] MITM session {session_id} started");
        Self {
            domain,
            session_id,
            sink: Arc::clone(&runtime.sink),
        }
    }

    pub fn record_request(&self, req: &FlowRequest) {
        self.sink.on_request(req);
    }

    pub fn record_response(&self, resp: &FlowResponse) {
        self.sink.on_response(resp);
    }

    pub fn record_ws_frame(&self, frame: &FlowWsFrame) {
        self.sink.on_ws_frame(frame);
    }

    /// Called once the MITM connection ends. Kept for lifecycle symmetry with
    /// the upstream project — there is no per-session storage state to flush
    /// here, but callers building on this crate may want a hook to log or
    /// account for session completion.
    pub fn close(&self) {
        debug!("[{}] MITM session {} closed", self.domain, self.session_id);
    }

    #[cfg(test)]
    pub(super) fn new_for_test(domain: String, session_id: String, sink: Arc<dyn FlowSink>) -> Self {
        Self {
            domain,
            session_id,
            sink,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::sink::test_support::CollectingSink;
    use crate::sink::{FlowRequest, FlowResponse, FlowWsFrame, WsDirection};

    use super::MitmSession;

    fn make_session() -> (MitmSession, Arc<CollectingSink>) {
        let sink = Arc::new(CollectingSink::default());
        let session = MitmSession::new_for_test(
            "api.example.com".to_string(),
            "session-1".to_string(),
            sink.clone(),
        );
        (session, sink)
    }

    #[test]
    fn record_request_forwards_to_sink() {
        let (session, sink) = make_session();
        session.record_request(&FlowRequest {
            domain: "api.example.com",
            method: "POST",
            path: "/v1/chat",
            version: "HTTP/1.1",
            headers: &[],
            body: b"{}",
        });

        let requests = sink.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "api.example.com");
        assert_eq!(requests[0].1, "POST");
        assert_eq!(requests[0].2, "/v1/chat");
    }

    #[test]
    fn record_response_forwards_to_sink() {
        let (session, sink) = make_session();
        session.record_response(&FlowResponse {
            domain: "api.example.com",
            status: 200,
            version: "HTTP/1.1",
            headers: &[],
            body: b"ok",
        });

        let responses = sink.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].1, 200);
    }

    #[test]
    fn record_ws_frame_forwards_to_sink() {
        let (session, sink) = make_session();
        session.record_ws_frame(&FlowWsFrame {
            domain: "api.example.com",
            direction: WsDirection::ClientToServer,
            is_text: true,
            payload: b"hi",
        });

        let frames = sink.ws_frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].2);
    }
}
