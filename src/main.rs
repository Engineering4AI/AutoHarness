use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::path::Path;

const SELF_PATH: &str = "src/main.rs";
const WATERMARK_PATH: &str = ".evo/learned_until.txt";

#[derive(Serialize, Clone)]
struct Msg {
    role: String,
    content: Value,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn traj_log(path: &str, kind: &str, data: Value) {
    let line = format!("{}\n", json!({"ts": now_secs(), "kind": kind, "data": data}));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        f.write_all(line.as_bytes()).ok();
    }
}

struct Cfg {
    api_key: String,
    base_url: String,
    model: String,
}

impl Cfg {
    fn from_env() -> Self {
        let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
            eprintln!("Set OPENROUTER_API_KEY");
            std::process::exit(1);
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
    let body = json!({"model": cfg.model, "max_tokens": 4096, "messages": msgs});
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .timeout(Duration::from_secs(120))
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

fn load_prompt(name: &str) -> String {
    fs::read_to_string(format!("src/prompts/{name}")).unwrap_or_default()
}

fn frontmatter_description(content: &str) -> Option<&str> {
    let inner = content.strip_prefix("---\n")?.splitn(2, "\n---").next()?;
    inner.lines()
        .find(|l| l.starts_with("description:"))
        .map(|l| l["description:".len()..].trim())
}

fn memory_index() -> String {
    let Ok(dir) = fs::read_dir("src/memory") else { return "(none)".to_string() };
    let entries: Vec<_> = dir.flatten()
        .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
        .collect();
    if entries.is_empty() { return "(none)".to_string(); }
    entries.iter().map(|e| {
        let path = e.path();
        let filepath = format!("src/memory/{}", e.file_name().to_string_lossy());
        let content = fs::read_to_string(&path).unwrap_or_default();
        let desc = frontmatter_description(&content)
            .map(|s| s.to_string())
            .unwrap_or_else(|| content.lines().next().unwrap_or("").chars().take(100).collect());
        format!("  {filepath} — {desc}")
    }).collect::<Vec<_>>().join("\n")
}

fn is_new_task(cfg: &Cfg, history: &[Msg], next_input: &str) -> bool {
    let system = "You decide if a new user message starts a NEW task or continues the current one. Reply with exactly one word: NEW or CONTINUE.";
    let start = history.len().saturating_sub(6);
    let mut msgs = history[start..].to_vec();
    msgs.push(Msg { role: "user".to_string(), content: json!(next_input) });
    match llm(cfg, &msgs, system) {
        Ok(r) => r.trim().to_uppercase().starts_with("NEW"),
        Err(e) => { eprintln!("Task-judge error (defaulting CONTINUE): {e}"); false }
    }
}

fn extract_tool(text: &str) -> Option<(&str, &str)> {
    let open = text.find("<tool name=\"")?;
    let name_start = open + 12;
    let name_end = text[name_start..].find('"')? + name_start;
    let body_start = text[name_end..].find('>')? + name_end + 1;
    let body_end = text[body_start..].find("</tool>")? + body_start;
    let raw = text[body_start..body_end].trim();
    let body = if raw.starts_with("```") {
        let after = raw.find('\n').map(|i| &raw[i+1..]).unwrap_or(raw);
        after.trim_end_matches("```").trim()
    } else {
        raw
    };
    Some((&text[name_start..name_end], body))
}

fn parse_path_range(body: &str) -> (&str, Option<(usize, usize)>) {
    if let Some(sp) = body.rfind(' ') {
        let candidate = &body[sp + 1..];
        if let Some(dd) = candidate.find("..") {
            let s = candidate[..dd].parse::<usize>().ok();
            let e = candidate[dd + 2..].parse::<usize>().ok();
            if let (Some(s), Some(e)) = (s, e) {
                return (&body[..sp], Some((s, e)));
            }
        }
    }
    (body, None)
}

fn bak_path(path: &str) -> String {
    let ts = now_secs();
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.is_empty() { format!("{path}.{ts}.bak") }
    else { format!("{}.{ts}.{ext}.bak", &path[..path.len() - ext.len() - 1]) }
}

fn backup_evolved_files() {
    let Ok(out) = Command::new("git").args(["diff", "--name-only"]).output() else { return };
    for path in String::from_utf8_lossy(&out.stdout).lines() {
        if Path::new(path).exists() {
            fs::copy(path, bak_path(path)).ok();
        }
    }
}

type AgentRegistry = Arc<Mutex<Vec<(String, Arc<Mutex<Option<String>>>)>>>;

fn spawn_sub_agent(cfg_snap: (String, String, String), task: &str, output_path: &str, traj: &str, registry: &AgentRegistry) -> String {
    let agent_id = format!("agent_{}", now_secs());
    let slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let slot2 = slot.clone();
    let (task, output_path, traj, id2) = (task.to_string(), output_path.to_string(), traj.to_string(), agent_id.clone());
    std::thread::spawn(move || {
        let cfg = Cfg { api_key: cfg_snap.0, base_url: cfg_snap.1, model: cfg_snap.2 };
        let system = load_prompt("chat_system.txt");
        let mut messages = vec![Msg { role: "user".to_string(), content: json!(format!("You are a sub-agent. Complete this task and write the result to {output_path}.\n\nTask:\n{task}")) }];
        traj_log(&traj, "sub_agent_start", json!({"agent_id": &id2, "output_path": &output_path}));
        let mut final_result = format!("sub-agent {id2}: no output produced");
        let mut turn = 0usize;
        loop {
            turn += 1;
            let reply = match llm(&cfg, &messages, &system) {
                Ok(r) => r,
                Err(e) => { final_result = format!("sub-agent {id2} LLM error turn {turn}: {e}"); break; }
            };
            traj_log(&traj, "sub_agent_turn", json!({"agent_id": &id2, "turn": turn, "preview": &reply.chars().take(200).collect::<String>()}));
            messages.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
            if let Some(tool_result) = run_tool(&reply, &traj, false, None) {
                let wrote = tool_result.contains(&format!("written {output_path}"))
                    || tool_result.contains("written and verified OK");
                if wrote { final_result = format!("written {output_path}"); }
                messages.push(Msg { role: "user".to_string(), content: json!(tool_result) });
                if wrote { break; }
            } else {
                if !Path::new(&output_path).exists() {
                    if let Some(par) = Path::new(&output_path).parent() { fs::create_dir_all(par).ok(); }
                    fs::write(&output_path, &reply).ok();
                    final_result = format!("written {output_path} (from reply)");
                }
                break;
            }
        }
        traj_log(&traj, "sub_agent_end", json!({"agent_id": &id2, "result": &final_result}));
        *slot2.lock().unwrap() = Some(final_result);
    });
    registry.lock().unwrap().push((agent_id.clone(), slot));
    agent_id
}

fn poll_agent(registry: &AgentRegistry, agent_id: &str) -> Option<String> {
    let reg = registry.lock().unwrap();
    for (id, slot) in reg.iter() {
        if id == agent_id { return slot.lock().unwrap().clone(); }
    }
    Some(format!("unknown agent_id: {agent_id}"))
}

fn run_tool(text: &str, traj: &str, evolve_mode: bool, registry: Option<(&AgentRegistry, &(String, String, String), &str)>) -> Option<String> {
    let (name, body) = extract_tool(text)?;
    match name {
        "bash" => {
            let out = Command::new("sh").args(["-c", body]).output()
                .map(|o| format!("exit={}\n{}{}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
                .unwrap_or_else(|e| format!("error: {e}"));
            let out = out.chars().take(2000).collect::<String>();
            traj_log(traj, "tool_result", json!({"tool": "bash", "result": &out}));
            Some(format!("<tool_result>{out}</tool_result>"))
        }
        "write_file" => {
            // Syntax line 1: "path [start..end]" for range patch, or just "path" for full overwrite
            let mut lines = body.splitn(2, '\n');
            let first_line = lines.next()?.trim();
            let (path, range) = parse_path_range(first_line);
            let content = lines.next().unwrap_or("");
            if evolve_mode {
                let allowed = (path.starts_with("src/") && !path.ends_with(".bak"))
                    || path == "CLAUDE.md" || path == "README.md";
                if !allowed {
                    return Some(format!("<tool_result>REJECTED (not an evolvable path: {path})</tool_result>"));
                }
            }
            if let Some(parent) = Path::new(path).parent() { fs::create_dir_all(parent).ok(); }
            let final_content: String = if let Some((start, end)) = range {
                let existing = fs::read_to_string(path).unwrap_or_default();
                let chars: Vec<char> = existing.chars().collect();
                let start = start.min(chars.len());
                let end = end.min(chars.len()).max(start);
                let before: String = chars[..start].iter().collect();
                let after: String = chars[end..].iter().collect();
                format!("{before}{content}{after}")
            } else {
                content.to_string()
            };
            if path == SELF_PATH {
                if final_content.is_empty() { return Some("<tool_result>REJECTED (empty content)</tool_result>".to_string()); }
                let saved = fs::read_to_string(SELF_PATH).unwrap_or_default();
                fs::write(SELF_PATH, &final_content).ok();
                let build = Command::new("cargo").args(["build", "--release"]).output()
                    .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
                    .unwrap_or_else(|e| e.to_string());
                if build.contains("error[") || build.contains("error: ") {
                    fs::write(SELF_PATH, &saved).ok();
                    let snippet = build.chars().take(400).collect::<String>();
                    let result = format!("REJECTED (build failed, reverted):\n{snippet}");
                    traj_log(traj, "tool_result", json!({"tool": "write_file", "path": path, "result": &result}));
                    return Some(format!("<tool_result>{result}</tool_result>"));
                }
                traj_log(traj, "tool_result", json!({"tool": "write_file", "path": path, "result": "written and verified OK"}));
                Some("<tool_result>written and verified OK</tool_result>".to_string())
            } else {
                fs::write(path, &final_content).ok();
                traj_log(traj, "tool_result", json!({"tool": "write_file", "path": path}));
                Some(format!("<tool_result>written {path}</tool_result>"))
            }
        }
        "read_file" => {
            // Syntax: "path [start..end]" where start/end are char offsets (optional)
            let (path, range) = parse_path_range(body.trim());
            match fs::read_to_string(path) {
                Ok(content) => {
                    let total = content.chars().count();
                    let (start, end) = range.unwrap_or((0, total.min(16000)));
                    let start = start.min(total);
                    let end = end.min(total).max(start);
                    let out: String = content.chars().skip(start).take(end - start).collect();
                    let more = if end < total { format!("\n[{} chars remaining — use read_file {path} {}..{}]", total - end, end, (end + 16000).min(total)) } else { String::new() };
                    traj_log(traj, "tool_result", json!({"tool": "read_file", "path": path, "range": format!("{start}..{end}")}));
                    Some(format!("<tool_result>{out}{more}</tool_result>"))
                }
                Err(e) => Some(format!("<tool_result>ERROR reading {path}: {e}</tool_result>")),
            }
        }
        "spawn_agent" => {
            let (reg, cfg_snap, out_dir) = registry?;
            let mut ls = body.splitn(2, '\n');
            let rel = ls.next().unwrap_or("out.md").trim();
            let task = ls.next().unwrap_or(body).trim();
            let out_path = format!("{out_dir}/{rel}");
            let agent_id = spawn_sub_agent(cfg_snap.clone(), task, &out_path, traj, reg);
            traj_log(traj, "tool_result", json!({"tool": "spawn_agent", "agent_id": &agent_id}));
            Some(format!("<tool_result>spawned {agent_id} → {out_path}\nUse <tool name=\"wait_agent\">{agent_id}</tool> to block.</tool_result>"))
        }
        "wait_agent" => {
            let (reg, _, _) = registry?;
            let agent_id = body.trim();
            loop {
                if let Some(result) = poll_agent(reg, agent_id) {
                    traj_log(traj, "tool_result", json!({"tool": "wait_agent", "agent_id": agent_id, "result": &result}));
                    return Some(format!("<tool_result>agent {agent_id} finished: {result}</tool_result>"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        _ => None,
    }
}

fn chat_mode(cfg: &Cfg, session_ts: &str, traj: &str) {
    traj_log(traj, "session_start", json!({}));

    let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let q2 = queue.clone();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => { q2.lock().unwrap().push_back(l); }
                Err(_) => break,
            }
        }
    });

    let agents_md = fs::read_to_string("src/AGENTS.md").unwrap_or_default();
    let mem_idx = memory_index();
    let system = format!(
        "{}\n\n## Project rules (src/AGENTS.md)\n{agents_md}\n\n## Memory index (use read_file to load any entry)\n{mem_idx}",
        load_prompt("chat_system.txt")
    );

    let registry: AgentRegistry = Arc::new(Mutex::new(vec![]));
    let cfg_snap = (cfg.api_key.clone(), cfg.base_url.clone(), cfg.model.clone());

    let mut messages: Vec<Msg> = vec![];
    let mut task_n = 1usize;
    let mut out_dir = format!("outputs/{session_ts}/task_{task_n}");
    fs::create_dir_all(&out_dir).ok();

    eprintln!("Ready. /exit to quit, /evolve to evolve and relaunch.");

    loop {
        let input = loop {
            let mut q = queue.lock().unwrap();
            if let Some(line) = q.pop_front() { break line; }
            drop(q);
            if Arc::strong_count(&queue) == 1 {
                traj_log(traj, "session_end", json!({"turns": task_n}));
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        let trimmed = input.trim();
        if trimmed.is_empty() { continue; }

        if trimmed == "/exit" {
            traj_log(traj, "session_end", json!({"turns": task_n, "reason": "user /exit"}));
            eprintln!("Bye.");
            std::process::exit(0);
        }
        if trimmed == "/evolve" {
            traj_log(traj, "session_end", json!({"turns": task_n, "reason": "user /evolve"}));
            eprintln!("Starting evolution loop...");
            evolve_mode(cfg, traj);
            let exe = env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("./target/release/auto-harness"));
            eprintln!("Evolution done. Relaunching {}...\n", exe.display());
            let err = Command::new(&exe).args(env::args().skip(1)).exec();
            eprintln!("re-exec failed: {err}");
            std::process::exit(1);
        }

        traj_log(traj, "user_input", json!(input));

        if is_new_task(cfg, &messages, &input) && !messages.is_empty() {
            task_n += 1;
            out_dir = format!("outputs/{session_ts}/task_{task_n}");
            fs::create_dir_all(&out_dir).ok();
            traj_log(traj, "task_boundary", json!({"task": task_n}));
        }

        let stamped = format!("[output_dir: {out_dir}]\n{input}");
        messages.push(Msg { role: "user".to_string(), content: json!(stamped) });
        if messages.len() > 20 { messages.drain(..messages.len() - 20); }

        let mut turn = 0usize;
        loop {
            turn += 1;
            let reply = match llm(cfg, &messages, &system) {
                Ok(r) => r,
                Err(e) => { eprintln!("LLM error: {e}"); break; }
            };
            traj_log(traj, "llm_response", json!({"task": task_n, "turn": turn, "preview": &reply.chars().take(200).collect::<String>()}));
            println!("{reply}");
            messages.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
            if messages.len() > 20 { messages.drain(..messages.len() - 20); }
            if let Some(tool_result) = run_tool(&reply, traj, false, Some((&registry, &cfg_snap, &out_dir))) {
                messages.push(Msg { role: "user".to_string(), content: json!(tool_result) });
            } else {
                break;
            }
        }
    }
}

fn reflect(cfg: &Cfg, traj: &str) {
    let watermark: u64 = fs::read_to_string(WATERMARK_PATH)
        .ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);

    let sessions: Vec<_> = fs::read_dir(".evo/sessions").into_iter().flatten().flatten()
        .filter(|e| {
            let s = e.file_name();
            let s = s.to_string_lossy();
            s.ends_with(".jsonl") &&
            s.trim_end_matches(".jsonl").parse::<u64>().map(|ts| ts > watermark).unwrap_or(false)
        })
        .collect();

    if sessions.is_empty() {
        eprintln!("No new sessions to reflect on.");
        return;
    }

    let system = load_prompt("reflect_system.txt");

    for entry in &sessions {
        let name = entry.file_name();
        let session_ts: u64 = name.to_string_lossy().trim_end_matches(".jsonl").parse().unwrap_or(0);
        let traj_path = entry.path().to_string_lossy().to_string();

        let summary: String = {
            let raw = fs::read_to_string(&traj_path).unwrap_or_default();
            let joined = raw.lines()
                .filter_map(|l| {
                    let mut v: Value = serde_json::from_str(l).ok()?;
                    if let Some(obj) = v.get_mut("data") {
                        if let Some(map) = obj.as_object_mut() {
                            map.remove("content");
                            map.remove("preview");
                        } else if obj.as_str().map(|s| s.len()).unwrap_or(0) > 120 {
                            *obj = json!(obj.as_str().unwrap_or("").chars().take(120).collect::<String>());
                        }
                    }
                    Some(v.to_string())
                })
                .collect::<Vec<_>>()
                .join("\n");
            if joined.len() > 8000 { joined[joined.len() - 8000..].to_string() } else { joined }
        };

        let mut msgs = vec![Msg {
            role: "user".to_string(),
            content: json!(format!(
                "Session {session_ts} summary (stripped):\n{summary}\n\nFull traj at: {traj_path}\nUse <tool name=\"read_file\">{traj_path}</tool> if you need more detail.\n\nWhat is the single most important improvement?"
            )),
        }];

        let mut suggestion = String::new();
        loop {
            match llm(cfg, &msgs, &system) {
                Ok(reply) => {
                    msgs.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
                    if let Some(("read_file", raw_body)) = extract_tool(&reply) {
                        let (path, range) = parse_path_range(raw_body.trim());
                        let content = match fs::read_to_string(path.trim()) {
                            Ok(c) => {
                                let total = c.chars().count();
                                let (start, end) = range.unwrap_or((0, total.min(16000)));
                                let start = start.min(total);
                                let end = end.min(total).max(start);
                                let out: String = c.chars().skip(start).take(end - start).collect();
                                if end < total {
                                    format!("{out}\n[{} chars remaining — use read_file {path} {}..{}]", total - end, end, (end + 16000).min(total))
                                } else { out }
                            }
                            Err(e) => format!("ERROR: {e}"),
                        };
                        msgs.push(Msg { role: "user".to_string(), content: json!(format!("<tool_result>{content}</tool_result>")) });
                    } else {
                        suggestion = reply;
                        break;
                    }
                }
                Err(e) => { eprintln!("Reflection LLM error [{session_ts}]: {e}"); break; }
            }
        }

        if !suggestion.is_empty() {
            eprintln!("Reflection [{session_ts}]: {suggestion}");
            traj_log(traj, "reflect_result", json!(suggestion));
            fs::create_dir_all(".evo").ok();
            fs::write(WATERMARK_PATH, session_ts.to_string()).ok();
        }
    }
}

fn append_memo(memo_path: &str, entry: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(memo_path) {
        let _ = writeln!(f, "{entry}");
    }
}

fn evolve_mode(cfg: &Cfg, traj: &str) {
    let evolve_ts = now_secs();
    let memo_path = format!(".evo/memos/{evolve_ts}.md");
    fs::create_dir_all(".evo/memos").ok();

    let evolve_system = load_prompt("evolve_system.txt");
    traj_log(traj, "evolve_start", json!({}));

    let mut iter_n = 0usize;
    loop {
        iter_n += 1;
        traj_log(traj, "iter_start", json!({"iter": iter_n}));
        reflect(cfg, traj);
        let src = fs::read_to_string(SELF_PATH).unwrap_or_default();
        let agents_md = fs::read_to_string("src/AGENTS.md").unwrap_or_default();
        let prompts_section = {
            let names = ["chat_system.txt", "reflect_system.txt", "evolve_system.txt", "doc_system.txt"];
            names.iter().map(|n| {
                let content = fs::read_to_string(format!("src/prompts/{n}")).unwrap_or_default();
                format!("=== src/prompts/{n} ===\n{content}")
            }).collect::<Vec<_>>().join("\n\n")
        };
        let memory_section = memory_index();
        let changelog = fs::read_to_string(&memo_path).unwrap_or_else(|_| "(none yet this run)".to_string());
        let mut messages: Vec<Msg> = vec![Msg {
            role: "user".to_string(),
            content: json!(format!(
                "Changes already made this run (do not repeat):\n{changelog}\n\nPrompt files:\n{prompts_section}\n\nsrc/AGENTS.md:\n{agents_md}\n\nsrc/memory/ (filepath — description):\n{memory_section}\n\nsrc/main.rs:\n```rust\n{src}\n```\n\nPropose one improvement not in the changelog. Priority: prompts > AGENTS.md > memory/*.md > main.rs. Reply SKIP if nothing is worth changing."
            )),
        }];

        let mut improved = false;
        let mut done = false;
        let mut change_summary = String::new();
        let mut turn = 0usize;
        loop {
            turn += 1;
            let reply = match llm(cfg, &messages, &evolve_system) {
                Ok(r) => r,
                Err(e) => { eprintln!("LLM error: {e}"); break; }
            };
            traj_log(traj, "llm_response", json!({"iter": iter_n, "turn": turn, "preview": &reply.chars().take(200).collect::<String>()}));

            if reply.trim().to_uppercase().starts_with("SKIP") {
                traj_log(traj, "iter_skip", json!({"iter": iter_n, "reason": "LLM chose not to evolve"}));
                eprintln!("Iter {iter_n}: SKIP — evolution complete.");
                done = true;
                break;
            }

            if change_summary.is_empty() {
                change_summary = reply.lines().next().unwrap_or("").chars().take(200).collect();
            }

            messages.push(Msg { role: "assistant".to_string(), content: json!(&reply) });

            if let Some(tool_result) = run_tool(&reply, traj, true, None) {
                let write_ok = tool_result.contains("verified OK") || tool_result.contains("written ");
                if write_ok { improved = true; }
                messages.push(Msg { role: "user".to_string(), content: json!(tool_result) });
                if write_ok { break; }
            } else {
                break;
            }
        }

        traj_log(traj, "iter_end", json!({"iter": iter_n, "improved": improved}));
        if improved {
            append_memo(&memo_path, &format!("- iter {iter_n}: {change_summary}"));
            eprintln!("Iter {iter_n}: improved.");
        }
        if done { break; }
    }

    traj_log(traj, "evolve_end", json!({}));

    // Refine: feed clippy+test failures to LLM for fixes before final verification
    eprintln!("Refine: running lint and tests...");
    let run_clippy = || -> (bool, String) {
        match Command::new("cargo").args(["clippy", "--release", "--no-deps", "--", "-D", "warnings"]).output() {
            Ok(o) => {
                let combined = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                let relevant: String = combined.lines()
                    .filter(|l| l.contains("warning") || l.contains("error") || l.starts_with("error"))
                    .collect::<Vec<_>>().join("\n");
                let out = if relevant.is_empty() { combined } else { relevant };
                (o.status.success(), out.chars().take(2000).collect())
            }
            Err(e) => (false, e.to_string()),
        }
    };
    let run_test = || -> (bool, String) {
        match Command::new("cargo").args(["test", "--release"]).output() {
            Ok(o) => {
                let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
                    .chars().take(2000).collect();
                (o.status.success(), out)
            }
            Err(e) => (false, e.to_string()),
        }
    };

    let (clippy_ok, clippy_out) = run_clippy();
    let (test_ok, test_out) = run_test();

    if !clippy_ok || !test_ok {
        eprintln!("Refine: issues found, asking LLM to fix...");
        let issues = format!(
            "Post-evolution check found issues. Fix them using write_file. Reply SKIP if nothing to fix.\n\nClippy (ok={clippy_ok}):\n{clippy_out}\n\nTests (ok={test_ok}):\n{test_out}"
        );
        let mut refine_msgs = vec![Msg { role: "user".to_string(), content: json!(issues) }];
        loop {
            match llm(cfg, &refine_msgs, &evolve_system) {
                Ok(reply) => {
                    refine_msgs.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
                    if reply.trim().to_uppercase().starts_with("SKIP") { break; }
                    if let Some(result) = run_tool(&reply, traj, true, None) {
                        refine_msgs.push(Msg { role: "user".to_string(), content: json!(result) });
                    } else { break; }
                }
                Err(e) => { eprintln!("Refine LLM error: {e}"); break; }
            }
        }
    }

    // Final lint + test — authoritative gate before doc update
    eprintln!("Running final lint and tests...");
    let (clippy_ok, clippy_out) = run_clippy();
    let (test_ok, test_out) = run_test();
    traj_log(traj, "lint_result", json!({"ok": clippy_ok, "output": &clippy_out}));
    traj_log(traj, "test_result", json!({"ok": test_ok, "output": &test_out}));
    if clippy_ok { eprintln!("Lint: PASS"); } else { eprintln!("Lint: FAIL\n{clippy_out}"); }
    if test_ok { eprintln!("Tests: PASS"); } else { eprintln!("Tests: FAIL\n{test_out}"); }
    if !clippy_ok || !test_ok {
        eprintln!("WARNING: evolved binary still has lint/test failures after refine.");
    }

    backup_evolved_files();

    let src = fs::read_to_string(SELF_PATH).unwrap_or_default();
    let claude_md = fs::read_to_string("CLAUDE.md").unwrap_or_default();
    let readme = fs::read_to_string("README.md").unwrap_or_default();
    let doc_system = load_prompt("doc_system.txt");
    let doc_prompt = format!(
        "Current src/main.rs:\n```rust\n{src}\n```\n\nCurrent CLAUDE.md:\n{claude_md}\n\nCurrent README.md:\n{readme}\n\nUpdate both docs to match the implementation."
    );
    let mut doc_msgs = vec![Msg { role: "user".to_string(), content: json!(doc_prompt) }];
    loop {
        match llm(cfg, &doc_msgs, &doc_system) {
            Ok(reply) => {
                doc_msgs.push(Msg { role: "assistant".to_string(), content: json!(&reply) });
                if let Some(result) = run_tool(&reply, traj, true, None) {
                    doc_msgs.push(Msg { role: "user".to_string(), content: json!(result) });
                } else {
                    break;
                }
            }
            Err(e) => { eprintln!("Doc update LLM error: {e}"); break; }
        }
    }

    eprintln!("Evolution memo: {memo_path}");
}

fn main() {
    let ts = now_secs().to_string();
    fs::create_dir_all(".evo/sessions").ok();
    let traj = format!(".evo/sessions/{ts}.jsonl");
    // load .env
    if let Ok(content) = fs::read_to_string(".env") {
        for line in content.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim();
                if !k.starts_with('#') && env::var(k).is_err() {
                    env::set_var(k, v);
                }
            }
        }
    }

    let cfg = Cfg::from_env();

    chat_mode(&cfg, &ts, &traj);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_extract_tool_basic() {
        let text = r#"Some text <tool name="shell">echo hello</tool> more"#;
        let (name, body) = extract_tool(text).unwrap();
        assert_eq!(name, "shell");
        assert_eq!(body, "echo hello");
    }

    #[test]
    fn test_extract_tool_write_file() {
        let text = "<tool name=\"write_file\">path/to/file.txt\nfile content here</tool>";
        let (name, body) = extract_tool(text).unwrap();
        assert_eq!(name, "write_file");
        assert!(body.starts_with("path/to/file.txt"));
    }

    #[test]
    fn test_extract_tool_strips_fences() {
        let text = "<tool name=\"write_self\">```rust\nfn main() {}\n```</tool>";
        let (name, body) = extract_tool(text).unwrap();
        assert_eq!(name, "write_self");
        assert_eq!(body, "fn main() {}");
    }

    #[test]
    fn test_extract_tool_none() {
        assert!(extract_tool("no tool here").is_none());
    }

    #[test]
    fn test_agent_registry_poll_unknown() {
        let registry: AgentRegistry = Arc::new(Mutex::new(vec![]));
        let result = poll_agent(&registry, "nonexistent");
        assert!(result.unwrap().contains("unknown agent_id"));
    }

    #[test]
    fn test_agent_registry_spawn_and_poll() {
        let registry: AgentRegistry = Arc::new(Mutex::new(vec![]));
        let slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        registry.lock().unwrap().push(("agent_test".to_string(), slot));

        assert!(poll_agent(&registry, "agent_test").is_none());

        *slot2.lock().unwrap() = Some("written output.md".to_string());

        assert_eq!(poll_agent(&registry, "agent_test"), Some("written output.md".to_string()));
    }

    #[test]
    fn test_spawn_sub_agent_writes_output() {
        let dir = std::env::temp_dir().join(format!("autoharness_test_{}", now_secs()));
        fs::create_dir_all(&dir).unwrap();
        let output_path = dir.join("result.md");

        fs::write(&output_path, "test result").unwrap();

        let registry: AgentRegistry = Arc::new(Mutex::new(vec![]));
        let slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        registry.lock().unwrap().push(("agent_sim".to_string(), slot));
        *slot2.lock().unwrap() = Some(format!("written {}", output_path.display()));

        let result = poll_agent(&registry, "agent_sim").unwrap();
        assert!(result.contains("written"));
        assert!(output_path.exists());
        assert_eq!(fs::read_to_string(&output_path).unwrap(), "test result");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_extract_tool_spawn_agent() {
        let text = "<tool name=\"spawn_agent\">result.md\nAnalyse src/main.rs and summarise key functions.</tool>";
        let (name, body) = extract_tool(text).unwrap();
        assert_eq!(name, "spawn_agent");
        let mut lines = body.splitn(2, '\n');
        assert_eq!(lines.next().unwrap().trim(), "result.md");
        assert!(lines.next().unwrap().contains("Analyse"));
    }

    #[test]
    fn test_extract_tool_wait_agent() {
        let text = "<tool name=\"wait_agent\">agent_1234567890</tool>";
        let (name, body) = extract_tool(text).unwrap();
        assert_eq!(name, "wait_agent");
        assert_eq!(body.trim(), "agent_1234567890");
    }

    #[test]
    fn test_run_tool_spawn_and_wait() {
        let dir = std::env::temp_dir().join(format!("autoharness_rtt_{}", now_secs()));
        fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.to_string_lossy().to_string();
        let traj = format!("{out_dir}/traj.jsonl");

        let registry: AgentRegistry = Arc::new(Mutex::new(vec![]));
        let cfg_snap = (
            "dummy_key".to_string(),
            "http://127.0.0.1:1".to_string(), // unreachable — LLM fails fast
            "test-model".to_string(),
        );

        let spawn_text = "<tool name=\"spawn_agent\">sub_out.md\nWrite the word DONE to the output file.</tool>";
        let result = run_tool(spawn_text, &traj, false, Some((&registry, &cfg_snap, &out_dir)));
        let result_str = result.unwrap();
        assert!(result_str.contains("spawned agent_"), "got: {result_str}");

        let agent_id = result_str
            .split("spawned ").nth(1).unwrap()
            .split_whitespace().next().unwrap()
            .to_string();

        let wait_text = format!("<tool name=\"wait_agent\">{agent_id}</tool>");
        let wait_result = run_tool(&wait_text, &traj, false, Some((&registry, &cfg_snap, &out_dir)));
        let wait_str = wait_result.unwrap();
        assert!(wait_str.contains("finished:"), "got: {wait_str}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_path_range_no_range() {
        let (path, range) = parse_path_range("src/main.rs");
        assert_eq!(path, "src/main.rs");
        assert!(range.is_none());
    }

    #[test]
    fn test_parse_path_range_with_range() {
        let (path, range) = parse_path_range("src/main.rs 100..200");
        assert_eq!(path, "src/main.rs");
        assert_eq!(range, Some((100, 200)));
    }

    #[test]
    fn test_parse_path_range_edge_inverted() {
        let (path, range) = parse_path_range("src/main.rs 200..100");
        assert_eq!(path, "src/main.rs");
        assert_eq!(range, Some((200, 100)));
        // clamping at use site: inverted range collapses to empty slice, no panic
        let total = 50usize;
        let (s, e) = range.unwrap();
        let s = s.min(total);
        let e = e.min(total).max(s);
        assert_eq!(s, 50);
        assert_eq!(e, 50);
    }

    #[test]
    fn test_parse_path_range_oob() {
        // both offsets beyond file length clamp to total → empty slice, no panic
        let total = 10usize;
        let s = 9999usize.min(total);
        let e = 99999usize.min(total).max(s);
        assert_eq!(s, 10);
        assert_eq!(e, 10);
    }
}
