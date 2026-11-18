use std::ffi::OsStr;
use std::fmt::Write;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use tokio::{
    process::{Child, Command},
    time::sleep,
};

use crate::model::{
    Delegation, Message, Page, SESSION_NOT_FOUND, Session, excerpt, message_text, valid_session_id,
};

struct Server {
    _child: LocalProcess,
    client: Client,
    address: String,
    password: String,
}

struct LocalProcess(Child);

impl Drop for LocalProcess {
    fn drop(&mut self) {
        // Package-manager shims can launch the real CLI as a separate process.
        #[cfg(unix)]
        if let Some(pid) = self.0.id() {
            // The child leads its own process group, so this targets only our launch.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
        let _ = self.0.start_kill();
    }
}

impl LocalProcess {
    async fn finish(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.id() {
            // Let the server flush and close its session database after a completed turn.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
            let _ = tokio::time::timeout(Duration::from_secs(1), self.0.wait()).await;
        }
    }
}

impl Server {
    async fn start(binary: &OsStr, project: &Path) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Could not generate an OpenCode server password."))?;
        let mut password = String::with_capacity(64);
        for byte in bytes {
            write!(password, "{byte:02x}")?;
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(1))
            .build()?;
        let mut command = Command::new(binary);
        command
            .current_dir(project)
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env_remove("OPENCODE_SERVER_PASSWORD")
            .env("OPENCODE_PASSWORD", &password)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = LocalProcess(command.spawn().context(
            "Could not start OpenCode. Install OpenCode 2 or set CHIMERA_OPENCODE_BIN.",
        )?);
        let address = format!("http://127.0.0.1:{port}");
        loop {
            if child.0.try_wait()?.is_some() {
                bail!(
                    "OpenCode exited before its local server was ready. Check the CLI installation and configuration."
                );
            }
            if let Ok(response) = client
                .get(format!("{address}/api/info"))
                .basic_auth("opencode", Some(&password))
                .timeout(Duration::from_secs(1))
                .send()
                .await
            {
                if response.status() == StatusCode::SERVICE_UNAVAILABLE {
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
                let info = response_json(response).await?;
                ensure!(
                    info["version"]
                        .as_str()
                        .is_some_and(|version| version.starts_with("2.")),
                    "Chimera requires OpenCode 2.x."
                );
                return Ok(Self {
                    _child: child,
                    client,
                    address,
                    password,
                });
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.client
            .get(format!("{}{path}", self.address))
            .basic_auth("opencode", Some(&self.password))
    }

    async fn post(&self, path: &str, data: Value) -> Result<Value> {
        let request = self
            .client
            .post(format!("{}{path}", self.address))
            .basic_auth("opencode", Some(&self.password))
            .json(&data);
        send(request).await
    }

    async fn session(&self, project: &Path, id: &str) -> Result<Value> {
        let session = send(self.get(&format!("/api/session/{id}"))).await?["data"].take();
        ensure!(in_project(&session, project), "{SESSION_NOT_FOUND}");
        Ok(session)
    }

    async fn messages(
        &self,
        id: &str,
        limit: usize,
        cursor: Option<&str>,
        role: Option<&str>,
    ) -> Result<Value> {
        let mut request = self
            .get(&format!("/api/session/{id}/message"))
            .query(&[("limit", limit)]);
        request = if let Some(cursor) = cursor {
            request.query(&[("cursor", cursor)])
        } else {
            request.query(&[("order", "desc")])
        };
        if let Some(role) = role {
            request = request.query(&[("type", role)]);
        }
        send(request).await
    }
}

async fn send(request: RequestBuilder) -> Result<Value> {
    response_json(
        request
            .send()
            .await
            .context("Could not reach the local OpenCode server.")?,
    )
    .await
}

async fn response_json(mut response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        bail!("{SESSION_NOT_FOUND}");
    }
    ensure!(status.is_success(), "OpenCode returned HTTP {status}.");
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Could not read the OpenCode response.")?
    {
        ensure!(
            body.len() + chunk.len() <= 16 * 1024 * 1024,
            "OpenCode output exceeded 16 MiB."
        );
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("OpenCode returned an invalid JSON response.")
}

fn in_project(session: &Value, project: &Path) -> bool {
    session["location"]["directory"]
        .as_str()
        .is_some_and(|path| Path::new(path) == project)
}

fn message(entry: &Value) -> Option<Message> {
    let (role, text) = match entry["type"].as_str()? {
        "user" => ("user", entry["text"].as_str()?.to_owned()),
        "assistant" => ("assistant", message_text(&entry["content"])),
        _ => return None,
    };
    (!text.is_empty()).then_some(Message { role, text })
}

pub async fn list(binary: &OsStr, project: &Path, limit: usize) -> Result<Vec<Session>> {
    let server = Server::start(binary, project).await?;
    let list = send(
        server
            .get("/api/session")
            .query(&[(
                "directory",
                project.to_str().context("Invalid project path.")?,
            )])
            .query(&[("limit", limit)]),
    )
    .await?;
    let entries = list["data"]
        .as_array()
        .context("OpenCode returned an invalid session list.")?;
    Ok(entries
        .iter()
        .filter(|entry| in_project(entry, project))
        .filter_map(|entry| {
            let id = entry["id"].as_str().filter(|id| valid_session_id(id))?;
            let updated =
                DateTime::<Utc>::from_timestamp_millis(entry["time"]["updated"].as_i64()?)?;
            Some(Session {
                id: id.to_owned(),
                title: excerpt(entry["title"].as_str().unwrap_or_default(), 240),
                updated_at: updated.to_rfc3339_opts(SecondsFormat::Millis, true),
            })
        })
        .collect())
}

pub async fn read(
    binary: &OsStr,
    project: &Path,
    id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<Page> {
    let server = Server::start(binary, project).await?;
    server.session(project, id).await?;
    let page = server.messages(id, limit, cursor, None).await?;
    let entries = page["data"]
        .as_array()
        .context("OpenCode returned an invalid history page.")?;
    let messages = entries
        .iter()
        .rev()
        .filter_map(message)
        .map(|mut entry| {
            entry.text = excerpt(&entry.text, 3000);
            entry
        })
        .collect();
    Ok(Page {
        messages,
        next_cursor: page["cursor"]["next"].as_str().map(str::to_owned),
        order: "newest-page-first",
    })
}

pub async fn delegate(binary: &OsStr, project: &Path, prompt: &str) -> Result<Delegation> {
    let mut server = Server::start(binary, project).await?;
    // OpenCode evaluates session rules after agent rules. The last match wins.
    let permissions = json!([
        {"action":"*","resource":"*","effect":"deny"},
        {"action":"read","resource":"*","effect":"allow"},
        {"action":"glob","resource":"*","effect":"allow"},
        {"action":"grep","resource":"*","effect":"allow"}
    ]);
    let created = server
        .post(
            "/api/session",
            json!({"location":{"directory":project},"permissions":permissions}),
        )
        .await?;
    let session = &created["data"];
    ensure!(
        in_project(session, project) && session["permissions"] == permissions,
        "OpenCode did not grant the requested local read-only session."
    );
    let id = session["id"]
        .as_str()
        .filter(|id| valid_session_id(id))
        .context("OpenCode returned an invalid session ID.")?;
    server
        .post(&format!("/api/session/{id}/prompt"), json!({"text":prompt}))
        .await?;
    loop {
        let current = server.session(project, id).await?;
        match current["outcome"].as_str() {
            Some("succeeded") => break,
            Some("failed" | "interrupted") => bail!(
                "OpenCode did not complete the task. Check authentication and provider access."
            ),
            _ => sleep(Duration::from_millis(250)).await,
        }
    }
    let replies = server.messages(id, 20, None, Some("assistant")).await?;
    let answer = replies["data"]
        .as_array()
        .and_then(|entries| entries.iter().find_map(message))
        .context("OpenCode completed the turn without a final answer.")?;
    server._child.finish().await;
    Ok(Delegation {
        session_id: id.to_owned(),
        text: excerpt(&answer.text, 16000),
    })
}
