use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;
use tokio::fs::{self, File};
use tokio::io::BufReader;

use crate::model::{
    Delegation, Message, Page, SESSION_NOT_FOUND, Session, excerpt, message_text, valid_session_id,
};
use crate::process::{HarnessProcess, missing, transcript_record};

pub struct History {
    projects: Option<PathBuf>,
}

struct TranscriptFile {
    id: String,
    path: PathBuf,
    updated: SystemTime,
}

#[derive(Default)]
struct Transcript {
    title: String,
    entries: HashMap<String, Entry>,
    leaf: Option<String>,
}

struct Entry {
    parent: Option<String>,
    role: Option<&'static str>,
    text: String,
}

impl History {
    pub fn new(config: Option<PathBuf>) -> Self {
        Self {
            projects: config.map(|path| path.join("projects")),
        }
    }

    async fn files(&self, project: &Path, session_id: Option<&str>) -> Result<Vec<TranscriptFile>> {
        let projects = self
            .projects
            .as_ref()
            .context("Set CLAUDE_CONFIG_DIR to locate Claude Code history.")?;
        // Claude replaces non-alphanumeric characters and shortens long bucket names with a hash.
        let slug: String = project
            .to_string_lossy()
            .encode_utf16()
            .map(|unit| {
                char::from_u32(u32::from(unit))
                    .filter(char::is_ascii_alphanumeric)
                    .unwrap_or('-')
            })
            .collect();
        let mut dirs = match fs::read_dir(projects).await {
            Ok(dirs) => dirs,
            Err(error) if missing(&error) => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).context("Could not open the Claude Code projects directory.");
            }
        };
        let mut files = Vec::new();
        while let Some(dir) = dirs.next_entry().await? {
            let name = dir.file_name();
            let name = name.to_string_lossy();
            let matches = name == slug || (slug.len() > 200 && name.starts_with(&slug[..180]));
            if !matches || !dir.file_type().await?.is_dir() {
                continue;
            }
            if let Some(id) = session_id {
                let path = dir.path().join(format!("{id}.jsonl"));
                if let Some(file) = transcript_file(path, id.to_owned()).await? {
                    files.push(file);
                }
            } else {
                let mut entries = fs::read_dir(dir.path()).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let name = entry.file_name();
                    let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".jsonl"))
                    else {
                        continue;
                    };
                    if valid_session_id(id)
                        && let Some(file) = transcript_file(entry.path(), id.to_owned()).await?
                    {
                        files.push(file);
                    }
                }
            }
        }
        files.sort_unstable_by(|left, right| {
            right
                .updated
                .cmp(&left.updated)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(files)
    }

    pub async fn list(&self, project: &Path, limit: usize) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();
        for file in self.files(project, None).await? {
            if let Some(transcript) = read_transcript(&file, project, false).await? {
                sessions.push(Session {
                    id: file.id,
                    title: transcript.title,
                    updated_at: DateTime::<Utc>::from(file.updated)
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                });
                if sessions.len() == limit {
                    break;
                }
            }
        }
        Ok(sessions)
    }

    pub async fn read(
        &self,
        project: &Path,
        session_id: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Page> {
        let offset = match cursor {
            None => 0,
            Some(cursor)
                if !cursor.is_empty() && cursor.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                cursor
                    .parse::<usize>()
                    .context("Use the nextCursor returned by Claude Code for this session.")?
            }
            Some(_) => bail!("Use the nextCursor returned by Claude Code for this session."),
        };
        for file in self.files(project, Some(session_id)).await? {
            if let Some(transcript) = read_transcript(&file, project, true).await? {
                return Ok(transcript.page(offset, limit));
            }
        }
        bail!("{SESSION_NOT_FOUND}");
    }
}

async fn transcript_file(path: PathBuf, id: String) -> Result<Option<TranscriptFile>> {
    match fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.is_file() => Ok(Some(TranscriptFile {
            id,
            path,
            updated: metadata.modified()?,
        })),
        Ok(_) => Ok(None),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error).context("Could not inspect a Claude Code transcript."),
    }
}

async fn read_transcript(
    file: &TranscriptFile,
    project: &Path,
    include_history: bool,
) -> Result<Option<Transcript>> {
    let file_handle = match File::open(&file.path).await {
        Ok(file) => file,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error).context("Could not open a Claude Code transcript."),
    };
    let mut reader = BufReader::new(file_handle);
    let mut transcript = Transcript::default();
    let (mut first_prompt, mut last_prompt, mut summary, mut custom_title) =
        (String::new(), String::new(), String::new(), String::new());
    let mut verified = false;
    while let Some(record) = transcript_record(&mut reader).await? {
        if record["isSidechain"] == true
            || record["sessionId"].as_str().is_some_and(|id| id != file.id)
        {
            continue;
        }
        let kind = record["type"].as_str().unwrap_or_default();
        let main_message = matches!(kind, "user" | "assistant");
        if main_message && let Some(cwd) = record["cwd"].as_str() {
            if Path::new(cwd) != project {
                return Ok(None);
            }
            verified = true;
        }
        match kind {
            "custom-title" => {
                custom_title = excerpt(record["customTitle"].as_str().unwrap_or_default(), 240)
            }
            "summary" | "ai-title" => {
                summary = excerpt(
                    record["summary"]
                        .as_str()
                        .or(record["aiTitle"].as_str())
                        .unwrap_or_default(),
                    240,
                );
            }
            "last-prompt" => {
                last_prompt = excerpt(record["lastPrompt"].as_str().unwrap_or_default(), 240)
            }
            _ => {}
        }
        let role = match kind {
            "user" if record["isMeta"] != true => Some("user"),
            "assistant" => Some("assistant"),
            _ => None,
        };
        let mut text = String::new();
        if role.is_some() && (include_history || (kind == "user" && first_prompt.is_empty())) {
            text = excerpt(&message_text(&record["message"]["content"]), 3000);
            if kind == "user" && first_prompt.is_empty() && !text.trim().is_empty() {
                first_prompt = excerpt(&text, 240);
            }
        }
        if include_history && let Some(uuid) = record["uuid"].as_str() {
            let parent = record["parentUuid"]
                .as_str()
                .or(record["logicalParentUuid"].as_str())
                .map(str::to_owned);
            transcript
                .entries
                .insert(uuid.to_owned(), Entry { parent, role, text });
            if role.is_some() {
                transcript.leaf = Some(uuid.to_owned());
            }
        }
    }
    if !verified {
        return Ok(None);
    }
    transcript.title = [custom_title, last_prompt, summary, first_prompt]
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .unwrap_or_default();
    Ok(Some(transcript))
}

impl Transcript {
    fn page(mut self, offset: usize, limit: usize) -> Page {
        let mut entries = Vec::new();
        let mut current = self.leaf.take();
        // Removing visited entries handles cycles in damaged transcripts and ignores old branches.
        while let Some(entry) = current.and_then(|uuid| self.entries.remove(&uuid)) {
            current = entry.parent;
            if let Some(role) = entry.role {
                entries.push(Message {
                    role,
                    text: entry.text,
                });
            }
        }
        let total = entries.len();
        let messages = entries
            .into_iter()
            .rev()
            .skip(offset)
            .take(limit)
            .filter(|message| !message.text.is_empty())
            .collect();
        let next = offset.saturating_add(limit);
        Page {
            messages,
            next_cursor: (next < total).then(|| next.to_string()),
            order: "oldest-page-first",
        }
    }
}

pub async fn delegate(binary: &OsStr, project: &Path, prompt: &str) -> Result<Delegation> {
    let args = [
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--restricted",
        "--permission-mode",
        "dontAsk",
        "--tools",
        "Read,Glob,Grep",
        "--allowedTools",
        "Read,Glob,Grep",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        r#"{"mcpServers":{}}"#,
        "--disable-slash-commands",
        "--no-chrome",
        "--max-turns",
        "12",
    ];
    let mut process = HarnessProcess::spawn(binary, project, &args, "Claude Code")?;
    process
        .send(&json!({
            "type": "user", "session_id": "", "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{"type": "text", "text": prompt}]}
        }))
        .await?;
    process.close_input();
    let mut result = None;
    while let Some(frame) = process.frame().await? {
        if frame["type"] == "result" {
            result = Some(frame);
        }
    }
    let status = process.wait().await?;
    let result = result.context(
        "Claude Code ended before returning a result. Check that the CLI supports --restricted.",
    )?;
    ensure!(
        status.success() && result["subtype"] == "success" && result["is_error"] != true,
        "Claude Code did not complete the task. Check authentication and provider access."
    );
    let text = result["result"]
        .as_str()
        .filter(|text| !text.is_empty())
        .context("Claude Code returned an empty answer.")?;
    let id = result["session_id"]
        .as_str()
        .context("Claude Code returned no session ID.")?;
    Ok(Delegation {
        session_id: id.to_owned(),
        text: excerpt(text, 16000),
    })
}
