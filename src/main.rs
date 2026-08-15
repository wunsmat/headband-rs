mod host;
mod notes;

use crossterm::clipboard::CopyToClipboard;
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
use std::io::{BufRead, BufReader, Read, Write};
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
    flash: String,
    share: Vec<(u16, String)>,
    hover: Option<u16>,
    pub_ip: Arc<Mutex<String>>,
    entry: Notes,
    notes: Notes,
    on_notes: bool,
    notes_inner: Rect,
    entry_rect: Rect,
    log_rect: Rect,
    log_scroll: usize,
    notes_hidden: bool,
    mine_only: bool,
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
            flash: String::new(),
            share: Vec::new(),
            hover: None,
            pub_ip: Arc::new(Mutex::new(String::new())),
            entry: Notes::new(),
            notes: Notes::new(),
            on_notes: false,
            notes_inner: Rect::ZERO,
            entry_rect: Rect::ZERO,
            log_rect: Rect::ZERO,
            log_scroll: 0,
            notes_hidden: false,
            mine_only: false,
            quit: false,
        }
    }

    fn active(&self) -> bool {
        if !self.joined {
            return true;
        }
        match self.state.phase.as_str() {
            "assign" => self.target().is_none_or(|p| p.thing.is_empty()),
            "play" => self.state.turn == self.you,
            _ => false,
        }
    }

    fn can_type(&self) -> bool {
        !self.joined || self.state.phase != "lobby"
    }

    fn target(&self) -> Option<&Player> {
        let &t = self.state.assigns.get(self.you)?;
        self.state.players.get(usize::try_from(t).ok()?)
    }

    fn target_name(&self) -> &str {
        self.target().map_or("", |p| p.name.as_str())
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
        if !self.active() {
            self.flash = "not your turn yet, this waits here.".into();
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
            let new_round = u.state.phase == "assign"
                && (self.state.phase != "assign" || u.state.log.len() < self.state.log.len());
            self.state = u.state;
            self.you = u.you;
            self.joined = true;
            if new_round {
                self.entry = Notes::new();
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        self.flash.clear();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') if ctrl => self.quit = true,
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
            KeyCode::Char('n') if ctrl => {
                self.notes_hidden = !self.notes_hidden;
                if self.notes_hidden {
                    self.on_notes = false;
                }
            }
            KeyCode::Char('g') if ctrl => {
                self.mine_only = !self.mine_only;
                self.log_scroll = 0;
            }
            KeyCode::Char('u') if ctrl => {
                self.log_scroll = (self.log_scroll + self.log_page()).min(self.max_log_scroll())
            }
            KeyCode::Char('d') if ctrl => {
                self.log_scroll = self.log_scroll.saturating_sub(self.log_page())
            }
            KeyCode::Tab => {
                if !self.notes_hidden {
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
        if !self.can_type() {
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

    fn log_lines(&self) -> Vec<&String> {
        self.state
            .log
            .iter()
            .filter(|l| !self.mine_only || speaker(&self.state, l) == Some(self.you))
            .collect()
    }

    fn max_log_scroll(&self) -> usize {
        self.log_lines()
            .len()
            .saturating_sub(self.log_rect.height as usize)
    }

    fn copy_share(&mut self, y: u16) -> bool {
        let Some((_, addr)) = self.share.iter().find(|(row, _)| *row == y) else {
            return false;
        };
        let addr = addr.clone();
        self.flash = match execute!(
            std::io::stdout(),
            CopyToClipboard::to_clipboard_from(addr.clone())
        ) {
            Ok(()) => format!("copied {addr}"),
            Err(e) => format!("could not copy: {e}"),
        };
        true
    }

    fn on_mouse(&mut self, x: u16, y: u16, kind: MouseEventKind) {
        self.hover = Some(y);
        if kind == MouseEventKind::Down(MouseButton::Left) && self.copy_share(y) {
            return;
        }
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
                if self.entry_rect.contains(Position::new(x, y)) && self.can_type() =>
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

fn speaker(state: &State, line: &str) -> Option<usize> {
    state.players.iter().position(|p| line.starts_with(&p.name))
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
                format!("Lobby: {} connected.", p.len()),
                accent,
            )];
            match &app.addr {
                None => {
                    lines.push(Line::from(vec![
                        Span::raw("Friends run  "),
                        Span::styled(format!("headband {}", local_ip(&app.port)), accent),
                        Span::styled(" ⧉ ", dim),
                        Span::styled("  same wifi", dim),
                    ]));
                    let ip = app.pub_ip.lock().unwrap().clone();
                    if !ip.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("          or "),
                            Span::styled(format!("headband {}:{}", ip, app.port), accent),
                            Span::styled(" ⧉ ", dim),
                            Span::styled("  internet", dim),
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
            if app.target().is_some_and(|p| !p.thing.is_empty()) {
                vec![Line::styled("Sent. Waiting for everyone else.", dim)]
            } else {
                vec![Line::styled(
                    format!("You assign {}. What are they?", app.target_name()),
                    accent,
                )]
            }
        }
        "play" => {
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
            if p[app.you].done {
                lines.insert(
                    0,
                    Line::styled(
                        format!(" YOU GOT IT: {} ", p[app.you].thing),
                        Style::new().fg(Color::Black).bg(Color::Green).bold(),
                    ),
                );
                lines.push(Line::styled(
                    "Hang around and watch the others suffer.",
                    dim,
                ));
            } else if app.state.turn == app.you {
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
                "-".to_string()
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

fn help_keys(app: &App) -> Vec<&'static str> {
    let mut keys = vec![
        "ctrl+n: notes",
        "ctrl+u/d: log",
        "ctrl+g: just me",
        "ctrl+k: pass",
        "ctrl+q: quit",
    ];
    if !app.notes_hidden {
        keys.insert(0, "tab: panes");
    }
    if app.you == 0 && app.joined {
        keys.insert(
            0,
            if app.state.phase == "lobby" {
                "ctrl+s: start"
            } else {
                "ctrl+r: new round"
            },
        );
    }
    keys
}

fn wrap(items: &[&str], sep: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut lines = vec![String::new()];
    for item in items {
        let line = lines.last_mut().unwrap();
        if line.is_empty() {
            line.push_str(item);
        } else if line.chars().count() + sep.chars().count() + item.chars().count() <= width {
            line.push_str(sep);
            line.push_str(item);
        } else {
            lines.push((*item).to_string());
        }
    }
    lines
}

fn ui(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    let (help_lines, colour) = {
        let w = screen.width.saturating_sub(2);
        let (msg, colour) = if app.err.is_empty() {
            (&app.flash, ACCENT)
        } else {
            (&app.err, BAD)
        };
        if msg.is_empty() {
            (wrap(&help_keys(app), " · ", w), DIM)
        } else {
            (wrap(&msg.split(' ').collect::<Vec<_>>(), " ", w), colour)
        }
    };
    let outer = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(help_lines.len() as u16),
    ])
    .split(screen);

    let footer: Vec<Line> = help_lines
        .into_iter()
        .map(|l| Line::styled(l, Style::new().fg(colour)))
        .collect();
    frame.render_widget(Paragraph::new(footer), outer[1].inner(Margin::new(1, 0)));

    let cols = if app.notes_hidden {
        Layout::horizontal([Constraint::Percentage(100)]).split(outer[0])
    } else {
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(outer[0])
    };
    let mut status_lines = status(app);
    let roster_lines = roster(app);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(status_lines.len() as u16 + 1),
        Constraint::Length(roster_lines.len() as u16 + 1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(cols[0].inner(Margin::new(1, 1)));

    frame.render_widget(
        Paragraph::new(Line::styled("HEADBAND", Style::new().fg(ACCENT).bold())),
        rows[0],
    );
    app.share.clear();
    for (i, line) in status_lines.iter_mut().enumerate() {
        let Some(j) = line
            .spans
            .iter()
            .position(|s| s.content.starts_with("headband "))
        else {
            continue;
        };
        let row = rows[1].y + i as u16;
        app.share.push((row, line.spans[j].content.to_string()));
        if app.hover == Some(row) {
            line.spans[j].style = line.spans[j].style.underlined();
            if let Some(icon) = line.spans.get_mut(j + 1) {
                icon.style = Style::new().fg(ACCENT).bold();
            }
        }
    }
    frame.render_widget(
        Paragraph::new(status_lines).wrap(Wrap { trim: false }),
        rows[1],
    );
    frame.render_widget(Paragraph::new(roster_lines), rows[2]);

    app.log_rect = rows[3];
    let fits = rows[3].height as usize;
    app.log_scroll = app.log_scroll.min(app.max_log_scroll());
    let shown = app.log_lines();
    let end = shown.len() - app.log_scroll;
    let mut log: Vec<Line> = shown[end.saturating_sub(fits)..end]
        .iter()
        .map(|l| {
            if l.ends_with('✓') {
                Line::styled(
                    format!(" {l} "),
                    Style::new().fg(Color::Black).bg(Color::Green).bold(),
                )
            } else {
                let mine = speaker(&app.state, l) == Some(app.you);
                Line::styled((*l).clone(), Style::new().fg(if mine { MINE } else { DIM }))
            }
        })
        .collect();
    if app.log_scroll > 0 {
        log.insert(
            0,
            Line::styled(
                format!("▲ {} older, scroll down for the latest", app.log_scroll),
                Style::new().fg(BAD),
            ),
        );
        log.truncate(fits);
    }
    frame.render_widget(Paragraph::new(log), rows[3]);

    app.entry_rect = rows[4];
    let held = !app.active();
    let dim = Style::new().fg(DIM);
    let mut spans = vec![];
    if !app.can_type() {
    } else if app.entry.lines[0].is_empty() {
        spans.push(Span::styled("> ", dim));
        let hint = if !app.joined {
            "your name".to_string()
        } else if held {
            "queue your next guess".to_string()
        } else if app.state.phase == "assign" {
            format!("what {} is", app.target_name())
        } else {
            "your guess".to_string()
        };
        spans.push(Span::styled(hint, dim));
    } else {
        spans.push(Span::styled("> ", if held { dim } else { Style::new() }));
        spans.push(Span::raw(app.entry.lines[0].clone()));
        if held {
            spans.push(Span::styled("  waiting for your turn", dim));
        }
    }
    let entry = Line::from(spans);
    frame.render_widget(Paragraph::new(entry), rows[4]);

    app.notes_inner = Rect::ZERO;
    if !app.notes_hidden {
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
    }

    let inner = app.notes_inner;
    if app.on_notes {
        if let Some((x, y)) = app.notes.cursor_at(inner) {
            frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
        }
    } else if app.can_type() {
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
        let mut resp = String::new();
        let _ = BufReader::new(s).read_to_string(&mut resp);
        let ip = resp
            .split_once("\r\n\r\n")
            .map_or("", |(_, b)| b)
            .trim()
            .to_string();
        if !ip.is_empty() && ip.len() < 46 {
            *slot.lock().unwrap() = ip;
        }
    });
}

fn main() -> std::io::Result<()> {
    let mut port = PORT.to_string();
    let mut addr = None;
    let mut hide_notes = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--hide-notes" => hide_notes = true,
            "-port" | "--port" => port = args.next().unwrap_or(port),
            "-h" | "--help" => {
                println!(
                    "headband            host a game\nheadband ADDRESS    join one\n  -port PORT\n  --hide-notes  start with the notes pane hidden"
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
    }

    let mut app = App::new(addr, port);
    app.notes_hidden = hide_notes;
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

    fn styles(app: &mut App, w: u16, h: u16, row: u16) -> Vec<Style> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..w).map(|x| buf[(x, row)].style()).collect()
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
    fn keybinds_are_a_full_width_footer_under_everything() {
        let lines = render(&mut playing(), 100, 40);
        let row = |n: &str| lines.iter().position(|l| l.contains(n));
        let entry = row("your guess").expect("no input line");
        let help = row("tab: panes").expect("no help line");
        let pane_bottom = lines
            .iter()
            .rposition(|l| l.contains('└'))
            .expect("no notes pane");

        assert!(entry < pane_bottom, "input should sit inside the columns");
        assert!(
            help > pane_bottom,
            "keybinds should be under the notes pane, not beside it"
        );
        assert_eq!(help, lines.len() - 1, "keybinds should be the last row");
        assert!(
            lines[help].trim_start().starts_with("tab: panes"),
            "footer should start at the left edge: {:?}",
            lines[help]
        );
        assert!(
            lines.iter().any(|l| l.contains("ctrl+q: quit")),
            "the quit key must never be cut off"
        );
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
    fn ctrl_n_hides_the_notes_and_gives_the_width_back() {
        let mut app = playing();
        let with = render(&mut app, 100, 20);
        assert!(with.iter().any(|l| l.contains("NOTES")), "no notes pane");

        app.on_key(ctrl('n'));
        let without = render(&mut app, 100, 20);
        assert!(app.notes_hidden);
        assert!(
            !without.iter().any(|l| l.contains("NOTES")),
            "notes pane still drawn"
        );
        assert!(with.iter().any(|l| l.contains('│')), "no pane border");
        assert!(
            !without.iter().any(|l| l.contains('│')),
            "pane border still drawn"
        );

        app.on_key(ctrl('n'));
        assert!(!app.notes_hidden, "ctrl+n toggles back");
    }

    #[test]
    fn help_wraps_instead_of_truncating_on_a_narrow_terminal() {
        for width in [40u16, 60, 100, 200] {
            let mut app = playing();
            app.you = 0;
            app.state.turn = 0;
            let lines = render(&mut app, width, 24);
            let joined = lines.join(" ");
            for key in ["ctrl+r: new round", "ctrl+k: pass", "ctrl+q: quit"] {
                assert!(joined.contains(key), "lost {key:?} at width {width}");
            }
        }
    }

    #[test]
    fn wrap_never_splits_a_key_in_half() {
        let items = ["tab: panes", "ctrl+n: notes", "ctrl+q: quit"];
        for width in [8u16, 20, 41, 200] {
            let lines = wrap(&items, " · ", width);
            for item in items {
                assert!(
                    lines.iter().any(|l| l.contains(item)),
                    "{item:?} was split at width {width}"
                );
            }
        }
        assert_eq!(wrap(&items, " · ", 200).len(), 1, "no needless wrapping");
    }

    #[test]
    fn an_error_replaces_the_footer() {
        let mut app = playing();
        app.err = "connection lost".into();
        let lines = render(&mut app, 100, 40);
        assert!(
            lines.iter().any(|l| l.contains("connection lost")),
            "the error never showed"
        );
        assert!(
            !lines.iter().any(|l| l.contains("ctrl+q: quit")),
            "the footer should give way to the error"
        );
    }

    #[test]
    fn hidden_notes_never_take_focus() {
        let mut app = playing();
        app.on_notes = true;
        app.on_key(ctrl('n'));
        assert!(!app.on_notes, "hiding must drop focus");

        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.on_notes, "tab must not reach a hidden pane");
    }

    #[test]
    fn typing_still_reaches_the_input_while_hidden() {
        let mut app = playing();
        app.state.turn = app.you;
        app.on_key(ctrl('n'));
        for c in "duck".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.entry.lines[0], "duck");
        assert!(app.notes.is_empty(), "notes must not have swallowed it");
    }

    #[test]
    fn lobby_addresses_are_clickable_rows() {
        let mut app = App::new(None, "7777".into());
        app.joined = true;
        app.state.phase = "lobby".into();
        app.state.players = vec![Player {
            name: "Matt".into(),
            ..Default::default()
        }];
        *app.pub_ip.lock().unwrap() = "1.2.3.4".into();
        let lines = render(&mut app, 100, 20);

        assert_eq!(app.share.len(), 2, "both addresses should be clickable");
        assert!(
            app.share.iter().any(|(_, a)| a == "headband 1.2.3.4:7777"),
            "the runnable command should be copied, not the bare address: {:?}",
            app.share
        );
        for (row, addr) in &app.share {
            assert!(
                lines[*row as usize].contains(addr.as_str()),
                "row {row} does not hold {addr}: {:?}",
                lines[*row as usize]
            );
            assert!(lines[*row as usize].contains('⧉'), "no copy icon on {addr}");
        }

        let miss = app.log_rect.y;
        assert!(!app.copy_share(miss), "only the address rows copy");

        let (row, _) = app.share[0].clone();
        let plain = styles(&mut app, 100, 20, row);
        app.on_mouse(0, row, MouseEventKind::Moved);
        let hot = styles(&mut app, 100, 20, row);
        assert_ne!(plain, hot, "hovered row should look different");
        assert!(
            hot.iter()
                .any(|s| s.add_modifier.contains(Modifier::UNDERLINED)),
            "address should underline on hover"
        );
        app.on_mouse(0, miss, MouseEventKind::Moved);
        assert_eq!(
            styles(&mut app, 100, 20, row),
            plain,
            "highlight should drop when the mouse leaves"
        );

        app.state.phase = "play".into();
        render(&mut app, 100, 20);
        assert!(app.share.is_empty(), "stale rows must not stay clickable");
    }

    #[test]
    fn finished_players_still_see_the_clock() {
        let mut app = playing();
        app.you = 0;
        app.state.turn = 1;
        let joined = render(&mut app, 100, 20).join("\n");
        assert!(joined.contains("YOU GOT IT: B"), "lost the winner banner");
        assert!(joined.contains("Todd is asking."), "no whose-turn line");
        assert!(joined.contains("72s"), "no clock: {joined}");
    }

    #[test]
    fn my_log_lines_stand_out_and_ctrl_g_keeps_only_mine() {
        let mut app = playing();
        app.state.log = vec![
            "Matt guessed \"cat\": nope.".into(),
            "Todd passed.".into(),
            "Todd guessed \"dog\": nope.".into(),
        ];
        render(&mut app, 100, 20);
        let (row, x) = (app.log_rect.y, app.log_rect.x as usize);
        assert_eq!(
            styles(&mut app, 100, 20, row)[x].fg,
            Some(DIM),
            "someone else's line should stay dim"
        );
        assert_eq!(
            styles(&mut app, 100, 20, row + 2)[x].fg,
            Some(MINE),
            "my own line should stand out"
        );

        app.on_key(ctrl('g'));
        let joined = render(&mut app, 100, 20).join("\n");
        assert!(
            !joined.contains("Matt guessed"),
            "someone else's line stayed"
        );
        assert!(joined.contains("dog"), "lost my guess");
        assert!(joined.contains("Todd passed."), "all my lines should show");

        app.on_key(ctrl('g'));
        let joined = render(&mut app, 100, 20).join("\n");
        assert!(joined.contains("Matt guessed"), "ctrl+g toggles back");
    }

    #[test]
    fn the_lobby_has_no_input_until_the_host_starts() {
        let mut app = App::new(None, "7777".into());
        app.joined = true;
        app.state.phase = "lobby".into();
        app.state.players = vec![Player {
            name: "Matt".into(),
            ..Default::default()
        }];
        let lines = render(&mut app, 100, 20);
        let row = app.entry_rect.y as usize;
        assert!(
            !lines[row].contains('>'),
            "still prompting: {:?}",
            lines[row]
        );

        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(
            app.entry.lines[0].is_empty(),
            "the lobby swallowed a keystroke"
        );

        app.state.phase = "assign".into();
        let lines = render(&mut app, 100, 20);
        assert!(
            lines[row].contains('>'),
            "no prompt once the game starts: {:?}",
            lines[row]
        );
    }

    #[test]
    fn a_new_round_clears_a_queued_guess() {
        let mut app = playing();
        let (tx, rx) = channel();
        app.rx = Some(rx);
        let send = |phase: &str, app: &App| {
            let mut state = app.state.clone();
            state.phase = phase.into();
            tx.send(Update {
                state,
                you: app.you,
            })
            .unwrap();
        };

        app.entry.lines[0] = "duck".into();
        app.entry.col = 4;
        send("assign", &app);
        app.drain();
        assert!(app.entry.lines[0].is_empty(), "a stale guess survived");
        assert_eq!(app.entry.col, 0, "the cursor kept the old column");

        app.entry.lines[0] = "cat".into();
        send("assign", &app);
        app.drain();
        assert_eq!(
            app.entry.lines[0], "cat",
            "only a new round should clear it"
        );

        send("assign", &app);
        app.state.log.push("stale".into());
        app.drain();
        assert!(
            app.entry.lines[0].is_empty(),
            "a restart mid-assign wipes the log, so it is a new round too"
        );
    }

    #[test]
    fn tab_swaps_panes_off_turn_too() {
        let mut app = playing();
        app.state.turn = 0;
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.on_notes, "tab should still reach the notes");
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.on_notes, "tab should come back to the input");
    }

    #[test]
    fn a_guess_waits_for_your_turn() {
        let mut app = playing();
        app.state.turn = 0;
        for c in "duck".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.entry.lines[0], "duck",
            "the queued guess was thrown away"
        );
        let joined = render(&mut app, 100, 20).join("\n");
        assert!(
            joined.contains("waiting for your turn"),
            "nothing says it cannot send yet: {joined}"
        );

        app.state.turn = app.you;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.entry.lines[0].is_empty(), "your turn should send it");
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
