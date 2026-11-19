use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use jsonc_parser::cst::{CstObject, CstObjectProp, CstRootNode};
use jsonc_parser::{ParseOptions, json};
use serde_json::{Value, json as json_value};

pub const HELP: &str =
    "Usage: chimera register [--agent <codex|claude|opencode>] [--project <directory>]
Register Chimera in one agent. Without --agent, choose interactively.
Project defaults to CHIMERA_PROJECT, then the current directory.
Codex uses global scope. Claude and OpenCode use project scope.
Use q, EOF, or Ctrl+C to cancel the picker without making changes.";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Agent {
    Codex,
    Claude,
    Opencode,
}

impl Agent {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "opencode" => Ok(Self::Opencode),
            _ => bail!("Choose codex, claude, or opencode."),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Opencode => "OpenCode",
        }
    }
}

fn choose(input: &mut impl BufRead, output: &mut impl Write) -> Result<Option<Agent>> {
    writeln!(
        output,
        "Register Chimera in:\n  1) Codex\n  2) Claude Code\n  3) OpenCode"
    )?;
    loop {
        write!(output, "Agent (1-3, or q to cancel): ")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 || matches!(line.trim(), "q" | "quit") {
            writeln!(output, "Registration canceled.")?;
            return Ok(None);
        }
        let name = match line.trim() {
            "1" => "codex",
            "2" => "claude",
            "3" => "opencode",
            name => name,
        };
        match Agent::parse(name) {
            Ok(agent) => return Ok(Some(agent)),
            Err(error) => writeln!(output, "{error}")?,
        }
    }
}

pub fn run(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let mut agent = None;
    let mut project = None;
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{HELP}");
            return Ok(());
        }
        let text = arg.to_str().context("Arguments must be valid Unicode.")?;
        let (flag, inline) = text
            .split_once('=')
            .map_or((text, None), |(k, v)| (k, Some(v)));
        ensure!(
            matches!(flag, "--agent" | "--project"),
            "Unknown argument {text}.\n{HELP}"
        );
        let value = inline
            .map(OsString::from)
            .or_else(|| {
                args.next()
                    .filter(|value| !value.as_encoded_bytes().starts_with(b"-"))
            })
            .with_context(|| format!("{flag} requires a value."))?;
        ensure!(!value.is_empty(), "{flag} requires a value.");
        match flag {
            "--agent" => {
                ensure!(agent.is_none(), "Only one --agent argument is supported.");
                let value = value.to_str().context("Agent must be valid Unicode.")?;
                agent = Some(Agent::parse(value)?);
            }
            _ => {
                ensure!(
                    project.is_none(),
                    "Only one --project argument is supported."
                );
                project = Some(value);
            }
        }
    }
    let project = project
        .or_else(|| env::var_os("CHIMERA_PROJECT"))
        .map(PathBuf::from)
        .map_or_else(env::current_dir, Ok)?
        .canonicalize()
        .context("Could not resolve the project directory.")?;
    ensure!(project.is_dir(), "The project path must be a directory.");
    ensure!(
        project.to_str().is_some(),
        "The project directory must have a valid Unicode path."
    );
    let agent = match agent {
        Some(agent) => agent,
        None => {
            ensure!(
                io::stdin().is_terminal() && io::stderr().is_terminal(),
                "Non-interactive registration requires --agent <codex|claude|opencode>."
            );
            match choose(&mut io::stdin().lock(), &mut io::stderr().lock())? {
                Some(agent) => agent,
                None => return Ok(()),
            }
        }
    };
    let executable = env::current_exe()
        .context("Could not locate the Chimera executable.")?
        .canonicalize()
        .context("Could not resolve the Chimera executable.")?;
    match agent {
        Agent::Opencode => register_opencode(&project, &executable)?,
        Agent::Codex | Agent::Claude => {
            let (variable, default) = match agent {
                Agent::Codex => ("CHIMERA_CODEX_BIN", "codex"),
                _ => ("CHIMERA_CLAUDE_BIN", "claude"),
            };
            let binary = env::var_os(variable)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.into());
            let mut command = Command::new(&binary);
            command.current_dir(&project).args(["mcp", "add"]);
            if agent == Agent::Claude {
                command.args(["--scope", "local", "--transport", "stdio"]);
            }
            let status = command
                .arg("chimera")
                .arg("--")
                .arg(&executable)
                .arg("--project")
                .arg(&project)
                .status()
                .with_context(|| {
                    format!("Could not run {default}. Install it or set {variable}.")
                })?;
            ensure!(
                status.success(),
                "{default} registration failed ({status})."
            );
        }
    }
    println!(
        "Registered Chimera in {} for {}. Start a new client session.",
        agent.label(),
        project.display()
    );
    Ok(())
}

fn register_opencode(project: &Path, executable: &Path) -> Result<()> {
    let json_path = project.join("opencode.json");
    let jsonc_path = project.join("opencode.jsonc");
    let json_permissions = config_permissions(&json_path)?;
    let jsonc_permissions = config_permissions(&jsonc_path)?;
    ensure!(
        json_permissions.is_none() || jsonc_permissions.is_none(),
        "Both opencode.json and opencode.jsonc exist. Choose which file to register Chimera in manually."
    );
    let (path, permissions, other_path) = if jsonc_permissions.is_some() {
        (&jsonc_path, jsonc_permissions, &json_path)
    } else {
        (&json_path, json_permissions, &jsonc_path)
    };
    let original = if permissions.is_some() {
        fs::read_to_string(path).with_context(|| format!("Could not read {}.", path.display()))?
    } else {
        String::from("{}\n")
    };

    // The CST preserves existing comments and formatting. Allow only JSONC syntax.
    let options = ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    };
    let config = CstRootNode::parse(&original, &options)
        .with_context(|| format!("{} must contain valid JSON or JSONC.", path.display()))?;
    let root = config
        .object_value()
        .context("OpenCode config must be an object.")?;
    let mcp = config_object(&root, "mcp")?;
    let servers = config_object(&mcp, "servers").context("Invalid mcp.servers config.")?;
    let executable = executable
        .to_str()
        .context("The Chimera executable must have a valid Unicode path.")?;
    let project_path = project
        .to_str()
        .context("The project directory must have a valid Unicode path.")?;
    if let Some(existing) = config_property(&servers, "chimera")? {
        let existing: Value = jsonc_parser::parse_to_serde_value(
            &existing
                .value()
                .context("Missing Chimera registration value.")?
                .to_string(),
            &options,
        )?;
        ensure!(
            existing
                == json_value!({"type": "local", "command": [executable, "--project", project_path]}),
            "A different Chimera registration exists in {}. The file was not changed.",
            path.display()
        );
        return Ok(());
    }
    let entry = json!({"type": "local", "command": [executable, "--project", project_path]});
    servers.append("chimera", entry);

    let mut temporary = tempfile::NamedTempFile::new_in(project)?;
    if let Some(permissions) = &permissions {
        temporary.as_file().set_permissions(permissions.clone())?;
    }
    write!(temporary, "{config}")?;
    temporary.as_file().sync_all()?;
    ensure!(
        config_permissions(other_path)?.is_none(),
        "OpenCode config changed during registration. Retry the command."
    );
    if permissions.is_some() {
        ensure!(
            config_permissions(path)? == permissions && fs::read_to_string(path)? == original,
            "OpenCode config changed during registration. Retry the command."
        );
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    }
    .with_context(|| format!("Could not save {}.", path.display()))?;
    Ok(())
}

fn config_permissions(path: &Path) -> Result<Option<fs::Permissions>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "{} must be a regular file.",
                path.display()
            );
            Ok(Some(metadata.permissions()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not inspect {}.", path.display())),
    }
}

fn config_property(object: &CstObject, name: &str) -> Result<Option<CstObjectProp>> {
    let mut matches = object
        .properties()
        .into_iter()
        .filter(|property| property.decoded_name().as_deref() == Some(name));
    let property = matches.next();
    ensure!(
        matches.next().is_none(),
        "Duplicate {name} keys in OpenCode config."
    );
    Ok(property)
}

fn config_object(object: &CstObject, name: &str) -> Result<CstObject> {
    config_property(object, name)?;
    object
        .object_value_or_create(name)
        .with_context(|| format!("{name} must be an object."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_selects_retries_and_cancels() {
        for (input, expected) in [
            ("1\n", Some(Agent::Codex)),
            ("bad\nclaude\n", Some(Agent::Claude)),
            ("3\n", Some(Agent::Opencode)),
            ("q\n", None),
            ("", None),
        ] {
            assert_eq!(
                choose(&mut input.as_bytes(), &mut Vec::new()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn opencode_preserves_config_and_refuses_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        fs::write(
            &path,
            r#"{"theme":"dark","mcp":{"servers":{"other":{"enabled":true}}}}"#,
        )
        .unwrap();
        register_opencode(dir.path(), Path::new("/chimera")).unwrap();
        let saved = fs::read(&path).unwrap();
        let config: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcp"]["servers"]["other"]["enabled"], true);
        assert_eq!(
            config["mcp"]["servers"]["chimera"]["command"],
            json_value!(["/chimera", "--project", dir.path()])
        );
        register_opencode(dir.path(), Path::new("/chimera")).unwrap();
        assert!(register_opencode(dir.path(), Path::new("/different")).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
    }

    #[test]
    fn opencode_rejects_malformed_or_ambiguous_config_without_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        for source in [
            "{bad",
            "[]",
            r#"{"mcp": {} "theme": "dark"}"#,
            r#"{"mcp":{},"mcp":{}}"#,
            r#"{"mcp":false}"#,
            r#"{"mcp":{"servers":[]}}"#,
        ] {
            fs::write(&path, source).unwrap();
            assert!(register_opencode(dir.path(), Path::new("/chimera")).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), source);
        }
        fs::write(&path, "{}").unwrap();
        let jsonc_path = dir.path().join("opencode.jsonc");
        fs::write(&jsonc_path, "{}").unwrap();
        assert!(register_opencode(dir.path(), Path::new("/chimera")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(fs::read_to_string(&jsonc_path).unwrap(), "{}");
    }
}
