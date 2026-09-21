//! Making the parent delegate.
//!
//! The Explore condition measures a subagent, so the parent must not do the
//! exploring. Taking the read-only tools away does not work: `--tools` bounds
//! the whole session, so a parent with only `Task` spawns a subagent that has
//! no tools either and reports nothing. Asking it in the prompt does not work
//! either — it reads anyway.
//!
//! What does work is a `PreToolUse` hook. Claude Code 2.1.278 puts `agent_type`
//! and `agent_id` in the hook's input for a subagent's tool call and leaves
//! them out for the parent's, so a hook that refuses a call with no
//! `agent_type` refuses exactly the parent's own reads. Exit 2 blocks the call
//! and hands the reason back to the model, which then does the one thing left
//! to it and delegates.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

/// The tools the parent is not allowed to use itself, as a hook matcher.
pub const BLOCKED: &str = "Read|Glob|Grep";

/// What the model is told when its call is blocked. It is the reason the hook
/// prints on stderr, and the only instruction that reaches the parent from
/// here.
pub const REASON: &str = "the parent must delegate to the Explore agent";

/// The hook script: POSIX shell and `grep`, so it needs nothing installed.
///
/// A subagent's call carries `agent_type`; the parent's does not, and that is
/// the whole test. Anything it cannot make sense of is allowed, because a hook
/// that fails closed would block the subagent too and measure nothing.
pub fn script() -> String {
    format!(
        "#!/bin/sh\n\
         # Written by eval-agent. Claude Code runs this before every {BLOCKED}\n\
         # call and hands it the call as JSON on stdin. A subagent's call has an\n\
         # `agent_type`; the parent's has none, and only the parent is refused.\n\
         # Exit 2 blocks the call and gives the model the reason below.\n\
         if grep -q '\"agent_type\"[[:space:]]*:[[:space:]]*\"' ; then\n\
         \texit 0\n\
         fi\n\
         echo '{REASON}' >&2\n\
         exit 2\n"
    )
}

/// The settings file that installs the script as a `PreToolUse` hook.
pub fn settings(script: &Path) -> serde_json::Value {
    json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": BLOCKED,
                "hooks": [{
                    "type": "command",
                    "command": script.display().to_string(),
                }],
            }],
        },
    })
}

/// Writes the script and the settings under `out` and returns the settings
/// path, to pass to `claude --settings`.
pub fn install(out: &Path) -> Result<PathBuf, String> {
    let script_path = out.join("delegate-only.sh");
    fs::write(&script_path, script())
        .map_err(|error| format!("{}: {error}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("{}: {error}", script_path.display()))?;
    }
    let settings_path = out.join("explore-settings.json");
    let text =
        serde_json::to_string_pretty(&settings(&script_path)).map_err(|error| error.to_string())?;
    fs::write(&settings_path, text)
        .map_err(|error| format!("{}: {error}", settings_path.display()))?;
    Ok(settings_path)
}

/// The settings file for a condition with no subagent: no hooks, and there to
/// be passed with `--setting-sources ""` so that both agent conditions run
/// under the same configuration.
pub fn install_plain(out: &Path) -> Result<PathBuf, String> {
    let path = out.join("agent-settings.json");
    fs::write(&path, "{}\n").map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    #[test]
    fn the_settings_install_the_script_on_the_read_only_tools() {
        let settings = settings(Path::new("/run/delegate-only.sh"));
        let hooks = &settings["hooks"]["PreToolUse"];
        assert_eq!(hooks.as_array().expect("one matcher").len(), 1);
        assert_eq!(hooks[0]["matcher"], BLOCKED);
        assert_eq!(hooks[0]["hooks"][0]["type"], "command");
        assert_eq!(hooks[0]["hooks"][0]["command"], "/run/delegate-only.sh");
        // `Task` is not blocked: delegating is the one thing the parent may do.
        assert!(!BLOCKED.contains("Task"), "{BLOCKED}");
    }

    /// The script is what decides whether a run measures a subagent or a
    /// parent pretending to be one, so it is run rather than read.
    #[cfg(unix)]
    #[test]
    fn the_script_blocks_the_parent_and_lets_a_subagent_through() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let out = TempDir::new("hook");
        let settings = install(out.path()).expect("the hook");
        assert!(settings.is_file());
        let script = out.path().join("delegate-only.sh");

        let run = |input: &str| {
            let mut child = Command::new(&script)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the hook script runs");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("the hook input");
            let output = child.wait_with_output().expect("the hook finishes");
            (
                output.status.code().expect("an exit code"),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        };

        // The parent's own call: no `agent_type` anywhere in the input.
        let (code, reason) = run(
            r#"{"session_id":"S","tool_name":"Read","tool_input":{"file_path":"/wiki/index.md"}}"#,
        );
        assert_eq!(code, 2, "a blocked call is exit 2, not {code}");
        assert!(reason.contains(REASON), "{reason}");

        // A subagent's call carries it, and is allowed through in silence.
        let (code, reason) = run(
            r#"{"session_id":"S","agent_type":"Explore","agent_id":"a1","tool_name":"Read","tool_input":{"file_path":"/wiki/index.md"}}"#,
        );
        assert_eq!(code, 0, "the subagent was blocked: {reason}");
        assert!(reason.is_empty(), "{reason}");

        // Whitespace in the JSON is still the subagent.
        let (code, _) = run("{\n  \"agent_type\" : \"Explore\",\n  \"tool_name\": \"Grep\"\n}");
        assert_eq!(code, 0);

        // A file whose name merely mentions the key is still the parent.
        let (code, _) =
            run(r#"{"tool_name":"Read","tool_input":{"file_path":"/w/agent_type.md"}}"#);
        assert_eq!(code, 2);
    }
}
