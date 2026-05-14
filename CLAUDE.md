# CLAUDE.md — AutoHarness

## Hard constraints (read first)
- **Do not add dependencies.** Current deps: `ureq`, `serde`, `serde_json` only.
- **Keep `src/main.rs` as the single source file.** No modules, no lib.rs.
- **Test compile before any structural change**: `cargo build --release`
- **System prompts are external files** in `src/prompts/` — loaded at runtime via `load_prompt()`. Do not inline them back into source.
- **The agent rewrites its own source** — any change you make is in scope for the agent to further modify.

## Project overview

Single-binary Rust agent. Always starts as an interactive REPL. Type `/evolve` to run a reflection + code-evolution loop and relaunch the evolved binary. The LLM is the judge — no numeric scoring.

## Build & run

```bash
cargo build --release
./target/release/auto-harness   # interactive REPL; /exit to quit, /evolve to evolve + relaunch
```

## Key constants (src/main.rs)

| Constant | Value | Meaning |
|---|---|---|
| `SELF_PATH` | `src/main.rs` | File the agent reads and rewrites |
| `WATERMARK_PATH` | `.evo/learned_until.txt` | Timestamp of last reflected session |

Note: evolution loop is **unbounded** — it runs until the LLM replies `SKIP` (nothing worth changing).

## Operation

### Interactive REPL (always-on)

Async stdin queue (background thread → `VecDeque`). LLM replies printed; all events go to traj. Runs until `/exit`, Ctrl+C, or EOF.

Slash commands:
- `/exit` — clean shutdown
- `/evolve` — run evolution loop, then re-exec the updated binary (same process slot)

Task grouping: the LLM judges each new message as `NEW` or `CONTINUE`. Each task gets its own output directory `outputs/<ts>/task_N`. After the assistant stops using tools, the harness runs a completion check; if more work is needed, it appends a continuation prompt for the same task. Check errors are logged and return control to the REPL.

### `/evolve` — reflection + code evolution

1. **Reflect**: reads session trajs newer than the watermark (progressive disclosure: stripped summary first; LLM may call `read_file` with a char range for full detail) → one concrete improvement suggestion → advances watermark.
2. **Evolve**: unbounded iterations. Each iter shows the LLM the full prompt files, `src/AGENTS.md`, `src/memory/` index (filepath + description), and `src/main.rs` → LLM proposes one change → verified. Stops when LLM replies `SKIP`.
3. **Refine**: runs clippy + tests, feeds any failures to the LLM for `write_file` fixes.
4. **Final lint + test**: `cargo clippy --no-deps -- -D warnings` then `cargo test --release`; results logged.
5. **Doc update**: LLM rewrites `CLAUDE.md` and `README.md` via `write_file` — docs reflect the verified, working state.
6. **Relaunch**: `exec()` replaces the current process with the freshly-built binary.

## Evolvable artifacts

| Artifact | Tool | Notes |
|---|---|---|
| `src/main.rs` | `write_file` | Atomic: backup → write → `cargo build` → restore on failure |
| Any `src/**` (non-`.bak`) | `write_file` | Covers all `.rs`, `.md`, `.txt` under `src/` |
| `CLAUDE.md` / `README.md` | `write_file` | Doc update step |

### Evolution file permissions (enforced in `run_tool`)

In evolve mode:
- `write_file`: allowed for any `src/**` path (excluding `.bak` files), plus `CLAUDE.md` and `README.md`
- Writing `src/main.rs` triggers `cargo build --release`; failure reverts the file automatically
- `delete_file`: allowed only within `src/`; `src/main.rs` and `src/AGENTS.md` are protected
- All modified files are auto-backed-up as `<stem>.<ts>.<ext>.bak` before modification

## Tool protocol

LLM emits plain-text tags parsed by `run_tool()`:

```
<tool name="bash">command here</tool>
<tool name="read_file">path/to/file</tool>
<tool name="read_file">path/to/file start..end</tool>
<tool name="write_file">path/to/file
...full file content...</tool>
<tool name="write_file">path/to/file start..end
...replacement for chars start..end...</tool>
<tool name="spawn_agent">output.md
task description</tool>
<tool name="wait_agent">agent_<ts></tool>
```

One tool per LLM turn. Results fed back as `<tool_result>...</tool_result>`. Loops are unbounded — exit on task/evolution completion only.

### read_file / write_file char ranges

Both tools accept an optional `start..end` char-offset range on the first line:
- `read_file path 1000..5000` — returns chars 1000–5000; appends a hint for the next window if more content follows
- `write_file path 500..800\ncontent` — splices chars 500–800 with `content`; rest of file unchanged
- Out-of-bounds offsets are clamped to file length; inverted ranges (`end < start`) produce an empty slice

### write_file safety for src/main.rs (atomic write-and-verify)

1. Reject if resulting content is empty
2. Back up `src/main.rs` to `src/main.<ts>.rs.bak`
3. Write new content (full overwrite or range patch)
4. `cargo build --release`
5. Fail → restore backup, report compiler error to LLM for retry
6. Pass → keep new file

## Progressive disclosure

| Call site | Limit | Mechanism |
|---|---|---|
| Reflection traj | 8 000 chars | Strip `content`/`preview` fields; cap strings at 120 chars; LLM may `read_file path start..end` for more |
| Task-grouping judge | 6 messages | Sliding window |
| Completion judge | 12 messages | Sliding window plus original user prompt |
| Chat history | 20 messages | `drain(..len-20)` after each push |
| bash output | 2 000 chars | `.chars().take(2000)` |
| Build error | 400 chars | Substring on compiler stderr |
| `read_file` default window | 16 000 chars | LLM sees hint to continue reading with next range |
| Evolve iter | full `src/main.rs` + prompts + `src/AGENTS.md` + `src/memory/` index (filepath + desc) | LLM must see whole files to propose a change |
| Doc update | full `src/main.rs` + `CLAUDE.md` + `README.md` | One-shot, acceptable |

## Trajectory logging

Every run creates `.evo/sessions/<unix_timestamp>/traj.jsonl`:

```json
{"ts": 1713300000, "kind": "session_start",  "data": {}}
{"ts": 1713300001, "kind": "user_input",      "data": "fix the bug"}
{"ts": 1713300005, "kind": "llm_response",    "data": {"task": 1, "turn": 1, "preview": "..."}}
{"ts": 1713300008, "kind": "task_boundary",   "data": {"task": 2}}
{"ts": 1713300010, "kind": "tool_result",     "data": {"tool": "write_self", "result": "written and verified OK"}}
{"ts": 1713300011, "kind": "session_end",     "data": {"turns": 4}}
{"ts": 1713300020, "kind": "iter_start",      "data": {"iter": 1}}
{"ts": 1713300025, "kind": "iter_end",        "data": {"iter": 1, "improved": true}}
{"ts": 1713300026, "kind": "iter_skip",       "data": {"iter": 2, "reason": "LLM chose not to evolve"}}
{"ts": 1713300027, "kind": "evolve_end",      "data": {}}
```

## Output layout

```
.evo/
  sessions/<ts>/traj.jsonl      # event log
  learned_until.txt             # reflection watermark
  memos/<evolve_ts>.md          # per-iter changelog for the current evolution run
outputs/<ts>/
  task_1/                       # artifacts for task 1
  task_2/                       # artifacts for task 2
src/
  main.rs                       # agent source (self-rewriting)
  AGENTS.md                     # agent orchestration guide (self-evolving, protected)
  memory/                       # reference notes (created/updated by evolution)
    *.md
  prompts/
    chat_system.txt             # chat mode system prompt
    reflect_system.txt          # reflection system prompt
    evolve_system.txt           # evolution system prompt
    doc_system.txt              # doc update system prompt
```

## Environment variables

| Variable | Default | Notes |
|---|---|---|
| `OPENROUTER_API_KEY` | required | API key |
| `INFERENCE_BASE_URL` | `https://openrouter.ai/api/v1` | Any OpenAI-compatible endpoint |
| `MODEL_NAME` | `anthropic/claude-opus-4` | Model to use |

## Common tasks

```bash
# Reset trajectories
rm -rf .evo/ outputs/

# Re-run reflection on already-processed sessions
rm .evo/learned_until.txt
# then type /evolve inside the running harness

# Inspect trajectories
cat .evo/sessions/<ts>/traj.jsonl | jq .

# Use a local model (Ollama)
OPENROUTER_API_KEY=unused INFERENCE_BASE_URL=http://localhost:11434/v1 MODEL_NAME=llama3 ./target/release/auto-harness
```
