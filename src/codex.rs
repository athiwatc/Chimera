use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};

use crate::model::{Delegation, Message, Page, SESSION_NOT_FOUND, Session, excerpt, message_text};
use crate::process::HarnessProcess;

struct AppServer {
    process: HarnessProcess,
    next_id: u32,
    pending: VecDeque<Value>,
}

impl AppServer {
    async fn connect(binary: &OsStr, project: &Path, delegation: bool) -> Result<Self> {
        let mut args = vec!["app-server", "--listen", "stdio://"];
        if delegation {
            args.extend([
                "--disable",
                "hooks",
                "--disable",
                "plugins",
                "--disable",
                "apps",
            ]);
        }
        let process = HarnessProcess::spawn(binary, project, &args, "Codex")?;
        let mut server = Self {
            process,
            next_id: 1,
            pending: VecDeque::new(),
        };
        server
            .request(
                "initialize",
                json!({"clientInfo": {
                    "name": "chimera", "version": env!("CARGO_PKG_VERSION")
                }}),
            )
            .await?;
        server
            .process
            .send(&json!({"method": "initialized"}))
            .await?;
        Ok(server)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.process
            .send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        while let Some(mut frame) = self.process.frame().await? {
            reject_server_request(&frame)?;
            if frame["id"].as_u64() != Some(u64::from(id)) || frame.get("method").is_some() {
                // A fast turn can emit its answer before the turn/start reply reaches us.
                if method == "turn/start"
                    && matches!(
                        frame["method"].as_str(),
                        Some("item/completed" | "turn/completed")
                    )
                {
                    self.pending.push_back(frame);
                }
                continue;
            }
            if let Some(error) = frame["error"]["message"].as_str() {
                bail!("Codex App Server {method}: {}", excerpt(error, 300));
            }
            return frame
                .as_object_mut()
                .and_then(|object| object.remove("result"))
                .with_context(|| format!("Codex App Server returned no result for {method}."));
        }
        bail!("Codex App Server closed during {method}.");
    }

    async fn event(&mut self) -> Result<Value> {
        let frame = match self.pending.pop_front() {
            Some(frame) => frame,
            None => self
                .process
                .frame()
                .await?
                .context("Codex App Server closed before the turn finished.")?,
        };
        reject_server_request(&frame)?;
        Ok(frame)
    }
}

fn reject_server_request(frame: &Value) -> Result<()> {
    ensure!(
        frame.get("method").is_none() || frame.get("id").is_none(),
        "Codex requested an interactive action during a non-interactive session."
    );
    Ok(())
}

fn in_project(thread: &Value, project: &Path) -> bool {
    thread["cwd"]
        .as_str()
        .is_some_and(|cwd| Path::new(cwd) == project)
}

pub async fn list(
    binary: &OsStr,
    project: &Path,
    limit: usize,
    archived: bool,
) -> Result<Vec<Session>> {
    let mut server = AppServer::connect(binary, project, false).await?;
    let result = server
        .request(
            "thread/list",
            json!({
                "cwd": project, "limit": limit, "archived": archived, "sortKey": "updated_at",
                "modelProviders": [], "sourceKinds": ["cli", "vscode", "exec", "appServer"]
            }),
        )
        .await?;
    let threads = result["data"]
        .as_array()
        .context("Codex App Server returned an invalid session list.")?;
    Ok(threads
        .iter()
        .filter(|thread| in_project(thread, project))
        .filter_map(|thread| {
            let timestamp = DateTime::<Utc>::from_timestamp(thread["updatedAt"].as_i64()?, 0)?;
            let title = thread["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .or_else(|| thread["preview"].as_str())
                .unwrap_or_default();
            Some(Session {
                id: thread["id"].as_str()?.to_owned(),
                title: excerpt(title, 240),
                updated_at: timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            })
        })
        .collect())
}

pub async fn read(
    binary: &OsStr,
    project: &Path,
    session_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<Page> {
    let mut server = AppServer::connect(binary, project, false).await?;
    let result = server
        .request(
            "thread/read",
            json!({"threadId": session_id, "includeTurns": false}),
        )
        .await?;
    ensure!(
        in_project(&result["thread"], project),
        "{SESSION_NOT_FOUND}"
    );
    let page = server
        .request(
            "thread/items/list",
            json!({
                "threadId": session_id, "sortDirection": "desc", "limit": limit, "cursor": cursor
            }),
        )
        .await?;
    let entries = page["data"]
        .as_array()
        .context("Codex App Server returned an invalid history page.")?;
    let messages = entries
        .iter()
        .rev()
        .filter_map(|entry| {
            let item = &entry["item"];
            let (role, text) = match item["type"].as_str()? {
                "userMessage" => ("user", message_text(&item["content"])),
                "agentMessage" => ("assistant", item["text"].as_str()?.to_owned()),
                _ => return None,
            };
            (!text.is_empty()).then(|| Message {
                role,
                text: excerpt(&text, 3000),
            })
        })
        .collect();
    Ok(Page {
        messages,
        next_cursor: page["nextCursor"].as_str().map(str::to_owned),
        order: "newest-page-first",
    })
}

pub async fn delegate(binary: &OsStr, project: &Path, prompt: &str) -> Result<Delegation> {
    let mut server = AppServer::connect(binary, project, true).await?;
    let config = server
        .request("config/read", json!({"cwd":project,"includeLayers":false}))
        .await?;
    // An empty MCP map would merge with, rather than replace, configured servers.
    let servers = config["config"]["mcp_servers"]
        .as_object()
        .map(|servers| {
            servers
                .keys()
                .map(|name| (name.clone(), json!({"enabled":false})))
                .collect::<Map<_, _>>()
        })
        .unwrap_or_default();
    let started = server
        .request(
            "thread/start",
            json!({
                "cwd": project, "sandbox": "read-only", "approvalPolicy": "never",
                "config": {"mcp_servers": servers,"web_search":"disabled"}
            }),
        )
        .await?;
    ensure!(
        started["approvalPolicy"] == "never"
            && started["sandbox"]["type"] == "readOnly"
            && started["sandbox"]["networkAccess"] == false
            && in_project(&started["thread"], project),
        "Codex did not grant the requested local read-only session."
    );
    let session_id = started["thread"]["id"]
        .as_str()
        .context("Codex returned no session ID.")?;
    let turn = server
        .request(
            "turn/start",
            json!({
                "threadId": session_id, "input": [{"type":"text","text":prompt,"text_elements":[]}],
                "approvalPolicy": "never", "sandboxPolicy": started["sandbox"]
            }),
        )
        .await?;
    let turn_id = turn["turn"]["id"]
        .as_str()
        .context("Codex returned no turn ID.")?;
    let mut answer = None;
    loop {
        let event = server.event().await?;
        let params = &event["params"];
        if params["threadId"] != session_id {
            continue;
        }
        match event["method"].as_str() {
            Some("item/completed")
                if params["turnId"] == turn_id && params["item"]["type"] == "agentMessage" =>
            {
                let item = &params["item"];
                if item["phase"] != "commentary"
                    && let Some(text) = item["text"].as_str().filter(|text| !text.is_empty())
                {
                    answer = Some(excerpt(text, 16000));
                }
            }
            Some("turn/completed") if params["turn"]["id"] == turn_id => {
                ensure!(
                    params["turn"]["status"] == "completed",
                    "Codex did not complete the task. Check authentication and provider access."
                );
                let text = answer.context("Codex completed the turn without a final answer.")?;
                return Ok(Delegation {
                    session_id: session_id.to_owned(),
                    text,
                });
            }
            _ => {}
        }
    }
}
