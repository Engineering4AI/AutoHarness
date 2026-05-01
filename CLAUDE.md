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
| `MAX_ITERS` | `10` | Max evolution iterations per `evolve` run |
| `PATIENCE` | `3` | Stop early if no improvement for N consecutive iters |
| `WATERMARK_PATH` | `.evo/learned_until.txt` | Timestamp of last reflected session |

## Operation

### Interactive REPL (always-on)

Async stdin queue (background thread → `VecDeque`). LLM replies printed; all events go to traj. Runs until `/exit`, Ctrl+C, or EOF.

Slash commands:
- `/exit` — clean shutdown
- `/evolve` — run evolution loop, then re-exec the updated binary (same process slot)

Task grouping: the LLM judges each new message as `NEW` or `CONTINUE`. Each task gets its own output directory `outputs/<ts>/task_N`.

### `/evolve` — reflection + code evolution

1. **Reflect**: reads session trajs newer than the watermark → asks LLM for one concrete improvement suggestion → advances watermark.
2. **Evolve**: up to `MAX_ITERS` iterations. Each iter shows the LLM the full prompt files, `src/AGENTS.md`, `src/memory/*.md`, and `src/main.rs` → LLM proposes one change → verified. Stops on `SKIP` or `PATIENCE` consecutive non-improving iters.
3. **Lint + test**: `cargo clippy --no-deps -- -D warnings` then `cargo test --release`; results logged; failures print WARNING to stderr.
4. **Doc update**: LLM rewrites `CLAUDE.md` and `README.md` via `write_file` — docs reflect the tested, working state.
5. **Relaunch**: `exec()` replaces the current process with the freshly-built binary.

## Evolvable artifacts

| Artifact | Tool | Notes |
|---|---|---|
| `src/main.rs` | `write_self` | Atomic: backup → write → `cargo build` → restore on failure |
| `src/AGENTS.md` | `write_file` | Backed up before overwrite |
| `src/prompts/*.txt` | `write_file` | Backed up before overwrite |
| `src/memory/*.md` | `write_file` | Reference notes; created freely |
| `CLAUDE.md` | `write_file` | Doc update step |
| `README.md` | `write_file` | Doc update step |

### Evolution file permissions (enforced in `run_tool`)

In evolve mode:
- `write_file`: allowed only to `src/memory/`, `src/prompts/`, `src/AGENTS.md`, `CLAUDE.md`, `README.md`
- `delete_file`: allowed only within `src/`; `src/main.rs` and `src/AGENTS.md` are protected; auto-backs-up before deletion
- `write_self`: `src/main.rs` only (always build-verified)
- New files may be created under `src/memory/` and `src/prompts/` freely
- All existing `src/` files are backed up with `<stem>.<ts>.<ext>.bak` before modification

## Tool protocol

LLM emits plain-text tags parsed by `run_tool()`:

```
<tool name="bash">command here</tool>
<tool name="read_file">path/to/file</tool>
<tool name="write_file">path/to/file
...full file content...</tool>
<tool name="write_self">...full file content...</tool>
<tool name="delete_file">src/path/to/file</tool>
<tool name="spawn_agent">output.md
task description</tool>
<tool name="wait_agent">agent_<ts></tool>
```

One tool per LLM turn. Results fed back as `<tool_result>...</tool_result>`. Up to 8 turns per iteration.

### write_self safety (atomic write-and-verify)

1. Reject if content is empty
2. Back up `src/main.rs` to `src/main.<ts>.rs.bak`
3. Write new content
4. `cargo build --release`
5. Fail → restore backup, report compiler error to LLM for retry
6. Pass → keep new file

## Progressive disclosure

| Call site | Limit | Mechanism |
|---|---|---|
| Reflection traj | 8 000 chars | Strip `content`/`preview` fields; cap strings at 120 chars; take last N lines |
| Task-grouping judge | 6 messages | Sliding window |
| Chat history | 20 messages | `drain(..len-20)` after each push |
| bash output | 2 000 chars | `.chars().take(2000)` |
| Build error | 400 chars | Substring on compiler stderr |
| Evolve iter | full `src/main.rs` + prompts + `src/AGENTS.md` + `src/memory/*.md` | LLM must see whole files to propose a change |
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
