use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const SELF_PATH: &str = "src/main.rs";
const MAX_ITERS: usize = 10;
const PATIENCE: usize = 3;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct Msg {
    role: String,
    content: Value,
}

// ── Trajectory / output dirs ──────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn traj_log(path: &str, kind: &str, data: Value) {
    let line = format!("{}\n", json!({ "ts": now_secs(), "kind": kind, "data": data }));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        f.write_all(line.as_bytes()).ok();
    }
}

// Returns (session_ts, traj_path, task_out_dir)
// Both traj and outputs share the same timestamp root so they're trivially linked.
fn session_dirs() -> (String, String, String) {
    let ts = now_secs().to_string();
    let traj_dir = format!(".evo/sessions/{ts}");
    let out_dir  = format!("outputs/{ts}/task_1");
    fs::create_dir_all(&traj_dir).ok();
    fs::create_dir_all(&out_dir).ok();
    (ts, format!("{traj_dir}/traj.jsonl"), out_dir)
}

// Advance to the next task directory under the same session timestamp.
fn next_task_dir(session_ts: &str, task_n: usize) -> String {
    let dir = format!("outputs/{session_ts}/task_{task_n}");
    fs::create_dir_all(&dir).ok();
    dir
}

// ── Shell / Build ─────────────────────────────────────────────────────────────

fn shell(cmd: &str) -> (i32, String) {
    let out = Command::new("sh").args(["-c", cmd]).output().expect("sh failed");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text.trim().to_string())
}

fn build() -> Result<(), String> {
    let (code, out) = shell("cargo build --release 2>&1");
    if code == 0 { Ok(()) } else { Err(out) }
}

// ── LLM ───────────────────────────────────────────────────────────────────────

struct Cfg {
    api_key: String,
    base_url: String,
    model: String,
}

impl Cfg {
    fn from_env() -> Self {
        let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
            eprintln!("Set OPENROUTER_API_KEY"); std::process::exit(1);
        });
        Self {
            api_key,
            base_url: env::var("INFERENCE_BASE_URL")
                .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string()),
            model: env::var("MODEL_NAME")
                .unwrap_or_else(|_| "anthropic/claude-opus-4".to_string()),
        }
    }
}

fn llm(cfg: &Cfg, messages: &[Msg], system: &str) -> Result<String, String> {
    let mut msgs = vec![Msg { role: "system".to_string(), content: json!(system) }];
    msgs.extend_from_slice(messages);
    let body = json!({ "model": cfg.model, "max_tokens": 4096, "messages": msgs });
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", cfg.api_key))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| e.to_string())?;
    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("bad response: {v}"))
}

// Ask LLM whether `next_input` is a continuation of the current task or a new one.
// Returns true if it is a new task.
fn is_new_task(cfg: &Cfg, history: &[Msg], next_input: &str) -> bool {
    let system = "You decide if a new user message starts a NEW task or continues the current one.\n\
                  Reply with exactly one word: NEW or CONTINUE.";
    let mut msgs = history.to_vec();
    msgs.push(Msg { role: "user".to_string(), content: json!(next_input) });
    match llm(cfg, &msgs, system) {
        Ok(r) => r.trim().to_uppercase().starts_with("NEW"),
        Err(_) => false,
    }
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

// `write_bak`: whether to update src/main.rs.bak on success (only in evolve mode).
fn run_tool(text: &str, out_dir: &str, write_bak: bool) -> Option<(String, String)> {
    let start = text.find("<tool ")?;
    let end = text[start..].find("</tool>")? + start;
    let tag = &text[start..end + 7];
    let name_start = tag.find("name=\"")? + 6;
    let name_end = tag[name_start..].find('"')? + name_start;
    let name = &tag[name_start..name_end];
    let content_start = tag.find('>')? + 1;
    let content_end = tag.rfind("</tool>").unwrap_or(tag.len());
    let content = tag[content_start..content_end].trim();

    match name {
        "shell" => {
            let (code, out) = shell(content);
            Some(("shell".to_string(), format!("exit={code}\n{out}")))
        }
        "write_self" => {
            let code = {
                let s = content.trim();
                let s = s.strip_prefix("```rust").or_else(|| s.strip_prefix("```")).unwrap_or(s);
                s.strip_suffix("```").unwrap_or(s).trim()
            };
            if code.is_empty() {
                return Some(("write_self".to_string(), "REJECTED (empty content)".to_string()));
            }
            fs::write(format!("{out_dir}/main.rs"), code).ok();
            let backup = fs::read_to_string(SELF_PATH).unwrap_or_default();
            fs::write(SELF_PATH, code).ok()?;
            match build() {
                Ok(_) => {
                    if write_bak {
                        fs::write(format!("{SELF_PATH}.bak"), &backup).ok();
                    }
                    Some(("write_self".to_string(), "written and verified OK".to_string()))
                }
                Err(e) => {
                    fs::write(SELF_PATH, &backup).ok();
                    fs::write(format!("{out_dir}/build_error.txt"), &e).ok();
                    Some(("write_self".to_string(),
                        format!("REJECTED (build failed, reverted):\n{}", e.chars().take(400).collect::<String>())))
                }
            }
        }
        "write_file" => {
            let (path, body) = content.split_once('\n').unwrap_or((content, ""));
            let path = path.trim();
            if path.is_empty() {
                return Some(("write_file".to_string(), "REJECTED (no path)".to_string()));
            }
            if let Some(parent) = std::path::Path::new(path).parent() {
                fs::create_dir_all(parent).ok();
            }
            match fs::write(path, body) {
                Ok(_)  => Some(("write_file".to_string(), format!("written: {path}"))),
                Err(e) => Some(("write_file".to_string(), format!("error: {e}"))),
            }
        }
        _ => None,
    }
}

// ── Mode 1: Interactive CLI ───────────────────────────────────────────────────

const CHAT_SYSTEM: &str = concat!(
    "You are a helpful coding assistant. The user gives you tasks.\n",
    "Tools (emit exactly one per turn):\n",
    "  <tool name=\"shell\">command here</tool>\n",
    "  <tool name=\"write_self\">...FULL new src/main.rs...</tool>\n",
    "Rules:\n",
    "- write_self auto-verifies build; on failure old file is restored.\n",
    "- Always emit COMPLETE file in write_self — never truncate.\n",
    "- In write_self emit RAW Rust only — no ```rust fences.\n",
    "- After each action emit a SUMMARY: line."
);

type Queue = Arc<Mutex<VecDeque<String>>>;
const EOF_SENTINEL: &str = "\x00";

fn spawn_reader(queue: Queue, traj: String) {
    thread::spawn(move || {
        let stdin = io::stdin();
        print!("> ");
        io::stdout().flush().ok();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    let l = l.trim().to_string();
                    if !l.is_empty() {
                        traj_log(&traj, "user_input", json!(l));
                        queue.lock().unwrap().push_back(l);
                    }
                    print!("> ");
                    io::stdout().flush().ok();
                }
                Err(_) => break,
            }
        }
        queue.lock().unwrap().push_back(EOF_SENTINEL.to_string());
    });
}

fn chat_mode(cfg: &Cfg, session_ts: &str, traj: &str) {
    let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
    spawn_reader(queue.clone(), traj.to_string());
    traj_log(traj, "session_start", json!({}));

    let mut messages: Vec<Msg> = Vec::new();
    let mut task_n = 1usize;
    let mut out_dir = format!("outputs/{session_ts}/task_{task_n}");
    fs::create_dir_all(&out_dir).ok();

    loop {
        // Spin until input arrives
        let input = loop {
            if let Some(i) = queue.lock().unwrap().pop_front() { break i; }
            thread::sleep(std::time::Duration::from_millis(50));
        };
        if input == EOF_SENTINEL {
            traj_log(traj, "session_end", json!({ "turns": messages.len() }));
            break;
        }

        // If there is prior context, ask LLM whether this starts a new task
        if !messages.is_empty() && is_new_task(cfg, &messages, &input) {
            task_n += 1;
            out_dir = next_task_dir(session_ts, task_n);
            traj_log(traj, "task_boundary", json!({ "task": task_n }));
        }

        messages.push(Msg { role: "user".to_string(), content: json!(&input) });

        let mut turns = 0usize;
        loop {
            turns += 1;
            if turns > 8 { break; }

            let reply = match llm(cfg, &messages, CHAT_SYSTEM) {
                Ok(r) => r,
                Err(e) => { traj_log(traj, "llm_error", json!(e)); break; }
            };
            traj_log(traj, "llm_response", json!({ "task": task_n, "turn": turns, "content": reply }));

            println!("{reply}");
            print!("> ");
            io::stdout().flush().ok();

            messages.push(Msg { role: "assistant".to_string(), content: json!(&reply) });

            if let Some((tool, result)) = run_tool(&reply, &out_dir, false) {
                traj_log(traj, "tool_result", json!({ "task": task_n, "tool": tool, "result": result }));
                messages.push(Msg {
                    role: "user".to_string(),
                    content: json!(format!("<tool_result>{result}</tool_result>")),
                });
                if tool == "write_self" && result.contains("verified OK") { break; }
            } else {
                break;
            }
        }
    }
}

// ── Mode 2: Evolve ────────────────────────────────────────────────────────────

const WATERMARK_PATH: &str = ".evo/learned_until.txt";

const EVOLVE_SYSTEM: &str = concat!(
    "You are a self-evolving Rust coding agent. You rewrite src/main.rs only when the change is clearly worth it.\n\n",
    "Simplicity criterion (from autoresearch):\n",
    "- A small improvement that adds complexity is NOT worth it.\n",
    "- Deleting code while keeping or improving behavior IS worth it.\n",
    "- Equal result with simpler code? Keep the simpler version.\n",
    "- If the improvement is marginal and the diff is large, reply SKIP.\n\n",
    "Tools (emit exactly one per turn):\n",
    "  <tool name=\"shell\">cargo test 2>&1</tool>\n",
    "  <tool name=\"write_self\">...FULL new src/main.rs content...</tool>\n\n",
    "Rules:\n",
    "- write_self auto-verifies the build; on failure the old file is restored and you get the error.\n",
    "- Always emit the COMPLETE file in write_self — never truncate.\n",
    "- In write_self, emit RAW Rust source only — NO ```rust fences, NO markdown.\n",
    "- After each action emit a SUMMARY: line.\n",
    "- If no improvement clears the simplicity bar this iteration, reply with exactly: SKIP"
);

fn read_watermark() -> u64 {
    fs::read_to_string(WATERMARK_PATH).ok()
        .and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

fn write_watermark() {
    fs::create_dir_all(".evo").ok();
    fs::write(WATERMARK_PATH, now_secs().to_string()).ok();
}

fn collect_unprocessed_trajs() -> Vec<String> {
    let watermark = read_watermark();
    let mut lines = Vec::new();
    let Ok(entries) = fs::read_dir(".evo/sessions") else { return lines; };
    for entry in entries.flatten() {
        let ts: u64 = entry.file_name().to_string_lossy().parse().unwrap_or(0);
        if ts <= watermark { continue; }
        if let Ok(content) = fs::read_to_string(entry.path().join("traj.jsonl")) {
            lines.extend(content.lines().map(|l| l.to_string()));
        }
    }
    lines
}

fn evolve_mode(cfg: &Cfg, traj: &str, out_dir: &str) {
    // Step 1: reflect on unprocessed trajectories → maybe update evolve system prompt
    let traj_lines = collect_unprocessed_trajs();
    if !traj_lines.is_empty() {
        let sample = traj_lines.iter().rev().take(300).cloned().collect::<Vec<_>>().join("\n");
        let reflect_prompt = format!(
            "Below are trajectory events from a self-evolving Rust coding assistant.\n\
             Identify ONE concrete improvement to its behavior.\n\
             Respond with exactly: NONE — or a plain-text suggestion.\n\n\
             Trajectories:\n{sample}"
        );
        let msgs = vec![Msg { role: "user".to_string(), content: json!(reflect_prompt) }];
        match llm(cfg, &msgs, "You are a concise agent improvement advisor.") {
            Ok(reply) => traj_log(traj, "reflect_result", json!(reply.chars().take(200).collect::<String>())),
            Err(e)    => traj_log(traj, "reflect_error", json!(e)),
        }
        write_watermark();
    }

    // Step 2: code evolution loop
    traj_log(traj, "evolve_start", json!({}));
    let mut no_improve = 0usize;

    'outer: for iter in 1..=MAX_ITERS {
        let src = fs::read_to_string(SELF_PATH).unwrap_or_default();
        let prompt = format!(
            "Iteration {iter}/{MAX_ITERS}.\n\nCurrent src/main.rs:\n```rust\n{src}\n```\n\nPropose one improvement.",
        );
        traj_log(traj, "iter_start", json!({ "iter": iter }));

        let mut messages = vec![Msg { role: "user".to_string(), content: json!(prompt) }];
        let mut turns = 0usize;
        loop {
            turns += 1;
            if turns > 8 { break; }
            let reply = match llm(cfg, &messages, EVOLVE_SYSTEM) {
                Ok(r) => r,
                Err(e) => { traj_log(traj, "llm_error", json!(e)); break; }
            };
            traj_log(traj, "llm_response", json!({ "turn": turns, "preview": reply.chars().take(200).collect::<String>() }));
            if reply.trim().to_uppercase().starts_with("SKIP") {
                traj_log(traj, "iter_skip", json!({ "iter": iter, "reason": "LLM chose not to evolve" }));
                break 'outer;
            }
            messages.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
            if let Some((tool, result)) = run_tool(&reply, out_dir, true) {
                traj_log(traj, "tool_result", json!({ "tool": tool, "result": result.chars().take(200).collect::<String>() }));
                messages.push(Msg {
                    role: "user".to_string(),
                    content: json!(format!("<tool_result>{result}</tool_result>")),
                });
                if tool == "write_self" && result.contains("verified OK") { break; }
            } else {
                break;
            }
        }

        let improved = messages.iter().any(|m| {
            m.content.as_str().map(|s| s.contains("verified OK")).unwrap_or(false)
        });
        traj_log(traj, "iter_end", json!({ "iter": iter, "improved": improved }));

        if !improved { no_improve += 1; } else { no_improve = 0; }
        if no_improve >= PATIENCE {
            traj_log(traj, "patience_exhausted", json!(null));
            break 'outer;
        }
    }
    // Step 3: update CLAUDE.md and README.md to reflect current implementation
    let src = fs::read_to_string(SELF_PATH).unwrap_or_default();
    let claude_md = fs::read_to_string("CLAUDE.md").unwrap_or_default();
    let readme_md = fs::read_to_string("README.md").unwrap_or_default();
    let doc_prompt = format!(
        "The evolution loop just finished. Below is the current src/main.rs.\n\
         Update CLAUDE.md and README.md to accurately reflect the current implementation.\n\
         Emit two tool calls (one per file) using:\n\
         <tool name=\"write_file\">CLAUDE.md\n...full content...</tool>\n\
         <tool name=\"write_file\">README.md\n...full content...</tool>\n\
         Preserve structure; only change what is factually wrong or outdated.\n\n\
         ### src/main.rs\n```rust\n{src}\n```\n\n\
         ### CLAUDE.md\n{claude_md}\n\n\
         ### README.md\n{readme_md}"
    );
    let doc_msgs = vec![Msg { role: "user".to_string(), content: json!(doc_prompt) }];
    let doc_system = "You are a precise technical writer. Update documentation to match the code. \
                      Emit write_file tool calls only.";
    let mut remaining = doc_msgs;
    for _ in 0..4 {
        match llm(cfg, &remaining, doc_system) {
            Ok(reply) => {
                traj_log(traj, "doc_update", json!(reply.chars().take(200).collect::<String>()));
                if let Some((tool, result)) = run_tool(&reply, out_dir, false) {
                    traj_log(traj, "tool_result", json!({ "tool": tool, "result": result }));
                    remaining.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
                    remaining.push(Msg { role: "user".to_string(), content: json!(format!("<tool_result>{result}</tool_result>")) });
                } else {
                    break;
                }
            }
            Err(e) => { traj_log(traj, "doc_update_error", json!(e)); break; }
        }
    }

    traj_log(traj, "evolve_end", json!({}));
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn load_env() {
    if let Ok(s) = fs::read_to_string(".env") {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((k, v)) = line.split_once('=') {
                env::set_var(k.trim(), v.trim().trim_matches('"').trim_matches('\''));
            }
        }
    }
}

fn main() {
    load_env();
    let cfg = Cfg::from_env();
    let (ts, traj, out_dir) = session_dirs();
    match env::args().nth(1).as_deref() {
        Some("evolve") => evolve_mode(&cfg, &traj, &out_dir),
        None           => chat_mode(&cfg, &ts, &traj),
        Some(cmd)      => { eprintln!("unknown subcommand: {cmd}"); std::process::exit(1); }
    }
}
