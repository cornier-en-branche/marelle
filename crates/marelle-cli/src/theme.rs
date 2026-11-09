// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Colours and the small pieces of the interface.
//!
//! Wisteria is the default, and every role can be changed with a HEX value in
//! the settings. Colour is only written when the output is a terminal, so
//! pipes stay clean.

use std::fmt::Write as _;

use marelle_config::{Config, Expected, SettingError};

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Color {
    /// Red.
    pub(crate) red: u8,
    /// Green.
    pub(crate) green: u8,
    /// Blue.
    pub(crate) blue: u8,
}

impl Color {
    /// The foreground escape sequence.
    pub(crate) fn foreground(self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.red, self.green, self.blue)
    }
}

/// Parses `#RGB` or `#RRGGBB`. A missing `#` is accepted too.
pub(crate) fn parse_hex(text: &str) -> Option<Color> {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    let digits: Vec<u8> = hex
        .chars()
        .map(|character| character.to_digit(16).map(|digit| digit as u8))
        .collect::<Option<Vec<_>>>()?;
    match digits.as_slice() {
        [r, g, b] => Some(Color {
            red: r * 17,
            green: g * 17,
            blue: b * 17,
        }),
        [r1, r2, g1, g2, b1, b2] => Some(Color {
            red: r1 * 16 + r2,
            green: g1 * 16 + g2,
            blue: b1 * 16 + b2,
        }),
        _ => None,
    }
}

/// The wisteria palette.
pub(crate) const WISTERIA: Color = Color {
    red: 0xB5,
    green: 0x7E,
    blue: 0xDC,
};

/// One named colour role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// The main accent.
    Primary,
    /// A quieter accent.
    Secondary,
    /// A highlight.
    Accent,
    /// The answer text, when the terminal default is not wanted.
    Text,
    /// Reasoning and less important notes.
    Dim,
    /// Errors.
    Error,
    /// Confirmations.
    Success,
    /// Warnings.
    Warning,
    /// The frame around an answer.
    Border,
    /// The input prompt.
    Prompt,
    /// The banner.
    Banner,
    /// Reasoning text.
    Reasoning,
}

impl Role {
    fn key(self) -> &'static str {
        match self {
            Role::Primary => "theme.primary",
            Role::Secondary => "theme.secondary",
            Role::Accent => "theme.accent",
            Role::Text => "theme.text",
            Role::Dim => "theme.dim",
            Role::Error => "theme.error",
            Role::Success => "theme.success",
            Role::Warning => "theme.warning",
            Role::Border => "theme.border",
            Role::Prompt => "theme.prompt",
            Role::Banner => "theme.banner",
            Role::Reasoning => "theme.reasoning",
        }
    }
}

/// Every role's colour, with the fallbacks already applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Theme {
    primary: Color,
    secondary: Color,
    accent: Color,
    text: Option<Color>,
    dim: Color,
    error: Color,
    success: Color,
    warning: Color,
    border: Color,
    prompt: Color,
    banner: Color,
    reasoning: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            primary: WISTERIA,
            secondary: parse_hex("#9B7EBD").expect("a valid colour"),
            accent: parse_hex("#D8BFD8").expect("a valid colour"),
            text: None,
            dim: parse_hex("#8A7F95").expect("a valid colour"),
            error: parse_hex("#E06C75").expect("a valid colour"),
            success: parse_hex("#98C379").expect("a valid colour"),
            warning: parse_hex("#E5C07B").expect("a valid colour"),
            border: WISTERIA,
            prompt: WISTERIA,
            banner: WISTERIA,
            reasoning: parse_hex("#8A7F95").expect("a valid colour"),
        }
    }
}

impl Theme {
    /// Reads the theme from the settings, falling back to wisteria.
    ///
    /// A value that is not a colour names the setting and is refused; it is
    /// never echoed, because a key pasted into a colour setting must not
    /// travel any further.
    pub(crate) fn from_config(config: &Config) -> Result<Self, SettingError> {
        let mut theme = Theme::default();
        let read = |key: &str| -> Result<Option<Color>, SettingError> {
            match config.get(key) {
                None => Ok(None),
                Some(text) if text.trim().is_empty() => Ok(None),
                Some(text) => parse_hex(text)
                    .map(Some)
                    .ok_or_else(|| SettingError::new(key, Expected::HexColor)),
            }
        };

        if let Some(color) = read(Role::Primary.key())? {
            theme.primary = color;
        }
        if let Some(color) = read(Role::Secondary.key())? {
            theme.secondary = color;
        }
        if let Some(color) = read(Role::Accent.key())? {
            theme.accent = color;
        }
        theme.text = read(Role::Text.key())?;
        if let Some(color) = read(Role::Dim.key())? {
            theme.dim = color;
        }
        if let Some(color) = read(Role::Error.key())? {
            theme.error = color;
        }
        if let Some(color) = read(Role::Success.key())? {
            theme.success = color;
        }
        if let Some(color) = read(Role::Warning.key())? {
            theme.warning = color;
        }
        theme.border = read(Role::Border.key())?.unwrap_or(theme.primary);
        theme.prompt = read(Role::Prompt.key())?.unwrap_or(theme.primary);
        theme.banner = read(Role::Banner.key())?.unwrap_or(theme.primary);
        theme.reasoning = read(Role::Reasoning.key())?.unwrap_or(theme.dim);
        Ok(theme)
    }

    /// The colour for a role.
    pub(crate) fn color(&self, role: Role) -> Option<Color> {
        Some(match role {
            Role::Primary => self.primary,
            Role::Secondary => self.secondary,
            Role::Accent => self.accent,
            Role::Text => self.text?,
            Role::Dim => self.dim,
            Role::Error => self.error,
            Role::Success => self.success,
            Role::Warning => self.warning,
            Role::Border => self.border,
            Role::Prompt => self.prompt,
            Role::Banner => self.banner,
            Role::Reasoning => self.reasoning,
        })
    }

    /// Wraps `text` in a colour when colour is enabled.
    pub(crate) fn paint(&self, role: Role, text: &str, enabled: bool) -> String {
        match (enabled, self.color(role)) {
            (true, Some(color)) => format!("{}{text}\x1b[0m", color.foreground()),
            _ => text.to_owned(),
        }
    }

    /// Wraps `text` in a colour and bold when colour is enabled.
    pub(crate) fn paint_bold(&self, role: Role, text: &str, enabled: bool) -> String {
        match (enabled, self.color(role)) {
            (true, Some(color)) => {
                format!("\x1b[1m{}{text}\x1b[0m", color.foreground())
            }
            _ => text.to_owned(),
        }
    }

    /// A label followed by its value, used by the information panel.
    pub(crate) fn label(&self, label: &str, value: &str, enabled: bool) -> String {
        format!(
            "{} {}",
            self.paint(Role::Secondary, label, enabled),
            self.paint(Role::Text, value, enabled)
        )
    }
}

/// The width to draw at, from `ui.width`, `COLUMNS`, or a sane default.
pub(crate) fn width(setting: Option<u64>, columns: Option<&str>) -> usize {
    let from_columns = columns
        .and_then(|text| text.trim().parse::<usize>().ok())
        .filter(|width| *width >= 20);
    let chosen = match setting {
        // Zero means "ask the terminal"; a tiny explicit value is widened.
        Some(0) | None => from_columns.unwrap_or(72),
        Some(width) => width as usize,
    };
    chosen.clamp(20, 160)
}

/// The banner: a small box with the name and version.
pub(crate) fn banner(
    theme: &Theme,
    version: &str,
    tagline: &str,
    width: usize,
    enabled: bool,
) -> String {
    let title = format!(" marelle {version} ");
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}{}",
        theme.paint(Role::Banner, "╭─", enabled),
        theme.paint_bold(Role::Banner, &title, enabled)
    );
    let fill = width.saturating_sub(title.chars().count() + 3);
    let _ = writeln!(
        out,
        "{}{}",
        theme.paint(Role::Banner, "│ ", enabled),
        theme.paint(Role::Dim, tagline, enabled)
    );
    let _ = writeln!(
        out,
        "{}{}",
        theme.paint(Role::Banner, "╰", enabled),
        theme.paint(Role::Banner, &"─".repeat(fill.max(1)), enabled)
    );
    out
}

/// A panel of label/value rows, framed like the banner.
pub(crate) fn panel<L: AsRef<str>>(
    theme: &Theme,
    rows: &[(L, String)],
    width: usize,
    enabled: bool,
) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let inner = width.saturating_sub(4);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        theme.paint(Role::Border, &format!("╭{}╮", "─".repeat(inner)), enabled)
    );
    for (label, value) in rows {
        let label = label.as_ref();
        let text = theme.label(label, value, enabled);
        let visible = label.chars().count() + 1 + value.chars().count();
        let padding = inner.saturating_sub(visible + 1);
        let _ = writeln!(
            out,
            "{}{} {}",
            theme.paint(Role::Border, "│", enabled),
            text,
            " ".repeat(padding)
        );
    }
    let _ = writeln!(
        out,
        "{}",
        theme.paint(Role::Border, &format!("╰{}╯", "─".repeat(inner)), enabled)
    );
    out
}

/// The prompt shown before input.
pub(crate) fn prompt(theme: &Theme, enabled: bool) -> String {
    theme.paint_bold(Role::Prompt, "❯", enabled) + " "
}

/// A horizontal rule, used to close an answer.
pub(crate) fn rule(theme: &Theme, width: usize, enabled: bool) -> String {
    theme.paint(Role::Border, &"─".repeat(width), enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marelle_config::Layer;

    #[test]
    fn hex_colours_are_read_in_both_lengths() {
        assert_eq!(
            parse_hex("#B57EDC"),
            Some(Color {
                red: 0xB5,
                green: 0x7E,
                blue: 0xDC
            })
        );
        assert_eq!(
            parse_hex("#fff"),
            Some(Color {
                red: 255,
                green: 255,
                blue: 255
            })
        );
        assert_eq!(parse_hex("B57EDC"), parse_hex("#B57EDC"));
        assert_eq!(parse_hex("#B57ED"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
        assert_eq!(parse_hex("purple"), None);
    }

    #[test]
    fn defaults_are_wisteria_and_roles_fall_back() {
        let theme = Theme::default();
        assert_eq!(theme.color(Role::Primary), Some(WISTERIA));
        assert_eq!(theme.color(Role::Border), Some(WISTERIA));
        assert_eq!(theme.color(Role::Text), None);
        assert_eq!(theme.color(Role::Reasoning), theme.color(Role::Dim));
    }

    #[test]
    fn settings_override_and_bad_values_name_the_key() {
        let mut config = Config::new();
        config.set(Layer::Global, "theme.primary", "#112233");
        config.set(Layer::Global, "theme.border", "#445566");
        let theme = Theme::from_config(&config).unwrap();
        assert_eq!(theme.color(Role::Primary), parse_hex("#112233"));
        assert_eq!(theme.color(Role::Border), parse_hex("#445566"));
        assert_eq!(
            theme.color(Role::Prompt),
            parse_hex("#112233"),
            "prompt follows primary"
        );

        config.set(Layer::Global, "theme.error", "sk-not-a-colour");
        let error = Theme::from_config(&config).unwrap_err();
        assert_eq!(error.key, "theme.error");
        assert!(!error.to_string().contains("sk-not-a-colour"));
    }

    #[test]
    fn colour_is_only_written_when_enabled() {
        let theme = Theme::default();
        assert_eq!(theme.paint(Role::Primary, "x", false), "x");
        assert!(
            theme
                .paint(Role::Primary, "x", true)
                .starts_with("\x1b[38;2;181;126;220m")
        );
    }

    #[test]
    fn the_banner_and_panel_stay_within_the_width() {
        let theme = Theme::default();
        let banner = banner(&theme, "1.0", "a tagline", 40, false);
        for line in banner.lines() {
            assert!(line.chars().count() <= 40, "{line}");
        }
        let panel = panel(
            &theme,
            &[
                ("provider", "zai".to_owned()),
                ("model", "glm-4.6".to_owned()),
            ],
            40,
            false,
        );
        for line in panel.lines() {
            assert!(line.chars().count() <= 40, "{line}");
        }
    }

    #[test]
    fn width_prefers_the_setting_then_columns() {
        assert_eq!(width(Some(50), Some("120")), 50);
        assert_eq!(width(Some(0), Some("120")), 120);
        assert_eq!(width(None, Some("90")), 90);
        assert_eq!(width(None, None), 72);
        assert_eq!(width(Some(5), Some("5")), 20);
        assert_eq!(width(Some(500), None), 160);
    }
}
