use std::collections::BTreeMap;

use ratatui::style::{Color, Style};

const PALETTE_CSV_LEN: usize = 18;
const PALETTE_SLOT_ORDER: &str =
    "fg,bg,black,red,green,yellow,blue,magenta,cyan,white,bright_black,bright_red,bright_green,bright_yellow,bright_blue,bright_magenta,bright_cyan,bright_white";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticRole {
    StatusRunning,
    StatusWaitingInput,
    StatusFinished,
    StatusTerminated,
    StatusUnknown,
    AppLabel,
    AgentDetailNeutral,
    AgentDetailPositive,
    AgentDetailWarning,
    GitInsertions,
    GitDeletions,
    HeadingAccent,
    MutedText,
}

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
    pub fn from_env() -> Self {
        let env = std::env::vars().collect::<BTreeMap<_, _>>();
        Self::from_env_map(&env)
    }

    pub fn base_style(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }

    pub fn style_for(&self, role: SemanticRole) -> Style {
        self.base_style().fg(match role {
            SemanticRole::StatusRunning => self.ansi_color(4),
            SemanticRole::StatusWaitingInput => self.ansi_color(3),
            SemanticRole::StatusFinished => self.ansi_color(2),
            SemanticRole::StatusTerminated => self.ansi_color(1),
            SemanticRole::StatusUnknown => self.ansi_color(8),
            SemanticRole::AppLabel => self.ansi_color(14),
            SemanticRole::AgentDetailNeutral => self.ansi_color(8),
            SemanticRole::AgentDetailPositive => self.ansi_color(2),
            SemanticRole::AgentDetailWarning => self.ansi_color(3),
            SemanticRole::GitInsertions => self.ansi_color(2),
            SemanticRole::GitDeletions => self.ansi_color(1),
            SemanticRole::HeadingAccent => self.ansi_color(12),
            SemanticRole::MutedText => self.ansi_color(8),
        })
    }

    fn from_env_map(env: &BTreeMap<String, String>) -> Self {
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

#[cfg(test)]
mod tests {
    use super::{Palette, SemanticRole};
    use ratatui::style::Color;
    use std::collections::BTreeMap;

    #[test]
    fn default_palette_uses_reset_base_and_ansi_indexed_roles() {
        let palette = Palette::default();

        assert_eq!(palette.base_style().fg, Some(Color::Reset));
        assert_eq!(palette.base_style().bg, Some(Color::Reset));
        assert_eq!(palette.style_for(SemanticRole::StatusRunning).fg, Some(Color::Indexed(4)));
        assert_eq!(
            palette.style_for(SemanticRole::AgentDetailPositive).fg,
            Some(Color::Indexed(2))
        );
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
}
