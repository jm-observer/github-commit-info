use tokio::sync::watch;

/// ShutdownToken provides a shared cancellation signal across all service components.
///
/// Internally backed by a `watch::Sender<bool>` for efficient single-value broadcasting.
/// When `cancel()` is called, all waiting receivers are notified immediately.
#[derive(Clone)]
pub struct ShutdownToken {
    tx: watch::Sender<bool>,
}

impl ShutdownToken {
    /// Create a new shutdown token in the non-cancelled state.
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx }
    }

    /// Create a child token that inherits the parent's cancelled state.
    ///
    /// If the parent is already cancelled, the child starts in the cancelled state.
    pub fn child_token(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }

    /// Cancel all child tokens and broadcast the shutdown signal.
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }

    /// Returns `true` if the shutdown signal has been sent.
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Returns a receiver that resolves when the shutdown signal is sent.
    pub fn cancelled(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

impl Default for ShutdownToken {
    fn default() -> Self {
        Self::new()
    }
}
