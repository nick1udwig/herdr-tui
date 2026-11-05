use crate::{copy::Copy, shell::palette::Palette, vt};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
pub fn terminal(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &vt::Frame,
    selection: Option<&Copy>,
    palette: &Palette,
) {
    let selected = selection_style(palette);
    let area = area.intersection(buffer.area);
    for (y, row) in snapshot.rows.iter().take(area.height as usize).enumerate() {
        self::row(
            buffer,
            area,
            y as u16,
            row,
            selection.map(|s| (s, selected)),
        );
    }
}
/// One terminal row at `y` within `area`, which must lie inside the buffer.
pub fn row(
    buffer: &mut Buffer,
    area: Rect,
    y: u16,
    row: &[vt::Cell],
    selection: Option<(&Copy, Style)>,
) {
    let mut tail = false;
    for (x, cell) in row.iter().take(area.width as usize).enumerate() {
        if tail {
            tail = false;
            continue;
        }
        let mut style = Style::default().fg(cell.fg).bg(cell.bg);
        for (enabled, modifier) in [
            (cell.bold, Modifier::BOLD),
            (cell.faint, Modifier::DIM),
            (cell.italic, Modifier::ITALIC),
            (cell.underline, Modifier::UNDERLINED),
            (cell.strike, Modifier::CROSSED_OUT),
            (cell.inverse, Modifier::REVERSED),
        ] {
            if enabled {
                style = style.add_modifier(modifier);
            }
        }
        if let Some((copy, selected)) = selection
            && copy.contains(x as u16, y)
        {
            style = selected;
        }
        buffer[(area.x + x as u16, area.y + y)]
            .set_symbol(if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            })
            .set_style(style);
        tail = cell.wide;
    }
}
/// Herdr's selection colors: the panel background lifted toward white (or black on light themes).
fn selection_style(palette: &Palette) -> Style {
    let base = match palette.panel_bg {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => return Style::reset().add_modifier(Modifier::REVERSED),
    };
    let luminance = |(r, g, b): (u8, u8, u8)| {
        let channel = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    };
    let target = if luminance(base) < 0.5 { 255.0 } else { 0.0 };
    let mix = |c: u8| (f32::from(c) + (target - f32::from(c)) * 0.28).round() as u8;
    let bg = (mix(base.0), mix(base.1), mix(base.2));
    let fg = if (luminance(bg) + 0.05) / 0.05 > 1.05 / (luminance(bg) + 0.05) {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    };
    Style::reset().fg(fg).bg(Color::Rgb(bg.0, bg.1, bg.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_colors_preserve_host_palette_and_program_overrides() {
        let area = Rect::new(0, 0, 12, 1);
        let mut vt = vt::Vt::new(area.width, area.height);
        let mut buffer = Buffer::empty(area);
        let mut render = |vt: &mut vt::Vt| {
            terminal(&mut buffer, area, &vt.frame(), None, &Palette::default());
            buffer.clone()
        };
        vt.write(b"\x1b[31;44mA\x1b[0;38;5;202;48;5;234mB\x1b[0;38;2;1;2;3;48;2;4;5;6mC\x1b[0;7mD");
        let before = render(&mut vt);
        assert_eq!(before[(0, 0)].fg, Color::Indexed(1));
        assert_eq!(before[(0, 0)].bg, Color::Indexed(4));
        assert_eq!(before[(1, 0)].fg, Color::Indexed(202));
        assert_eq!(before[(1, 0)].bg, Color::Indexed(234));
        assert_eq!(before[(2, 0)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(before[(2, 0)].bg, Color::Rgb(4, 5, 6));
        assert_eq!(before[(3, 0)].fg, Color::Reset);
        assert_eq!(before[(3, 0)].bg, Color::Reset);
        assert!(before[(3, 0)].modifier.contains(Modifier::REVERSED));

        vt.write(b"\x1b]4;1;rgb:12/34/56\x07");
        assert_eq!(render(&mut vt)[(0, 0)].fg, Color::Rgb(0x12, 0x34, 0x56));
        vt.write(b"\x1b]104;1\x1b\\");
        assert_eq!(render(&mut vt)[(0, 0)].fg, Color::Indexed(1));

        vt.write(b"\x1b[0;42m\x1b[K");
        assert_eq!(render(&mut vt)[(4, 0)].bg, Color::Indexed(2));
        vt.write(b"\x1b[48;2;7;8;9m\x1b[K");
        assert_eq!(render(&mut vt)[(4, 0)].bg, Color::Rgb(7, 8, 9));

        vt.write(b"\x1b]10;rgb:11/22/33\x07\x1b]11;rgb:44/55/66\x1b\\");
        let changed = render(&mut vt);
        assert_eq!(changed[(3, 0)].fg, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(changed[(3, 0)].bg, Color::Rgb(0x44, 0x55, 0x66));
        vt.write(b"\x1b]110\x07\x1b]111\x07");
        let reset = render(&mut vt);
        assert_eq!(reset[(3, 0)].fg, Color::Reset);
        assert_eq!(reset[(3, 0)].bg, Color::Reset);
    }
}
