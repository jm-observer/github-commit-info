//! `net-policy-mitm` — a reusable TLS man-in-the-middle (MITM) proxy engine.
//!
//! Forked from the `system-prompt-show` project (`D:\git\system-prompt-show`), which
//! is a Rust MITM explicit forward proxy: it accepts a CONNECT request, learns the
//! real domain, dials an upstream (SOCKS5 / HTTP CONNECT) proxy routed by domain
//! (mihomo), and either tunnels the TCP stream verbatim (non-intercepted domains)
//! or performs a TLS MITM (rustls server + rcgen per-domain leaf certs signed by a
//! local CA) to get plaintext HTTP/1.1, HTTP/2, and WebSocket traffic.
//!
//! Unlike the upstream project — which extracts OpenAI/Claude/Gemini system prompts
//! from the decrypted traffic and persists them to SQLite — this crate has **no**
//! opinion about what the decrypted flow means. All prompt-extraction and storage
//! logic has been removed and replaced with a single generic callback trait,
//! [`sink::FlowSink`], that the caller implements to observe decrypted requests,
//! responses, and WebSocket frames.
//!
//! Intended consumer: `net-policy`'s L4 layer (TLS application-plaintext
//! decryption), which wants the MITM engine's plumbing (CA/cert cache, upstream
//! chaining, HTTP/1.1 + HTTP/2 + WebSocket parsing) without any built-in capture
//! semantics.

pub mod cert;
pub mod http;
pub mod proxy;
pub mod shutdown;
pub mod sink;
pub mod upstream;

/// Install the process-level rustls `CryptoProvider` (ring backend).
///
/// rustls 0.23 requires a process-wide crypto provider to be installed once at
/// startup; without it, the first TLS handshake panics with "Could not
/// automatically determine the process-level CryptoProvider". Any consumer that
/// drives TLS through this crate (e.g. via [`proxy::run_proxy`]) **must** call
/// this once at process start. Idempotent — a second call is a no-op.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
