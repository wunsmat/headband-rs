use ratatui::layout::Rect;

#[derive(Default)]
pub struct Notes {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    pub scroll: usize,
    anchor: Option<(usize, usize)>,
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

fn width(s: &str) -> usize {
    s.chars().count()
}

impl Notes {
    pub fn new() -> Self {
        Notes {
            lines: vec![String::new()],
            ..Default::default()
        }
    }

    fn line(&self) -> &String {
        &self.lines[self.row]
    }

    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let b = (self.row, self.col);
        if a == b {
            return None;
        }
        Some(if a < b { (a, b) } else { (b, a) })
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            return false;
        };
        let head: String = self.lines[s.0].chars().take(s.1).collect();
        let tail: String = self.lines[e.0].chars().skip(e.1).collect();
        self.lines.drain(s.0..=e.0);
        self.lines.insert(s.0, head + &tail);
        (self.row, self.col) = s;
        self.anchor = None;
        true
    }

    pub fn segments(&self, idx: usize) -> Vec<(String, bool)> {
        let line = &self.lines[idx];
        let plain = || vec![(line.clone(), false)];
        let Some((s, e)) = self.selection() else {
            return plain();
        };
        if idx < s.0 || idx > e.0 {
            return plain();
        }
        let n = width(line);
        let start = if idx == s.0 { s.1 } else { 0 };
        let end = if idx == e.0 { e.1 } else { n };
        let cut = |a: usize, b: usize| -> String {
            line.chars().skip(a).take(b.saturating_sub(a)).collect()
        };
        let mut out = vec![];
        if start > 0 {
            out.push((cut(0, start), false));
        }
        let mut mid = cut(start, end);
        if mid.is_empty() {
            mid.push(' ');
        }
        out.push((mid, true));
        if end < n {
            out.push((cut(end, n), false));
        }
        out
    }

    pub fn insert(&mut self, ch: char) {
        self.delete_selection();
        let at = byte_at(self.line(), self.col);
        self.lines[self.row].insert(at, ch);
        self.col += 1;
    }

    pub fn newline(&mut self) {
        self.delete_selection();
        let at = byte_at(self.line(), self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.col > 0 {
            let at = byte_at(self.line(), self.col - 1);
            self.lines[self.row].remove(at);
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = width(self.line());
            self.lines[self.row].push_str(&cur);
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.col < width(self.line()) {
            let at = byte_at(self.line(), self.col);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = width(self.line());
        }
    }

    pub fn right(&mut self) {
        if self.col < width(self.line()) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(width(self.line()));
        }
    }

    pub fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(width(self.line()));
        }
    }

    pub fn home(&mut self) {
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.col = width(self.line());
    }

    pub fn click(&mut self, x: u16, y: u16, inner: Rect) {
        self.move_to(x, y, inner);
        self.anchor = None;
    }

    pub fn drag_to(&mut self, x: u16, y: u16, inner: Rect) {
        if self.anchor.is_none() {
            self.anchor = Some((self.row, self.col));
        }
        self.move_to(x, y, inner);
    }

    fn move_to(&mut self, x: u16, y: u16, inner: Rect) {
        let dy = y.saturating_sub(inner.y) as usize;
        let dx = x.saturating_sub(inner.x) as usize;
        self.row = (self.scroll + dy).min(self.lines.len() - 1);
        self.col = dx.min(width(self.line()));
    }

    pub fn scroll_by(&mut self, delta: isize, height: u16) {
        let max = self.lines.len().saturating_sub(height as usize);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    pub fn follow(&mut self, height: u16) {
        let h = height.max(1) as usize;
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + h {
            self.scroll = self.row + 1 - h;
        }
    }

    pub fn cursor_at(&self, inner: Rect) -> Option<(u16, u16)> {
        let dy = self.row.checked_sub(self.scroll)?;
        if dy >= inner.height as usize {
            return None;
        }
        Some((inner.x + self.col as u16, inner.y + dy as u16))
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Notes {
        let mut n = Notes::new();
        for ch in s.chars() {
            if ch == '\n' {
                n.newline()
            } else {
                n.insert(ch)
            }
        }
        n
    }

    #[test]
    fn edits_text() {
        let mut n = typed("hello\nworld");
        assert_eq!(n.lines, vec!["hello", "world"]);
        n.backspace();
        assert_eq!(n.lines, vec!["hello", "worl"]);
        n.home();
        n.backspace();
        assert_eq!(n.lines, vec!["helloworl"], "backspace at col 0 joins lines");
        assert_eq!((n.row, n.col), (0, 5));
    }

    #[test]
    fn handles_wide_chars() {
        let mut n = typed("héllo");
        n.home();
        n.right();
        n.delete();
        assert_eq!(n.lines, vec!["hllo"], "delete must not split a utf-8 char");
    }

    #[test]
    fn click_lands_on_the_clicked_character() {
        let mut n = typed("one\ntwo\nthree");
        let inner = Rect::new(10, 5, 20, 3);
        n.click(13, 6, inner);
        assert_eq!((n.row, n.col), (1, 3));

        n.click(99, 99, inner);
        assert_eq!((n.row, n.col), (2, 5), "clicks past the text clamp to it");
    }

    #[test]
    fn click_accounts_for_scroll() {
        let mut n = typed("a\nb\nc\nd\ne");
        n.scroll = 3;
        let inner = Rect::new(0, 0, 10, 2);
        n.click(0, 0, inner);
        assert_eq!(n.row, 3, "first visible row is the scrolled-to row");
    }

    #[test]
    fn scroll_stops_at_the_ends() {
        let mut n = typed("a\nb\nc\nd\ne");
        n.scroll_by(-5, 2);
        assert_eq!(n.scroll, 0);
        n.scroll_by(99, 2);
        assert_eq!(n.scroll, 3, "last page stays full");
    }

    #[test]
    fn cursor_hides_when_scrolled_away() {
        let mut n = typed("a\nb\nc\nd");
        let inner = Rect::new(0, 0, 10, 2);
        n.row = 3;
        n.col = 0;
        assert_eq!(n.cursor_at(inner), None);
        n.follow(2);
        assert_eq!(n.cursor_at(inner), Some((0, 1)));
    }

    const INNER: Rect = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 5,
    };

    #[test]
    fn dragging_selects_a_span() {
        let mut n = typed("hello world");
        n.click(2, 0, INNER);
        assert_eq!(n.selection(), None, "a plain click selects nothing");
        n.drag_to(7, 0, INNER);
        assert_eq!(n.selection(), Some(((0, 2), (0, 7))));
    }

    #[test]
    fn dragging_backwards_still_orders_the_span() {
        let mut n = typed("hello world");
        n.click(7, 0, INNER);
        n.drag_to(2, 0, INNER);
        assert_eq!(n.selection(), Some(((0, 2), (0, 7))));
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut n = typed("hello world");
        n.click(0, 0, INNER);
        n.drag_to(6, 0, INNER);
        n.insert('b');
        assert_eq!(n.lines, vec!["bworld"]);
        assert_eq!((n.row, n.col), (0, 1));
        assert_eq!(n.selection(), None, "selection is consumed");
    }

    #[test]
    fn backspace_deletes_across_lines() {
        let mut n = typed("one\ntwo\nthree");
        n.click(1, 0, INNER);
        n.drag_to(3, 2, INNER);
        n.backspace();
        assert_eq!(n.lines, vec!["oee"]);
        assert_eq!((n.row, n.col), (0, 1));
    }

    #[test]
    fn segments_split_the_line_for_rendering() {
        let mut n = typed("hello world");
        n.click(2, 0, INNER);
        n.drag_to(7, 0, INNER);
        assert_eq!(
            n.segments(0),
            vec![
                ("he".to_string(), false),
                ("llo w".to_string(), true),
                ("orld".to_string(), false),
            ]
        );
    }

    #[test]
    fn middle_lines_select_whole_and_blanks_stay_visible() {
        let mut n = typed("one\n\nthree");
        n.click(1, 0, INNER);
        n.drag_to(2, 2, INNER);
        assert_eq!(
            n.segments(0),
            vec![("o".into(), false), ("ne".into(), true)]
        );
        assert_eq!(n.segments(1), vec![(" ".into(), true)], "blank line shows");
        assert_eq!(
            n.segments(2),
            vec![("th".into(), true), ("ree".into(), false)]
        );
    }

    #[test]
    fn typing_after_a_click_does_not_eat_itself() {
        let mut n = typed("hello");
        n.click(2, 0, INNER);
        n.insert('X');
        assert_eq!(n.selection(), None, "a bare click must select nothing");
        n.insert('Y');
        assert_eq!(
            n.lines,
            vec!["heXYllo"],
            "the second letter overwrote the first"
        );
    }

    #[test]
    fn a_new_click_drops_the_old_selection() {
        let mut n = typed("hello world");
        n.click(2, 0, INNER);
        n.drag_to(7, 0, INNER);
        n.click(1, 0, INNER);
        assert_eq!(n.selection(), None);
        assert_eq!(n.segments(0), vec![("hello world".to_string(), false)]);
    }
}
