mod app;
mod demo;
mod opts;
mod topo;
mod ui;
mod usbmon;

use app::App;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Opens usbmon, loading the module or re-running under sudo when it must.
fn open_monitor() -> io::Result<Arc<std::fs::File>> {
    let root = unsafe { libc::geteuid() } == 0;
    match usbmon::open() {
        Err(e)
            if !root && matches!(e.kind(), ErrorKind::PermissionDenied | ErrorKind::NotFound) =>
        {
            let me = std::env::current_exe()?;
            Err(Command::new("sudo")
                .arg(me)
                .args(std::env::args_os().skip(1))
                .exec())
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Command::new("modprobe").arg("usbmon").status()?;
            // udev creates the node a moment after the module loads.
            for _ in 0..20 {
                if std::path::Path::new(usbmon::DEVICE).exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            usbmon::open()
        }
        other => other,
    }
}

/// `usbtron --dump SECS`: prints one table and exits, for checks and scripts.
fn dump(app: &mut App, secs: f64) {
    app.tick();
    std::thread::sleep(Duration::from_secs_f64(secs));
    app.tick();
    for (d, r) in app.devs.iter().zip(&app.rates) {
        let errs = app.errors.get(&d.key()).copied().unwrap_or(0);
        println!(
            "{:<14} {:>5}M tt={} in={:>7} out={:>7} err={errs} {}",
            d.name,
            d.mbps,
            d.tt,
            ui::human(r.input),
            ui::human(r.output),
            d.label
        );
    }
    println!("dropped={}", app.dropped);
}

/// Menu lines the hotkeys jump straight to.
const HIDE_IDLE: usize = 4;
const FIRST_BOX: usize = 5;

fn run(app: &mut App) -> io::Result<()> {
    let mut term = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = (|| -> io::Result<()> {
        let mut next = Instant::now();
        loop {
            if Instant::now() >= next {
                app.tick();
                next = Instant::now() + Duration::from_millis(app.opts.tick_ms);
            }
            term.draw(|f| ui::draw(f, app))?;
            if event::poll(next.saturating_duration_since(Instant::now()))? {
                match event::read()? {
                    Event::Key(k) if k.code == KeyCode::Char('q') => return Ok(()),
                    Event::Key(k) if matches!(k.code, KeyCode::Esc | KeyCode::Char('o')) => {
                        app.toggle_menu()
                    }
                    Event::Key(k) => match (app.menu, k.code) {
                        (Some(_), KeyCode::Up | KeyCode::Char('k')) => app.menu_move(-1),
                        (Some(_), KeyCode::Down | KeyCode::Char('j')) => app.menu_move(1),
                        (Some(i), KeyCode::Left | KeyCode::Char('h')) => app.change(i, false),
                        (Some(i), KeyCode::Right | KeyCode::Char('l' | ' ') | KeyCode::Enter) => {
                            app.change(i, true)
                        }
                        (Some(_), _) => {}
                        (None, KeyCode::Up | KeyCode::Char('k')) => app.move_sel(-1),
                        (None, KeyCode::Down | KeyCode::Char('j')) => app.move_sel(1),
                        (None, KeyCode::PageUp) => app.move_sel(-10),
                        (None, KeyCode::PageDown) => app.move_sel(10),
                        (None, KeyCode::Char('h')) => app.change(HIDE_IDLE, true),
                        (None, KeyCode::Char(c @ '1'..='4')) => {
                            app.change(FIRST_BOX + (c as usize - '1' as usize), true)
                        }
                        _ => {}
                    },
                    Event::Mouse(m) => match (m.kind, app.menu.is_some()) {
                        (MouseEventKind::Down(MouseButton::Left), true) => {
                            app.menu_click(m.column, m.row)
                        }
                        (MouseEventKind::Down(MouseButton::Left), false) => {
                            app.click(m.column, m.row)
                        }
                        (MouseEventKind::ScrollUp, true) => app.menu_move(-1),
                        (MouseEventKind::ScrollDown, true) => app.menu_move(1),
                        (MouseEventKind::ScrollUp, false) => app.move_sel(-1),
                        (MouseEventKind::ScrollDown, false) => app.move_sel(1),
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    })();
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

const HELP: &str = "\
usbtron: live USB bandwidth, topology and errors in a btop-style terminal UI

Usage: usbtron [--demo] [--dump [SECONDS]]

  --demo            invented hardware and traffic; needs no root
  --dump [SECONDS]  print one table after SECONDS (default 3) and exit
  -h, --help        show this help
  -V, --version     show the version

Keys: o or Esc options, 1-4 boxes, h idle devices, q quit
";

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("-h") || has("--help") {
        print!("{HELP}");
        return Ok(());
    }
    if has("-V") || has("--version") {
        println!("usbtron {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let totals = Arc::new(Mutex::new(HashMap::new()));
    let feed = totals.clone();
    let mut app = if has("--demo") {
        std::thread::spawn(move || demo::feed(feed));
        let mut app = App::new(totals, None, topo::SYSFS);
        app.use_fixed_devices(demo::devices());
        app
    } else {
        let mon = open_monitor().map_err(|e| {
            io::Error::new(e.kind(), format!("cannot open {}: {e}", usbmon::DEVICE))
        })?;
        let reader = mon.clone();
        std::thread::spawn(move || usbmon::read_forever(reader, feed));
        App::new(totals, Some(mon), topo::SYSFS)
    };
    app.opts = opts::Options::load();
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(3.0);
        dump(&mut app, secs);
        return Ok(());
    }
    run(&mut app)
}
