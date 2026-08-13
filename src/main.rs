mod host;
mod notes;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use host::{Cmd, Player, State, TURN_LIMIT, Update, now_ms, serve};
use notes::Notes;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PORT: &str = "7777";
const BAR_CELLS: usize = 24;
const WHEEL: usize = 3;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const BAD: Color = Color::Red;
const MINE: Color = Color::Magenta;

struct App {
    addr: Option<String>,
    port: String,
    out: Option<TcpStream>,
    rx: Option<Receiver<Update>>,
    state: State,
    you: usize,
    joined: bool,
    err: String,
    pub_ip: Arc<Mutex<String>>,
    entry: Notes,
    notes: Notes,
    on_notes: bool,
    was_active: bool,
    notes_inner: Rect,
    entry_rect: Rect,
    log_rect: Rect,
    log_scroll: usize,
    quit: bool,
}

impl App {
    fn new(addr: Option<String>, port: String) -> Self {
        App {
            addr,
            port,
            out: None,
            rx: None,
            state: State::default(),
            you: 0,
            joined: false,
            err: String::new(),
            pub_ip: Arc::new(Mutex::new(String::new())),
            entry: Notes::new(),
            notes: Notes::new(),
            on_notes: false,
            was_active: true,
            notes_inner: Rect::ZERO,
            entry_rect: Rect::ZERO,
            log_rect: Rect::ZERO,
            log_scroll: 0,
            quit: false,
        }
    }

    fn active(&self) -> bool {
        if !self.joined {
            return true;
        }
        match self.state.phase.as_str() {
            "assign" => self.target().thing.is_empty(),
            "play" => self.state.turn == self.you,
            _ => false,
        }
    }

    fn target(&self) -> Player {
        if self.state.assigns.len() != self.state.players.len() {
            return Player::default();
        }
        match self.state.assigns.get(self.you) {
            Some(&t) if t >= 0 => self.state.players[t as usize].clone(),
            _ => Player::default(),
        }
    }

    fn connect(&mut self, name: &str) -> std::io::Result<()> {
        let target = match &self.addr {
            Some(a) => a.clone(),
            None => {
                serve(&format!("0.0.0.0:{}", self.port))?;
                format!("127.0.0.1:{}", self.port)
            }
        };
        let stream = TcpStream::connect(target)?;
        let reader = stream.try_clone()?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Update>(&line) {
                    Ok(u) => {
                        if tx.send(u).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        self.out = Some(stream);
        self.rx = Some(rx);
        self.send(Cmd {
            cmd: "join".into(),
            name: name.into(),
            ..Default::default()
        });
        Ok(())
    }

    fn send(&mut self, c: Cmd) {
        let Some(out) = self.out.as_mut() else { return };
        let mut line = serde_json::to_vec(&c).unwrap_or_default();
        line.push(b'\n');
        if out.write_all(&line).is_err() {
            self.err = "connection lost".into();
        }
    }

    fn submit(&mut self) {
        let text = self.entry.lines[0].trim().to_string();
        if text.is_empty() {
            return;
        }
        self.entry.lines[0].clear();
        self.entry.col = 0;
        if !self.joined {
            if let Err(e) = self.connect(&text) {
                self.err = format!("could not connect: {e}");
            }
            return;
        }
        let cmd = match self.state.phase.as_str() {
            "assign" => "thing",
            "play" => "guess",
            _ => return,
        };
        self.send(Cmd {
            cmd: cmd.into(),
            text,
            ..Default::default()
        });
    }

    fn drain(&mut self) {
        let mut latest = None;
        if let Some(rx) = &self.rx {
            while let Ok(u) = rx.try_recv() {
                latest = Some(u);
            }
        }
        if let Some(u) = latest {
            self.state = u.state;
            self.you = u.you;
            self.joined = true;
            let now = self.active();
            if now != self.was_active {
                self.was_active = now;
                self.on_notes = !now;
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char(c @ ('s' | 'r' | 'k')) if ctrl => {
                let cmd = match c {
                    's' => "start",
                    'r' => "restart",
                    _ => "skip",
                };
                self.send(Cmd {
                    cmd: cmd.into(),
                    ..Default::default()
                })
            }
            KeyCode::Char('u') if ctrl => {
                self.log_scroll = (self.log_scroll + self.log_page()).min(self.max_log_scroll())
            }
            KeyCode::Char('d') if ctrl => {
                self.log_scroll = self.log_scroll.saturating_sub(self.log_page())
            }
            KeyCode::Tab => {
                if self.active() {
                    self.on_notes = !self.on_notes;
                }
            }
            _ if self.on_notes => edit(&mut self.notes, k),
            _ => self.entry_key(k),
        }
        if self.on_notes {
            self.notes.follow(self.notes_inner.height);
        }
    }

    fn entry_key(&mut self, k: KeyEvent) {
        if !self.active() {
            return;
        }
        if k.code == KeyCode::Enter {
            self.submit();
        } else {
            edit(&mut self.entry, k);
        }
    }

    fn log_page(&self) -> usize {
        (self.log_rect.height as usize / 2).max(1)
    }

    fn max_log_scroll(&self) -> usize {
        self.state
            .log
            .len()
            .saturating_sub(self.log_rect.height as usize)
    }

    fn on_mouse(&mut self, x: u16, y: u16, kind: MouseEventKind) {
        let in_notes = self.notes_inner.contains(Position::new(x, y));
        let in_log = self.log_rect.contains(Position::new(x, y));
        match kind {
            MouseEventKind::ScrollUp if in_log => {
                self.log_scroll = (self.log_scroll + WHEEL).min(self.max_log_scroll())
            }
            MouseEventKind::ScrollDown if in_log => {
                self.log_scroll = self.log_scroll.saturating_sub(WHEEL)
            }
            MouseEventKind::ScrollUp if in_notes => self
                .notes
                .scroll_by(-(WHEEL as isize), self.notes_inner.height),
            MouseEventKind::ScrollDown if in_notes => self
                .notes
                .scroll_by(WHEEL as isize, self.notes_inner.height),
            MouseEventKind::Down(MouseButton::Left) if in_notes => {
                self.on_notes = true;
                self.notes.click(x, y, self.notes_inner);
            }
            MouseEventKind::Drag(MouseButton::Left) if in_notes && self.on_notes => {
                self.notes.drag_to(x, y, self.notes_inner);
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.entry_rect.contains(Position::new(x, y)) && self.active() =>
            {
                self.on_notes = false;
                let dx = x.saturating_sub(self.entry_rect.x + 2) as usize;
                self.entry.col = dx.min(self.entry.lines[0].chars().count());
            }
            _ => {}
        }
    }
}

fn edit(n: &mut Notes, k: KeyEvent) {
    use KeyCode::*;
    if matches!(k.code, Left | Right | Up | Down | Home | End) {
        n.clear_selection();
    }
    match k.code {
        Char(c) => n.insert(c),
        Enter => n.newline(),
        Backspace => n.backspace(),
        Delete => n.delete(),
        Left => n.left(),
        Right => n.right(),
        Up => n.up(),
        Down => n.down(),
        Home => n.home(),
        End => n.end(),
        _ => {}
    }
}

fn clock(state: &State) -> Line<'static> {
    if state.deadline == 0 {
        return Line::default();
    }
    let left = ((state.deadline - now_ms()).max(0) as u64).div_ceil(1000) as usize;
    let total = TURN_LIMIT.as_secs() as usize;
    let filled = (left * BAR_CELLS / total).min(BAR_CELLS);
    let color = if left <= 15 { BAD } else { ACCENT };
    Line::from(vec![
        Span::styled(
            format!("{left:2}s {}", "█".repeat(filled)),
            Style::new().fg(color).bold(),
        ),
        Span::styled("░".repeat(BAR_CELLS - filled), Style::new().fg(DIM)),
    ])
}

fn status(app: &App) -> Vec<Line<'static>> {
    let accent = Style::new().fg(ACCENT).bold();
    let dim = Style::new().fg(DIM);
    if !app.joined {
        return vec![Line::styled("Type your name and hit enter.", accent)];
    }
    let p = &app.state.players;
    match app.state.phase.as_str() {
        "lobby" => {
            let mut lines = vec![Line::styled(
                format!("Lobby — {} connected.", p.len()),
                accent,
            )];
            match &app.addr {
                None => {
                    lines.push(Line::from(vec![
                        Span::raw("Friends run  "),
                        Span::styled(format!("headband {}", local_ip(&app.port)), accent),
                        Span::styled("   same wifi", dim),
                    ]));
                    let ip = app.pub_ip.lock().unwrap().clone();
                    if !ip.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("          or "),
                            Span::styled(format!("headband {}:{}", ip, app.port), accent),
                            Span::styled("   internet — needs port forward or tailscale", dim),
                        ]));
                    }
                }
                Some(a) => lines.push(Line::styled(format!("Joined {a}."), dim)),
            }
            lines.push(if app.you == 0 {
                Line::styled("ctrl+s to start (2+ players).", accent)
            } else {
                Line::styled("Waiting for the host to start.", dim)
            });
            lines
        }
        "assign" => {
            if !app.target().thing.is_empty() {
                vec![Line::styled("Sent. Waiting for everyone else.", dim)]
            } else {
                vec![Line::styled(
                    format!("You assign {}. What are they?", app.target().name),
                    accent,
                )]
            }
        }
        "play" => {
            if p[app.you].done {
                return vec![
                    Line::styled(
                        format!(" YOU GOT IT: {} ", p[app.you].thing),
                        Style::new().fg(Color::Black).bg(Color::Green).bold(),
                    ),
                    Line::styled("Hang around and watch the others suffer.", dim),
                ];
            }
            let mut head = if app.state.turn == app.you {
                vec![Span::styled("YOUR TURN", accent), Span::raw("  ")]
            } else {
                vec![
                    Span::styled(format!("{} is asking.", p[app.state.turn].name), dim),
                    Span::raw("  "),
                ]
            };
            head.extend(clock(&app.state).spans);
            let mut lines = vec![Line::from(head)];
            if app.state.turn == app.you {
                lines.push(Line::styled(
                    "ask your yes/no question, then type a guess, or ctrl+k to pass.",
                    dim,
                ));
            }
            lines
        }
        _ => vec![Line::styled(
            format!("Game over. You were: {}", p[app.you].thing),
            accent,
        )],
    }
}

fn roster(app: &App) -> Vec<Line<'static>> {
    app.state
        .players
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let me = i == app.you;
            let thing = if me && !q.done {
                "???".to_string()
            } else if q.thing.is_empty() {
                "—".to_string()
            } else {
                q.thing.clone()
            };
            let name = if me {
                format!("{} (you)", q.name)
            } else {
                q.name.clone()
            };
            let style = if me {
                Style::new().fg(MINE).bold()
            } else {
                Style::new()
            };
            let mut spans = vec![
                Span::styled(
                    if app.state.phase == "play" && app.state.turn == i {
                        "▶ "
                    } else {
                        "  "
                    },
                    Style::new().fg(ACCENT).bold(),
                ),
                Span::styled(format!("{name:<20}"), style),
                Span::styled(thing, style),
            ];
            if q.done {
                spans.push(Span::raw(" ✓"));
            }
            if q.off {
                spans.push(Span::styled(" (gone)", Style::new().fg(DIM)));
            }
            Line::from(spans)
        })
        .collect()
}

fn ui(frame: &mut Frame, app: &mut App) {
    let cols = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(frame.area());
    let status_lines = status(app);
    let roster_lines = roster(app);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(status_lines.len() as u16 + 1),
        Constraint::Length(roster_lines.len() as u16 + 1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(cols[0].inner(Margin::new(1, 1)));

    frame.render_widget(
        Paragraph::new(Line::styled("HEADBAND", Style::new().fg(ACCENT).bold())),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(status_lines).wrap(Wrap { trim: false }),
        rows[1],
    );
    frame.render_widget(Paragraph::new(roster_lines), rows[2]);

    app.log_rect = rows[3];
    let fits = rows[3].height as usize;
    app.log_scroll = app.log_scroll.min(app.max_log_scroll());
    let end = app.state.log.len() - app.log_scroll;
    let mut log: Vec<Line> = app.state.log[end.saturating_sub(fits)..end]
        .iter()
        .map(|l| {
            if l.ends_with('✓') {
                Line::styled(
                    format!(" {l} "),
                    Style::new().fg(Color::Black).bg(Color::Green).bold(),
                )
            } else {
                Line::styled(l.clone(), Style::new().fg(DIM))
            }
        })
        .collect();
    if app.log_scroll > 0 {
        log.insert(
            0,
            Line::styled(
                format!("▲ {} older — scroll down for the latest", app.log_scroll),
                Style::new().fg(BAD),
            ),
        );
        log.truncate(fits);
    }
    frame.render_widget(Paragraph::new(log), rows[3]);

    app.entry_rect = rows[4];
    let entry = if app.active() {
        let hint = if !app.joined {
            "your name".to_string()
        } else if app.state.phase == "assign" {
            format!("what {} is", app.target().name)
        } else {
            "your guess".to_string()
        };
        if app.entry.lines[0].is_empty() {
            Line::from(vec![
                Span::raw("> "),
                Span::styled(hint, Style::new().fg(DIM)),
            ])
        } else {
            Line::raw(format!("> {}", app.entry.lines[0]))
        }
    } else {
        Line::styled(
            "  (nothing to type — you're taking notes)",
            Style::new().fg(DIM),
        )
    };
    frame.render_widget(Paragraph::new(entry), rows[4]);

    let mut help = vec![
        "tab: panes",
        "ctrl+u/d: log",
        "ctrl+k: pass",
        "ctrl+c: quit",
    ];
    if app.you == 0 && app.joined {
        help.insert(
            0,
            if app.state.phase == "lobby" {
                "ctrl+s: start"
            } else {
                "ctrl+r: new round"
            },
        );
    }
    let help = if app.err.is_empty() {
        Line::styled(help.join(" · "), Style::new().fg(DIM))
    } else {
        Line::styled(app.err.clone(), Style::new().fg(BAD))
    };
    frame.render_widget(Paragraph::new(help), rows[5]);

    let notes_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(if app.on_notes { ACCENT } else { DIM }))
        .title(Span::styled(" NOTES ", Style::new().fg(ACCENT).bold()));
    let inner = notes_block.inner(cols[1]);
    app.notes_inner = inner;
    frame.render_widget(notes_block, cols[1]);

    if app.notes.is_empty() && !app.on_notes {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "click here to take notes",
                Style::new().fg(DIM),
            )),
            inner,
        );
    } else {
        let top = app.notes.scroll;
        let rows = (app.notes.lines.len() - top).min(inner.height as usize);
        let selected = Style::new().fg(Color::Black).bg(ACCENT);
        let visible: Vec<Line> = (top..top + rows)
            .map(|i| {
                Line::from(
                    app.notes
                        .segments(i)
                        .into_iter()
                        .map(|(text, sel)| {
                            Span::styled(text, if sel { selected } else { Style::new() })
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        frame.render_widget(Paragraph::new(visible), inner);
    }

    if app.on_notes {
        if let Some((x, y)) = app.notes.cursor_at(inner) {
            frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
        }
    } else if app.active() {
        frame.set_cursor_position((rows[4].x + 2 + app.entry.col as u16, rows[4].y));
    }
}

fn local_ip(port: &str) -> String {
    let ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            Ok(s.local_addr()?.ip().to_string())
        })
        .unwrap_or_else(|_| "127.0.0.1".into());
    format!("{ip}:{port}")
}

fn fetch_public_ip(slot: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let Ok(mut s) = TcpStream::connect("api.ipify.org:80") else {
            return;
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        if s.write_all(b"GET / HTTP/1.0\r\nHost: api.ipify.org\r\n\r\n")
            .is_err()
        {
            return;
        }
        let mut body = String::new();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if line.trim().is_empty() {
                body.clear();
                let _ = reader.read_line(&mut body);
                break;
            }
            line.clear();
        }
        let ip = body.trim().to_string();
        if !ip.is_empty() && ip.len() < 46 {
            *slot.lock().unwrap() = ip;
        }
    });
}

fn main() -> std::io::Result<()> {
    let mut port = PORT.to_string();
    let mut addr = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-port" | "--port" if i + 1 < args.len() => {
                port = args[i + 1].clone();
                i += 1;
            }
            "-h" | "--help" => {
                println!(
                    "headband            host a game\nheadband ADDRESS    join one\n  -port PORT"
                );
                return Ok(());
            }
            a => {
                addr = Some(if a.contains(':') {
                    a.to_string()
                } else {
                    format!("{a}:{port}")
                })
            }
        }
        i += 1;
    }

    let mut app = App::new(addr, port);
    if app.addr.is_none() {
        fetch_public_ip(app.pub_ip.clone());
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> std::io::Result<()> {
    while !app.quit {
        terminal.draw(|f| ui(f, app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                Event::Mouse(m) => app.on_mouse(m.column, m.row, m.kind),
                _ => {}
            }
        }
        app.drain();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use host::Player;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn playing() -> App {
        let mut app = App::new(None, "7777".into());
        app.joined = true;
        app.you = 1;
        app.state = State {
            phase: "play".into(),
            turn: 1,
            players: vec![
                Player {
                    name: "Matt".into(),
                    thing: "B".into(),
                    done: true,
                    off: false,
                },
                Player {
                    name: "Todd".into(),
                    thing: "A".into(),
                    done: false,
                    off: false,
                },
            ],
            assigns: vec![1, 0],
            log: vec!["Todd passed.".into()],
            deadline: now_ms() + 72_000,
        };
        app
    }

    #[test]
    fn log_follows_the_roster_instead_of_sinking() {
        let lines = render(&mut playing(), 100, 40);
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle));
        let roster = row("Matt").expect("no roster");
        let log = row("Todd passed.").expect("no log");
        assert!(
            log - roster <= 3,
            "log drifted from the roster: roster row {roster}, log row {log}"
        );
    }

    #[test]
    fn input_and_help_stay_pinned_to_the_bottom() {
        let lines = render(&mut playing(), 100, 40);
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle));
        let entry = row("your guess").expect("no input line");
        let help = row("ctrl+c: quit").expect("no help line");
        assert!(entry >= lines.len() - 3, "input drifted up to row {entry}");
        assert_eq!(help, entry + 1, "help should sit under the input");
    }

    #[test]
    fn long_log_keeps_the_newest_lines() {
        let mut app = playing();
        app.state.log = (0..60).map(|i| format!("event {i}")).collect();
        let joined = render(&mut app, 100, 20).join("\n");
        assert!(joined.contains("event 59"), "newest line was cut");
        assert!(
            !joined.contains("event 0 "),
            "oldest line should scroll off"
        );
    }

    #[test]
    fn wheel_scrolls_the_log_and_snaps_back() {
        let mut app = playing();
        app.state.log = (0..60).map(|i| format!("event {i}")).collect();
        render(&mut app, 100, 20);

        let (x, y) = (app.log_rect.x, app.log_rect.y);
        app.on_mouse(x, y, MouseEventKind::ScrollUp);
        let joined = render(&mut app, 100, 20).join("\n");
        assert_eq!(app.log_scroll, 3);
        assert!(joined.contains("▲ 3 older"), "no scrolled-back marker");
        assert!(!joined.contains("event 59"), "should be showing history");

        for _ in 0..99 {
            app.on_mouse(x, y, MouseEventKind::ScrollDown);
        }
        let joined = render(&mut app, 100, 20).join("\n");
        assert_eq!(app.log_scroll, 0, "scrolling down should pin to newest");
        assert!(joined.contains("event 59"));
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_u_and_d_page_the_log() {
        let mut app = playing();
        app.state.log = (0..60).map(|i| format!("event {i}")).collect();
        render(&mut app, 100, 20);
        let page = app.log_page();
        assert!(page > 1, "a 20-row screen should give a real page");

        app.on_key(ctrl('u'));
        assert_eq!(app.log_scroll, page, "ctrl+u goes back a page");
        app.on_key(ctrl('u'));
        assert_eq!(app.log_scroll, page * 2);
        app.on_key(ctrl('d'));
        assert_eq!(app.log_scroll, page, "ctrl+d comes forward a page");

        for _ in 0..99 {
            app.on_key(ctrl('d'));
        }
        assert_eq!(app.log_scroll, 0, "ctrl+d stops at the newest line");
        for _ in 0..99 {
            app.on_key(ctrl('u'));
        }
        assert_eq!(
            app.log_scroll,
            app.max_log_scroll(),
            "ctrl+u stops at the oldest"
        );
    }

    #[test]
    fn log_cannot_scroll_past_its_history() {
        let mut app = playing();
        app.state.log = (0..60).map(|i| format!("event {i}")).collect();
        render(&mut app, 100, 20);
        for _ in 0..99 {
            app.on_mouse(app.log_rect.x, app.log_rect.y, MouseEventKind::ScrollUp);
        }
        render(&mut app, 100, 20);
        assert_eq!(app.log_scroll, app.max_log_scroll());
        assert!(app.log_scroll < app.state.log.len(), "scrolled off the end");
    }
}
