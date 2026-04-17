# AutoHarness

A self-evolving coding agent in Rust — the smallest possible implementation that actually works.

<img width="1200" height="800" alt="AutoResearch" src="https://github.com/user-attachments/assets/805635cc-88d4-4f26-9467-07ef8ca99b7b" />

The agent has two modes: an interactive CLI where you give it tasks, and a self-evolution loop where it reads its own source, proposes improvements, verifies them, and repeats. The LLM is the judge — no numeric scoring.

## How it works

```mermaid
flowchart TD
    A[auto-harness] --> B{subcommand?}
    B -->|none| C[chat mode]
    B -->|evolve| D[evolve mode]

    C --> C1[async stdin queue]
    C1 --> C2[LLM judge: NEW task or CONTINUE?]
    C2 --> C3[send to LLM → print reply]
    C3 --> C4[run tool if present]
    C4 --> C1

    D --> D1[reflect on unprocessed trajs]
    D1 --> D2[evolution loop\nup to MAX_ITERS]
    D2 --> D3{LLM reply}
    D3 -->|SKIP| D5[exit loop]
    D3 -->|write_self| D4[backup → write → cargo build]
    D4 -->|fail| D6[restore + report error to LLM]
    D6 --> D2
    D4 -->|pass| D8{improved?}
    D3 -->|write_file| D9[write prompts / AGENTS.md]
    D9 --> D8
    D8 -->|yes: reset streak| D2
    D8 -->|no: streak++| D10{streak ≥ PATIENCE?}
    D10 -->|no| D2
    D10 -->|yes| D5
    D5 --> D7[doc update: CLAUDE.md + README.md]
    D7 --> D11[cargo clippy -D warnings]
    D11 --> D12[cargo test --release]
    D12 --> D13[log lint_result + test_result to traj]
```

### Chat mode

Interactive REPL. Stdin is read in a background thread and pushed to a queue so you can keep typing while the LLM is processing. Each reply is printed; everything else is logged to `.evo/sessions/<ts>/traj.jsonl`.

The LLM automatically groups your messages into tasks — if a new message starts a different topic, artifacts go into a new `outputs/<ts>/task_N` directory.

### Evolve mode

1. **Reflect** — reads chat session trajs newer than the last watermark, asks the LLM for one concrete improvement suggestion, logs it.
2. **Evolve** — up to `MAX_ITERS` iterations. Each iteration: show LLM current `src/main.rs` and `src/AGENTS.md` → propose one change → verify with `cargo build`. Stops on `SKIP` or `PATIENCE` consecutive non-improving iters.
3. **Doc update** — after the loop, the LLM rewrites `CLAUDE.md` and `README.md` to match the current implementation.
4. **Lint + test** — `cargo clippy -- -D warnings` then `cargo test --release`; results logged to traj; failures print a WARNING to stderr.

### What the agent can evolve

Beyond just its own source code, the agent can improve all of these via `write_file`:

| Artifact | Purpose |
|---|---|
| `src/main.rs` | Core agent logic (atomic rewrite with build verification) |
| `src/AGENTS.md` | Agent orchestration best practices guide |
| `src/prompts/chat_system.txt` | Chat mode persona and rules |
| `src/prompts/reflect_system.txt` | Trajectory analysis instructions |
| `src/prompts/evolve_system.txt` | Evolution loop instructions |
| `src/prompts/doc_system.txt` | Doc update instructions |

### Tool dispatch

The LLM emits plain-text XML-like tags — no framework, no function-calling schema:

```
<tool name="shell">cargo test 2>&1</tool>
<tool name="write_self">...full new src/main.rs...</tool>
<tool name="write_file">path/to/file
...full content...</tool>
```

`write_self` is atomic: backup → write → `cargo build --release` → restore on failure, reporting the exact compiler error back to the LLM so it can self-correct.

### Progressive disclosure

Every LLM call site is bounded — no unbounded context growth:

- **Reflection**: traj stripped to metadata-only (no content blobs), capped at 8 000 chars
- **Task judge**: last 6 messages only
- **Chat history**: sliding window of 20 messages
- **Shell output**: capped at 2 000 chars
- **Build errors**: capped at 400 chars

## Installation

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Clone the repository
git clone https://github.com/Engineering4AI/AutoHarness
cd AutoHarness

# Set API key
echo "OPENROUTER_API_KEY=sk-or-..." > .env

# Build and run
cargo build --release
./target/release/auto-harness          # interactive chat
./target/release/auto-harness evolve   # self-evolution loop
```

Any OpenAI-compatible endpoint works (Ollama, vLLM, Together, etc.):

```bash
export OPENROUTER_API_KEY=anything
export INFERENCE_BASE_URL=http://localhost:11434/v1
export MODEL_NAME=llama3
```

## File layout

```
.
├── Cargo.toml
├── src/
│   ├── main.rs               # the entire agent (~420 lines)
│   ├── AGENTS.md             # agent orchestration guide (self-evolving)
│   └── prompts/
│       ├── chat_system.txt   # chat mode system prompt
│       ├── reflect_system.txt
│       ├── evolve_system.txt
│       └── doc_system.txt
├── .env                      # API keys (not committed)
├── .evo/
│   ├── sessions/<ts>/        # one dir per run, contains traj.jsonl
│   └── learned_until.txt     # reflection watermark
└── outputs/<ts>/
    ├── task_1/               # artifacts for task 1
    └── task_2/               # artifacts for task 2 (if new task detected)
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `OPENROUTER_API_KEY` | — | OpenRouter key (required) |
| `INFERENCE_BASE_URL` | `https://openrouter.ai/api/v1` | Any OpenAI-compat base URL |
| `MODEL_NAME` | `anthropic/claude-opus-4` | Model identifier |

`MAX_ITERS` (default `10`) and `PATIENCE` (default `3`) are compile-time constants in `src/main.rs`.

## Citation

If you use AutoHarness in your research, please cite:

```bibtex
@software{autoharness2026,
  title  = {AutoHarness: A Self-Evolving Coding Agent in Rust},
  author = {Zhao, Zhimin},
  year   = {2026},
  url    = {https://github.com/Engineering4AI/AutoHarness}
}
```
