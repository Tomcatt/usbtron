//! The btop-style screen: rounded boxes, braille graphs, block meters.

use crate::app::{App, Series};
use crate::opts::{self, Options};
use crate::topo::{self, Device};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Widget};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

const MIN_SCALE: f64 = 1024.0;

struct Theme {
    hi: Color,
    dim: Color,
    faint: Color,
    accent: Color,
    warn: Color,
    bad: Color,
    text: Color,
    /// Graph colours, bottom to top.
    stops: [(u8, u8, u8); 3],
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Same order as `opts::THEMES`.
const THEMES: [Theme; 3] = [
    Theme {
        hi: rgb(0, 230, 255),
        dim: rgb(0, 110, 130),
        faint: rgb(30, 60, 70),
        accent: rgb(255, 150, 0),
        warn: rgb(255, 220, 0),
        bad: rgb(255, 60, 60),
        text: Color::White,
        stops: [(0, 230, 255), (255, 150, 0), (255, 60, 60)],
    },
    Theme {
        hi: rgb(255, 170, 60),
        dim: rgb(150, 90, 30),
        faint: rgb(70, 45, 25),
        accent: rgb(255, 90, 40),
        warn: rgb(255, 220, 100),
        bad: rgb(255, 50, 50),
        text: rgb(255, 235, 210),
        stops: [(255, 190, 80), (255, 100, 40), (230, 30, 30)],
    },
    Theme {
        hi: rgb(230, 230, 230),
        dim: rgb(130, 130, 130),
        faint: rgb(60, 60, 60),
        accent: rgb(200, 200, 200),
        warn: rgb(255, 255, 255),
        bad: rgb(255, 255, 255),
        text: rgb(240, 240, 240),
        stops: [(120, 120, 120), (190, 190, 190), (255, 255, 255)],
    },
];

// ponytail: one global theme index, pass a Theme through if a second view ever needs its own.
static THEME: AtomicUsize = AtomicUsize::new(0);

fn th() -> &'static Theme {
    &THEMES[THEME.load(Ordering::Relaxed)]
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u8
}

/// Cyan at rest, orange in the middle, red at the top: btop's graph gradient.
fn gradient(f: f64) -> Color {
    let f = f.clamp(0.0, 1.0);
    let [low, mid, top] = th().stops;
    let ((a, b, c), (x, y, z), t) = if f < 0.5 {
        (low, mid, f * 2.0)
    } else {
        (mid, top, (f - 0.5) * 2.0)
    };
    Color::Rgb(lerp(a, x, t), lerp(b, y, t), lerp(c, z, t))
}

pub fn human(b: f64) -> String {
    scaled(b, ["B", "K", "M", "G"])
}

fn rate(b: f64, o: &Options) -> String {
    if o.bits {
        scaled(b * 8.0, ["b", "Kb", "Mb", "Gb"])
    } else {
        human(b)
    }
}

fn scaled(b: f64, units: [&str; 4]) -> String {
    let (mut v, mut u) = (b, 0);
    while v >= 1000.0 && u < 3 {
        v /= 1024.0;
        u += 1;
    }
    if v < 0.5 {
        "0".into()
    } else if u == 0 || v >= 100.0 {
        format!("{v:.0}{}", units[u])
    } else {
        format!("{v:.1}{}", units[u])
    }
}

fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th().dim))
        .title(Line::from(vec![
            Span::styled("┐", Style::new().fg(th().dim)),
            Span::styled(title, Style::new().fg(th().hi).add_modifier(Modifier::BOLD)),
            Span::styled("┌", Style::new().fg(th().dim)),
        ]))
}

/// Filled braille area graph. Each cell holds two samples, four dots tall;
/// `hang` draws from the top down, for the mirrored half.
struct Graph<'a> {
    data: &'a VecDeque<f64>,
    scale: f64,
    hang: bool,
    log: bool,
}

impl Widget for Graph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        const UP: [[u8; 4]; 2] = [[0x40, 0x04, 0x02, 0x01], [0x80, 0x20, 0x10, 0x08]];
        const DOWN: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        let dots_tall = area.height as usize * 4;
        if dots_tall == 0 {
            return;
        }
        let dots = if self.hang { &DOWN } else { &UP };
        let fraction = |v: f64| {
            if self.log {
                (1.0 + v).ln() / (1.0 + self.scale).ln()
            } else {
                v / self.scale
            }
        };
        let height = |v: f64| match (fraction(v) * dots_tall as f64).ceil() as usize {
            0 if v > 0.0 => 1,
            h => h.min(dots_tall),
        };
        let sample = |back: usize| self.data.iter().rev().nth(back).map_or(0, |v| height(*v));
        for col in 0..area.width {
            let from_right = (area.width - 1 - col) as usize;
            let (l, r) = (sample(from_right * 2 + 1), sample(from_right * 2));
            for row in 0..area.height as usize {
                let mut mask = 0u8;
                for (side, h) in [l, r].into_iter().enumerate() {
                    let fill = h.saturating_sub(row * 4).min(4);
                    mask |= dots[side][..fill].iter().sum::<u8>();
                }
                if mask != 0 {
                    let y = if self.hang {
                        area.y + row as u16
                    } else {
                        area.bottom() - 1 - row as u16
                    };
                    let color = gradient((row as f64 + 0.5) / f64::from(area.height));
                    let ch = char::from_u32(0x2800 + u32::from(mask)).unwrap();
                    buf[(area.x + col, y)].set_char(ch).set_fg(color);
                }
            }
        }
    }
}

fn scale_for(s: &Series, width: u16) -> f64 {
    let n = width as usize * 2;
    s.input
        .iter()
        .rev()
        .take(n)
        .chain(s.output.iter().rev().take(n))
        .fold(MIN_SCALE, |m, v| m.max(*v))
}

/// Mirrored graph: in rises from the middle, out hangs below it.
fn mirrored(f: &mut Frame, area: Rect, s: &Series, o: &Options) {
    let scale = scale_for(s, area.width);
    let [up, down] = Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).areas(area);
    f.render_widget(
        Graph {
            data: &s.input,
            scale,
            hang: false,
            log: o.log_scale,
        },
        up,
    );
    f.render_widget(
        Graph {
            data: &s.output,
            scale,
            hang: true,
            log: o.log_scale,
        },
        down,
    );
    let peak = Line::styled(format!(" {}/s", rate(scale, o)), Style::new().fg(th().dim));
    f.render_widget(Paragraph::new(peak), Rect { height: 1, ..area });
}

fn meter(frac: f64, width: usize) -> Line<'static> {
    let on = ((frac.clamp(0.0, 1.0) * width as f64).ceil() as usize).min(width);
    let on = if frac > 0.0 { on.max(1) } else { 0 };
    Line::from(
        (0..width)
            .map(|i| {
                let color = if i < on {
                    gradient(i as f64 / width as f64)
                } else {
                    th().faint
                };
                Span::styled("■", Style::new().fg(color))
            })
            .collect::<Vec<_>>(),
    )
}

fn load(rate: f64, d: &Device) -> f64 {
    if d.mbps == 0.0 {
        0.0
    } else {
        rate * 8.0 / (d.mbps * 1e6)
    }
}

/// Root hubs report a protocol too, but they have no translator.
fn is_tt_hub(d: &Device) -> bool {
    d.hub && d.depth() > 0 && d.mbps == 480.0
}

fn stuck_at_12m(d: &Device) -> bool {
    d.hub && d.mbps == 12.0 && d.usb.starts_with('2')
}

fn tt_tag(d: &Device) -> &'static str {
    match (is_tt_hub(d), d.tt) {
        (true, 1) => " STT",
        (true, 2) => " MTT",
        _ => "",
    }
}

fn link_style(d: &Device) -> Style {
    // A USB 2 hub stuck at 12M usually means a bad link, and it pushes its
    // whole branch through its parent's translator.
    if stuck_at_12m(d) {
        Style::new().fg(th().warn)
    } else {
        Style::new().fg(th().dim)
    }
}

fn devices(f: &mut Frame, area: Rect, app: &mut App) {
    let drops = if app.dropped > 0 {
        format!(" {} dropped ", app.dropped)
    } else {
        String::new()
    };
    let block = panel("devices").title_bottom(Line::styled(
        format!(" ↑↓/click select · o options · 1-4 boxes · h idle · q quit {drops}"),
        Style::new().fg(if app.dropped > 0 { th().warn } else { th().dim }),
    ));
    let header = Row::new(["device", "link", "in", "out", "load", "err"].map(Cell::from))
        .style(Style::new().fg(th().hi).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .shown
        .iter()
        .filter_map(|i| Some((app.devs.get(*i)?, app.rates.get(*i)?)))
        .map(|(d, r)| {
            let branch = if d.depth() == 0 {
                String::new()
            } else {
                format!("{}└ ", "  ".repeat(d.depth() - 1))
            };
            let errs = app.errors.get(&d.key()).copied().unwrap_or(0);
            let name_style = if d.depth() == 0 {
                Style::new().fg(th().hi).add_modifier(Modifier::BOLD)
            } else if d.hub {
                Style::new().fg(th().dim)
            } else {
                Style::new().fg(th().text)
            };
            let name = Line::from(vec![
                Span::styled(branch, Style::new().fg(th().faint)),
                Span::styled(d.label.clone(), name_style),
                Span::styled(
                    tt_tag(d),
                    Style::new().fg(if d.tt == 1 { th().warn } else { th().dim }),
                ),
            ]);
            let active = |v: f64| Style::new().fg(if v >= 1.0 { th().hi } else { th().faint });
            Row::new(vec![
                Cell::from(name),
                Cell::from(Span::styled(format!("{}M", d.mbps), link_style(d))),
                Cell::from(Span::styled(rate(r.input, &app.opts), active(r.input))),
                Cell::from(Span::styled(rate(r.output, &app.opts), active(r.output))),
                Cell::from(meter(load(r.input + r.output, d), 8)),
                Cell::from(Span::styled(
                    if errs > 0 {
                        errs.to_string()
                    } else {
                        "·".into()
                    },
                    Style::new().fg(if errs > 0 { th().bad } else { th().faint }),
                )),
            ])
        })
        .collect();
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(5),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::new().fg(Color::Black).bg(th().accent));
    app.table_area = area;
    f.render_stateful_widget(table, area, &mut app.table);
}

fn buses(f: &mut Frame, area: Rect, app: &App) {
    let inner = area.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    f.render_widget(panel("buses"), area);
    let width = (inner.width as usize).saturating_sub(17).max(4);
    let lines: Vec<Line> = app
        .devs
        .iter()
        .zip(&app.rates)
        .filter(|(d, _)| d.depth() == 0)
        .map(|(d, r)| {
            let sum = r.input + r.output;
            let mut spans = vec![
                Span::styled(format!("{:>2} ", d.bus), Style::new().fg(th().hi)),
                Span::styled(format!("{:>5}M ", d.mbps), Style::new().fg(th().dim)),
            ];
            spans.extend(meter(load(sum, d), width).spans);
            spans.push(Span::styled(
                format!(" {:>6}", rate(sum, &app.opts)),
                Style::new().fg(th().hi),
            ));
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn detail_lines(app: &App) -> Vec<Line<'static>> {
    let Some(d) = app.selected() else {
        return vec![];
    };
    let r = app.selected_rate();
    let kv = |k: &str, v: String, s: Style| {
        Line::from(vec![
            Span::styled(format!("{k:<8}"), Style::new().fg(th().dim)),
            Span::styled(v, s),
        ])
    };
    let text = Style::new().fg(th().text);
    let mut out = vec![
        kv("path", format!("{}  {}", d.name, d.id), text),
        kv("link", format!("{} Mbit/s, USB {}", d.mbps, d.usb), text),
        kv(
            "in",
            format!("{}/s", rate(r.input, &app.opts)),
            Style::new().fg(th().hi),
        ),
        kv(
            "out",
            format!("{}/s", rate(r.output, &app.opts)),
            Style::new().fg(th().hi),
        ),
    ];
    if is_tt_hub(d) && matches!(d.tt, 1 | 2) {
        let name = if d.tt == 1 {
            "Single, all slow ports share one"
        } else {
            "Multi, one per port"
        };
        out.push(kv(
            "TT",
            name.into(),
            Style::new().fg(if d.tt == 1 { th().warn } else { th().hi }),
        ));
    }
    if let Some(h) = topo::translator(&app.devs, d) {
        let shared = app
            .devs
            .iter()
            .filter(|c| c.mbps < 480.0 && !c.hub && c.is_inside(h))
            .count();
        let (kind, style) = match h.tt {
            1 => (
                format!("Single, shared by {shared} devices"),
                Style::new().fg(th().warn),
            ),
            2 => ("Multi".to_string(), Style::new().fg(th().hi)),
            _ => ("unknown".to_string(), text),
        };
        out.push(kv("via TT", format!("{}: {kind}", h.name), style));
    }
    if stuck_at_12m(d) {
        out.push(kv(
            "link!",
            "USB 2 hub running at 12M".into(),
            Style::new().fg(th().warn),
        ));
    }
    out
}

fn detail(f: &mut Frame, area: Rect, app: &App) {
    let title = app.selected().map_or(String::new(), |d| d.label.clone());
    f.render_widget(panel(&title), area);
    let inner = area.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    let lines = detail_lines(app);
    let [text, graph] =
        Layout::vertical([Constraint::Length(lines.len() as u16), Constraint::Fill(1)])
            .areas(inner);
    f.render_widget(Paragraph::new(lines), text);
    if let Some(s) = app.selected().and_then(|d| app.hist.get(&d.name)) {
        mirrored(f, graph, s, &app.opts);
    }
}

fn events(f: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = app
        .log
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|l| {
            let color = if l.contains(" ! ") {
                th().bad
            } else {
                th().dim
            };
            Line::styled(l.clone(), Style::new().fg(color))
        })
        .collect();
    let body = if lines.is_empty() {
        vec![Line::styled(
            "no errors or changes yet",
            Style::new().fg(th().faint),
        )]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(body).block(panel("events")), area);
}

/// Splits `area` between two boxes, giving all of it to whichever is on.
fn split(layout: Layout, area: Rect, a: bool, b: bool) -> (Rect, Rect) {
    match (a, b) {
        (true, true) => {
            let parts = layout.split(area);
            (parts[0], parts[1])
        }
        (true, false) => (area, Rect::default()),
        (false, true) => (Rect::default(), area),
        _ => (Rect::default(), Rect::default()),
    }
}

fn options_menu(f: &mut Frame, app: &mut App, line: usize) {
    const WIDTH: u16 = 44;
    let height = opts::ITEMS as u16 + 2;
    let s = f.area();
    let area = Rect {
        x: s.x + s.width.saturating_sub(WIDTH) / 2,
        y: s.y + s.height.saturating_sub(height) / 2,
        width: WIDTH.min(s.width),
        height: height.min(s.height),
    };
    app.menu_area = area;
    let rows: Vec<Line> = (0..opts::ITEMS)
        .map(|i| {
            let style = if i == line {
                Style::new().fg(Color::Black).bg(th().accent)
            } else {
                Style::new().fg(th().text)
            };
            Line::styled(
                format!(
                    " {:<20}{:>19} ",
                    Options::label(i),
                    format!("◄ {} ►", app.opts.value(i))
                ),
                style,
            )
        })
        .collect();
    let hint = Line::styled(
        " ↑↓ pick · ←→ change · esc close ",
        Style::new().fg(th().dim),
    );
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(rows).block(panel("options").title_bottom(hint)),
        area,
    );
}

pub fn draw(f: &mut Frame, app: &mut App) {
    THEME.store(app.opts.theme, Ordering::Relaxed);
    let o = app.opts.clone();
    let bus_rows = app.devs.iter().filter(|d| d.depth() == 0).count() as u16 + 2;
    let side_by_side = Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]);
    let (top, bottom) = split(
        Layout::vertical([Constraint::Length(bus_rows.max(9)), Constraint::Fill(1)]),
        f.area(),
        o.total || o.buses,
        true,
    );
    let (total, bus) = split(side_by_side.clone(), top, o.total, o.buses);
    let (left, right) = split(side_by_side, bottom, true, o.detail || o.events);
    let (selected, log) = split(
        Layout::vertical([Constraint::Fill(3), Constraint::Fill(2)]),
        right,
        o.detail,
        o.events,
    );

    let i = app.total.input.back().copied().unwrap_or(0.0);
    let u = app.total.output.back().copied().unwrap_or(0.0);
    if o.total {
        f.render_widget(
            panel(&format!("usb  in {}/s  out {}/s", rate(i, &o), rate(u, &o))),
            total,
        );
        mirrored(
            f,
            total.inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            }),
            &app.total,
            &o,
        );
    }
    if o.buses {
        buses(f, bus, app);
    }
    devices(f, left, app);
    if o.detail {
        detail(f, selected, app);
    }
    if o.events {
        events(f, log, app);
    }
    if let Some(line) = app.menu {
        options_menu(f, app, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::Totals;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn human_units() {
        assert_eq!(human(0.0), "0");
        assert_eq!(human(512.0), "512B");
        assert_eq!(human(2048.0), "2.0K");
        assert_eq!(human(5.0 * 1048576.0), "5.0M");
        assert_eq!(
            rate(
                1.25 * 1024.0,
                &Options {
                    bits: true,
                    ..Options::default()
                }
            ),
            "10.0Kb"
        );
    }

    #[test]
    fn graph_fills_up_and_hangs_down() {
        let data: VecDeque<f64> = [10.0, 10.0].into();
        let area = Rect::new(0, 0, 1, 1);
        let mut b = Buffer::empty(area);
        Graph {
            data: &data,
            scale: 10.0,
            hang: false,
            log: false,
        }
        .render(area, &mut b);
        assert_eq!(b[(0, 0)].symbol(), "⣿");
        let half: VecDeque<f64> = [5.0, 5.0].into();
        let mut b = Buffer::empty(area);
        Graph {
            data: &half,
            scale: 10.0,
            hang: false,
            log: false,
        }
        .render(area, &mut b);
        assert_eq!(b[(0, 0)].symbol(), "⣤");
        let mut b = Buffer::empty(area);
        Graph {
            data: &half,
            scale: 10.0,
            hang: true,
            log: false,
        }
        .render(area, &mut b);
        assert_eq!(b[(0, 0)].symbol(), "⠛");
    }

    #[test]
    fn draws_the_real_tree_at_small_and_large_sizes() {
        let t = Arc::new(Mutex::new(HashMap::<_, Totals>::new()));
        let mut app = App::new(t, None, crate::topo::SYSFS);
        app.tick();
        for (w, h) in [(40, 10), (120, 40), (240, 70)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &mut app)).unwrap();
            // Every box off, then every theme, with the menu on top.
            app.menu = Some(0);
            for i in 5..opts::ITEMS {
                app.opts.change(i, true);
            }
            term.draw(|f| draw(f, &mut app)).unwrap();
            for i in 5..opts::ITEMS {
                app.opts.change(i, true);
            }
            for _ in 0..opts::THEMES.len() {
                app.opts.change(1, true);
                term.draw(|f| draw(f, &mut app)).unwrap();
            }
            app.menu = None;
        }
    }
}
