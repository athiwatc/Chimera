use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn start(project: &Path, config: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_chimera"))
            .arg("--project")
            .arg(project)
            .env("CLAUDE_CONFIG_DIR", config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let initialized = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "chimera-test", "version": "0.1.0"}
            }),
        );
        assert_eq!(initialized["serverInfo"]["name"], "chimera");
        client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        client
    }

    fn send(&mut self, frame: &Value) {
        writeln!(self.stdin, "{frame}").unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                self.stdout.read_line(&mut line).unwrap(),
                0,
                "server exited during {method}"
            );
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["id"] == id {
                assert!(frame.get("error").is_none(), "{frame}");
                return frame["result"].clone();
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn transcript_path(config: &Path, project: &Path, id: &str) -> PathBuf {
    let slug: String = project
        .to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = config.join("projects").join(slug);
    fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{id}.jsonl"))
}

fn write_message(
    file: &mut File,
    project: &Path,
    session: &str,
    id: &str,
    parent: Option<&str>,
    role: &str,
    content: Value,
) {
    let entry = json!({"type": role, "uuid": id, "parentUuid": parent, "cwd": project,
        "sessionId": session, "isSidechain": false, "message": {"role": role, "content": content}});
    writeln!(file, "{entry}").unwrap();
}

#[test]
fn pages_local_history_and_excludes_colliding_projects_and_abandoned_branches() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let config = root.join("claude");
    let project = root.join("project-a");
    let other = root.join("project_a");
    fs::create_dir(&project).unwrap();
    fs::create_dir(&other).unwrap();
    let allowed = "11111111-1111-4111-8111-111111111111";
    let foreign = "22222222-2222-4222-8222-222222222222";

    let mut file = File::create(transcript_path(&config, &project, allowed)).unwrap();
    write_message(
        &mut file,
        &project,
        allowed,
        "question",
        None,
        "user",
        json!("What changed?"),
    );
    write_message(
        &mut file,
        &project,
        allowed,
        "old-answer",
        Some("question"),
        "assistant",
        json!("Abandoned answer"),
    );
    write_message(
        &mut file,
        &project,
        allowed,
        "answer",
        Some("question"),
        "assistant",
        json!([
            {"type": "thinking", "thinking": "omit this block"}, {"type": "text", "text": "The build passed."}
        ]),
    );
    writeln!(file, "{{\"incomplete\":").unwrap();
    let mut file = File::create(transcript_path(&config, &other, foreign)).unwrap();
    write_message(
        &mut file,
        &other,
        foreign,
        "outside",
        None,
        "user",
        json!("Foreign project transcript"),
    );

    let mut client = McpClient::start(&project, &config);
    let list = client.request("tools/list", json!({}));
    let mut tools: Vec<_> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    tools.sort_unstable();
    assert_eq!(tools, ["delegate_task", "list_sessions", "read_session"]);
    let listed = client.call("list_sessions", json!({"harness": "claude", "limit": 1}));
    let sessions = listed["structuredContent"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], allowed);

    let first = client.call(
        "read_session",
        json!({"harness": "claude", "sessionId": allowed, "limit": 1}),
    );
    assert_eq!(
        first["structuredContent"]["messages"],
        json!([{"role": "user", "text": "What changed?"}])
    );
    let second = client.call(
        "read_session",
        json!({"harness": "claude", "sessionId": allowed, "limit": 1,
        "cursor": first["structuredContent"]["nextCursor"]}),
    );
    assert_eq!(
        second["structuredContent"]["messages"],
        json!([{"role": "assistant", "text": "The build passed."}])
    );
    assert_eq!(second["structuredContent"]["nextCursor"], Value::Null);

    let rejected = client.call(
        "read_session",
        json!({"harness": "claude", "sessionId": foreign}),
    );
    assert_eq!(rejected["isError"], true);
    assert!(!rejected.to_string().contains("Foreign project transcript"));
}
