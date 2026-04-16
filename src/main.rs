use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{env, fs, process::Command};

const SELF: &str = "src/main.rs";
const HIST: &str = ".evo/history.json";
const MAX: usize = 10;

#[derive(Serialize, Deserialize, Clone)]
struct Msg {
    role: String,
    content: String,
}

#[derive(Serialize, Deserialize)]
struct Iter {
    n: usize,
    score: f64,
    summary: String,
}

fn shell(cmd: &str) -> (i32, String) {
    let o = Command::new("sh").args(["-c", cmd]).output().expect("sh failed");
    let t = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    (o.status.code().unwrap_or(-1), t.trim().to_string())
}

fn build() -> Result<(), String> {
    let (c, o) = shell("cargo build --release 2>&1");
    if c == 0 { Ok(()) } else { Err(o) }
}

fn score() -> f64 {
    let src = fs::read_to_string(SELF).unwrap_or_default();
    let lines = src.lines().count() as f64;
    if lines == 0.0 { return 0.0; }
    match build() {
        Ok(_) => 1.0 / lines * 1000.0 + 1.0,
        Err(_) => 1.0 / lines * 1000.0,
    }
}

struct Cfg {
    api_key: String,
    base_url: String,
    model: String,
}

impl Cfg {
    fn from_env() -> Self {
        let api_key = env::var("OPENROUTER_API_KEY")
            .or_else(|_| env::var("ANTHROPIC_API_KEY"))
            .unwrap_or_else(|_| {
                eprintln!("Neither OPENROUTER_API_KEY nor ANTHROPIC_API_KEY is set");
                std::process::exit(1);
            });
        Self {
            api_key,
            base_url: env::var("INFERENCE_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string()),
            model: env::var("MODEL_NAME").unwrap_or_else(|_| "anthropic/claude-opus-4".to_string()),
        }
    }
}

const SYSTEM: &str = r###"You are a self-evolving Rust coding agent. Your goal: make src/main.rs shorter, more correct, and more capable — while keeping it compiling. Available tools (emit exactly one per turn): <tool name="shell">cmd