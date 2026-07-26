//! Color palette for the dark green/terminal theme, ported from the
//! `Postgres Monitor TUI` design mockup (claude.ai/design project
//! `ce4aac14-ae5c-4fda-9e54-dfd87abae33a`).

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders},
};

pub const BG: Color = Color::Rgb(0x00, 0x00, 0x00);
pub const PANEL_BG: Color = Color::Rgb(0x00, 0x00, 0x00);
pub const PANEL_BG_ALT: Color = Color::Rgb(0x00, 0x00, 0x00);

pub const BORDER: Color = Color::Rgb(0x20, 0x2a, 0x24);
pub const BORDER_DIAGNOSIS: Color = Color::Rgb(0x33, 0x45, 0x2f);
pub const BORDER_DETAIL: Color = Color::Rgb(0x2b, 0x3a, 0x2f);
pub const BORDER_WARN: Color = Color::Rgb(0x4a, 0x3a, 0x2c);

pub const TEXT_BRIGHT: Color = Color::Rgb(0xe6, 0xef, 0xe9);
pub const TEXT: Color = Color::Rgb(0xc3, 0xcf, 0xc7);
pub const TEXT_DIM: Color = Color::Rgb(0x8b, 0x9a, 0x90);
pub const TEXT_DIMMER: Color = Color::Rgb(0x6b, 0x7a, 0x71);
pub const TEXT_DIMMEST: Color = Color::Rgb(0x4e, 0x5c, 0x53);

pub const OK: Color = Color::Rgb(0x8f, 0xe3, 0x9f);
pub const WARN: Color = Color::Rgb(0xe3, 0xc9, 0x8f);
pub const BAD: Color = Color::Rgb(0xe3, 0x9f, 0x8f);

pub const ROW_SELECTED_BG: Color = Color::Rgb(0x16, 0x21, 0x1a);

/// Standard bordered panel block with a visible title — `Block::title()` with a
/// plain string otherwise inherits `border_style`'s color, and `BORDER` is too
/// close to `PANEL_BG` to read as a title. Styled dim-grey-but-bold so it reads
/// as a title (distinct from both bright values and normal body text) without
/// competing with them for attention.
pub fn block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .title(Span::styled(title.into(), Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL_BG))
}
