// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Marelle's terminal interface.
//!
//! This module owns the interface: message layout, input editing, history,
//! scrolling, command completion and rendering. Crossterm is used only as a
//! terminal driver for raw mode, keyboard events and screen control.

use std::collections::VecDeque;
use std::io::{self, Stdout, Write, stdout};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};

use crate::VERSION;
use crate::theme::{Role as ThemeRole, Theme};

const MAX_INPUT_CHARS: usize = 32 * 1024;
const MAX_HISTORY: usize = 200;
const MAX_MESSAGES: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    User,
    Assistant,
    Notice,
}

#[derive(Debug, Clone)]
struct UiMessage {
    kind: MessageKind,
    text: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InputAction {
    Continue,
    Submit(String),
    Exit,
}

/// Owns terminal mode and all visible UI state.
pub(crate) struct Tui {
    output: Stdout,
    theme: Theme,
    color: bool,
    provider: String,
    model: String,
    status: String,
    input: String,
    cursor: usize,
    history: VecDeque<String>,
    history_index: Option<usize>,
    history_draft: String,
    messages: Vec<UiMessage>,
    scroll: usize,
}

impl Tui {
    /// Enters the alternate screen. Dropping the value always restores the
    /// terminal, including when a request or an input read fails.
    pub(crate) fn enter(
        theme: Theme,
        provider: String,
        model: String,
        color: bool,
    ) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut output = stdout();
        if let Err(error) = execute!(
            output,
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide,
            Clear(ClearType::All)
        ) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }

        let mut tui = Self {
            output,
            theme,
            color,
            provider,
            model,
            status: "Ready".to_owned(),
            input: String::new(),
            cursor: 0,
            history: VecDeque::new(),
            history_index: None,
            history_draft: String::new(),
            messages: Vec::new(),
            scroll: 0,
        };
        tui.render()?;
        Ok(tui)
    }

    pub(crate) fn set_model(&mut self, provider: String, model: String) -> io::Result<()> {
        self.provider = provider;
        self.model = model;
        self.render()
    }

    pub(crate) fn set_status(&mut self, status: impl Into<String>) -> io::Result<()> {
        self.status = status.into().trim().to_owned();
        self.render()
    }

    pub(crate) fn clear_messages(&mut self) -> io::Result<()> {
        self.messages.clear();
        self.scroll = 0;
        self.render()
    }

    pub(crate) fn push_user(&mut self, text: String) -> io::Result<()> {
        self.push_message(MessageKind::User, text);
        self.scroll = 0;
        self.render()
    }

    pub(crate) fn push_notice(&mut self, text: String) -> io::Result<()> {
        if !text.trim().is_empty() {
            self.push_message(MessageKind::Notice, text);
            self.scroll = 0;
            self.render()?;
        }
        Ok(())
    }

    pub(crate) fn start_answer(&mut self) -> io::Result<()> {
        self.push_message(MessageKind::Assistant, String::new());
        self.scroll = 0;
        self.status = "Generating…".to_owned();
        self.render()
    }

    pub(crate) fn append_answer(&mut self, bytes: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(bytes);
        if self
            .messages
            .last()
            .is_some_and(|message| message.kind == MessageKind::Assistant)
        {
            if let Some(last) = self.messages.last_mut() {
                last.text.push_str(&text);
            }
        } else {
            self.push_message(MessageKind::Assistant, text.into_owned());
        }
        self.scroll = 0;
        self.render()
    }

    pub(crate) fn finish_answer(&mut self) -> io::Result<()> {
        if self
            .messages
            .last()
            .is_some_and(|message| message.kind == MessageKind::Assistant && message.text.is_empty())
        {
            self.messages.pop();
        }
        self.render()
    }

    pub(crate) fn answer_writer(&mut self) -> AnswerWriter<'_> {
        AnswerWriter { tui: self }
    }

    fn push_message(&mut self, kind: MessageKind, text: String) {
        self.messages.push(UiMessage { kind, text });
        if self.messages.len() > MAX_MESSAGES {
            let excess = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(0..excess);
        }
    }

    /// Reads and handles one terminal event.
    pub(crate) fn next_action(&mut self) -> io::Result<InputAction> {
        match event::read()? {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.insert_text(&text.replace("\r\n", "\n").replace('\r', "\n"));
                self.render()?;
                Ok(InputAction::Continue)
            }
            Event::Resize(_, _) => {
                self.render()?;
                Ok(InputAction::Continue)
            }
            _ => Ok(InputAction::Continue),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<InputAction> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Ok(InputAction::Continue);
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if control && matches!(key.code, KeyCode::Char('c')) {
            return Ok(InputAction::Exit);
        }
        if control && matches!(key.code, KeyCode::Char('d')) && self.input.is_empty() {
            return Ok(InputAction::Exit);
        }
        if control && matches!(key.code, KeyCode::Char('u')) {
            self.input.clear();
            self.cursor = 0;
            self.reset_history_navigation();
            self.render()?;
            return Ok(InputAction::Continue);
        }
        if control && matches!(key.code, KeyCode::Char('w')) {
            self.delete_previous_word();
            self.render()?;
            return Ok(InputAction::Continue);
        }

        match key.code {
            KeyCode::Enter if shift => self.insert_text("\n"),
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.input);
                self.cursor = 0;
                self.reset_history_navigation();
                if !line.trim().is_empty() {
                    self.history.push_back(line.clone());
                    if self.history.len() > MAX_HISTORY {
                        self.history.pop_front();
                    }
                    self.render()?;
                    return Ok(InputAction::Submit(line));
                }
            }
            KeyCode::Char(character) if !control && !alt => {
                self.insert_text(&character.to_string());
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            KeyCode::Home => self.cursor = self.line_start(self.cursor),
            KeyCode::End => self.cursor = self.line_end(self.cursor),
            KeyCode::Up => {
                if self.input.contains('\n') {
                    self.move_vertical(-1);
                } else {
                    self.history_up();
                }
            }
            KeyCode::Down => {
                if self.input.contains('\n') {
                    self.move_vertical(1);
                } else {
                    self.history_down();
                }
            }
            KeyCode::PageUp => {
                let (_, height) = terminal::size().unwrap_or((80, 24));
                self.scroll = self.scroll.saturating_add(usize::from(height).saturating_sub(7).max(1));
            }
            KeyCode::PageDown => {
                let (_, height) = terminal::size().unwrap_or((80, 24));
                self.scroll = self.scroll.saturating_sub(usize::from(height).saturating_sub(7).max(1));
            }
            KeyCode::Tab => self.complete_command(),
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.reset_history_navigation();
                }
            }
            _ => {}
        }

        self.render()?;
        Ok(InputAction::Continue)
    }

    fn insert_text(&mut self, text: &str) {
        let current = self.input.chars().count();
        let incoming: String = text
            .chars()
            .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
            .take(MAX_INPUT_CHARS.saturating_sub(current))
            .collect();
        let byte = byte_index(&self.input, self.cursor);
        self.input.insert_str(byte, &incoming);
        self.cursor += incoming.chars().count();
        self.reset_history_navigation();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = byte_index(&self.input, self.cursor - 1);
        let end = byte_index(&self.input, self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
        self.reset_history_navigation();
    }

    fn delete(&mut self) {
        let count = self.input.chars().count();
        if self.cursor >= count {
            return;
        }
        let start = byte_index(&self.input, self.cursor);
        let end = byte_index(&self.input, self.cursor + 1);
        self.input.replace_range(start..end, "");
        self.reset_history_navigation();
    }

    fn delete_previous_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let from = byte_index(&self.input, start);
        let to = byte_index(&self.input, self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor = start;
        self.reset_history_navigation();
    }

    fn line_start(&self, cursor: usize) -> usize {
        self.input
            .chars()
            .take(cursor)
            .enumerate()
            .filter_map(|(index, character)| (character == '\n').then_some(index + 1))
            .last()
            .unwrap_or(0)
    }

    fn line_end(&self, cursor: usize) -> usize {
        self.input
            .chars()
            .skip(cursor)
            .position(|character| character == '\n')
            .map_or(self.input.chars().count(), |offset| cursor + offset)
    }

    fn move_vertical(&mut self, direction: isize) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut starts = vec![0usize];
        for (index, character) in chars.iter().enumerate() {
            if *character == '\n' {
                starts.push(index + 1);
            }
        }
        let current_line = starts
            .iter()
            .rposition(|start| *start <= self.cursor)
            .unwrap_or(0);
        let column = self.cursor.saturating_sub(starts[current_line]);
        let target = if direction < 0 {
            current_line.saturating_sub(1)
        } else {
            (current_line + 1).min(starts.len() - 1)
        };
        let target_start = starts[target];
        let target_end = chars[target_start..]
            .iter()
            .position(|character| *character == '\n')
            .map_or(chars.len(), |offset| target_start + offset);
        self.cursor = (target_start + column).min(target_end);
    }

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft = self.input.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(next);
        self.input.clone_from(&self.history[next]);
        self.cursor = self.input.chars().count();
    }

    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input.clone_from(&self.history_draft);
            self.cursor = self.input.chars().count();
        } else {
            self.history_index = Some(index + 1);
            self.input.clone_from(&self.history[index + 1]);
            self.cursor = self.input.chars().count();
        }
    }

    fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.history_draft.clear();
    }

    fn complete_command(&mut self) {
        let Some(command) = self.input.strip_prefix('/') else {
            return;
        };
        if command.contains(char::is_whitespace) {
            return;
        }
        const COMMANDS: &[&str] = &[
            "/clear", "/compare", "/exit", "/fallback", "/help", "/judge", "/model", "/quit",
            "/system",
        ];
        let matches: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .filter(|candidate| candidate[1..].starts_with(command))
            .collect();
        if matches.len() == 1 {
            self.input = format!("{} ", matches[0]);
            self.cursor = self.input.chars().count();
        } else if !matches.is_empty() {
            self.status = matches.join("   ");
        }
    }

    fn render(&mut self) -> io::Result<()> {
        let (columns, rows) = terminal::size().unwrap_or((80, 24));
        let width = usize::from(columns.max(1));
        let height = usize::from(rows.max(1));
        let input_width = width.saturating_sub(4).max(1);
        let mut input_lines = wrap(&self.input, input_width);
        if input_lines.is_empty() {
            input_lines.push(String::new());
        }
        let cursor_line = cursor_line_column(&self.input, self.cursor, input_width).0;
        let max_input_lines = height.saturating_sub(7).clamp(1, 4);
        let shown_input_lines = input_lines.len().min(max_input_lines);
        let input_scroll = cursor_line.saturating_sub(shown_input_lines.saturating_sub(1));
        let input_top = height.saturating_sub(shown_input_lines + 3);
        let content_height = input_top.saturating_sub(3);

        let mut transcript: Vec<(String, ThemeRole)> = Vec::new();
        for message in &self.messages {
            let (label, label_role, body_role) = match message.kind {
                MessageKind::User => ("YOU", ThemeRole::Primary, ThemeRole::Text),
                MessageKind::Assistant => ("MARELLE", ThemeRole::Accent, ThemeRole::Text),
                MessageKind::Notice => ("INFO", ThemeRole::Secondary, ThemeRole::Dim),
            };
            transcript.push((format!("  {label}"), label_role));
            for line in wrap(&message.text, input_width) {
                transcript.push((format!("  {line}"), body_role));
            }
            transcript.push((String::new(), ThemeRole::Dim));
        }
        if transcript.is_empty() {
            transcript.push(("  Start a conversation by typing below.".to_owned(), ThemeRole::Dim));
        }

        queue!(self.output, MoveTo(0, 0), Clear(ClearType::All))?;
        self.draw_line(
            0,
            &format!("╭─ Marelle {VERSION} ── {} / {}", self.provider, self.model),
            ThemeRole::Banner,
            width,
        )?;
        self.draw_line(
            1,
            &format!("│ {}", self.status),
            ThemeRole::Secondary,
            width,
        )?;
        self.draw_line(
            2,
            &format!("├{}┤", "─".repeat(width.saturating_sub(2))),
            ThemeRole::Border,
            width,
        )?;

        let start = transcript
            .len()
            .saturating_sub(content_height.saturating_add(self.scroll));
        for offset in 0..content_height {
            let index = start + offset;
            if let Some((text, role)) = transcript.get(index) {
                self.draw_line(3 + offset, text, *role, width)?;
            }
        }

        self.draw_line(
            input_top,
            &format!("╭─ Message {} ", "─".repeat(width.saturating_sub(13).max(1))),
            ThemeRole::Border,
            width,
        )?;
        for visible in 0..shown_input_lines {
            let line_index = input_scroll + visible;
            let content = input_lines.get(line_index).map_or("", String::as_str);
            let prefix = if line_index == 0 { "❯ " } else { "  " };
            self.draw_line(
                input_top + 1 + visible,
                &format!("{prefix}{content}"),
                ThemeRole::Prompt,
                width,
            )?;
        }
        self.draw_line(
            input_top + 1 + shown_input_lines,
            &format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
            ThemeRole::Border,
            width,
        )?;

        let hint = command_hint(&self.input);
        let footer = if hint.is_empty() {
            "Enter send · Shift+Enter newline · ↑↓ history · PgUp/PgDn scroll · Ctrl+C quit".to_owned()
        } else {
            format!("{hint}  ·  Enter send · Shift+Enter newline")
        };
        self.draw_line(
            input_top + 2 + shown_input_lines,
            &footer,
            ThemeRole::Dim,
            width,
        )?;

        let (mut cursor_row, mut cursor_column) =
            cursor_line_column(&self.input, self.cursor, input_width);
        cursor_row = cursor_row.saturating_sub(input_scroll);
        cursor_row = cursor_row.min(shown_input_lines.saturating_sub(1));
        cursor_column = cursor_column.min(input_width.saturating_sub(1));
        let y = input_top + 1 + cursor_row;
        let x = 2 + cursor_column;
        queue!(
            self.output,
            MoveTo(x.min(u16::MAX as usize) as u16, y.min(u16::MAX as usize) as u16),
            Show
        )?;
        self.output.flush()
    }

    fn draw_line(
        &mut self,
        row: usize,
        text: &str,
        role: ThemeRole,
        width: usize,
    ) -> io::Result<()> {
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        if row >= usize::from(rows) {
            return Ok(());
        }
        let line = fit(text, width);
        let styled = self.theme.paint(role, &line, self.color);
        queue!(
            self.output,
            MoveTo(0, row.min(u16::MAX as usize) as u16),
            Print(styled)
        )
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = execute!(
            self.output,
            DisableBracketedPaste,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

/// A writer that routes streamed assistant text into the transcript.
pub(crate) struct AnswerWriter<'a> {
    tui: &'a mut Tui,
}

impl Write for AnswerWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.tui.append_answer(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tui.render()
    }
}

fn byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(byte, _)| byte)
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    for character in text.chars() {
        if character == '\n' {
            lines.push(std::mem::take(&mut current));
            count = 0;
            continue;
        }
        current.push(character);
        count += 1;
        if count >= width {
            lines.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() || lines.is_empty() || text.ends_with('\n') {
        lines.push(current);
    }
    lines
}

fn fit(text: &str, width: usize) -> String {
    let mut output: String = text.chars().take(width).collect();
    let count = output.chars().count();
    if count < width {
        output.push_str(&" ".repeat(width - count));
    }
    output
}

fn cursor_line_column(text: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut line = 0usize;
    let mut column = 0usize;
    for character in text.chars().take(cursor) {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
            if column >= width {
                line += 1;
                column = 0;
            }
        }
    }
    (line, column)
}

fn command_hint(input: &str) -> String {
    let Some(command) = input.strip_prefix('/') else {
        return String::new();
    };
    if command.contains(char::is_whitespace) {
        return String::new();
    }
    [
        "/clear", "/compare", "/exit", "/fallback", "/help", "/judge", "/model", "/quit",
        "/system",
    ]
    .into_iter()
    .filter(|candidate| candidate[1..].starts_with(command))
    .collect::<Vec<_>>()
    .join("   ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_byte_offsets_are_unicode_safe() {
        let text = "aЖ🙂z";
        assert_eq!(byte_index(text, 0), 0);
        assert_eq!(byte_index(text, 1), 1);
        assert_eq!(byte_index(text, 2), 3);
        assert_eq!(byte_index(text, 3), 7);
        assert_eq!(byte_index(text, 4), text.len());
    }

    #[test]
    fn wrapping_preserves_newlines_and_long_words() {
        assert_eq!(
            wrap("alpha beta\nsecond", 5),
            ["alpha", " beta", "secon", "d"]
        );
        assert_eq!(wrap("", 10), [""]);
        assert_eq!(wrap("one\n", 10), ["one", ""]);
    }

    #[test]
    fn cursor_tracks_multiline_and_wrapped_input() {
        assert_eq!(cursor_line_column("abcd", 4, 4), (1, 0));
        assert_eq!(cursor_line_column("ab\ncd", 3, 4), (1, 0));
        assert_eq!(cursor_line_column("abcdef", 5, 4), (1, 1));
    }

    #[test]
    fn fitting_never_exceeds_the_requested_character_width() {
        assert_eq!(fit("abcdef", 4), "abcd");
        assert_eq!(fit("xy", 4), "xy  ");
    }
}
