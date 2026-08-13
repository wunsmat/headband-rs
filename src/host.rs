use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TURN_LIMIT: Duration = Duration::from_secs(90);

const LOG_KEEP: usize = 60;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Player {
    pub name: String,
    pub thing: String,
    pub done: bool,
    pub off: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub phase: String,
    pub turn: usize,
    pub players: Vec<Player>,
    pub assigns: Vec<i32>,
    pub log: Vec<String>,
    pub deadline: i64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Cmd {
    pub cmd: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Serialize, Deserialize)]
pub struct Update {
    pub state: State,
    pub you: usize,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub struct Host {
    pub state: State,
    conns: HashMap<usize, TcpStream>,
    pub limit: Duration,
}

impl Host {
    pub fn new() -> Self {
        Host {
            state: State {
                phase: "lobby".into(),
                ..Default::default()
            },
            conns: HashMap::new(),
            limit: TURN_LIMIT,
        }
    }

    pub fn join(&mut self, name: String, conn: Option<TcpStream>) -> usize {
        let me = self.state.players.len();
        let mut name = name.trim().to_string();
        if name.is_empty() {
            name = format!("player{}", me + 1);
        } else if name.chars().count() > 16 {
            name = name.chars().take(16).collect();
        }
        self.state.players.push(Player {
            name,
            ..Default::default()
        });
        if let Some(c) = conn {
            self.conns.insert(me, c);
        }
        self.broadcast();
        me
    }

    pub fn apply(&mut self, me: usize, c: Cmd) {
        match c.cmd.as_str() {
            "start" if me == 0 && self.state.phase == "lobby" && self.live().len() >= 2 => {
                self.new_round("Assign phase — everyone name the thing for their target.");
            }
            "restart" if me == 0 && self.state.phase != "lobby" && self.live().len() >= 2 => {
                self.state.log.clear();
                self.new_round("New round — assign again.");
            }
            "thing"
                if self.state.phase == "assign"
                    && !c.text.trim().is_empty()
                    && me < self.state.assigns.len()
                    && self.state.assigns[me] >= 0 =>
            {
                let target = self.state.assigns[me] as usize;
                self.state.players[target].thing = c.text;
                self.start_play();
            }
            "guess" | "skip" if self.state.phase == "play" && me == self.state.turn => {
                let name = self.state.players[me].name.clone();
                let thing = self.state.players[me].thing.clone();
                if c.cmd == "skip" {
                    self.log(&format!("{name} passed."));
                } else if matches(&c.text, &thing) {
                    self.state.players[me].done = true;
                    self.log(&format!("{name} got it: {thing} ✓"));
                } else {
                    self.log(&format!("{name} guessed \"{}\" — nope.", c.text));
                }
                self.next_turn();
            }
            _ => return,
        }
        self.broadcast();
    }

    pub fn leave(&mut self, me: usize) {
        self.conns.remove(&me);
        self.state.players[me].off = true;
        let name = self.state.players[me].name.clone();
        self.log(&format!("{name} dropped."));
        match self.state.phase.as_str() {
            "assign" => {
                let owed = self.state.assigns.get(me).copied().unwrap_or(-1);
                if owed >= 0 && self.state.players[owed as usize].thing.is_empty() {
                    self.new_round("Redrawing — they left before assigning.");
                }
                self.start_play();
            }
            "play" if self.state.turn == me => self.next_turn(),
            _ => {}
        }
        self.broadcast();
    }

    fn new_round(&mut self, why: &str) {
        for p in &mut self.state.players {
            p.thing.clear();
            p.done = false;
        }
        self.state.phase = "assign".into();
        self.state.assigns = ring(&self.state.players);
        self.state.deadline = 0;
        self.log(why);
    }

    fn start_play(&mut self) {
        let ids = self.live();
        if ids.is_empty()
            || self
                .state
                .players
                .iter()
                .any(|p| p.thing.is_empty() && !p.off)
        {
            return;
        }
        self.state.phase = "play".into();
        self.state.turn = ids[rand::rng().random_range(0..ids.len())];
        self.arm();
        let name = self.state.players[self.state.turn].name.clone();
        self.log(&format!("Game on. {name} goes first."));
    }

    pub fn next_turn(&mut self) {
        if !self.state.players.iter().any(|p| !p.done && !p.off) {
            self.state.phase = "over".into();
            self.state.deadline = 0;
            self.log("Everyone got it. GG.");
            return;
        }
        let n = self.state.players.len();
        let mut t = self.state.turn;
        for _ in 0..n {
            t = (t + 1) % n;
            if !self.state.players[t].done && !self.state.players[t].off {
                break;
            }
        }
        self.state.turn = t;
        self.arm();
    }

    pub fn tick(&mut self) {
        if self.state.phase != "play" || self.state.deadline == 0 || now_ms() < self.state.deadline
        {
            return;
        }
        let name = self.state.players[self.state.turn].name.clone();
        self.log(&format!("{name} ran out of time."));
        self.next_turn();
        self.broadcast();
    }

    fn contenders(&self) -> usize {
        self.state
            .players
            .iter()
            .filter(|p| !p.done && !p.off)
            .count()
    }

    fn arm(&mut self) {
        self.state.deadline = if self.contenders() > 1 {
            now_ms() + self.limit.as_millis() as i64
        } else {
            0
        };
    }

    pub fn log(&mut self, line: &str) {
        self.state.log.push(line.to_string());
        let n = self.state.log.len();
        if n > LOG_KEEP {
            self.state.log.drain(..n - LOG_KEEP);
        }
    }

    fn live(&self) -> Vec<usize> {
        live_of(&self.state.players)
    }

    pub fn broadcast(&mut self) {
        let state = self.state.clone();
        let mut dead = vec![];
        for (&i, conn) in self.conns.iter_mut() {
            let mut line = serde_json::to_vec(&Update {
                state: state.clone(),
                you: i,
            })
            .unwrap();
            line.push(b'\n');
            if conn.write_all(&line).is_err() {
                dead.push(i);
            }
        }
        for i in dead {
            self.conns.remove(&i);
        }
    }
}

fn live_of(players: &[Player]) -> Vec<usize> {
    players
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.off)
        .map(|(i, _)| i)
        .collect()
}

pub fn ring(players: &[Player]) -> Vec<i32> {
    let mut ids = live_of(players);
    ids.shuffle(&mut rand::rng());
    let mut a = vec![-1; players.len()];
    for (i, &id) in ids.iter().enumerate() {
        a[id] = ids[(i + 1) % ids.len()] as i32;
    }
    a
}

pub fn matches(guess: &str, target: &str) -> bool {
    let g = guess.trim().to_lowercase();
    let t = target.trim().to_lowercase();
    if g.is_empty() || t.is_empty() {
        return false;
    }
    g == t || (g.chars().count() > 3 && t.contains(&g)) || (t.chars().count() > 3 && g.contains(&t))
}

pub fn serve(addr: &str) -> std::io::Result<String> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?.to_string();
    let host = Arc::new(Mutex::new(Host::new()));

    let accept = host.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let h = accept.clone();
            thread::spawn(move || client(h, stream));
        }
    });

    let ticker = host.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(200));
            ticker.lock().unwrap().tick();
        }
    });

    Ok(local)
}

fn client(host: Arc<Mutex<Host>>, stream: TcpStream) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut me: Option<usize> = None;
    for line in BufReader::new(read_half).lines() {
        let Ok(line) = line else { break };
        let Ok(cmd) = serde_json::from_str::<Cmd>(&line) else {
            continue;
        };
        match me {
            None => {
                if cmd.cmd != "join" {
                    break;
                }
                let Ok(write_half) = stream.try_clone() else {
                    break;
                };
                me = Some(host.lock().unwrap().join(cmd.name, Some(write_half)));
            }
            Some(i) => host.lock().unwrap().apply(i, cmd),
        }
    }
    if let Some(i) = me {
        host.lock().unwrap().leave(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_host(names: &[&str]) -> Host {
        let mut h = Host::new();
        for n in names {
            h.join(n.to_string(), None);
        }
        h
    }

    fn cmd(c: &str, text: &str) -> Cmd {
        Cmd {
            cmd: c.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ring_is_a_derangement() {
        for n in [2usize, 3, 7] {
            for _ in 0..200 {
                let a = ring(&vec![Player::default(); n]);
                let mut seen = vec![false; n];
                for (i, &t) in a.iter().enumerate() {
                    assert_ne!(t, i as i32, "player {i} drew themselves");
                    seen[t as usize] = true;
                }
                assert!(seen.iter().all(|&s| s), "{a:?} misses someone");
            }
        }
    }

    #[test]
    fn dropped_players_leave_the_draw() {
        let players = vec![
            Player::default(),
            Player {
                off: true,
                ..Default::default()
            },
            Player::default(),
            Player::default(),
        ];
        for _ in 0..200 {
            let a = ring(&players);
            assert_eq!(a[1], -1, "a dropped player draws nobody");
            assert!(!a.contains(&1), "a dropped player is drawn by nobody");
        }
    }

    #[test]
    fn plays_a_whole_game() {
        let mut h = new_host(&["ann", "bo", "cy"]);

        h.apply(1, cmd("start", ""));
        assert_eq!(h.state.phase, "lobby", "a guest started the game");
        h.apply(0, cmd("start", ""));
        assert_eq!(h.state.phase, "assign");

        let things = ["Batman", "a duck", "Gandalf"];
        for i in 0..3 {
            h.apply(i, cmd("thing", things[h.state.assigns[i] as usize]));
        }
        assert_eq!(h.state.phase, "play");
        for (i, p) in h.state.players.iter().enumerate() {
            assert_eq!(p.thing, things[i], "player {i} got the wrong thing");
        }
        assert!(h.state.deadline > 0, "play started with no clock");
        h.state.turn = 0;

        h.apply(1, cmd("guess", things[1]));
        assert!(!h.state.players[1].done, "out-of-turn guess landed");

        h.apply(0, cmd("guess", "Sauron"));
        assert!(!h.state.players[0].done);
        assert_eq!(h.state.turn, 1, "a miss passes the turn");

        h.apply(1, cmd("skip", ""));
        assert_eq!(h.state.turn, 2);

        h.apply(2, cmd("guess", "gandalf the grey"));
        assert!(h.state.players[2].done, "close enough should count");
        assert_eq!(h.state.turn, 0, "finished players are skipped");

        h.apply(0, cmd("guess", things[0]));
        h.apply(1, cmd("guess", "duck"));
        assert_eq!(h.state.phase, "over");
        assert_eq!(h.state.deadline, 0, "the clock stops when the game ends");
    }

    #[test]
    fn drop_during_assign_redraws() {
        let mut h = new_host(&["ann", "bo", "cy"]);
        h.apply(0, cmd("start", ""));
        h.apply(0, cmd("thing", "x"));
        h.apply(1, cmd("thing", "y"));
        h.leave(2);

        assert_eq!(h.state.phase, "assign", "should be redrawing");
        assert!(
            h.state.players.iter().all(|p| p.thing.is_empty()),
            "redraw should clear things"
        );
        h.apply(0, cmd("thing", "x"));
        h.apply(1, cmd("thing", "y"));
        assert_eq!(h.state.phase, "play", "the two left should be able to play");
        assert_ne!(h.state.turn, 2, "turn landed on the player who left");
    }

    #[test]
    fn host_restarts_the_round() {
        let mut h = new_host(&["ann", "bo"]);
        h.apply(0, cmd("start", ""));
        h.apply(0, cmd("thing", "x"));
        h.apply(1, cmd("thing", "y"));
        h.state.players[0].done = true;

        h.apply(1, cmd("restart", ""));
        assert_eq!(h.state.phase, "play", "a guest restarted the game");
        h.apply(0, cmd("restart", ""));
        assert_eq!(h.state.phase, "assign");
        assert!(
            h.state
                .players
                .iter()
                .all(|p| p.thing.is_empty() && !p.done),
            "restart should wipe the board"
        );
    }

    #[test]
    fn turn_times_out() {
        let mut h = new_host(&["ann", "bo"]);
        h.limit = Duration::from_millis(20);
        h.apply(0, cmd("start", ""));
        h.apply(0, cmd("thing", "x"));
        h.apply(1, cmd("thing", "y"));
        let first = h.state.turn;

        h.tick();
        assert_eq!(h.state.turn, first, "fired before the clock ran out");

        thread::sleep(Duration::from_millis(40));
        h.tick();
        assert!(
            h.state.log.iter().any(|l| l.contains("ran out of time")),
            "turn never timed out: {:?}",
            h.state.log
        );
        assert_ne!(h.state.turn, first);
    }

    #[test]
    fn last_player_standing_gets_no_clock() {
        let mut h = new_host(&["ann", "bo", "cy"]);
        h.limit = Duration::from_millis(20);
        h.apply(0, cmd("start", ""));
        let things = ["a", "b", "c"];
        for i in 0..3 {
            h.apply(i, cmd("thing", things[h.state.assigns[i] as usize]));
        }
        assert!(h.state.deadline > 0, "three players should be on the clock");

        h.state.turn = 0;
        h.apply(0, cmd("guess", "a"));
        assert!(h.state.deadline > 0, "two left, clock still runs");

        h.apply(h.state.turn, cmd("guess", things[h.state.turn]));
        assert_eq!(h.state.deadline, 0, "one left, no clock");

        thread::sleep(Duration::from_millis(40));
        h.tick();
        assert!(
            !h.state.log.iter().any(|l| l.contains("ran out of time")),
            "timed out the last player: {:?}",
            h.state.log
        );
        assert_eq!(h.state.phase, "play", "game should still be going");
    }

    #[test]
    fn two_clients_see_each_other() {
        let addr = serve("127.0.0.1:0").expect("bind");
        let mut last: Option<Update> = None;
        let mut keep = vec![];
        for name in ["ann", "bo"] {
            let mut s = TcpStream::connect(&addr).expect("connect");
            serde_json::to_writer(
                &mut s,
                &Cmd {
                    cmd: "join".into(),
                    name: name.into(),
                    ..Default::default()
                },
            )
            .unwrap();
            s.write_all(b"\n").unwrap();
            let mut line = String::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            while r.read_line(&mut line).unwrap() > 0 {
                let u: Update = serde_json::from_str(line.trim()).unwrap();
                let done = u.state.players.len() == 2;
                last = Some(u);
                line.clear();
                if done || name == "ann" {
                    break;
                }
            }
            keep.push(s);
        }
        let u = last.expect("no update arrived");
        assert_eq!(u.you, 1);
        assert_eq!(u.state.players.len(), 2);
        assert_eq!(u.state.players[0].name, "ann");
    }
}
