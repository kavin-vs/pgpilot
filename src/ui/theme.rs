//! Color palette for the monochrome grey/white theme, ported from the
//! `Postgres Monitor TUI` design mockup (claude.ai/design project
//! `ce4aac14-ae5c-4fda-9e54-dfd87abae33a`) after its post-v2 revision toward
//! a more concise look — grey/white throughout, with green/amber/red used
//! only for ok/warn/bad semantic states, not as the base theme color.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders},
};

pub const BG: Color = Color::Rgb(0x0a, 0x0b, 0x0a);
pub const PANEL_BG: Color = BG;
pub const PANEL_BG_ALT: Color = BG;

pub const BORDER: Color = Color::Rgb(0x23, 0x25, 0x23);
pub const BORDER_DETAIL: Color = Color::Rgb(0x3a, 0x3d, 0x3a);

pub const TEXT_BRIGHT: Color = Color::Rgb(0xff, 0xff, 0xff);
pub const TEXT: Color = Color::Rgb(0xd6, 0xd8, 0xd6);
pub const TEXT_DIM: Color = Color::Rgb(0x7a, 0x7d, 0x7a);
pub const TEXT_DIMMER: Color = Color::Rgb(0x5c, 0x5f, 0x5c);
pub const TEXT_DIMMEST: Color = Color::Rgb(0x4a, 0x4d, 0x4a);

pub const OK: Color = Color::Rgb(0x7f, 0xa0, 0x6f);
pub const WARN: Color = Color::Rgb(0xc1, 0xa0, 0x5a);
pub const BAD: Color = Color::Rgb(0xbd, 0x74, 0x66);

pub const ROW_SELECTED_BG: Color = BORDER;

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
