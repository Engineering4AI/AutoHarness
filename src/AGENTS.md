# Agent Orchestration Patterns

## When to spawn a sub-agent
- Task is independently scoped with a clear completion criterion
- Isolation is needed: parallel work, large context, or risky/destructive operation
- Two or more sub-tasks share no data dependency → fan-out immediately
- Task requires a different "role" (planner vs executor vs reviewer)

## Context handoff discipline (the sub-agent starts fresh — act accordingly)
- Prompt must be fully self-contained: goal, relevant file paths, key snippets, constraints
- Never reference "the work above" or "what we discussed" — sub-agent has no parent memory
- State the expected output explicitly: file path, format, and success criterion
- Include hard constraints the sub-agent must not violate (e.g. no new deps, keep LOC low)

## Parent behavior while sub-agent runs
- Independent sub-tasks: launch ALL in parallel (single message, multiple tool calls)
- Dependent steps: wait for result before issuing the next call — no polling, no sleeping
- Never use sleep to wait for a sub-agent; use foreground blocking or background notification
- If a sub-agent errors, the parent re-briefs it with the error rather than fixing it inline

## Team patterns

### Fan-out (parallel, independent)
Parent decomposes work → N agents each own one slice → parent merges outputs.
Use when: N files to analyse, N endpoints to implement, N tests to write — no ordering needed.

### Chain (sequential, dependent)
Agent A produces output → Agent B consumes it → … → final result.
Use when: each step needs the previous result (plan → implement → review).

### Planner + Executor
Planner reads code, produces a step-by-step spec (write to file). Executor reads spec, implements.
Planner never writes code. Executor never makes design decisions.
Use when: task is large enough that design and implementation are separable concerns.

### Executor + Reviewer
Executor produces a diff or file. Reviewer reads it and produces a severity-rated critique.
Executor applies critiques in a second pass.
Use when: correctness or security matters more than speed.

### Parallel Specialists
Each agent is expert in one domain (security, performance, correctness). All run in parallel on same artifact. Parent synthesises reports.
Use when: multi-dimensional review is needed and dimensions are independent.

## Output contract (survives context compaction)
- Every sub-agent MUST write results to an absolute-path file before returning
- Terminal summary is optional and secondary — the file is the contract
- Parent reads the file, not the summary, to continue work
- File naming: `outputs/<session_ts>/task_<N>/<artifact>` or an absolute path the parent specifies

## Verification discipline
- "Done" means behavior passes a concrete verification command, not "code looks correct"
- Each task must have an associated verification command stated upfront
- Sub-agent must run verification and include the result in its output file
- If verification fails, sub-agent reports failure with exact error — parent decides next step

## Scope discipline (WIP = 1)
- One active task per agent at a time; finishing and verifying beats starting three
- If agent discovers a second problem while fixing the first, log it — don't fix it inline
- Atomic commits: one logical change per commit, all tests passing before committing

## Session continuity
- Before context fills: write PROGRESS.md (done / in-progress / blocked / next step)
- Design decisions go in DECISIONS.md with rationale — not in comments, not in summaries
- A fresh session must be able to answer "what is this?", "how do I run it?", "what's next?" from repo alone
- Rebuild cost target: < 3 minutes from cold start to executable state

## Anti-patterns to avoid
- Instruction bloat: one 600-line file that tries to contain everything → use routing files
- Critical rules buried in the middle of a long prompt → put them at top or bottom
- "Mostly done" progress updates → always include a concrete next executable step
- Accepting "code looks fine" as evidence → only passing verification counts
- Mixing initialization and implementation → dedicate a phase to each
- Deferring cleanup → entropy compounds; clean state at every session end
