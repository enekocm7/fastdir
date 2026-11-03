//! User-defined commands shown in the context menu's *Run* submenu.
//!
//! An action is a program, its arguments and a working folder, with
//! `{path}`, `{name}`, `{parent}` and `{ext}` placeholders. Everything runs
//! directly (no shell) unless the action asks for one, so paths with spaces
//! and non-ASCII characters need no quoting: each argument is passed to the
//! child process as one value. Shell actions get their placeholder values
//! quoted for `cmd.exe` / POSIX `sh`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The values substituted into an action's templates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionContext {
    /// The selected file or folder.
    pub path: PathBuf,
    /// The folder containing it (itself for a drive root).
    pub parent: PathBuf,
    /// The item's file name (`C:\Users` for the `C:\Users` folder).
    pub name: String,
    /// Extension without the dot; empty for folders and extension-less files.
    pub ext: String,
    /// The item is a folder.
    pub is_dir: bool,
}

impl ActionContext {
    pub fn new(path: &Path, is_dir: bool) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
        let ext = if is_dir {
            String::new()
        } else {
            path.extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Self {
            path: path.to_path_buf(),
            parent,
            name,
            ext,
            is_dir,
        }
    }

    /// The folder a command runs in when no `working_dir` is configured: the
    /// folder itself, or the folder containing a selected file.
    pub fn working_dir(&self) -> &Path {
        if self.is_dir {
            &self.path
        } else {
            &self.parent
        }
    }

    fn value(&self, key: &str) -> Option<Cow<'_, str>> {
        match key {
            "path" => Some(self.path.to_string_lossy()),
            "parent" => Some(self.parent.to_string_lossy()),
            "name" => Some(Cow::Borrowed(&self.name)),
            "ext" => Some(Cow::Borrowed(&self.ext)),
            _ => None,
        }
    }
}

/// Replace `{path}`, `{name}`, `{parent}` and `{ext}` in `template`. Unknown
/// placeholders such as `{foo}` are left untouched.
pub fn expand(template: &str, ctx: &ActionContext) -> String {
    expand_with(template, ctx, false)
}

/// [`expand`] with placeholder values quoted for the platform shell.
fn expand_for_shell(template: &str, ctx: &ActionContext) -> String {
    expand_with(template, ctx, true)
}

fn expand_with(template: &str, ctx: &ActionContext, quote: bool) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        match ctx.value(key) {
            Some(value) if quote => out.push_str(&shell_quote(&value)),
            Some(value) => out.push_str(&value),
            None => {
                out.push('{');
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Quote one placeholder value for the platform shell. Values that need no
/// quoting are returned unchanged.
pub fn shell_quote(value: &str) -> String {
    #[cfg(windows)]
    {
        if !value.is_empty() && !value.contains([' ', '\t', '"', '&', '|', '<', '>', '^', '(', ')'])
        {
            return value.to_string();
        }
        // cmd.exe: a doubled quote inside a quoted argument is a literal quote.
        format!("\"{}\"", value.replace('"', "\"\""))
    }
    #[cfg(not(windows))]
    {
        if !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_alphanumeric() || "-_./\\:@,+=".contains(c))
        {
            return value.to_string();
        }
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// An expanded command, ready to spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandLine {
    pub program: String,
    pub args: Vec<String>,
    /// `None` runs in the app's current folder (only when `working_dir` is
    /// deliberately empty and the target has no folder).
    pub cwd: Option<PathBuf>,
    /// Run through `cmd /C` / `sh -c` instead of spawning the program directly.
    pub shell: bool,
}

impl CommandLine {
    /// Expand an action's templates for one selected item. An empty
    /// `working_dir` means "the item's folder" (the item itself for folders).
    pub fn expand(
        program: &str,
        args: &[String],
        working_dir: &str,
        shell: bool,
        ctx: &ActionContext,
    ) -> Self {
        let sub = |text: &str| {
            if shell {
                expand_for_shell(text, ctx)
            } else {
                expand(text, ctx)
            }
        };
        let cwd_template = working_dir.trim();
        let cwd = if cwd_template.is_empty() {
            Some(ctx.working_dir().to_path_buf())
        } else {
            Some(PathBuf::from(sub(cwd_template)))
        };
        Self {
            program: sub(program),
            args: args.iter().map(|arg| sub(arg)).collect(),
            cwd,
            shell,
        }
    }

    /// The command text handed to the shell (or a readable form of the argv
    /// for a direct command). Used for the confirmation dialog and output.
    pub fn display(&self) -> String {
        if self.shell {
            let mut text = self.program.clone();
            for arg in &self.args {
                text.push(' ');
                text.push_str(arg);
            }
            return text;
        }
        let mut text = display_arg(&self.program);
        for arg in &self.args {
            text.push(' ');
            text.push_str(&display_arg(arg));
        }
        text
    }

    /// Spawn the command, wait for it and capture its output.
    pub fn run(&self) -> Result<RunOutput, String> {
        let mut command = if self.shell {
            let text = {
                let mut text = self.program.clone();
                for arg in &self.args {
                    text.push(' ');
                    text.push_str(arg);
                }
                text
            };
            #[cfg(windows)]
            {
                let mut c = Command::new("cmd");
                c.args(["/C", &text]);
                c
            }
            #[cfg(not(windows))]
            {
                let mut c = Command::new("sh");
                c.args(["-c", &text]);
                c
            }
        } else {
            let mut c = Command::new(&self.program);
            c.args(&self.args);
            c
        };
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // GUI app: do not flash a console window for every command. Tools
            // that should open a window (a terminal) spawn it themselves.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command
            .output()
            .map_err(|e| format!("{}: {e}", self.program))?;
        Ok(RunOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// What a finished command produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl RunOutput {
    /// Both streams, one after the other, for the output dialog.
    pub fn combined(&self) -> String {
        match (self.stdout.trim_end(), self.stderr.trim_end()) {
            ("", "") => String::new(),
            ("", err) => err.to_string(),
            (out, "") => out.to_string(),
            (out, err) => format!("{out}\n{err}"),
        }
    }
}

/// Quote one argv element so the display line can be pasted into a shell.
fn display_arg(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t', '"']) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

/// Whether an action's optional `match` pattern applies to this item.
///
/// A pattern with `*` / `?` matches the item's name. A plain pattern matches
/// the name of a selected file, and for a folder it must name something the
/// folder contains (`Cargo.toml`, `.git`).
pub fn matches_pattern(pattern: &str, path: &Path, is_dir: bool, name: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return true;
    }
    if pattern.contains(['*', '?']) {
        return crate::filter::glob_match(pattern, name);
    }
    if is_dir {
        path.join(pattern).exists()
    } else {
        name.to_lowercase() == pattern.to_lowercase()
    }
}

/// Split the arguments field of the settings dialog. Double or single quotes
/// keep a space inside one argument (`--dir="C:\my folder"`).
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in text.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
                started = true;
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started || !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started || !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ActionContext {
        ActionContext::new(
            Path::new(r"C:\Users\Eneko\My Project\notes file.txt"),
            false,
        )
    }

    #[test]
    fn placeholders_expand() {
        let ctx = ctx();
        assert_eq!(
            expand("{path}|{parent}|{name}|{ext}", &ctx),
            r"C:\Users\Eneko\My Project\notes file.txt|C:\Users\Eneko\My Project|notes file.txt|txt"
        );
        // Unknown placeholders stay as written.
        assert_eq!(
            expand("{path} {foo}", &ctx),
            r"C:\Users\Eneko\My Project\notes file.txt {foo}"
        );
        // A folder has no extension and is its own parent for `{parent}`.
        let dir = ActionContext::new(Path::new(r"C:\Users\Eneko\My Project"), true);
        assert_eq!(expand("{ext}", &dir), "");
        assert_eq!(expand("{name}", &dir), "My Project");
        assert_eq!(expand("{parent}", &dir), r"C:\Users\Eneko");
    }

    #[test]
    fn direct_commands_keep_paths_whole() {
        let line = CommandLine::expand(
            "code",
            &["--reuse-window".to_string(), "{path}".to_string()],
            "",
            false,
            &ctx(),
        );
        assert_eq!(line.args[1], r"C:\Users\Eneko\My Project\notes file.txt");
        assert_eq!(
            line.cwd.as_deref(),
            Some(Path::new(r"C:\Users\Eneko\My Project"))
        );
        assert!(
            line.display()
                .contains("\"C:\\Users\\Eneko\\My Project\\notes file.txt\"")
        );
    }

    #[test]
    fn folder_targets_are_their_own_working_folder() {
        let dir = ActionContext::new(Path::new(r"C:\code\thing"), true);
        let line = CommandLine::expand("cargo", &["clean".to_string()], "", false, &dir);
        assert_eq!(line.cwd.as_deref(), Some(Path::new(r"C:\code\thing")));
        let explicit = CommandLine::expand("ls", &[], "{parent}", false, &dir);
        assert_eq!(explicit.cwd.as_deref(), Some(Path::new(r"C:\code")));
    }

    #[test]
    fn shell_placeholders_are_quoted() {
        let line = CommandLine::expand("echo", &["{path}".to_string()], "{path}", true, &ctx());
        assert!(line.shell);
        let path = r"C:\Users\Eneko\My Project\notes file.txt";
        #[cfg(windows)]
        assert_eq!(line.args[0], format!("\"{path}\""));
        #[cfg(not(windows))]
        assert_eq!(line.args[0], format!("'{path}'"));
        assert!(line.cwd.is_some());
    }

    #[test]
    fn shell_quoting_handles_quotes() {
        #[cfg(windows)]
        {
            assert_eq!(shell_quote("plain"), "plain");
            assert_eq!(shell_quote("two words"), "\"two words\"");
            assert_eq!(shell_quote("a\"b"), "\"a\"\"b\"");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(shell_quote("plain"), "plain");
            assert_eq!(shell_quote("two words"), "'two words'");
            assert_eq!(shell_quote("it's"), "'it'\\''s'");
        }
    }

    #[test]
    fn unicode_paths_survive_expansion() {
        let ctx = ActionContext::new(Path::new(r"C:\Users\Eneko\Café büro\naïve 文件.txt"), false);
        assert_eq!(
            expand("{path}", &ctx),
            r"C:\Users\Eneko\Café büro\naïve 文件.txt"
        );
        let line = CommandLine::expand("code", &["{path}".to_string()], "", false, &ctx);
        assert_eq!(line.args[0], r"C:\Users\Eneko\Café büro\naïve 文件.txt");
        let shell = CommandLine::expand("echo", &["{name}".to_string()], "", true, &ctx);
        assert!(shell.args[0].contains("naïve 文件.txt"));
    }

    #[test]
    fn args_split_on_spaces_and_keep_quotes() {
        assert_eq!(split_args("clean --verbose"), ["clean", "--verbose"]);
        assert_eq!(
            split_args(r#"--dir="C:\my folder" --flag"#),
            [r"--dir=C:\my folder", "--flag"]
        );
        assert_eq!(split_args("'a b' c"), ["a b", "c"]);
        assert_eq!(split_args("   "), Vec::<String>::new());
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("empty \"\""), ["empty", ""]);
    }

    #[test]
    fn match_patterns_use_names_and_folder_contents() {
        let tmp = std::env::temp_dir().join("fastdir-action-match-test");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("Cargo.toml"), "[package]\n").unwrap();
        assert!(matches_pattern("Cargo.toml", &tmp, true, "project"));
        assert!(!matches_pattern("package.json", &tmp, true, "project"));
        assert!(matches_pattern("", &tmp, true, "project"));
        assert!(matches_pattern("*.toml", &tmp, true, "Cargo.toml"));
        assert!(!matches_pattern("*.toml", &tmp, true, "Cargo.lock"));
        // Files compare by name.
        let file = tmp.join("notes.txt");
        assert!(matches_pattern("NOTES.TXT", &file, false, "notes.txt"));
        assert!(matches_pattern("*.txt", &file, false, "notes.txt"));
        assert!(!matches_pattern("*.md", &file, false, "notes.txt"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn output_combines_streams() {
        let out = RunOutput {
            success: true,
            code: Some(0),
            stdout: "done\n".into(),
            stderr: String::new(),
        };
        assert_eq!(out.combined(), "done");
        let err = RunOutput {
            success: false,
            code: Some(1),
            stdout: "out".into(),
            stderr: "bad\n".into(),
        };
        assert_eq!(err.combined(), "out\nbad");
        assert_eq!(RunOutput::default().combined(), "");
    }
}
