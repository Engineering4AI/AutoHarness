# CLAUDE.md — AutoHarness

## Project overview

Single-binary Rust agent with two modes: an interactive CLI that logs everything to `.evo/`, and a self-evolution mode that reflects on past trajectories and rewrites its own source. The LLM is the judge — no numeric scoring.

## Build & run

```bash
cargo build --release
./target/release/auto-harness          # interactive chat (logs to .evo/)
./target/release/auto-harness evolve   # reflect on trajs + run code evolution loop
```

## Key constants (src/main.rs)

| Constant | Value | Meaning |
|---|---|---|
| `SELF_PATH` | `src/main.rs` | File the agent reads and rewrites |
| `MAX_ITERS` | `10` | Max evolution iterations per `evolve` run |
| `PATIENCE` | `3` | Stop early if no `write_self` succeeds for N consecutive iters |
| `WATERMARK_PATH` | `.evo/learned_until.txt` | Timestamp of last reflected session |

## Two modes

### `auto-harness` (default) — interactive chat

Starts an interactive REPL with an async input queue (background stdin thread → `VecDeque`). LLM replies are printed; all other events go to traj only. Runs until Ctrl+C or EOF.

Task grouping: when a new message arrives, the LLM judges whether it continues the current task or starts a new one (`NEW` / `CONTINUE`). Each task gets its own output directory `outputs/<ts>/task_N`. Traj and outputs share the same session timestamp.

### `auto-harness evolve` — reflection + code evolution

1. **Reflect**: reads all session trajs newer than the watermark. Sends up to 300 lines to the LLM asking for one concrete improvement suggestion. Logs the result; advances the watermark.
2. **Evolve**: runs up to `MAX_ITERS` iterations. Each iteration shows the LLM the current `src/main.rs` and asks for one improvement. The LLM may call `write_self`, `shell`, or reply `SKIP` to exit immediately. Stops early on `SKIP` or after `PATIENCE` consecutive non-improving iterations.
3. **Doc update**: after the loop, asks the LLM to update `CLAUDE.md` and `README.md` via `write_file` tool calls to reflect the current implementation.

## Tool protocol

LLM emits plain-text tags, parsed by `run_tool()`:

```
<tool name="shell">command here</tool>
<tool name="write_self">...full file content...</tool>
<tool name="write_file">path/to/file
...full file content...</tool>
```

Only one tool per LLM turn. Results are fed back as `<tool_result>...</tool_result>` user messages. Up to 8 turns per iteration.

### write_self safety (atomic write-and-verify)

`write_self` never leaves a broken file on disk:

1. Reject immediately if content is empty
2. Back up current `src/main.rs` to `src/main.rs.bak` (evolve mode only)
3. Write the new content
4. Run `cargo build --release`
5. If build fails → restore backup, report compiler error to LLM so it can retry
6. If build passes → keep new file

## Trajectory logging

Every run creates `.evo/sessions/<unix_timestamp>/traj.jsonl`. Each line is a JSON event:

```json
{"ts": 1713300000, "kind": "session_start",    "data": {}}
{"ts": 1713300001, "kind": "user_input",        "data": "fix the bug in shell()"}
{"ts": 1713300005, "kind": "llm_response",      "data": {"task": 1, "turn": 1, "content": "..."}}
{"ts": 1713300008, "kind": "task_boundary",     "data": {"task": 2}}
{"ts": 1713300010, "kind": "tool_result",       "data": {"tool": "write_self", "result": "written and verified OK"}}
{"ts": 1713300011, "kind": "session_end",       "data": {"turns": 4}}
{"ts": 1713300020, "kind": "iter_start",        "data": {"iter": 1}}
{"ts": 1713300025, "kind": "iter_end",          "data": {"iter": 1, "improved": true}}
{"ts": 1713300026, "kind": "iter_skip",         "data": {"iter": 2, "reason": "LLM chose not to evolve"}}
{"ts": 1713300027, "kind": "evolve_end",        "data": {}}
```

## Output layout

```
.evo/
  sessions/<ts>/traj.jsonl      # event log for the session
  learned_until.txt             # watermark for reflection
outputs/<ts>/
  task_1/                       # artifacts for task 1 (same ts as traj)
  task_2/                       # artifacts for task 2
```

## Environment variables

| Variable | Default | Notes |
|---|---|---|
| `OPENROUTER_API_KEY` | required | API key |
| `INFERENCE_BASE_URL` | `https://openrouter.ai/api/v1` | Any OpenAI-compatible endpoint |
| `MODEL_NAME` | `anthropic/claude-opus-4` | Model to use |

## Important rules for editing this codebase

- **Do not add dependencies** without a strong reason. Current deps: `ureq`, `serde`, `serde_json` only.
- **Keep `src/main.rs` as the single source file.** No modules, no lib.rs.
- **The agent rewrites its own source** — any change you make will be in scope for the agent to further modify.
- **Test compile before any structural change**: `cargo build --release`
- **System prompt uses `concat!`** not raw strings — avoids `r###"..."###` delimiter collisions when the LLM rewrites the file containing the prompt.

## Common tasks

### Reset trajectories
```bash
rm -rf .evo/ outputs/
```

### Re-run reflection on already-processed sessions
```bash
rm .evo/learned_until.txt
./target/release/auto-harness evolve
```

### Inspect session trajectories
```bash
ls .evo/sessions/
cat .evo/sessions/<ts>/traj.jsonl | jq .
```

### Use a local model (Ollama)
```env
OPENROUTER_API_KEY=unused
INFERENCE_BASE_URL=http://localhost:11434/v1
MODEL_NAME=llama3
```
