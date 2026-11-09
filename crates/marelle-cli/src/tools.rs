// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Local tools exposed to the model. Every filesystem operation is confined
//! to configured roots; shell and network calls have separate permissions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use marelle_config::Config;
use marelle_core::ToolCall;
use marelle_providers::ToolDefinition;
use serde_json::{Value, json};

const MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_TOOL_OUTPUT: usize = 32 * 1024;
const MAX_SEARCH_FILES: usize = 500;
const MAX_SEARCH_MATCHES: usize = 100;

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// The first useful set of tools for coding and project work.
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "terminal".into(),
            description: "Run one shell command in the current project directory. Use for builds, tests, git and system inspection. The user may be asked to approve each command.".into(),
            parameters: schema(json!({
                "command": { "type": "string", "description": "Shell command to run." }
            }), &["command"]),
        },
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a UTF-8 text file inside the configured allowed paths.".into(),
            parameters: schema(json!({
                "path": { "type": "string", "description": "File path, relative to the project or absolute inside an allowed root." }
            }), &["path"]),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or replace a UTF-8 text file inside the configured allowed paths. Writing requires permission.".into(),
            parameters: schema(json!({
                "path": { "type": "string" },
                "content": { "type": "string" }
            }), &["path", "content"]),
        },
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace an exact text fragment in a file. By default exactly one occurrence must exist.".into(),
            parameters: schema(json!({
                "path": { "type": "string" },
                "old": { "type": "string" },
                "new": { "type": "string" },
                "replace_all": { "type": "boolean", "default": false }
            }), &["path", "old", "new"]),
        },
        ToolDefinition {
            name: "list_directory".into(),
            description: "List files and directories at a path inside the configured allowed paths.".into(),
            parameters: schema(json!({
                "path": { "type": "string", "default": "." }
            }), &[]),
        },
        ToolDefinition {
            name: "search_text".into(),
            description: "Search text in project files. Skips .git, target, node_modules and hidden directories.".into(),
            parameters: schema(json!({
                "query": { "type": "string" },
                "path": { "type": "string", "default": "." }
            }), &["query"]),
        },
        ToolDefinition {
            name: "fetch_url".into(),
            description: "Fetch a public HTTP or HTTPS URL as text, without sending credentials or following redirects.".into(),
            parameters: schema(json!({
                "url": { "type": "string" }
            }), &["url"]),
        },
        ToolDefinition {
            name: "list_skills".into(),
            description: "List available local skills. Skills are instruction files in .marelle/skills or ~/.config/marelle/skills.".into(),
            parameters: schema(json!({}), &[]),
        },
        ToolDefinition {
            name: "load_skill".into(),
            description: "Load the instructions for a named local skill.".into(),
            parameters: schema(json!({
                "name": { "type": "string" }
            }), &["name"]),
        },
    ]
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Missing or empty string argument: {key}"))
}

fn permission(config: &Config, key: &str, default: &str) -> &'static str {
    match config.get(key).unwrap_or(default).trim() {
        "allow" => "allow",
        "deny" => "deny",
        _ => "ask",
    }
}

fn confirm(
    config: &Config,
    key: &str,
    default: &str,
    label: &str,
    detail: &str,
    approve: &mut dyn FnMut(&str, &str) -> io::Result<bool>,
) -> io::Result<bool> {
    match permission(config, key, default) {
        "allow" => Ok(true),
        "deny" => Ok(false),
        _ => approve(label, detail),
    }
}

fn home_path(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME").map_or_else(|| PathBuf::from(path), PathBuf::from);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map_or_else(|| PathBuf::from(path), |home| PathBuf::from(home).join(rest));
    }
    PathBuf::from(path)
}

fn canonical_candidate(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return path.canonicalize().map_err(|error| error.to_string());
    }
    let mut missing = Vec::new();
    let mut ancestor = path;
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| "Path has no existing parent directory.".to_owned())?;
        missing.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
    }
    let mut resolved = ancestor.canonicalize().map_err(|error| {
        format!("Existing parent cannot be resolved: {error}")
    })?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn roots(config: &Config, key: &str, default: &[&str]) -> Vec<PathBuf> {
    let values = config
        .get_list(key)
        .ok()
        .flatten()
        .unwrap_or_else(|| default.iter().map(|item| (*item).to_owned()).collect());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    values
        .iter()
        .filter_map(|value| {
            let raw = home_path(value);
            let path = if raw.is_absolute() { raw } else { cwd.join(raw) };
            path.canonicalize().ok()
        })
        .collect()
}

fn checked_path(raw: &str, config: &Config, must_exist: bool) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let input = home_path(raw);
    let candidate = if input.is_absolute() { input } else { cwd.join(input) };
    let canonical = if must_exist {
        candidate
            .canonicalize()
            .map_err(|error| format!("Cannot resolve {raw}: {error}"))?
    } else {
        canonical_candidate(&candidate)?
    };

    let allowed = roots(config, "permissions.allowed_paths", &["."]);
    if allowed.is_empty() || !allowed.iter().any(|root| canonical.starts_with(root)) {
        return Err(format!("Path is outside permissions.allowed_paths: {raw}"));
    }
    let denied = roots(config, "permissions.denied_paths", &[]);
    if denied.iter().any(|root| canonical.starts_with(root)) {
        return Err(format!("Path is blocked by permissions.denied_paths: {raw}"));
    }
    Ok(canonical)
}

fn truncate(text: String) -> String {
    if text.len() <= MAX_TOOL_OUTPUT {
        return text;
    }
    let mut end = MAX_TOOL_OUTPUT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [output truncated]", &text[..end])
}

fn list_directory(args: &Value, config: &Config) -> Result<String, String> {
    let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let path = checked_path(raw, config, true)?;
    if !path.is_dir() {
        return Err(format!("Not a directory: {raw}"));
    }
    let mut entries = fs::read_dir(&path)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .take(200)
        .map(|entry| {
            let kind = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                "dir "
            } else {
                "file"
            };
            format!("{kind}  {}", entry.file_name().to_string_lossy())
        })
        .collect::<Vec<_>>();
    entries.sort();
    Ok(if entries.is_empty() {
        "(empty directory)".to_owned()
    } else {
        entries.join("\n")
    })
}

fn read_file(args: &Value, config: &Config) -> Result<String, String> {
    let raw = text_arg(args, "path")?;
    let path = checked_path(raw, config, true)?;
    let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!("Not a regular file: {raw}"));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("File exceeds the {} byte read limit.", MAX_FILE_BYTES));
    }
    fs::read_to_string(&path)
        .map(truncate)
        .map_err(|error| format!("Cannot read UTF-8 text file: {error}"))
}

fn write_file(args: &Value, config: &Config) -> Result<String, String> {
    let raw = text_arg(args, "path")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing string argument: content".to_owned())?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("Content exceeds the {} byte write limit.", MAX_FILE_BYTES));
    }
    let path = checked_path(raw, config, false)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&path, content).map_err(|error| error.to_string())?;
    Ok(format!("Wrote {} bytes to {}.", content.len(), path.display()))
}

fn edit_file(args: &Value, config: &Config) -> Result<String, String> {
    let raw = text_arg(args, "path")?;
    let old = args
        .get("old")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing string argument: old".to_owned())?;
    let new = args
        .get("new")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing string argument: new".to_owned())?;
    if old.is_empty() {
        return Err("The old fragment cannot be empty.".to_owned());
    }
    let path = checked_path(raw, config, true)?;
    let mut content = fs::read_to_string(&path)
        .map_err(|error| format!("Cannot read UTF-8 text file: {error}"))?;
    let count = content.matches(old).count();
    if count == 0 {
        return Err("The old fragment was not found; file left unchanged.".to_owned());
    }
    let replace_all = args
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if count > 1 && !replace_all {
        return Err(format!(
            "The fragment occurs {count} times. Set replace_all=true or provide a more specific fragment."
        ));
    }
    if replace_all {
        content = content.replace(old, new);
    } else {
        content = content.replacen(old, new, 1);
    }
    fs::write(&path, content.as_bytes()).map_err(|error| error.to_string())?;
    Ok(format!("Updated {}.", path.display()))
}

fn ignored_directory(name: &str) -> bool {
    name.starts_with('.')
        || matches!(name, "target" | "node_modules" | "vendor" | "dist" | "build")
}

fn search_tree(
    dir: &Path,
    query: &str,
    seen_files: &mut usize,
    matches: &mut Vec<String>,
) -> io::Result<()> {
    if *seen_files >= MAX_SEARCH_FILES || matches.len() >= MAX_SEARCH_MATCHES {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => continue,
        };
        if kind.is_dir() {
            if !ignored_directory(&entry.file_name().to_string_lossy()) {
                let _ = search_tree(&path, query, seen_files, matches);
            }
        } else if kind.is_file() {
            *seen_files += 1;
            if *seen_files > MAX_SEARCH_FILES {
                break;
            }
            let Ok(metadata) = entry.metadata() else { continue };
            if metadata.len() > 256 * 1024 {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    matches.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                    if matches.len() >= MAX_SEARCH_MATCHES {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn search_text(args: &Value, config: &Config) -> Result<String, String> {
    let query = text_arg(args, "query")?;
    let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let path = checked_path(raw, config, true)?;
    let mut matches = Vec::new();
    let mut seen = 0;
    if path.is_file() {
        let content = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        for (index, line) in content.lines().enumerate() {
            if line.contains(query) {
                matches.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            }
        }
    } else {
        search_tree(&path, query, &mut seen, &mut matches)
            .map_err(|error| error.to_string())?;
    }
    Ok(if matches.is_empty() {
        "No matches.".to_owned()
    } else {
        matches.join("\n")
    })
}

fn terminal(args: &Value, config: &Config) -> Result<String, String> {
    let command = text_arg(args, "command")?;
    if command.len() > 16 * 1024 {
        return Err("Command is too long.".to_owned());
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    // Do not inherit a model-supplied working directory. Commands start in
    // the directory where Marelle was launched.
    let output = Command::new("sh")
        .args(["-lc", command])
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("Could not start the shell: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let result = format!(
        "exit code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code().map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        stdout,
        stderr
    );
    let _ = config;
    Ok(truncate(result))
}

fn fetch_url(args: &Value) -> Result<String, String> {
    let raw = text_arg(args, "url")?;
    let parsed = reqwest::Url::parse(raw).map_err(|_| "Invalid URL.".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("Only credential-free HTTP and HTTPS URLs are accepted.".to_owned());
    }
    let output = Command::new("curl")
        .args([
            "--proto",
            "=http,https",
            "--max-time",
            "20",
            "--max-filesize",
            "1048576",
            "--silent",
            "--show-error",
            "--fail",
            "--output",
            "-",
            parsed.as_str(),
        ])
        .output()
        .map_err(|error| format!("Could not start curl: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    if output.stdout.len() > MAX_FILE_BYTES as usize {
        return Err("Response exceeds the 1 MiB limit.".to_owned());
    }
    Ok(truncate(String::from_utf8_lossy(&output.stdout).into_owned()))
}

fn skill_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join(".marelle").join("skills"));
        roots.push(cwd.join(".agents").join("skills"));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        roots.push(PathBuf::from(config).join("marelle").join("skills"));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".config").join("marelle").join("skills"));
    }
    roots
}

pub(crate) fn list_skills() -> String {
    let mut names = Vec::new();
    for root in skill_roots() {
        let Ok(entries) = fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().join("SKILL.md").is_file() {
                names.push(name);
            }
        }
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        "No local skills found. Add .marelle/skills/<name>/SKILL.md.".to_owned()
    } else {
        names.join("\n")
    }
}

fn load_skill(args: &Value) -> Result<String, String> {
    let name = text_arg(args, "name")?;
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err("Skill names may contain only ASCII letters, digits, '-' and '_'.".to_owned());
    }
    for root in skill_roots() {
        let path = root.join(name).join("SKILL.md");
        if path.is_file() {
            let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
            if metadata.len() > 64 * 1024 {
                return Err("Skill file exceeds the 64 KiB limit.".to_owned());
            }
            return fs::read_to_string(&path)
                .map(truncate)
                .map_err(|error| error.to_string());
        }
    }
    Err(format!("Skill not found: {name}"))
}

/// Executes one validated tool call. The returned string is sent back to the
/// model as an untrusted tool result.
pub(crate) fn execute(
    call: &ToolCall,
    config: &Config,
    approve: &mut dyn FnMut(&str, &str) -> io::Result<bool>,
) -> Result<String, String> {
    let args: Value = serde_json::from_str(&call.arguments)
        .map_err(|error| format!("Tool arguments are not valid JSON: {error}"))?;
    match call.name.as_str() {
        "terminal" => {
            let command = text_arg(&args, "command")?;
            if !confirm(config, "permissions.shell", "ask", "Run terminal command", command, approve)
                .map_err(|error| error.to_string())?
            {
                return Ok("Permission denied by the user.".to_owned());
            }
            terminal(&args, config)
        }
        "read_file" => {
            let path = text_arg(&args, "path")?;
            if !confirm(config, "permissions.files_read", "allow", "Read file", path, approve)
                .map_err(|error| error.to_string())?
            {
                return Ok("Permission denied by the user.".to_owned());
            }
            read_file(&args, config)
        }
        "list_directory" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            if !confirm(config, "permissions.files_read", "allow", "List directory", path, approve)
                .map_err(|error| error.to_string())?
            {
                return Ok("Permission denied by the user.".to_owned());
            }
            list_directory(&args, config)
        }
        "search_text" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let detail = format!("{} in {path}", args.get("query").and_then(Value::as_str).unwrap_or(""));
            if !confirm(config, "permissions.files_read", "allow", "Search files", &detail, approve)
                .map_err(|error| error.to_string())?
            {
                return Ok("Permission denied by the user.".to_owned());
            }
            search_text(&args, config)
        }
        "write_file" | "edit_file" => {
            let path = text_arg(&args, "path")?;
            if !confirm(config, "permissions.files_write", "ask", "Modify file", path, approve)
                .map_err(|error| error.to_string())?
            {
                return Ok("Permission denied by the user.".to_owned());
            }
            if call.name == "write_file" {
                write_file(&args, config)
            } else {
                edit_file(&args, config)
            }
        }
        "fetch_url" => {
            let url = text_arg(&args, "url")?;
            if !confirm(config, "permissions.network", "ask", "Fetch URL", url, approve)
                .map_err(|error| error.to_string())?
            {
                return Ok("Permission denied by the user.".to_owned());
            }
            fetch_url(&args)
        }
        "list_skills" => Ok(list_skills()),
        "load_skill" => load_skill(&args),
        name => Err(format!("Unknown tool: {name}")),
    }
}

/// Reads the project's local instructions. These files are data, not trusted
/// code; their contents are included only as user-provided context.
pub(crate) fn project_context(config: &Config) -> String {
    let cwd = match std::env::current_dir() {
        Ok(path) => path,
        Err(_) => return String::new(),
    };
    let mut pieces = Vec::new();
    for relative in [
        "AGENTS.md",
        "CLAUDE.md",
        "SOUL.md",
        ".cursorrules",
        ".marelle/AGENTS.md",
        ".marelle/CONTEXT.md",
    ] {
        let candidate = cwd.join(relative);
        let Ok(path) = checked_path(candidate.to_str().unwrap_or_default(), config, true) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else { continue };
        if !metadata.is_file() || metadata.len() > 32 * 1024 {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            if !text.trim().is_empty() {
                pieces.push(format!("--- {} ---\n{}", relative, text));
            }
        }
    }
    truncate(pieces.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use marelle_config::{Config, Layer};

    fn config(root: &Path) -> Config {
        let mut config = Config::new();
        config.set(Layer::Default, "permissions.allowed_paths", "[\".\"]");
        config.set(Layer::Default, "permissions.denied_paths", "[]");
        let _ = root;
        config
    }

    #[test]
    fn tool_schemas_are_complete_and_unique() {
        let definitions = definitions();
        let mut names = definitions.iter().map(|item| item.name.as_str()).collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), definitions.len());
        assert!(definitions.iter().all(|item| item.parameters["type"] == "object"));
    }

    #[test]
    fn path_checks_reject_parent_traversal_outside_the_project() {
        let root = std::env::current_dir().unwrap();
        let config = config(&root);
        let outside = root.parent().unwrap().join("marelle-outside-test");
        assert!(checked_path(outside.to_str().unwrap(), &config, false).is_err());
    }

    #[test]
    fn write_file_can_create_nested_paths_inside_an_allowed_root() {
        let dir = std::env::temp_dir().join(format!("marelle-tools-nested-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut config = Config::new();
        config.set(Layer::Default, "permissions.allowed_paths", format!("[\"{}\"]", dir.display()));
        config.set(Layer::Default, "permissions.denied_paths", "[]");
        let target = dir.join("new").join("nested").join("file.txt");
        let args = json!({"path": target.to_string_lossy(), "content": "created"});
        write_file(&args, &config).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "created");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_edit_requires_a_unique_match_by_default() {
        let dir = std::env::temp_dir().join(format!("marelle-tools-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.txt");
        fs::write(&path, "one one").unwrap();
        let mut config = Config::new();
        config.set(Layer::Default, "permissions.allowed_paths", format!("[\"{}\"]", dir.display()));
        config.set(Layer::Default, "permissions.denied_paths", "[]");
        let args = json!({"path": path.to_string_lossy(), "old": "one", "new": "two"});
        assert!(edit_file(&args, &config).unwrap_err().contains("occurs 2 times"));
        let mut args = args;
        args["replace_all"] = json!(true);
        edit_file(&args, &config).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two two");
        fs::remove_dir_all(&dir).unwrap();
    }
}
