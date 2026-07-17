//! Terminal palette parsing and semantic color roles for the TUI.
//!
//! Eighteen-slot CSV overrides come from TOML or the one-run CLI flag;
//! `SemanticRole` maps session status, agent labels, and git diff colors onto those slots.

#[cfg(test)]
use std::collections::BTreeMap;

#[cfg(feature = "tui")]
use ratatui::style::{Color, Style};

const PALETTE_CSV_LEN: usize = 18;
const PALETTE_SLOT_ORDER: &str =
    "fg,bg,black,red,green,yellow,blue,magenta,cyan,white,bright_black,bright_red,bright_green,bright_yellow,bright_blue,bright_magenta,bright_cyan,bright_white";

/// One of the sixteen ANSI palette slots shared by the popup and tmux renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteSlot {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl PaletteSlot {
    const fn index(self) -> u8 {
        match self {
            Self::Black => 0,
            Self::Red => 1,
            Self::Green => 2,
            Self::Yellow => 3,
            Self::Blue => 4,
            Self::Magenta => 5,
            Self::Cyan => 6,
            Self::White => 7,
            Self::BrightBlack => 8,
            Self::BrightRed => 9,
            Self::BrightGreen => 10,
            Self::BrightYellow => 11,
            Self::BrightBlue => 12,
            Self::BrightMagenta => 13,
            Self::BrightCyan => 14,
            Self::BrightWhite => 15,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "black" => Some(Self::Black),
            "red" => Some(Self::Red),
            "green" => Some(Self::Green),
            "yellow" => Some(Self::Yellow),
            "blue" => Some(Self::Blue),
            "magenta" => Some(Self::Magenta),
            "cyan" => Some(Self::Cyan),
            "white" => Some(Self::White),
            "bright_black" => Some(Self::BrightBlack),
            "bright_red" => Some(Self::BrightRed),
            "bright_green" => Some(Self::BrightGreen),
            "bright_yellow" => Some(Self::BrightYellow),
            "bright_blue" => Some(Self::BrightBlue),
            "bright_magenta" => Some(Self::BrightMagenta),
            "bright_cyan" => Some(Self::BrightCyan),
            "bright_white" => Some(Self::BrightWhite),
            _ => None,
        }
    }
}

/// Validated state foreground color shared by the popup and tmux fragments.
///
/// `palette:<slot>` follows Ilmari's active palette for both the popup and tmux.
/// Other variants are rendered from structured values, never copied into a tmux
/// style expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorSpec {
    Default,
    Palette(PaletteSlot),
    Ansi(u8),
    Rgb(u8, u8, u8),
}

impl ColorSpec {
    /// Parse `default`, `palette:<slot>`, `ansi:<name-or-index>`, a bare ANSI
    /// name/index, or `#RRGGBB` into a safe terminal color representation.
    pub fn parse(value: &str) -> Result<Self, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("must not be empty".to_string());
        }
        if trimmed.eq_ignore_ascii_case("default") {
            return Ok(Self::Default);
        }
        if let Some(slot) = trimmed.strip_prefix("palette:") {
            return PaletteSlot::parse(slot)
                .map(Self::Palette)
                .ok_or_else(|| format!("unknown palette slot `{slot}`"));
        }
        if let Some(ansi) = trimmed.strip_prefix("ansi:") {
            return Self::parse_ansi(ansi);
        }
        if trimmed.starts_with('#') {
            return parse_rgb_hex(trimmed).map(|(red, green, blue)| Self::Rgb(red, green, blue));
        }
        Self::parse_ansi(trimmed)
    }

    fn parse_ansi(value: &str) -> Result<Self, String> {
        if let Some(slot) = PaletteSlot::parse(value) {
            return Ok(Self::Ansi(slot.index()));
        }
        value
            .trim()
            .parse::<u8>()
            .map(Self::Ansi)
            .map_err(|_| {
                format!(
                    "expected `default`, `palette:<slot>`, `ansi:<name-or-index>`, an ANSI name/index, or #RRGGBB; got `{value}`"
                )
            })
    }

    /// Produce the only tmux foreground style syntax this typed representation allows.
    pub fn tmux_foreground(&self, palette: &Palette) -> String {
        palette.state_color_tmux(self)
    }
}

#[cfg(not(feature = "tui"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    Reset,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Named styling intent resolved through the active 18-slot palette.
///
/// Roles keep the radar independent of raw ANSI indexes so palette CSV overrides
/// retarget status, agent labels, and git accents without touching layout code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub enum SemanticRole {
    StatusRunning,
    StatusWaitingInput,
    StatusFinished,
    StatusTerminated,
    StatusUnknown,
    /// Agent name in the app column.
    AppLabel,
    AgentDetailNeutral,
    AgentDetailAmpDeep,
    AgentDetailAmpSmart,
    AgentDetailAmpRush,
    GitInsertions,
    GitDeletions,
    HeadingAccent,
    MutedText,
}

/// Foreground, background, and ANSI role colors used by the radar TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    fg: Color,
    bg: Color,
    ansi: [Color; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            fg: Color::Reset,
            bg: Color::Reset,
            ansi: std::array::from_fn(|idx| Color::Indexed(idx as u8)),
        }
    }
}

impl Palette {
    /// Parse the fixed 18-slot `fg,bg,black,...,bright_white` CSV override.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the CSV length or a color token is invalid.
    pub fn from_csv(value: &str) -> Result<Self, String> {
        Self::parse_csv(value.trim())
    }

    #[cfg(feature = "tui")]
    /// Default foreground/background style for radar chrome.
    pub fn base_style(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }

    #[cfg(feature = "tui")]
    /// Map a semantic role onto the active palette slot colors.
    pub fn style_for(&self, role: SemanticRole) -> Style {
        self.base_style().fg(match role {
            SemanticRole::StatusRunning => self.ansi_color(4),
            SemanticRole::StatusWaitingInput => self.ansi_color(3),
            SemanticRole::StatusFinished => self.ansi_color(2),
            SemanticRole::StatusTerminated => self.ansi_color(1),
            SemanticRole::StatusUnknown => self.ansi_color(8),
            SemanticRole::AppLabel => self.ansi_color(14),
            SemanticRole::AgentDetailNeutral => self.ansi_color(8),
            SemanticRole::AgentDetailAmpDeep => self.ansi_color(6),
            SemanticRole::AgentDetailAmpSmart => self.ansi_color(10),
            SemanticRole::AgentDetailAmpRush => self.ansi_color(3),
            SemanticRole::GitInsertions => self.ansi_color(2),
            SemanticRole::GitDeletions => self.ansi_color(1),
            SemanticRole::HeadingAccent => self.ansi_color(12),
            SemanticRole::MutedText => self.ansi_color(8),
        })
    }

    #[cfg(feature = "tui")]
    /// Resolve a validated state color against this popup's active palette.
    pub fn style_for_color(&self, color: &ColorSpec) -> Style {
        self.base_style().fg(self.state_color(color))
    }

    /// Resolve a state color to tmux syntax through this palette's active slots.
    #[allow(unreachable_patterns)] // `ratatui::Color` has variants outside Ilmari's palette subset.
    pub fn state_color_tmux(&self, color: &ColorSpec) -> String {
        match self.state_color(color) {
            Color::Reset => "fg=default".to_string(),
            Color::Indexed(index) => format!("fg=colour{index}"),
            Color::Rgb(red, green, blue) => format!("fg=#{red:02x}{green:02x}{blue:02x}"),
            _ => unreachable!("Ilmari palettes contain only reset, indexed, or RGB colors"),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_env_map(env: &BTreeMap<String, String>) -> Self {
        let Some(value) = env.get("ILMARI_TUI_PALETTE").or_else(|| env.get("ILMARI_PALETTE"))
        else {
            return Self::default();
        };

        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Self::default();
        }

        Self::parse_csv(trimmed).unwrap_or_default()
    }

    fn parse_csv(value: &str) -> Result<Self, String> {
        let parts: Vec<&str> = value.split(',').map(|part| part.trim()).collect();
        if parts.len() != PALETTE_CSV_LEN {
            return Err(format!(
                "expected {} comma-separated colors ({PALETTE_SLOT_ORDER}), got {}",
                PALETTE_CSV_LEN,
                parts.len()
            ));
        }

        let fg = parse_palette_color(parts[0])?;
        let bg = parse_palette_color(parts[1])?;
        let mut ansi = [Color::Reset; 16];
        for (idx, part) in parts.iter().skip(2).enumerate() {
            ansi[idx] = parse_palette_color(part)?;
        }

        Ok(Self { fg, bg, ansi })
    }

    fn ansi_color(&self, idx: usize) -> Color {
        self.ansi[idx]
    }

    fn state_color(&self, color: &ColorSpec) -> Color {
        match color {
            ColorSpec::Default => Color::Reset,
            ColorSpec::Palette(slot) => self.ansi_color(slot.index() as usize),
            ColorSpec::Ansi(index) => Color::Indexed(*index),
            ColorSpec::Rgb(red, green, blue) => Color::Rgb(*red, *green, *blue),
        }
    }
}

fn parse_palette_color(value: &str) -> Result<Color, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("empty color".to_string());
    }

    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("rgb:") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 3 {
            return Err(format!("invalid rgb: value: {trimmed}"));
        }
        let r = parse_hex_channel(parts[0])?;
        let g = parse_hex_channel(parts[1])?;
        let b = parse_hex_channel(parts[2])?;
        return Ok(Color::Rgb(r, g, b));
    }

    let hex = trimmed
        .strip_prefix('#')
        .or_else(|| trimmed.strip_prefix("0x"))
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    if hex.len() != 6 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("invalid hex color: {trimmed} (expected #RRGGBB)"));
    }

    let rgb = u32::from_str_radix(hex, 16).map_err(|_| format!("invalid hex color: {trimmed}"))?;
    Ok(Color::Rgb(((rgb >> 16) & 0xFF) as u8, ((rgb >> 8) & 0xFF) as u8, (rgb & 0xFF) as u8))
}

fn parse_rgb_hex(value: &str) -> Result<(u8, u8, u8), String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("invalid hex color: {value} (expected #RRGGBB)"));
    }
    let rgb = u32::from_str_radix(hex, 16).map_err(|_| format!("invalid hex color: {value}"))?;
    Ok(((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
}

fn parse_hex_channel(value: &str) -> Result<u8, String> {
    let trimmed = value.trim();
    if trimmed.len() == 2 {
        return u8::from_str_radix(trimmed, 16)
            .map_err(|_| format!("invalid rgb: component {trimmed}"));
    }
    if trimmed.len() == 4 {
        let parsed = u16::from_str_radix(trimmed, 16)
            .map_err(|_| format!("invalid rgb: component {trimmed}"))?;
        return Ok((parsed >> 8) as u8);
    }

    Err(format!("invalid rgb: component {trimmed} (expected 2 or 4 hex digits)"))
}

#[cfg(all(test, feature = "tui"))]
mod tests {
    use super::{ColorSpec, Palette, PaletteSlot, SemanticRole};
    use ratatui::style::Color;
    use std::collections::BTreeMap;

    #[test]
    fn default_palette_uses_reset_base_and_ansi_indexed_roles() {
        let palette = Palette::default();

        assert_eq!(palette.base_style().fg, Some(Color::Reset));
        assert_eq!(palette.base_style().bg, Some(Color::Reset));
        assert_eq!(palette.style_for(SemanticRole::StatusRunning).fg, Some(Color::Indexed(4)));
        assert_eq!(palette.style_for(SemanticRole::AgentDetailAmpDeep).fg, Some(Color::Indexed(6)));
        assert_eq!(
            palette.style_for(SemanticRole::AgentDetailAmpSmart).fg,
            Some(Color::Indexed(10))
        );
        assert_eq!(palette.style_for(SemanticRole::AgentDetailAmpRush).fg, Some(Color::Indexed(3)));
        assert_eq!(palette.style_for(SemanticRole::AppLabel).fg, Some(Color::Indexed(14)));
        assert_eq!(palette.style_for(SemanticRole::HeadingAccent).fg, Some(Color::Indexed(12)));
    }

    #[test]
    fn env_palette_prefers_ilmari_tui_palette() {
        let mut env = BTreeMap::new();
        env.insert(
            "ILMARI_PALETTE".to_string(),
            "#010101,#020202,#030303,#040404,#050505,#060606,#070707,#080808,#090909,#0a0a0a,#0b0b0b,#0c0c0c,#0d0d0d,#0e0e0e,#0f0f0f,#101010,#111111,#121212".to_string(),
        );
        env.insert(
            "ILMARI_TUI_PALETTE".to_string(),
            "#111111,#222222,#000000,#ff0000,#00ff00,#ffff00,#0000ff,#ff00ff,#00ffff,#cccccc,#555555,#ff5555,#55ff55,#ffff55,#5555ff,#ff55ff,#55ffff,#ffffff".to_string(),
        );

        let palette = Palette::from_env_map(&env);

        assert_eq!(
            palette.style_for(SemanticRole::StatusRunning).fg,
            Some(Color::Rgb(0x00, 0x00, 0xff))
        );
        assert_eq!(palette.base_style().fg, Some(Color::Rgb(0x11, 0x11, 0x11)));
        assert_eq!(palette.base_style().bg, Some(Color::Rgb(0x22, 0x22, 0x22)));
    }

    #[test]
    fn env_palette_uses_compatibility_alias_when_primary_is_missing() {
        let mut env = BTreeMap::new();
        env.insert(
            "ILMARI_PALETTE".to_string(),
            "010101,020202,030303,040404,050505,060606,070707,080808,090909,0a0a0a,0b0b0b,0c0c0c,0d0d0d,0e0e0e,0f0f0f,101010,111111,121212".to_string(),
        );

        let palette = Palette::from_env_map(&env);

        assert_eq!(palette.base_style().fg, Some(Color::Rgb(0x01, 0x01, 0x01)));
        assert_eq!(palette.base_style().bg, Some(Color::Rgb(0x02, 0x02, 0x02)));
        assert_eq!(
            palette.style_for(SemanticRole::StatusTerminated).fg,
            Some(Color::Rgb(0x04, 0x04, 0x04))
        );
    }

    #[test]
    fn palette_parser_accepts_plain_hex_and_rgb_variants() {
        let palette = Palette::parse_csv(
            "112233,445566,778899,0xAA0000,BBCCDD,rgb:789A/9ABC/BCDE,rgb:12/34/56,#010203,#111213,#212223,#313233,#414243,#515253,#616263,#717273,#818283,#919293,#A1A2A3",
        )
        .expect("palette should parse");

        assert_eq!(palette.base_style().fg, Some(Color::Rgb(0x11, 0x22, 0x33)));
        assert_eq!(palette.base_style().bg, Some(Color::Rgb(0x44, 0x55, 0x66)));
        assert_eq!(
            palette.style_for(SemanticRole::StatusRunning).fg,
            Some(Color::Rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(
            palette.style_for(SemanticRole::HeadingAccent).fg,
            Some(Color::Rgb(0x71, 0x72, 0x73))
        );
    }

    #[test]
    fn empty_palette_override_is_treated_as_absent() {
        let mut env = BTreeMap::new();
        env.insert("ILMARI_TUI_PALETTE".to_string(), "   ".to_string());

        let palette = Palette::from_env_map(&env);

        assert_eq!(palette.base_style().fg, Some(Color::Reset));
        assert_eq!(palette.style_for(SemanticRole::MutedText).fg, Some(Color::Indexed(8)));
    }

    #[test]
    fn malformed_palette_falls_back_cleanly() {
        let mut env = BTreeMap::new();
        env.insert("ILMARI_TUI_PALETTE".to_string(), "#000000,#111111".to_string());

        let palette = Palette::from_env_map(&env);

        assert_eq!(palette.style_for(SemanticRole::StatusFinished).fg, Some(Color::Indexed(2)));
        assert_eq!(palette.base_style().fg, Some(Color::Reset));
    }

    #[test]
    fn state_color_specs_are_typed_and_generate_only_safe_tmux_foregrounds() {
        assert_eq!(ColorSpec::parse("default"), Ok(ColorSpec::Default));
        assert_eq!(
            ColorSpec::parse("palette:bright-blue"),
            Ok(ColorSpec::Palette(PaletteSlot::BrightBlue))
        );
        assert_eq!(ColorSpec::parse("ansi:12"), Ok(ColorSpec::Ansi(12)));
        assert_eq!(ColorSpec::parse("yellow"), Ok(ColorSpec::Ansi(3)));
        assert_eq!(ColorSpec::parse("#123456"), Ok(ColorSpec::Rgb(0x12, 0x34, 0x56)));
        assert_eq!(
            ColorSpec::parse("palette:bright-blue").unwrap().tmux_foreground(&Palette::default()),
            "fg=colour12"
        );
        assert_eq!(
            ColorSpec::parse("#123456").unwrap().tmux_foreground(&Palette::default()),
            "fg=#123456"
        );
        assert!(ColorSpec::parse("fg=red,bold").is_err());
    }

    #[test]
    fn palette_state_colors_follow_the_active_popup_palette() {
        let palette = Palette::from_csv(
            "#010101,#020202,#030303,#040404,#050505,#060606,#070707,#080808,#090909,#0a0a0a,#0b0b0b,#0c0c0c,#0d0d0d,#0e0e0e,#0f0f0f,#101010,#111111,#121212",
        )
        .expect("palette should parse");
        let style = palette.style_for_color(&ColorSpec::parse("palette:bright-blue").unwrap());

        assert_eq!(style.fg, Some(Color::Rgb(0x0f, 0x0f, 0x0f)));
        assert_eq!(
            ColorSpec::parse("palette:bright-blue").unwrap().tmux_foreground(&palette),
            "fg=#0f0f0f"
        );
    }
}
