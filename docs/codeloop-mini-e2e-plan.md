# Codeloop Mini E2E Plan

> Target design doc: `docs/codeloop-mini-feature-design.md`
>
> This plan validates the Zero Desktop codeloop workflow with a deliberately small implementation task.

## Objective

Run one complete workflow:

```text
design review -> code implementation -> implementation completion -> code review -> final PASS
```

The feature under test is intentionally small: add `parse_stage_marker()` plus unit tests in `codeloop-core`.

## Preconditions

- Zero Desktop is running with the latest local build.
- `codex` and `claude` CLIs are installed, logged in, and on PATH.
- Claude and Codex sessions are in this repository: `D:\git\toolkit`.
- The codeloop page can list both sessions.
- The working tree may be dirty, but unrelated user changes must not be reverted.
- `sqlite3` is on PATH (Windows does not ship it; install via `winget install SQLite.SQLite` or use any other sqlite client).

## Time Budget Per Step

Rough upper bounds for "still healthy, keep waiting" — exceed these and treat as a probable hang.

| Step | Expected wait |
|---|---|
| Codex design review (one round) | up to 3 min |
| Claude implementation (single round, this tiny feature) | up to 5 min |
| Codex code review (one round, against worktree) | up to 3 min |

## Recommended UI Settings

| Stage | target | mode | 新 Agent | worktree | GO | rounds | step confirm |
|---|---|---|---|---|---|---|---|
| 1. Design review | `docs/codeloop-mini-feature-design.md` | 设计复核 | — | off | off | 2 | on |
| 2. Code implementation (from design record) | (auto) | 实现复核 | on | on | off (first run) | 2 | on |
| 3. Code review (from impl record) | (auto, worktree locked) | 实现复核 + `resume_worktree_path` | on | locked | optional | 2 | on |

For the first full run keep `GO` off everywhere — the goal is observability.

## E2E Steps

### 1. Start Design Review

1. Open Zero Desktop -> codeloop.
2. Select one Claude session and one Codex session for `D:\git\toolkit`.
3. Use target `docs/codeloop-mini-feature-design.md`.
4. Start a `设计复核` loop.
5. Wait for Codex review.

Expected:

- A new design record is created.
- Transcript shows at least one `codex_review`.
- Final state is `done / pass`, or Codex lists concrete design issues.

If Codex returns `NEEDS_WORK`, let Claude revise the design doc only if the issue is valid. Otherwise stop and record the finding.

### 2. Start Code Implementation From The Design Record

From the completed design record, use the stage action panel:

1. Keep `新 Agent` checked.
2. Keep `worktree` checked.
3. Keep `GO` unchecked for visibility.
4. Click `开始代码实现`.

Expected:

- A new implementation record is created with `parent_loop_id` pointing to the design record.
- The record enters implementation phase.
- A system message says the implementation command was sent.
- Claude eventually replies with a `WORKTREE: <absolute path>` line.
- The record shows `claude_implement`.
- `worktree_path` is populated.

### 3. Recover If Zero Desktop Stops Tracking

This step verifies the "Claude kept working but Zero Desktop lost the stream" recovery path.
Trigger it deliberately on one run so the recovery code is actually exercised. Prefer the UI
`停止` button first because it exercises the product stop path; closing/killing Zero Desktop is
a harsher variant and may also terminate the spawned CLI process on some runs.

- While Claude is still working on implementation (step 2 has not yet logged
  `claude_implement`), click `停止` on the implementation record.
- Then watch the Claude transcript/session. Continue this recovery check only if Claude keeps
  writing and eventually emits a final reply with `WORKTREE: <absolute path>`.

Then:

1. Wait until Claude finishes in its own transcript (the Claude CLI session window).
2. Restart Zero Desktop, reopen the codeloop page, refresh the implementation record detail.

Expected:

- `loop_messages` backfills from Claude transcript.
- The record shows a system message saying the implementation result was recovered.
- The record shows the final `claude_implement` message.
- `worktree_path` is populated.
- The detail panel shows `下一步：代码审核`.

### 4. Start Code Review From The Worktree

From the implementation record with `worktree_path`:

1. Keep `新 Agent` checked.
2. Confirm `worktree` is checked/locked.
3. Click `开始代码审核`.

How Codex sees the code: the new record is started with a request-level `resume_worktree_path`
equal to the impl record's `worktree_path`. The runtime skips Claude's
implement step (`mod.rs:460`), relocates the Codex session into that worktree as its cwd,
and Codex reviews via direct `git diff` / file reads against that working copy.

Expected:

- A new code-review record is created with `parent_loop_id` pointing to the implementation record.
- The new record stores `worktree_path` = parent's `worktree_path` (still mode=`implementation`; see Database Checks note).
- It skips the Claude implementation step (no `claude_implement` message at round 0).
- Codex reviews the worktree code directly.
- Final state is `done / pass`, or `needs_work` creates a normal revise/review loop.

### 5. Verify The Actual Code

Run (against the implementation worktree, not the main checkout):

```powershell
cd <impl_record.worktree_path>
cargo test -p codeloop-core          # full crate, not just the new test — see acceptance criteria below
cargo check -p zero-desktop
```

Expected:

- Both commands pass.
- The new test `parse_stage_marker` is included in the test output.
- Existing `parse_verdict` / `parse_ask_user` / `parse_worktree_path` tests still pass.
- No unrelated files were reverted.

## Database Checks

Use the Zero Desktop codeloop DB (path source of truth:
`crates/zero-desktop/src/shared/workspace.rs::codeloop_db_path` —
`<workspace>/codeloop/state.db`; default workspace on Windows is `%LOCALAPPDATA%\zero-desktop`):

```powershell
$db = "$env:LOCALAPPDATA\zero-desktop\codeloop\state.db"
sqlite3 $db "SELECT id,parent_loop_id,mode,status,final_verdict,total_rounds,worktree_path,target_repo_rel FROM loops ORDER BY id DESC LIMIT 10;"
sqlite3 $db "SELECT loop_id,round,kind,verdict,substr(content,1,160) FROM loop_messages WHERE loop_id IN (<design_id>,<impl_id>,<review_id>) ORDER BY loop_id,id;"
```

Expected record chain:

```text
design_id: parent_loop_id NULL, mode design, status done, final_verdict pass
impl_id: parent_loop_id design_id, mode implementation, worktree_path populated, has claude_implement
review_id: parent_loop_id impl_id, mode implementation, status done, final_verdict pass, no claude_implement at round 0
```

Note: the code-review record's `mode` is also `implementation` — codeloop only has two
modes (`design` / `implementation`, see `codeloop-core/src/prompt.rs::ReviewMode`). What
distinguishes a code review from a fresh implementation is:

- the record has `worktree_path` populated immediately from the parent implementation record
- its round-0 messages include a system note: `从已完成 worktree 继续复核`
- it has no `claude_implement` message

`resume_worktree_path` is a request field, not a persisted DB column.

## Pass Criteria

The E2E run passes when all are true:

- design review reaches `PASS`
- implementation record is created from the design record
- implementation message is visible in the record detail
- worktree path is detected or recovered
- code review record starts from the worktree without re-running implementation
- final code review reaches `PASS`
- `cargo test -p codeloop-core parse_stage_marker` passes
- `cargo check -p zero-desktop` passes

## Failure Notes To Capture

For any failure, capture:

- loop id
- parent loop id
- selected Claude/Codex session ids
- current phase
- `final_verdict`
- whether `loop_messages` contains `system`, `claude_implement`, `codex_review`
- whether `worktree_path` is populated
- last 30 lines of the Claude transcript
- relevant Zero Desktop logs

## Cleanup / Repeatability

Each run leaves persistent state. Decide upfront whether to keep history or wipe between runs.

Wipe between runs (recommended while debugging the workflow):

```powershell
# 1. stop Zero Desktop first
# 2. drop the three records created in this run (replace ids from the DB check above)
sqlite3 $db "DELETE FROM loop_messages WHERE loop_id IN (<design_id>,<impl_id>,<review_id>);"
sqlite3 $db "DELETE FROM loops WHERE id IN (<design_id>,<impl_id>,<review_id>);"
# 3. remove the worktree dir (path from impl_record.worktree_path)
git worktree remove <impl_record.worktree_path>   # if still registered
Remove-Item -Recurse -Force <impl_record.worktree_path>   # if already detached
```

Keep history: do nothing — new run creates a new chain. Use the rounds/timestamps to filter.

## Recommendation

For the first run, keep `GO` off. The goal is observability. Once the record chain and recovery behavior are verified, rerun the same tiny feature with `GO` on to validate the fully automatic path.
