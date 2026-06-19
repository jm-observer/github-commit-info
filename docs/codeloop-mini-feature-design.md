# Codeloop Mini Feature Design

> Purpose: this tiny feature exists to exercise the full codeloop workflow with a low-risk code change.

## Goal

Add a small parser helper to `codeloop-core` that recognizes codeloop stage markers in agent replies.

The helper is intentionally tiny so the E2E test focuses on the workflow:

1. design review
2. code implementation
3. implementation completion detection
4. code review from worktree
5. final PASS

## Functional Requirement

Add a public function in `crates/codeloop-core/src/parse.rs`:

```rust
pub fn parse_stage_marker(reply: &str) -> Option<String>
```

It scans `reply` line by line and returns the last non-empty stage marker.

Supported marker format:

```text
STAGE: <value>
```

Rules:

- Prefix is case-insensitive: `STAGE:`, `stage:`, `Stage:` all match.
- The value is trimmed.
- Empty values are ignored.
- If multiple markers appear, the last valid one wins.
- If no valid marker exists, return `None`.
- The returned value preserves the original value text after trimming.

## Tests

Add unit tests in `parse.rs` covering:

- parses a basic marker
- prefix is case-insensitive
- last valid marker wins
- empty marker is ignored
- returns `None` when absent

## Non-Goals

- Do not change codeloop runtime behavior.
- Do not wire this helper into the UI.
- Do not change existing parser semantics for `VERDICT`, `ASK_USER`, or `WORKTREE`.
- Do not touch unrelated files.

## Acceptance Criteria

- `cargo test -p codeloop-core parse_stage_marker` passes.
- `cargo check -p zero-desktop` passes.
- Existing tests for `parse_verdict`, `parse_ask_user`, and `parse_worktree_path` still pass.
