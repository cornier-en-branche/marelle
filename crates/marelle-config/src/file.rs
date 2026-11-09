// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Reading and writing TOML setting files.
//!
//! A file mirrors the dotted keys as nested tables:
//!
//! ```toml
//! [chat]
//! provider = "zai"
//! model = "glm-4.6"
//!
//! [theme]
//! primary = "#B57EDC"
//! ```
//!
//! Arrays become JSON arrays in the flat [`Config`], so a list can travel
//! through the same string-valued layers as every other setting. Tables under
//! `extra_body` (and inline tables anywhere) become JSON objects for the same
//! reason.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::Config;

/// What a value means when it is written back to TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// A plain string.
    String,
    /// `true` or `false`.
    Bool,
    /// A whole number.
    Integer,
    /// A decimal number.
    Float,
    /// A list of strings, written as a TOML array.
    Array,
    /// A JSON object, written as a TOML inline table.
    Object,
}

impl ValueKind {
    /// Converts a setting's text to the TOML value of this kind.
    pub fn parse(self, text: &str) -> Result<Value, String> {
        let text = text.trim();
        match self {
            ValueKind::String => Ok(Value::from(text.to_owned())),
            ValueKind::Bool => match text.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Ok(Value::from(true)),
                "false" | "no" | "off" | "0" => Ok(Value::from(false)),
                _ => Err("expected true or false".to_owned()),
            },
            ValueKind::Integer => text
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| "expected a whole number".to_owned()),
            ValueKind::Float => text
                .parse::<f64>()
                .map(Value::from)
                .map_err(|_| "expected a number".to_owned()),
            ValueKind::Array => {
                let items = if text.starts_with('[') {
                    serde_json::from_str::<Vec<Json>>(text)
                        .map_err(|_| "expected a JSON array of strings".to_owned())?
                } else {
                    text.split(',')
                        .map(|item| Json::String(item.trim().to_owned()))
                        .filter(|item| !item.as_str().unwrap_or_default().is_empty())
                        .collect()
                };
                let mut array = Array::new();
                for item in items {
                    match item {
                        Json::String(text) => array.push(text),
                        _ => return Err("expected a list of strings".to_owned()),
                    }
                }
                Ok(Value::Array(array))
            }
            ValueKind::Object => {
                let json: Json =
                    serde_json::from_str(text).map_err(|_| "expected a JSON object".to_owned())?;
                if !json.is_object() {
                    return Err("expected a JSON object".to_owned());
                }
                json_to_toml(&json).ok_or_else(|| "expected a JSON object".to_owned())
            }
        }
    }
}

/// A setting file could not be read, parsed or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    /// The file involved.
    pub path: PathBuf,
    /// The line, when the parser reported one.
    pub line: Option<usize>,
    /// The column, when the parser reported one.
    pub column: Option<usize>,
    /// What went wrong, without repeating a secret value.
    pub message: String,
}

impl FileError {
    /// An error for a file.
    pub fn new(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line: None,
            column: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
            if let Some(column) = self.column {
                write!(f, ":{column}")?;
            }
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for FileError {}

/// Reads a TOML file and adds its values to `config` at `layer`.
pub fn load(config: &mut Config, layer: crate::Layer, path: &Path) -> Result<(), FileError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(FileError::new(path, error.to_string())),
    };
    load_text(config, layer, path, &text)
}

/// Adds values from TOML text to `config`. `path` is only used in errors.
pub fn load_text(
    config: &mut Config,
    layer: crate::Layer,
    path: &Path,
    text: &str,
) -> Result<(), FileError> {
    let document = text
        .parse::<DocumentMut>()
        .map_err(|error| parse_error(path, error))?;
    flatten_table(config, layer, "", document.as_table())?;
    Ok(())
}

fn parse_error(path: &Path, error: toml_edit::TomlError) -> FileError {
    let message = error.to_string();
    let (line, column) = match position(&message) {
        Some((line, column)) => (Some(line), Some(column)),
        None => (None, None),
    };
    FileError {
        path: path.to_path_buf(),
        line,
        column,
        message,
    }
}

/// Reads `line N column M` from a toml_edit message when it is there.
fn position(message: &str) -> Option<(usize, usize)> {
    let rest = message.split("line ").nth(1)?;
    let mut words = rest.split_whitespace();
    let line = words.next()?.parse().ok()?;
    let column = words
        .next()
        .and_then(|word| word.strip_prefix("column "))
        .and_then(|word| word.parse().ok())?;
    Some((line, column))
}

fn flatten_table(
    config: &mut Config,
    layer: crate::Layer,
    prefix: &str,
    table: &Table,
) -> Result<(), FileError> {
    for (key, item) in table {
        let full = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        flatten_item(config, layer, &full, item)?;
    }
    Ok(())
}

fn flatten_item(
    config: &mut Config,
    layer: crate::Layer,
    key: &str,
    item: &Item,
) -> Result<(), FileError> {
    // `extra_body` and `headers` are objects, not namespaces of settings.
    if is_object_key(key) {
        if let Some(json) = item_to_json(item) {
            config.set(layer, key, serde_json::to_string(&json).unwrap_or_default());
            return Ok(());
        }
    }
    match item {
        Item::Table(table) => flatten_table(config, layer, key, table),
        Item::ArrayOfTables(array) => {
            let json = Json::Array(array.iter().map(table_to_json).collect::<Vec<_>>());
            config.set(layer, key, serde_json::to_string(&json).unwrap_or_default());
            Ok(())
        }
        Item::Value(value) => {
            let text = value_to_text(value);
            config.set(layer, key, text);
            Ok(())
        }
        Item::None => Ok(()),
    }
}

fn is_object_key(key: &str) -> bool {
    let last = key.rsplit('.').next().unwrap_or(key);
    last == "extra_body" || last == "headers"
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.value().clone(),
        Value::Integer(number) => number.value().to_string(),
        Value::Float(number) => number.value().to_string(),
        Value::Boolean(boolean) => boolean.value().to_string(),
        Value::Datetime(datetime) => datetime.value().to_string(),
        Value::Array(array) => {
            serde_json::to_string(&array.iter().map(value_to_json).collect::<Vec<Json>>())
                .unwrap_or_default()
        }
        Value::InlineTable(table) => {
            serde_json::to_string(&inline_table_to_json(table)).unwrap_or_default()
        }
    }
}

fn value_to_json(value: &Value) -> Json {
    match value {
        Value::String(text) => Json::String(text.value().clone()),
        Value::Integer(number) => Json::from(*number.value()),
        Value::Float(number) => Json::from(*number.value()),
        Value::Boolean(boolean) => Json::Bool(*boolean.value()),
        Value::Datetime(datetime) => Json::String(datetime.value().to_string()),
        Value::Array(array) => Json::Array(array.iter().map(value_to_json).collect()),
        Value::InlineTable(table) => inline_table_to_json(table),
    }
}

fn table_to_json(table: &Table) -> Json {
    let mut map = serde_json::Map::new();
    for (key, item) in table {
        let value = match item {
            Item::Value(value) => value_to_json(value),
            Item::Table(table) => table_to_json(table),
            Item::ArrayOfTables(array) => Json::Array(array.iter().map(table_to_json).collect()),
            Item::None => continue,
        };
        map.insert(key.to_string(), value);
    }
    Json::Object(map)
}

fn inline_table_to_json(table: &InlineTable) -> Json {
    let mut map = serde_json::Map::new();
    for (key, value) in table {
        map.insert(key.to_string(), value_to_json(value));
    }
    Json::Object(map)
}

fn item_to_json(item: &Item) -> Option<Json> {
    match item {
        Item::Value(Value::InlineTable(table)) => Some(inline_table_to_json(table)),
        Item::Table(table) => Some(table_to_json(table)),
        _ => None,
    }
}

fn json_to_toml(json: &Json) -> Option<Value> {
    match json {
        Json::Null => None,
        Json::Bool(boolean) => Some(Value::from(*boolean)),
        Json::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Some(Value::from(integer))
            } else {
                number.as_f64().map(Value::from)
            }
        }
        Json::String(text) => Some(Value::from(text.clone())),
        Json::Array(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(json_to_toml(item)?);
            }
            Some(Value::Array(array))
        }
        Json::Object(map) => {
            let mut table = InlineTable::new();
            for (key, value) in map {
                table.insert(key, json_to_toml(value)?);
            }
            Some(Value::InlineTable(table))
        }
    }
}

/// Writes `key = value` into a TOML file, creating the file and parent
/// directories when needed. Comments and other settings are kept.
pub fn set(path: &Path, key: &str, value: &str, kind: ValueKind) -> Result<(), FileError> {
    let mut document = read_document(path)?;
    let value = kind
        .parse(value)
        .map_err(|error| FileError::new(path, error))?;
    insert(&mut document, key, value).map_err(|error| FileError::new(path, error))?;
    write_document(path, &document)
}

/// Removes `key` from a TOML file. An absent key is not an error.
pub fn unset(path: &Path, key: &str) -> Result<(), FileError> {
    if !path.exists() {
        return Ok(());
    }
    let mut document = read_document(path)?;
    remove(&mut document, key);
    write_document(path, &document)
}

fn read_document(path: &Path) -> Result<DocumentMut, FileError> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|error| parse_error(path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(FileError::new(path, error.to_string())),
    }
}

fn insert(document: &mut DocumentMut, key: &str, value: Value) -> Result<(), String> {
    let mut parts: Vec<&str> = key.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return Err("the setting key is empty".to_owned());
    }
    let leaf = parts.pop().expect("checked above");
    let mut table: &mut Table = document.as_table_mut();
    for part in parts {
        if !table.contains_key(part) {
            table.insert(part, Item::Table(Table::new()));
        }
        let item = table.get_mut(part).expect("just inserted");
        table = item
            .as_table_mut()
            .ok_or_else(|| format!("{part} is already a value, not a table"))?;
    }
    table.insert(leaf, Item::Value(value));
    Ok(())
}

fn remove(document: &mut DocumentMut, key: &str) {
    let parts: Vec<&str> = key.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return;
    }
    let mut table: &mut Table = document.as_table_mut();
    for (index, part) in parts.iter().enumerate() {
        if index + 1 == parts.len() {
            table.remove(part);
            return;
        }
        let Some(item) = table.get_mut(part) else {
            return;
        };
        let Some(next) = item.as_table_mut() else {
            return;
        };
        table = next;
    }
}

fn write_document(path: &Path, document: &DocumentMut) -> Result<(), FileError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| FileError::new(path, error.to_string()))?;
    }
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, document.to_string())
        .map_err(|error| FileError::new(path, error.to_string()))?;
    std::fs::rename(&temporary, path).map_err(|error| FileError::new(path, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;

    fn text() -> &'static str {
        r##"
[chat]
provider = "zai"
model = "glm-4.6"
stream = false
max_tokens = 4000

[theme]
primary = "#B57EDC"

[models.fast]
provider = "openai"
model = "gpt-5"

[providers.zai]
api_key_file = "~/.config/marelle/zai.key"

[providers.zai.extra_body]
thinking = "off"
budget = 12

[models.compare]
list = ["fast", "deep"]
"##
    }

    #[test]
    fn nested_tables_become_dotted_keys() {
        let mut config = Config::new();
        load_text(&mut config, Layer::Global, Path::new("test.toml"), text()).unwrap();
        assert_eq!(config.get("chat.provider"), Some("zai"));
        assert_eq!(config.get("chat.stream"), Some("false"));
        assert_eq!(config.get("chat.max_tokens"), Some("4000"));
        assert_eq!(config.get("theme.primary"), Some("#B57EDC"));
        assert_eq!(config.get("models.fast.model"), Some("gpt-5"));
    }

    #[test]
    fn arrays_become_json_and_extra_body_becomes_an_object() {
        let mut config = Config::new();
        load_text(&mut config, Layer::Global, Path::new("test.toml"), text()).unwrap();
        assert_eq!(
            config.get("models.compare.list"),
            Some(r#"["fast","deep"]"#)
        );
        let extra: Json =
            serde_json::from_str(config.get("providers.zai.extra_body").unwrap()).unwrap();
        assert_eq!(extra["thinking"], "off");
        assert_eq!(extra["budget"], 12);
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let mut config = Config::new();
        load(
            &mut config,
            Layer::Global,
            Path::new("/definitely/not/here/marelle.toml"),
        )
        .unwrap();
        assert!(config.keys().is_empty());
    }

    #[test]
    fn broken_toml_names_the_file() {
        let mut config = Config::new();
        let error = load_text(
            &mut config,
            Layer::Global,
            Path::new("broken.toml"),
            "[chat\nmodel = 1",
        )
        .unwrap_err();
        assert_eq!(error.path, Path::new("broken.toml"));
        assert!(!error.message.is_empty());
    }

    #[test]
    fn values_round_trip_through_a_file() {
        let dir = std::env::temp_dir().join(format!("marelle-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        set(&path, "chat.provider", "zai", ValueKind::String).unwrap();
        set(&path, "chat.stream", "false", ValueKind::Bool).unwrap();
        set(&path, "chat.max_tokens", "4096", ValueKind::Integer).unwrap();
        set(
            &path,
            "models.compare.list",
            r#"["fast","deep"]"#,
            ValueKind::Array,
        )
        .unwrap();
        set(
            &path,
            "providers.zai.extra_body",
            r#"{"thinking":"off"}"#,
            ValueKind::Object,
        )
        .unwrap();

        let mut config = Config::new();
        load(&mut config, Layer::Global, &path).unwrap();
        assert_eq!(config.get("chat.provider"), Some("zai"));
        assert_eq!(config.get("chat.stream"), Some("false"));
        assert_eq!(config.get("chat.max_tokens"), Some("4096"));
        assert_eq!(
            config.get("models.compare.list"),
            Some(r#"["fast","deep"]"#)
        );
        assert_eq!(
            config.get("providers.zai.extra_body"),
            Some(r#"{"thinking":"off"}"#)
        );

        unset(&path, "chat.max_tokens").unwrap();
        unset(&path, "providers.zai.extra_body").unwrap();
        let mut config = Config::new();
        load(&mut config, Layer::Global, &path).unwrap();
        assert_eq!(config.get("chat.max_tokens"), None);
        assert_eq!(config.get("providers.zai.extra_body"), None);
        assert_eq!(config.get("chat.provider"), Some("zai"), "others stay");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[chat]"));
        assert!(!text.contains("max_tokens"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_values_are_explained_without_echoing_them() {
        for (kind, value) in [
            (ValueKind::Bool, "maybe"),
            (ValueKind::Integer, "4.5"),
            (ValueKind::Array, "[1,2]"),
            (ValueKind::Object, "[1]"),
        ] {
            let error = kind.parse(value).unwrap_err();
            assert!(!error.contains(value), "{kind:?}: {error}");
        }
    }
}
