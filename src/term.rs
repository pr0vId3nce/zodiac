use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Blits a vt100 screen into the ratatui buffer.
pub struct TermView<'a> {
    pub screen: &'a vt100::Screen,
}

fn conv_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

impl Widget for TermView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for r in 0..rows.min(area.height) {
            for c in 0..cols.min(area.width) {
                let Some(cell) = self.screen.cell(r, c) else {
                    continue;
                };
                let target = &mut buf[(area.x + c, area.y + r)];
                let contents = cell.contents();
                if contents.is_empty() {
                    target.set_symbol(" ");
                } else {
                    target.set_symbol(&contents);
                }
                let mut style = Style::default()
                    .fg(conv_color(cell.fgcolor()))
                    .bg(conv_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    }
}
