use std::fs;
use std::process::{Command, Stdio};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chimera"));
    command.env_remove("CHIMERA_PROJECT").stdin(Stdio::null());
    command
}

#[test]
fn help_and_noninteractive_validation_do_not_register() {
    for args in [vec!["--help"], vec!["register", "--help"]] {
        let output = command().args(args).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("register"));
    }
    for args in [
        vec!["register"],
        vec!["register", "--agent", "unknown"],
        vec!["register", "--agent"],
        vec!["register", "--project"],
        vec!["register", "--project", "--agent=opencode"],
        vec!["register", "--agent", "codex", "--agent", "claude"],
        vec!["register", "--unknown"],
    ] {
        let output = command().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn opencode_registration_defaults_to_cwd_and_is_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let output = command()
            .current_dir(dir.path())
            .args(["register", "--agent=opencode"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("opencode.json")).unwrap()).unwrap();
    assert_eq!(
        config["mcp"]["servers"]["chimera"]["command"],
        serde_json::json!([
            fs::canonicalize(env!("CARGO_BIN_EXE_chimera")).unwrap(),
            "--project",
            dir.path().canonicalize().unwrap()
        ])
    );
}

#[cfg(unix)]
#[test]
fn harness_registration_passes_literal_paths_and_propagates_failure() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project with spaces; literal");
    fs::create_dir(&project).unwrap();
    let binary = dir.path().join("fake harness");
    fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$CHIMERA_TEST_RECORD\"\nexit \"${CHIMERA_TEST_EXIT:-0}\"\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let record = dir.path().join("args");
    for (agent, variable, prefix) in [
        (
            "codex",
            "CHIMERA_CODEX_BIN",
            vec!["mcp", "add", "chimera", "--"],
        ),
        (
            "claude",
            "CHIMERA_CLAUDE_BIN",
            vec![
                "mcp",
                "add",
                "--scope",
                "local",
                "--transport",
                "stdio",
                "chimera",
                "--",
            ],
        ),
    ] {
        for exit in ["0", "7"] {
            let output = command()
                .args(["register", "--agent", agent, "--project"])
                .arg(&project)
                .env(variable, &binary)
                .env("CHIMERA_TEST_RECORD", &record)
                .env("CHIMERA_TEST_EXIT", exit)
                .output()
                .unwrap();
            assert_eq!(output.status.success(), exit == "0");
            let saved = fs::read_to_string(&record).unwrap();
            let lines: Vec<_> = saved.lines().collect();
            assert_eq!(lines[0], project.canonicalize().unwrap().to_str().unwrap());
            assert_eq!(&lines[1..1 + prefix.len()], prefix);
            assert_eq!(
                lines[1 + prefix.len()],
                fs::canonicalize(env!("CARGO_BIN_EXE_chimera"))
                    .unwrap()
                    .to_str()
                    .unwrap()
            );
            assert_eq!(lines[2 + prefix.len()], "--project");
            assert_eq!(
                lines[3 + prefix.len()],
                project.canonicalize().unwrap().to_str().unwrap()
            );
            assert_eq!(lines.len(), prefix.len() + 4);
        }
    }
    let output = command()
        .args(["register", "--agent", "codex"])
        .env("CHIMERA_CODEX_BIN", dir.path().join("missing"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("CHIMERA_CODEX_BIN"));
}

#[test]
fn project_flag_overrides_environment_and_accepts_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    let from_env = dir.path().join("environment");
    let from_flag = dir.path().join("explicit");
    fs::create_dir(&from_env).unwrap();
    fs::create_dir(&from_flag).unwrap();
    let output = command()
        .current_dir(dir.path())
        .env("CHIMERA_PROJECT", &from_env)
        .args(["register", "--agent", "opencode", "--project=explicit"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(from_flag.join("opencode.json").exists());
    assert!(!from_env.join("opencode.json").exists());
    let output = command()
        .env("CHIMERA_PROJECT", &from_env)
        .args(["register", "--agent", "opencode"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(from_env.join("opencode.json").exists());
    let output = command()
        .current_dir(dir.path())
        .args(["register", "--agent", "opencode", "--project", "missing"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn opencode_registration_preserves_jsonc_and_is_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.jsonc");
    fs::write(
        &path,
        r#"{
  // Local servers for this project.
  "mcp": {
    "servers": {
      "existing": {"type": "local", "command": ["other"]},
    },
  },
}
"#,
    )
    .unwrap();
    let register = || {
        let output = command()
            .current_dir(dir.path())
            .args(["register", "--agent", "opencode"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    register();
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains("// Local servers for this project."));
    assert!(!dir.path().join("opencode.json").exists());
    let config: serde_json::Value =
        jsonc_parser::parse_to_serde_value(&saved, &Default::default()).unwrap();
    let servers = &config["mcp"]["servers"];
    assert_eq!(servers["existing"]["command"], serde_json::json!(["other"]));
    assert_eq!(
        servers["chimera"]["command"],
        serde_json::json!([
            fs::canonicalize(env!("CARGO_BIN_EXE_chimera")).unwrap(),
            "--project",
            dir.path().canonicalize().unwrap()
        ])
    );
    register();
    assert_eq!(fs::read_to_string(path).unwrap(), saved);
}
