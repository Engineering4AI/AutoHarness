# evo-agent

A self-evolving coding agent in Rust — the smallest possible implementation that actually works.

The agent reads its own source code, asks an LLM to improve it, executes the proposed changes, scores the result, and repeats. Over time it tries to make itself shorter, more correct, and more capable.

## How it works

```
┌─────────────────────────────────────────────────────────┐
│                      agent_loop                         │
│                                                         │
│  for each iteration:                                    │
│    1. read src/main.rs                                  │
│    2. score it  (1000/lines + compile_bonus)            │
│    3. send source + score to LLM                        │
│    4. LLM replies with a <tool> call                    │
│       ├── <tool name="shell">...</tool>   → run shell   │
│       └── <tool name="write_self">...</tool> → overwrite│
│    5. feed tool result back to LLM (multi-turn)         │
│    6. rescore, persist to .evo/history.json             │
└─────────────────────────────────────────────────────────┘
```

### Scoring

```
score = 1000 / line_count          (fewer lines = higher score)
      + 1.0   if cargo build passes (compile bonus)
```

The LLM is incentivised to compress the source while keeping it compiling.

### Tool dispatch

The LLM emits plain-text XML-like tags — no framework, no function-calling schema:

```
<tool name="shell">cargo test 2>&1</tool>
<tool name="write_self">...full new src/main.rs...</tool>
```

The agent parses these with string search, executes the action, and feeds the result back as the next user message. The LLM must always compile-check before rewriting.

### History

Each iteration's score is saved to `.evo/history.json`. On restart the agent resumes where it left off (iteration counter continues from the last saved entry).

## Quick start

### 1. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 2. Configure API key

Create a `.env` file in the project root:

```env
# OpenRouter (recommended — supports many models)
OPENROUTER_API_KEY=sk-or-...

# Optional overrides
INFERENCE_BASE_URL=https://openrouter.ai/api/v1   # default
MODEL_NAME=anthropic/claude-opus-4                 # default
```

Or export directly:

```bash
export OPENROUTER_API_KEY=sk-or-...
```

You can also use Anthropic directly:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
export INFERENCE_BASE_URL=https://api.anthropic.com/v1
export MODEL_NAME=claude-opus-4-5
```

Or any OpenAI-compatible endpoint (Ollama, vLLM, Together, etc.):

```bash
export OPENROUTER_API_KEY=anything   # or ANTHROPIC_API_KEY
export INFERENCE_BASE_URL=http://localhost:11434/v1
export MODEL_NAME=llama3
```

### 3. Build and run

```bash
cargo build --release
./target/release/evo-agent          # run the agent loop (10 iterations)
./target/release/evo-agent eval     # print current score and exit
```

## File layout

```
.
├── Cargo.toml              # ureq + serde + serde_json
├── src/
│   └── main.rs             # the entire agent (~260 lines)
├── .env                    # API keys (not committed)
└── .evo/
    └── history.json        # iteration scores (auto-created)
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `OPENROUTER_API_KEY` | — | OpenRouter key (checked first) |
| `ANTHROPIC_API_KEY` | — | Anthropic key (fallback) |
| `INFERENCE_BASE_URL` | `https://openrouter.ai/api/v1` | Any OpenAI-compat base URL |
| `MODEL_NAME` | `anthropic/claude-opus-4` | Model identifier |

`MAX_ITERS` (default `10`) is a compile-time constant in `src/main.rs`.

## What happens on each run

```
[evo-agent] starting iteration 1 / 10
[iter 1] I'll first check the current build status...
  → shell: exit=0
    Compiling evo-agent v0.1.0
[iter 1] Now I'll refactor the tool parser to save 8 lines...
  → write_self: written
[evo] iter=1 score 3.843->4.102
[evo-agent] done. 1 iterations total.
```

If the score drops after a rewrite, the agent warns you — you can manually `git checkout src/main.rs` to revert.
