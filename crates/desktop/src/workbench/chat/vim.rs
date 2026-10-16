//! Vim's modal editing over a [`TextInput`]: Insert, Normal and Visual (charwise) with counts,
//! operators and motions, one unnamed register, undo and `.` repeat. Pure data: no GPUI, no
//! clock; the views translate keystrokes into [`Key`]s and feed them to [`Vim::key`].
//!
//! The behaviour follows Vim's documented model (`:help motion.txt`, `:help change.txt`), not
//! any editor's code. Words are runs of letters, digits and `_` (or of other non-blank
//! characters), classified by [`char::is_alphanumeric`], so Cyrillic and other scripts are
//! words like Latin ones.

use super::input::TextInput;

/// Vim's modes (Visual is charwise only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Typing inserts text.
    #[default]
    Insert,
    /// Keys are commands.
    Normal,
    /// Motions extend a selection; operators act on it.
    Visual,
}

impl Mode {
    /// The word of the mode pill.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Insert => "INSERT",
            Mode::Normal => "NORMAL",
            Mode::Visual => "VISUAL",
        }
    }

    /// Whether the caret is a block (Normal, Visual) rather than a bar (Insert).
    pub fn block_caret(self) -> bool {
        self != Mode::Insert
    }
}

/// A key press, reduced to what the modal layer answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A typed character (shift already applied: `W`, `$`).
    Char(char),
    /// ⌃ and a letter.
    Ctrl(char),
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// What [`Vim::key`] did with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The key was used (possibly as a no-op: Normal mode swallows keys it does not know).
    Handled,
    /// Not the modal layer's: the field's own key handling applies (Enter, Tab, Escape in Normal
    /// mode, and in Insert mode every key when the field types for itself).
    Ignored,
}

/// How the field the keys go to behaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// A single-line field: `j`/`k` do nothing, lines are the whole text (`dd` clears it).
    pub single_line: bool,
    /// The field types for itself in Insert mode: only Escape is the modal layer's there.
    pub insert_passthrough: bool,
    /// Yanks and deletes also go to the system clipboard, and `p` pastes from it.
    pub use_system_clipboard: bool,
}

/// The system clipboard, as far as the register needs it.
pub trait Clipboard {
    fn read(&mut self) -> Option<String>;
    fn write(&mut self, text: &str);
}

/// No clipboard (the register is private to the field).
impl Clipboard for () {
    fn read(&mut self) -> Option<String> {
        None
    }
    fn write(&mut self, _: &str) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

/// `f`, `t`, `F`, `T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Find {
    forward: bool,
    till: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Prefix {
    #[default]
    None,
    /// `g` was typed (`gg`).
    G,
    /// `r` was typed: the next character replaces.
    Replace,
    /// `f`, `t`, `F` or `T` was typed: the next character is the target.
    Find(Find),
    /// `i` or `a` after an operator (or in Visual): the next key names the object.
    Object { around: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    Word {
        big: bool,
    },
    Back {
        big: bool,
    },
    End {
        big: bool,
    },
    LineStart,
    FirstNonBlank,
    LineEnd,
    FileStart,
    FileEnd,
    Find(Find, char),
    RepeatFind {
        reverse: bool,
    },
    /// The caret's line and the ones below it (`dd`, `yy`, `cc` with a count).
    Lines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Debug, Clone, Copy)]
struct Target {
    to: usize,
    kind: Kind,
}

/// The text an operator acts on: character indices, `end` exclusive.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    linewise: bool,
}

/// What a command in progress has collected.
#[derive(Debug, Clone, Default)]
struct Pending {
    count: Option<usize>,
    op: Option<Op>,
    op_count: Option<usize>,
    prefix: Prefix,
}

impl Pending {
    fn is_idle(&self) -> bool {
        self.count.is_none()
            && self.op.is_none()
            && self.op_count.is_none()
            && self.prefix == Prefix::None
    }

    /// The count before and after the operator, multiplied; `None` when there was none.
    fn total(&self) -> Option<usize> {
        match (self.count, self.op_count) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
        }
    }
}

/// The unnamed register.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Register {
    text: String,
    linewise: bool,
}

impl Register {
    /// What the system clipboard gets: a line goes with its newline.
    fn clipboard_form(&self) -> String {
        if self.linewise {
            format!("{}\n", self.text)
        } else {
            self.text.clone()
        }
    }
}

/// The last change, for `.`: the keys of its command and the text typed in the Insert mode it
/// ended in.
#[derive(Debug, Clone, Default)]
struct Change {
    keys: Vec<Key>,
    inserted: Option<String>,
}

/// The modal state of one field. The text stays in the [`TextInput`] the keys are applied to.
#[derive(Debug, Clone, Default)]
pub struct Vim {
    mode: Mode,
    pending: Pending,
    /// Where Visual started (byte offset).
    anchor: usize,
    /// The column `j`/`k` aim for (`usize::MAX` after `$`).
    want_col: Option<usize>,
    register: Register,
    last_find: Option<(Find, char)>,
    /// Keys of the Normal-mode command in progress.
    rec: Vec<Key>,
    /// The command in progress changed the text.
    changed: bool,
    last_change: Option<Change>,
    replaying: bool,
    /// Where the current Insert session started (byte offset) and how much text followed the
    /// caret then: what was typed is between the start and the caret when it ends.
    insert_origin: Option<(usize, usize)>,
}

impl Vim {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Back to Insert with nothing pending (a field that opens for typing). The register and
    /// the last change survive.
    pub fn reset(&mut self) {
        self.mode = Mode::Insert;
        self.pending = Pending::default();
        self.rec.clear();
        self.changed = false;
        self.want_col = None;
        self.insert_origin = None;
    }

    /// The keys typed so far of a command in progress (`2d`), for display.
    pub fn pending_keys(&self) -> String {
        let mut out = String::new();
        if let Some(count) = self.pending.count {
            out.push_str(&count.to_string());
        }
        match self.pending.op {
            Some(Op::Delete) => out.push('d'),
            Some(Op::Change) => out.push('c'),
            Some(Op::Yank) => out.push('y'),
            None => {}
        }
        if let Some(count) = self.pending.op_count {
            out.push_str(&count.to_string());
        }
        match self.pending.prefix {
            Prefix::None => {}
            Prefix::G => out.push('g'),
            Prefix::Replace => out.push('r'),
            Prefix::Find(f) => out.push(match (f.forward, f.till) {
                (true, false) => 'f',
                (true, true) => 't',
                (false, false) => 'F',
                (false, true) => 'T',
            }),
            Prefix::Object { around } => out.push(if around { 'a' } else { 'i' }),
        }
        out
    }

    /// What the unnamed register holds, and whether it is whole lines.
    pub fn register(&self) -> (&str, bool) {
        (&self.register.text, self.register.linewise)
    }

    /// The selected bytes in Visual mode, the character under the caret included.
    pub fn selection(&self, input: &TextInput) -> Option<std::ops::Range<usize>> {
        if self.mode != Mode::Visual {
            return None;
        }
        let text = input.text();
        let (lo, hi) = (
            self.anchor.min(input.cursor()),
            self.anchor.max(input.cursor()),
        );
        let hi = text[hi.min(text.len())..]
            .chars()
            .next()
            .map_or(text.len(), |c| hi + c.len_utf8());
        Some(lo.min(text.len())..hi.min(text.len()))
    }

    /// Applies a key to `input`.
    pub fn key(
        &mut self,
        input: &mut TextInput,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        match self.mode {
            Mode::Insert => self.insert_key(input, key, opts),
            Mode::Normal | Mode::Visual => self.command_key(input, key, opts, clip),
        }
    }

    // ---- Insert ----------------------------------------------------------------------------

    fn insert_key(&mut self, input: &mut TextInput, key: Key, opts: Options) -> Outcome {
        if key == Key::Escape {
            self.leave_insert(input);
            return Outcome::Handled;
        }
        if opts.insert_passthrough {
            return Outcome::Ignored;
        }
        match key {
            Key::Char(c) => input.insert(c.encode_utf8(&mut [0; 4])),
            Key::Backspace => input.backspace(),
            Key::Delete => input.delete(),
            Key::Left => input.left(),
            Key::Right => input.right(),
            Key::Home => input.home(),
            Key::End => input.end(),
            Key::Up if !opts.single_line => {
                input.up();
            }
            Key::Down if !opts.single_line => {
                input.down();
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Handled
    }

    fn enter_insert(&mut self, input: &mut TextInput) {
        input.begin_group();
        self.mode = Mode::Insert;
        let cursor = input.cursor();
        self.insert_origin = Some((cursor, input.text().len() - cursor));
        self.pending = Pending::default();
    }

    fn leave_insert(&mut self, input: &mut TextInput) {
        if !self.rec.is_empty() && !self.replaying {
            let inserted = self.insert_origin.and_then(|(start, tail)| {
                let cursor = input.cursor();
                let text = input.text();
                (start <= cursor
                    && text.len() - cursor == tail
                    && text.is_char_boundary(start)
                    && start < cursor)
                    .then(|| text[start..cursor].to_owned())
            });
            self.last_change = Some(Change {
                keys: std::mem::take(&mut self.rec),
                inserted,
            });
        }
        self.rec.clear();
        self.changed = false;
        self.insert_origin = None;
        input.end_group();
        self.mode = Mode::Normal;
        self.want_col = None;
        // Like Vim, the caret steps back onto the last character typed.
        let buf = Buf::new(input.text());
        let at = buf.idx(input.cursor());
        if at > buf.line_start(at) {
            input.set_cursor(buf.byte(at - 1));
        }
    }

    // ---- Normal and Visual -----------------------------------------------------------------

    fn command_key(
        &mut self,
        input: &mut TextInput,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let outcome = self.dispatch(input, key, opts, clip);
        if self.mode == Mode::Normal && self.pending.is_idle() {
            // The command is complete.
            if self.changed && !self.replaying {
                self.last_change = Some(Change {
                    keys: std::mem::take(&mut self.rec),
                    inserted: None,
                });
            }
            self.rec.clear();
            self.changed = false;
            input.end_group();
            self.clamp_normal(input);
        } else if self.mode == Mode::Visual {
            self.rec.clear();
            self.changed = false;
        }
        outcome
    }

    fn dispatch(
        &mut self,
        input: &mut TextInput,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let visual = self.mode == Mode::Visual;
        // `.` replays; it is not itself a change to replay.
        if !visual && self.pending.is_idle() && key == Key::Char('.') {
            self.repeat(input, opts, clip);
            return Outcome::Handled;
        }
        if !visual && !self.replaying {
            self.rec.push(key);
        }

        match self.pending.prefix {
            Prefix::None => {}
            prefix => return self.dispatch_prefixed(input, prefix, key, opts, clip),
        }

        match key {
            Key::Escape => {
                if !self.pending.is_idle() {
                    self.pending = Pending::default();
                    Outcome::Handled
                } else if visual {
                    self.leave_visual(input);
                    Outcome::Handled
                } else {
                    self.rec.clear();
                    Outcome::Ignored
                }
            }
            Key::Enter | Key::Tab => {
                self.pending = Pending::default();
                Outcome::Ignored
            }
            Key::Ctrl('r') if !visual => {
                self.pending = Pending::default();
                input.redo();
                self.want_col = None;
                Outcome::Handled
            }
            Key::Ctrl(_) => {
                self.pending = Pending::default();
                Outcome::Ignored
            }
            Key::Left => self.motion_key(input, Motion::Left, opts, clip),
            Key::Right => self.motion_key(input, Motion::Right, opts, clip),
            Key::Up => self.motion_key(input, Motion::Up, opts, clip),
            Key::Down => self.motion_key(input, Motion::Down, opts, clip),
            Key::Home => self.motion_key(input, Motion::LineStart, opts, clip),
            Key::End => self.motion_key(input, Motion::LineEnd, opts, clip),
            Key::Backspace => self.motion_key(input, Motion::Left, opts, clip),
            Key::Delete => self.char_command(input, 'x', opts, clip),
            Key::Char(c) => self.char_command(input, c, opts, clip),
        }
    }

    fn dispatch_prefixed(
        &mut self,
        input: &mut TextInput,
        prefix: Prefix,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        self.pending.prefix = Prefix::None;
        let Key::Char(c) = key else {
            self.pending = Pending::default();
            return Outcome::Handled;
        };
        match prefix {
            Prefix::None => {}
            Prefix::G => {
                if c == 'g' {
                    self.motion_key(input, Motion::FileStart, opts, clip);
                } else {
                    self.pending = Pending::default();
                }
            }
            Prefix::Replace => {
                let count = self.pending.total().unwrap_or(1);
                self.pending = Pending::default();
                self.replace_chars(input, count, c);
            }
            Prefix::Find(find) => {
                self.last_find = Some((find, c));
                self.motion_key(input, Motion::Find(find, c), opts, clip);
            }
            Prefix::Object { around } => {
                let big = match c {
                    'w' => false,
                    'W' => true,
                    _ => {
                        self.pending = Pending::default();
                        return Outcome::Handled;
                    }
                };
                self.object(input, around, big, opts, clip);
            }
        }
        Outcome::Handled
    }

    /// A character key in Normal or Visual mode.
    fn char_command(
        &mut self,
        input: &mut TextInput,
        c: char,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let visual = self.mode == Mode::Visual;
        // Counts: `0` is a motion unless a count has started.
        if let Some(digit) = c.to_digit(10)
            && (digit != 0 || self.count_slot().is_some())
        {
            let slot = self.count_slot_mut();
            *slot = Some(
                slot.unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize)
                    .min(99_999),
            );
            return Outcome::Handled;
        }
        let motion = match c {
            'h' => Some(Motion::Left),
            'l' | ' ' => Some(Motion::Right),
            'j' => Some(Motion::Down),
            'k' => Some(Motion::Up),
            'w' => Some(Motion::Word { big: false }),
            'W' => Some(Motion::Word { big: true }),
            'b' => Some(Motion::Back { big: false }),
            'B' => Some(Motion::Back { big: true }),
            'e' => Some(Motion::End { big: false }),
            'E' => Some(Motion::End { big: true }),
            '0' => Some(Motion::LineStart),
            '^' => Some(Motion::FirstNonBlank),
            '$' => Some(Motion::LineEnd),
            'G' => Some(Motion::FileEnd),
            ';' => Some(Motion::RepeatFind { reverse: false }),
            ',' => Some(Motion::RepeatFind { reverse: true }),
            _ => None,
        };
        if let Some(motion) = motion {
            return self.motion_key(input, motion, opts, clip);
        }
        match c {
            'g' => self.pending.prefix = Prefix::G,
            'f' | 't' | 'F' | 'T' => {
                self.pending.prefix = Prefix::Find(Find {
                    forward: c == 'f' || c == 't',
                    till: c == 't' || c == 'T',
                });
            }
            'i' | 'a' if visual || self.pending.op.is_some() => {
                self.pending.prefix = Prefix::Object { around: c == 'a' };
            }
            'd' | 'c' | 'y' => {
                let op = match c {
                    'd' => Op::Delete,
                    'c' => Op::Change,
                    _ => Op::Yank,
                };
                if visual {
                    self.visual_op(input, op, opts, clip);
                } else if self.pending.op == Some(op) {
                    // `dd`, `cc`, `yy`: the line.
                    self.run(input, Motion::Lines, opts, clip);
                } else if self.pending.op.is_none() {
                    self.pending.op = Some(op);
                } else {
                    self.pending = Pending::default();
                }
            }
            _ if visual => self.visual_command(input, c, opts, clip),
            _ if self.pending.op.is_some() => {
                // Not a motion or an object: the operator is cancelled.
                self.pending = Pending::default();
            }
            _ => self.normal_command(input, c, opts, clip),
        }
        Outcome::Handled
    }

    fn count_slot(&self) -> Option<usize> {
        if self.pending.op.is_some() {
            self.pending.op_count
        } else {
            self.pending.count
        }
    }

    fn count_slot_mut(&mut self) -> &mut Option<usize> {
        if self.pending.op.is_some() {
            &mut self.pending.op_count
        } else {
            &mut self.pending.count
        }
    }

    fn motion_key(
        &mut self,
        input: &mut TextInput,
        motion: Motion,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        self.run(input, motion, opts, clip);
        Outcome::Handled
    }

    /// Normal-mode commands that are not operators or motions.
    fn normal_command(
        &mut self,
        input: &mut TextInput,
        c: char,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let count = self.pending.total();
        let n = count.unwrap_or(1).max(1);
        match c {
            'x' => self.run_op(input, Op::Delete, Motion::Right, opts, clip),
            'X' => self.run_op(input, Op::Delete, Motion::Left, opts, clip),
            'D' => self.run_op(input, Op::Delete, Motion::LineEnd, opts, clip),
            'C' => self.run_op(input, Op::Change, Motion::LineEnd, opts, clip),
            's' => self.run_op(input, Op::Change, Motion::Right, opts, clip),
            'S' => self.run_op(input, Op::Change, Motion::Lines, opts, clip),
            'Y' => self.run_op(input, Op::Yank, Motion::Lines, opts, clip),
            'i' => {
                self.changed = true;
                self.pending = Pending::default();
                self.enter_insert(input);
            }
            'a' => {
                self.changed = true;
                self.pending = Pending::default();
                let buf = Buf::new(input.text());
                let at = buf.idx(input.cursor());
                if at < buf.line_end(at) {
                    input.set_cursor(buf.byte(at + 1));
                }
                self.enter_insert(input);
            }
            'I' => {
                self.changed = true;
                self.pending = Pending::default();
                let buf = Buf::new(input.text());
                let at = buf.idx(input.cursor());
                let to = buf.first_nonblank(buf.line_start(at));
                input.set_cursor(buf.byte(to));
                self.enter_insert(input);
            }
            'A' => {
                self.changed = true;
                self.pending = Pending::default();
                let buf = Buf::new(input.text());
                let at = buf.idx(input.cursor());
                input.set_cursor(buf.byte(buf.line_end(at)));
                self.enter_insert(input);
            }
            'o' | 'O' => {
                self.changed = true;
                self.pending = Pending::default();
                let buf = Buf::new(input.text());
                let at = buf.idx(input.cursor());
                if opts.single_line {
                    let to = if c == 'o' { buf.len() } else { 0 };
                    input.set_cursor(buf.byte(to));
                    self.enter_insert(input);
                } else {
                    input.begin_group();
                    if c == 'o' {
                        input.set_cursor(buf.byte(buf.line_end(at)));
                        input.insert("\n");
                    } else {
                        let ls = buf.byte(buf.line_start(at));
                        input.set_cursor(ls);
                        input.insert("\n");
                        input.set_cursor(ls);
                    }
                    self.enter_insert(input);
                }
            }
            'p' | 'P' => {
                self.pending = Pending::default();
                self.paste(input, c == 'P', n, opts, clip);
            }
            'u' => {
                self.pending = Pending::default();
                for _ in 0..n {
                    if !input.undo() {
                        break;
                    }
                }
                self.want_col = None;
            }
            'r' => self.pending.prefix = Prefix::Replace,
            '~' => {
                self.pending = Pending::default();
                self.toggle_case(input, n);
            }
            'v' => {
                self.pending = Pending::default();
                self.anchor = input.cursor();
                self.mode = Mode::Visual;
                self.want_col = None;
            }
            _ => self.pending = Pending::default(),
        }
    }

    // ---- motions and operators -------------------------------------------------------------

    /// A motion key: moves the caret, or acts as the target of a pending operator.
    fn run(
        &mut self,
        input: &mut TextInput,
        motion: Motion,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        if let Some(op) = self.pending.op {
            self.run_op(input, op, motion, opts, clip);
            return;
        }
        let count = self.pending.total();
        self.pending = Pending::default();
        let buf = Buf::new(input.text());
        let from = buf.idx(input.cursor());
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.want_col = None;
        }
        let Some(target) = self.target(&buf, from, motion, count, false, opts) else {
            return;
        };
        if motion == Motion::LineEnd {
            self.want_col = Some(usize::MAX);
        }
        let to = buf.normal_clamp(target.to);
        input.set_cursor(buf.byte(to));
    }

    /// An operator and its motion (or the keys that stand for them: `x`, `D`, `cc`).
    fn run_op(
        &mut self,
        input: &mut TextInput,
        op: Op,
        motion: Motion,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let count = self.pending.total();
        self.pending = Pending::default();
        let buf = Buf::new(input.text());
        let from = buf.idx(input.cursor());
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.want_col = None;
        }
        // `cw` on a word changes up to its end, like `ce` (but stays on a one-letter word).
        let on_word = buf.at(from).is_some_and(|c| class(c, false) != 0);
        let target = match (op, motion) {
            (Op::Change, Motion::Word { big }) if on_word && buf.at(from).is_some() => {
                let mut to = buf.run_end(from, big);
                for _ in 1..count.unwrap_or(1) {
                    to = buf.word_end(to, big);
                }
                Some(Target {
                    to,
                    kind: Kind::Inclusive,
                })
            }
            _ => self.target(&buf, from, motion, count, true, opts),
        };
        let Some(target) = target else {
            return;
        };
        let span = buf.span(from, target, opts.single_line);
        self.apply(input, op, span, from, opts, clip);
    }

    /// Operator on `iw`, `aw`, `iW`, `aW`.
    fn object(
        &mut self,
        input: &mut TextInput,
        around: bool,
        big: bool,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let buf = Buf::new(input.text());
        let from = buf.idx(input.cursor());
        let Some((start, end)) = buf.word_object(from, around, big) else {
            self.pending = Pending::default();
            return;
        };
        if self.mode == Mode::Visual {
            self.pending = Pending::default();
            if end > start {
                self.anchor = buf.byte(start);
                input.set_cursor(buf.byte(end - 1));
            }
            return;
        }
        let Some(op) = self.pending.op else {
            self.pending = Pending::default();
            return;
        };
        self.pending = Pending::default();
        let span = Span {
            start,
            end,
            linewise: false,
        };
        self.apply(input, op, span, from, opts, clip);
    }

    /// Applies `op` to `span`; `from` is where the caret was.
    fn apply(
        &mut self,
        input: &mut TextInput,
        op: Op,
        span: Span,
        from: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let buf = Buf::new(input.text());
        let text: String = buf.chars[span.start..span.end].iter().collect();
        let empty = span.start == span.end && !span.linewise;
        if !empty {
            self.register = Register {
                text,
                linewise: span.linewise,
            };
            if !self.register.text.is_empty() {
                self.write_clipboard(opts, clip);
            }
        }
        match op {
            Op::Yank => {
                let at = if span.linewise {
                    let col = from - buf.line_start(from);
                    let ls = buf.line_start(span.start);
                    let le = buf.line_end(span.start);
                    ls + col.min((le - ls).saturating_sub(1))
                } else {
                    span.start
                };
                input.set_cursor(buf.byte(at));
            }
            Op::Delete => {
                if empty {
                    return;
                }
                self.changed = true;
                input.begin_group();
                self.delete_span(input, &buf, span, false);
            }
            Op::Change => {
                self.changed = true;
                input.begin_group();
                if !empty {
                    self.delete_span(input, &buf, span, true);
                }
                self.enter_insert(input);
            }
        }
    }

    /// Removes `span`. A deleted line takes its newline with it, unless `keep_line` (a changed
    /// line stays, empty).
    fn delete_span(&mut self, input: &mut TextInput, buf: &Buf, span: Span, keep_line: bool) {
        if !span.linewise {
            input.remove_range(buf.byte(span.start)..buf.byte(span.end));
            return;
        }
        if keep_line {
            input.remove_range(buf.byte(span.start)..buf.byte(span.end));
            return;
        }
        let (start, end) = if span.end < buf.len() {
            (span.start, span.end + 1)
        } else if span.start > 0 {
            (span.start - 1, span.end)
        } else {
            (span.start, span.end)
        };
        input.remove_range(buf.byte(start)..buf.byte(end));
        let after = Buf::new(input.text());
        let line = buf.line_of(span.start).min(after.line_count() - 1);
        let at = after.first_nonblank(after.line_start_of(line));
        input.set_cursor(after.byte(at));
    }

    fn write_clipboard(&self, opts: Options, clip: &mut dyn Clipboard) {
        if opts.use_system_clipboard {
            clip.write(&self.register.clipboard_form());
        }
    }

    /// Where `motion` leads from `from`; `None` when it cannot move (the command does nothing).
    /// `op` is whether an operator waits for it (a few motions differ then).
    fn target(
        &mut self,
        buf: &Buf,
        from: usize,
        motion: Motion,
        count: Option<usize>,
        op: bool,
        opts: Options,
    ) -> Option<Target> {
        let n = count.unwrap_or(1).max(1);
        let exclusive = |to| {
            Some(Target {
                to,
                kind: Kind::Exclusive,
            })
        };
        let inclusive = |to| {
            Some(Target {
                to,
                kind: Kind::Inclusive,
            })
        };
        let linewise = |to| {
            Some(Target {
                to,
                kind: Kind::Linewise,
            })
        };
        match motion {
            Motion::Left => {
                let ls = buf.line_start(from);
                exclusive(from.saturating_sub(n).max(ls))
            }
            Motion::Right => {
                let le = buf.line_end(from);
                let limit = if op {
                    le
                } else {
                    buf.last_in_line(from).max(from)
                };
                exclusive((from + n).min(limit))
            }
            Motion::Up | Motion::Down => {
                if opts.single_line {
                    return None;
                }
                let current = buf.line_of(from);
                let total = buf.line_count();
                let line = if motion == Motion::Up {
                    if current == 0 {
                        return None;
                    }
                    current.saturating_sub(n)
                } else {
                    if current + 1 >= total {
                        return None;
                    }
                    (current + n).min(total - 1)
                };
                let col = *self.want_col.get_or_insert(from - buf.line_start(from));
                let start = buf.line_start_of(line);
                let len = buf.line_end(start) - start;
                linewise(start + col.min(len.saturating_sub(1)))
            }
            Motion::Word { big } => {
                let mut to = from;
                let mut last_start = from;
                for _ in 0..n {
                    last_start = to;
                    to = buf.word_forward(to, big);
                }
                if op && to > from && buf.line_of(to) > buf.line_of(last_start) {
                    // The last word of a line: the operator stops at the line's end.
                    to = buf.line_end(last_start);
                }
                exclusive(to)
            }
            Motion::Back { big } => {
                let mut to = from;
                for _ in 0..n {
                    to = buf.word_back(to, big);
                }
                exclusive(to)
            }
            Motion::End { big } => {
                let mut to = from;
                for _ in 0..n {
                    to = buf.word_end(to, big);
                }
                inclusive(to)
            }
            Motion::LineStart => exclusive(buf.line_start(from)),
            Motion::FirstNonBlank => exclusive(buf.first_nonblank(buf.line_start(from))),
            Motion::LineEnd => {
                let line = (buf.line_of(from) + n - 1).min(buf.line_count() - 1);
                inclusive(buf.last_in_line(buf.line_start_of(line)))
            }
            Motion::FileStart | Motion::FileEnd => {
                let last = buf.line_count() - 1;
                let line = match (count, motion) {
                    (Some(n), _) => (n - 1).min(last),
                    (None, Motion::FileStart) => 0,
                    (None, _) => last,
                };
                let start = buf.line_start_of(line);
                linewise(buf.first_nonblank(start))
            }
            Motion::Find(find, ch) => buf.find(from, find, ch, n).map(|to| Target {
                to,
                kind: if find.forward {
                    Kind::Inclusive
                } else {
                    Kind::Exclusive
                },
            }),
            Motion::RepeatFind { reverse } => {
                let (find, ch) = self.last_find?;
                let find = Find {
                    forward: find.forward != reverse,
                    till: find.till,
                };
                // A repeated `t` would stay in place before the same character: step over it.
                let start = match (find.till, find.forward) {
                    (true, true) if buf.at(from + 1) == Some(ch) => from + 1,
                    (true, false) if from > 0 && buf.at(from - 1) == Some(ch) => from - 1,
                    _ => from,
                };
                buf.find(start, find, ch, n).map(|to| Target {
                    to,
                    kind: if find.forward {
                        Kind::Inclusive
                    } else {
                        Kind::Exclusive
                    },
                })
            }
            Motion::Lines => {
                let line = (buf.line_of(from) + n - 1).min(buf.line_count() - 1);
                linewise(buf.line_start_of(line))
            }
        }
    }

    // ---- other commands --------------------------------------------------------------------

    fn replace_chars(&mut self, input: &mut TextInput, count: usize, c: char) {
        let buf = Buf::new(input.text());
        let from = buf.idx(input.cursor());
        if c == '\n' || from + count > buf.line_end(from) {
            return;
        }
        self.changed = true;
        let with: String = std::iter::repeat_n(c, count).collect();
        input.begin_group();
        input.replace_range(buf.byte(from)..buf.byte(from + count), &with);
        input.set_cursor(buf.byte(from) + with.len() - c.len_utf8());
    }

    fn toggle_case(&mut self, input: &mut TextInput, count: usize) {
        let buf = Buf::new(input.text());
        let from = buf.idx(input.cursor());
        let end = (from + count).min(buf.line_end(from));
        if end == from {
            return;
        }
        self.changed = true;
        input.begin_group();
        let toggled = toggle_case_of(&buf.chars[from..end]);
        input.replace_range(buf.byte(from)..buf.byte(end), &toggled);
        let after = Buf::new(input.text());
        let to = (from + (end - from)).min(after.last_in_line(from).max(from));
        input.set_cursor(after.byte(to));
    }

    fn paste(
        &mut self,
        input: &mut TextInput,
        before: bool,
        count: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let Some(mut register) = self.paste_source(opts, clip) else {
            return;
        };
        if opts.single_line {
            register.linewise = false;
            register.text = register.text.replace(['\r', '\n'], " ");
        }
        if register.text.is_empty() && !register.linewise {
            return;
        }
        self.changed = true;
        input.begin_group();
        let buf = Buf::new(input.text());
        let from = buf.idx(input.cursor());
        if register.linewise {
            let block = vec![register.text.as_str(); count].join("\n");
            if before {
                let ls = buf.byte(buf.line_start(from));
                input.set_cursor(ls);
                input.insert(&format!("{block}\n"));
                input.set_cursor(ls);
            } else {
                let le = buf.byte(buf.line_end(from));
                input.set_cursor(le);
                input.insert(&format!("\n{block}"));
                input.set_cursor(le + 1);
            }
            let after = Buf::new(input.text());
            let at = after.idx(input.cursor());
            input.set_cursor(after.byte(after.first_nonblank(at)));
        } else {
            let block = register.text.repeat(count);
            let at = if before || buf.at(from).is_none_or(|c| c == '\n') {
                from
            } else {
                from + 1
            };
            let start = buf.byte(at);
            input.set_cursor(start);
            input.insert(&block);
            if block.contains('\n') {
                input.set_cursor(start);
            } else if let Some(last) = block.chars().next_back() {
                input.set_cursor(start + block.len() - last.len_utf8());
            }
        }
    }

    /// The register, or the system clipboard when it holds something else.
    fn paste_source(&mut self, opts: Options, clip: &mut dyn Clipboard) -> Option<Register> {
        if opts.use_system_clipboard
            && let Some(text) = clip.read().filter(|t| !t.is_empty())
        {
            if self.register.clipboard_form() == text {
                return Some(self.register.clone());
            }
            return Some(match text.strip_suffix('\n') {
                Some(line) => Register {
                    text: line.to_owned(),
                    linewise: true,
                },
                None => Register {
                    text,
                    linewise: false,
                },
            });
        }
        (!self.register.text.is_empty() || self.register.linewise).then(|| self.register.clone())
    }

    fn repeat(&mut self, input: &mut TextInput, opts: Options, clip: &mut dyn Clipboard) {
        let Some(change) = self.last_change.clone() else {
            return;
        };
        self.replaying = true;
        for key in change.keys {
            self.key(input, key, opts, clip);
        }
        if self.mode == Mode::Insert {
            if let Some(text) = &change.inserted {
                input.insert(text);
            }
            self.leave_insert(input);
        }
        self.replaying = false;
        self.rec.clear();
        self.changed = false;
    }

    // ---- Visual ----------------------------------------------------------------------------

    fn leave_visual(&mut self, input: &mut TextInput) {
        self.mode = Mode::Normal;
        self.pending = Pending::default();
        self.want_col = None;
        self.clamp_normal(input);
    }

    fn visual_span(&self, input: &TextInput) -> Span {
        let buf = Buf::new(input.text());
        let a = buf.idx(self.anchor.min(input.text().len()));
        let b = buf.idx(input.cursor());
        Span {
            start: a.min(b),
            end: (a.max(b) + 1).min(buf.len()),
            linewise: false,
        }
    }

    fn visual_op(
        &mut self,
        input: &mut TextInput,
        op: Op,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let span = self.visual_span(input);
        let from = Buf::new(input.text()).idx(input.cursor());
        self.mode = Mode::Normal;
        self.pending = Pending::default();
        self.apply(input, op, span, from, opts, clip);
        // Changes made in Visual are not repeated (Vim repeats them on a same-sized region).
        self.changed = false;
        self.rec.clear();
    }

    fn visual_command(
        &mut self,
        input: &mut TextInput,
        c: char,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        match c {
            'v' => self.leave_visual(input),
            'o' => {
                let cursor = input.cursor();
                input.set_cursor(self.anchor);
                self.anchor = cursor;
            }
            'x' => self.visual_op(input, Op::Delete, opts, clip),
            's' => self.visual_op(input, Op::Change, opts, clip),
            '~' => {
                let span = self.visual_span(input);
                let buf = Buf::new(input.text());
                self.mode = Mode::Normal;
                if span.end > span.start {
                    input.begin_group();
                    let toggled = toggle_case_of(&buf.chars[span.start..span.end]);
                    input.replace_range(buf.byte(span.start)..buf.byte(span.end), &toggled);
                    input.set_cursor(buf.byte(span.start));
                }
                self.changed = false;
                self.rec.clear();
            }
            _ => {}
        }
        if self.mode == Mode::Visual {
            self.pending = Pending::default();
        }
    }

    /// Normal mode never rests past the last character of a line.
    fn clamp_normal(&mut self, input: &mut TextInput) {
        if self.mode != Mode::Normal {
            return;
        }
        let buf = Buf::new(input.text());
        let at = buf.idx(input.cursor());
        let to = buf.normal_clamp(at);
        if to != at {
            input.set_cursor(buf.byte(to));
        }
    }
}

fn toggle_case_of(chars: &[char]) -> String {
    chars
        .iter()
        .flat_map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().collect::<Vec<_>>()
            } else if c.is_uppercase() {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![*c]
            }
        })
        .collect()
}

/// 0 blank, 1 punctuation, 2 word (everything non-blank with `big`).
fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        2
    } else {
        1
    }
}

/// The text as characters with their byte offsets, for motions in character indices.
struct Buf {
    chars: Vec<char>,
    /// `offs[i]` is the byte offset of character `i`; one more entry for the end.
    offs: Vec<usize>,
}

impl Buf {
    fn new(text: &str) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut offs = Vec::with_capacity(text.len() + 1);
        for (at, c) in text.char_indices() {
            chars.push(c);
            offs.push(at);
        }
        offs.push(text.len());
        Self { chars, offs }
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn at(&self, i: usize) -> Option<char> {
        self.chars.get(i).copied()
    }

    /// The character index of byte offset `byte`.
    fn idx(&self, byte: usize) -> usize {
        self.offs.binary_search(&byte).unwrap_or_else(|i| i)
    }

    fn byte(&self, i: usize) -> usize {
        self.offs[i.min(self.chars.len())]
    }

    fn line_start(&self, i: usize) -> usize {
        let mut i = i.min(self.len());
        while i > 0 && self.chars[i - 1] != '\n' {
            i -= 1;
        }
        i
    }

    /// The index of the `\n` ending the line (or the text's end).
    fn line_end(&self, i: usize) -> usize {
        let mut i = i.min(self.len());
        while i < self.len() && self.chars[i] != '\n' {
            i += 1;
        }
        i
    }

    fn line_of(&self, i: usize) -> usize {
        self.chars[..i.min(self.len())]
            .iter()
            .filter(|c| **c == '\n')
            .count()
    }

    fn line_count(&self) -> usize {
        self.chars.iter().filter(|c| **c == '\n').count() + 1
    }

    fn line_start_of(&self, line: usize) -> usize {
        let mut seen = 0;
        for (i, c) in self.chars.iter().enumerate() {
            if seen == line {
                return self.line_start(i);
            }
            if *c == '\n' {
                seen += 1;
            }
        }
        if seen == line {
            self.len()
        } else {
            self.line_start(self.len())
        }
    }

    /// The last character of the line holding `i` (its start when the line is empty).
    fn last_in_line(&self, i: usize) -> usize {
        let (ls, le) = (self.line_start(i), self.line_end(i));
        if le > ls { le - 1 } else { ls }
    }

    /// The first character of the line starting at `ls` that is not a space or tab, or its last
    /// character when it is all blank.
    fn first_nonblank(&self, ls: usize) -> usize {
        let le = self.line_end(ls);
        let mut i = ls;
        while i < le && matches!(self.chars[i], ' ' | '\t') {
            i += 1;
        }
        if i == le && le > ls { le - 1 } else { i }
    }

    /// Normal mode's caret cannot rest on the line's end, except on an empty line.
    fn normal_clamp(&self, i: usize) -> usize {
        let i = i.min(self.len());
        let (ls, le) = (self.line_start(i), self.line_end(i));
        if i >= le && le > ls { le - 1 } else { i }
    }

    /// `w`: the start of the next word.
    fn word_forward(&self, from: usize, big: bool) -> usize {
        let len = self.len();
        let mut i = from;
        if let Some(c) = self.at(i) {
            let cls = class(c, big);
            if cls != 0 {
                while i < len && class(self.chars[i], big) == cls {
                    i += 1;
                }
            }
        }
        while i < len {
            let c = self.chars[i];
            if c == '\n' {
                // An empty line is a word of its own.
                if i > from && (i == 0 || self.chars[i - 1] == '\n') {
                    break;
                }
                i += 1;
            } else if class(c, big) == 0 {
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    /// `b`: the start of the word before `from`.
    fn word_back(&self, from: usize, big: bool) -> usize {
        if from == 0 {
            return 0;
        }
        let mut i = from - 1;
        while i > 0 && class(self.chars[i], big) == 0 {
            if self.chars[i] == '\n' && self.chars[i - 1] == '\n' {
                return i;
            }
            i -= 1;
        }
        let cls = class(self.chars[i], big);
        if cls == 0 {
            return i;
        }
        while i > 0 && class(self.chars[i - 1], big) == cls {
            i -= 1;
        }
        i
    }

    /// `e`: the end of the word after `from`.
    fn word_end(&self, from: usize, big: bool) -> usize {
        let len = self.len();
        if len == 0 {
            return 0;
        }
        let mut i = from + 1;
        while i < len && class(self.chars[i], big) == 0 {
            i += 1;
        }
        if i >= len {
            return len - 1;
        }
        self.run_end(i, big)
    }

    /// The last character of the run (of the same class) that holds `from`.
    fn run_end(&self, from: usize, big: bool) -> usize {
        let Some(c) = self.at(from) else {
            return from;
        };
        let cls = class(c, big);
        let mut i = from;
        while i + 1 < self.len()
            && class(self.chars[i + 1], big) == cls
            && self.chars[i + 1] != '\n'
        {
            i += 1;
        }
        i
    }

    /// `iw` / `aw` around `from`: `(start, end)`, end exclusive.
    fn word_object(&self, from: usize, around: bool, big: bool) -> Option<(usize, usize)> {
        let c = self.at(from)?;
        if c == '\n' {
            return None;
        }
        let cls = class(c, big);
        let same = |i: usize| {
            self.at(i)
                .is_some_and(|c| c != '\n' && class(c, big) == cls)
        };
        let mut start = from;
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = from + 1;
        while same(end) {
            end += 1;
        }
        if !around {
            return Some((start, end));
        }
        let blank = |i: usize| self.at(i).is_some_and(|c| c != '\n' && class(c, big) == 0);
        if cls == 0 {
            // Blanks, and the word after them.
            if let Some(next) = self.at(end).filter(|c| *c != '\n') {
                let next_cls = class(next, big);
                while self
                    .at(end)
                    .is_some_and(|c| c != '\n' && class(c, big) == next_cls)
                {
                    end += 1;
                }
            }
        } else if blank(end) {
            while blank(end) {
                end += 1;
            }
        } else {
            while start > 0 && blank(start - 1) {
                start -= 1;
            }
        }
        Some((start, end))
    }

    /// `f`, `t`, `F`, `T`: the `n`-th `ch` after (before) `from` on its line.
    fn find(&self, from: usize, find: Find, ch: char, n: usize) -> Option<usize> {
        let (ls, le) = (self.line_start(from), self.line_end(from));
        let mut found = None;
        let mut left = n;
        if find.forward {
            for i in from + 1..le {
                if self.chars[i] == ch {
                    left -= 1;
                    if left == 0 {
                        found = Some(i);
                        break;
                    }
                }
            }
            found.map(|i| if find.till { i - 1 } else { i })
        } else {
            for i in (ls..from).rev() {
                if self.chars[i] == ch {
                    left -= 1;
                    if left == 0 {
                        found = Some(i);
                        break;
                    }
                }
            }
            found.map(|i| if find.till { i + 1 } else { i })
        }
    }

    /// The text an operator takes for a motion from `from` to `target`.
    fn span(&self, from: usize, target: Target, single_line: bool) -> Span {
        let (lo, hi) = (from.min(target.to), from.max(target.to));
        match target.kind {
            Kind::Exclusive => Span {
                start: lo,
                end: hi,
                linewise: false,
            },
            Kind::Inclusive => Span {
                start: lo,
                end: if hi < self.len() && self.chars[hi] != '\n' {
                    hi + 1
                } else {
                    hi
                },
                linewise: false,
            },
            Kind::Linewise if single_line => Span {
                start: 0,
                end: self.len(),
                linewise: false,
            },
            Kind::Linewise => Span {
                start: self.line_start(lo),
                end: self.line_end(hi),
                linewise: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory system clipboard.
    struct Board(Option<String>);

    impl Clipboard for Board {
        fn read(&mut self) -> Option<String> {
            self.0.clone()
        }
        fn write(&mut self, text: &str) {
            self.0 = Some(text.to_owned());
        }
    }

    /// A field and its modal state, fed keys written like Vim documents them: plain
    /// characters, `<Esc>`, `<C-r>`, `<CR>`.
    struct Rig {
        input: TextInput,
        vim: Vim,
        opts: Options,
        clip: Option<String>,
    }

    impl Rig {
        /// Normal mode with the caret at `|` in `text` (at the end when there is none).
        fn new(text: &str) -> Self {
            let mut input = TextInput::new();
            let (plain, cursor) = match text.find('|') {
                Some(at) => (text.replacen('|', "", 1), at),
                None => (text.to_owned(), text.len()),
            };
            input.reset_to(&plain);
            input.set_cursor(cursor);
            let mut vim = Vim::new();
            vim.mode = Mode::Normal;
            Self {
                input,
                vim,
                opts: Options::default(),
                clip: None,
            }
        }

        fn single(text: &str) -> Self {
            let mut rig = Self::new(text);
            rig.opts.single_line = true;
            rig
        }

        fn key(&mut self, key: Key) -> Outcome {
            let mut board = Board(self.clip.take());
            let outcome = self.vim.key(&mut self.input, key, self.opts, &mut board);
            self.clip = board.0;
            outcome
        }

        fn keys(&mut self, keys: &str) -> &mut Self {
            let mut chars = keys.chars();
            while let Some(c) = chars.next() {
                let key = if c == '<' {
                    let name: String = chars.by_ref().take_while(|c| *c != '>').collect();
                    match name.as_str() {
                        "Esc" => Key::Escape,
                        "CR" => Key::Enter,
                        "BS" => Key::Backspace,
                        "Left" => Key::Left,
                        n if n.starts_with("C-") => Key::Ctrl(n.chars().nth(2).unwrap()),
                        n => panic!("unknown key <{n}>"),
                    }
                } else {
                    Key::Char(c)
                };
                self.key(key);
            }
            self
        }

        fn text(&self) -> &str {
            self.input.text()
        }

        /// The text with `|` before the caret.
        fn shown(&self) -> String {
            let mut s = self.input.text().to_owned();
            s.insert(self.input.cursor(), '|');
            s
        }
    }

    /// `text` in Normal mode, `keys` typed, the result shown with the caret.
    fn run(text: &str, keys: &str) -> String {
        Rig::new(text).keys(keys).shown()
    }

    #[test]
    fn horizontal_motions_stay_on_the_line() {
        assert_eq!(run("|abc def", "l"), "a|bc def");
        assert_eq!(run("|abc def", "3l"), "abc| def");
        assert_eq!(run("|abc def", "99l"), "abc de|f");
        assert_eq!(run("abc de|f", "h"), "abc d|ef");
        assert_eq!(run("a|bc", "5h"), "|abc");
        assert_eq!(run("one\n|two", "h"), "one\n|two", "h does not wrap");
        assert_eq!(run("ab|c\ndef", "l"), "ab|c\ndef", "l does not wrap");
        assert_eq!(run("  ab|c", "0"), "|  abc");
        assert_eq!(run("  ab|c", "^"), "  |abc");
        assert_eq!(run("|abc\ndef", "$"), "ab|c\ndef");
        assert_eq!(
            run("abc de|f", "0"),
            "|abc def",
            "a leading zero is a motion"
        );
        assert_eq!(
            run("abcdefghijkl|m", "10h"),
            "ab|cdefghijklm",
            "a zero after a digit counts"
        );
    }

    #[test]
    fn vertical_motions_keep_the_column() {
        let text = "abc|def\nxy\nlonger line";
        assert_eq!(run(text, "j"), "abcdef\nx|y\nlonger line");
        assert_eq!(run(text, "jj"), "abcdef\nxy\nlon|ger line");
        assert_eq!(run(text, "jjk"), "abcdef\nx|y\nlonger line");
        assert_eq!(run("|abc\ndef\nghi", "G"), "abc\ndef\n|ghi");
        assert_eq!(run("abc\ndef\n|ghi", "gg"), "|abc\ndef\nghi");
        assert_eq!(run("|abc\ndef\nghi", "2G"), "abc\n|def\nghi");
        assert_eq!(run("|abc\ndef\nghi", "3gg"), "abc\ndef\n|ghi");
        assert_eq!(
            run("|abc\n  def", "G"),
            "abc\n  |def",
            "G goes to the first non-blank"
        );
        assert_eq!(run("ab|c", "j"), "ab|c", "no line below");
        assert_eq!(run("ab|c", "k"), "ab|c", "no line above");
        assert_eq!(
            run("|ab\nlonger", "$j"),
            "ab\nlonge|r",
            "$ sticks to the line end"
        );
    }

    #[test]
    fn word_motions() {
        assert_eq!(run("|foo bar  baz", "w"), "foo |bar  baz");
        assert_eq!(run("|foo bar  baz", "2w"), "foo bar  |baz");
        assert_eq!(run("|foo bar  baz", "9w"), "foo bar  ba|z");
        assert_eq!(
            run("|foo.bar baz", "w"),
            "foo|.bar baz",
            "punctuation is a word of its own"
        );
        assert_eq!(run("|foo.bar baz", "W"), "foo.bar |baz");
        assert_eq!(run("foo bar  ba|z", "b"), "foo bar  |baz");
        assert_eq!(run("foo bar  ba|z", "2b"), "foo |bar  baz");
        assert_eq!(run("foo b|ar", "b"), "foo |bar");
        assert_eq!(run("foo.b|ar", "B"), "|foo.bar");
        assert_eq!(run("|foo bar", "e"), "fo|o bar");
        assert_eq!(run("fo|o bar", "e"), "foo ba|r");
        assert_eq!(run("|foo.bar", "E"), "foo.ba|r");
        assert_eq!(run("|foo\nbar", "w"), "foo\n|bar", "w crosses lines");
        assert_eq!(
            run("|foo\n\nbar", "w"),
            "foo\n|\nbar",
            "an empty line is a word"
        );
        assert_eq!(run("foo\n\n|bar", "b"), "foo\n|\nbar");
        assert_eq!(
            run("|a_b c", "w"),
            "a_b |c",
            "the underscore belongs to words"
        );
    }

    #[test]
    fn words_are_unicode_aware() {
        let text = "|привет мир, ёжик";
        assert_eq!(run(text, "w"), "привет |мир, ёжик");
        assert_eq!(run(text, "ww"), "привет мир|, ёжик");
        assert_eq!(run(text, "e"), "приве|т мир, ёжик");
        assert_eq!(run("привет мир|, ёжик", "b"), "привет |мир, ёжик");
        assert_eq!(run(text, "dw"), "|мир, ёжик");
        assert_eq!(run(text, "cwпока<Esc>"), "пок|а мир, ёжик");
        assert_eq!(run(text, "x"), "|ривет мир, ёжик");
        assert_eq!(run("при|вет", "X"), "пр|вет");
        assert_eq!(run("|ёж", "~"), "Ё|ж");
        assert_eq!(run("|привет мир", "ywP"), "привет| привет мир");
        assert_eq!(run("пр|ивет мир", "ciwX<Esc>"), "|X мир");
        assert_eq!(run("при|вет мир", "daw"), "|мир");
    }

    #[test]
    fn find_motions() {
        assert_eq!(run("|a,b,c,d", "f,"), "a|,b,c,d");
        assert_eq!(run("|a,b,c,d", "2f,"), "a,b|,c,d");
        assert_eq!(run("|ab,c", "t,"), "a|b,c");
        assert_eq!(run("a,b,c|d", "F,"), "a,b|,cd");
        assert_eq!(run("|a,b,c,d", "f,;"), "a,b|,c,d");
        assert_eq!(run("|a,b,c,d", "f,;,"), "a|,b,c,d");
        assert_eq!(run("|a,b,c,d", "df,"), "|b,c,d");
        assert_eq!(run("|ab,c", "dt,"), "|,c");
        assert_eq!(
            run("|a,b", "fz"),
            "|a,b",
            "no such character: nothing happens"
        );
    }

    #[test]
    fn x_and_friends() {
        assert_eq!(run("a|bc", "x"), "a|c");
        assert_eq!(run("a|bcd", "2x"), "a|d");
        assert_eq!(run("ab|c", "x"), "a|b", "the caret steps back at the end");
        assert_eq!(run("a|bc", "X"), "|bc");
        assert_eq!(run("|abc", "X"), "|abc");
        assert_eq!(run("a|bcd", "99x"), "|a");
        assert_eq!(
            run("one\n|\ntwo", "x"),
            "one\n|\ntwo",
            "x on an empty line does nothing"
        );
        assert_eq!(run("|", "x"), "|");
    }

    #[test]
    fn delete_with_motions() {
        assert_eq!(run("|foo bar baz", "dw"), "|bar baz");
        assert_eq!(run("|foo bar baz", "2dw"), "|baz");
        assert_eq!(run("|foo bar baz", "d2w"), "|baz");
        assert_eq!(run("|foo bar baz", "de"), "| bar baz");
        assert_eq!(run("foo b|ar baz", "db"), "foo |ar baz");
        assert_eq!(run("foo b|ar\nbaz", "d0"), "|ar\nbaz");
        assert_eq!(run("  fo|o", "d^"), "  |o");
        assert_eq!(
            run("|foo\nbar", "dw"),
            "|\nbar",
            "dw on the last word keeps the newline"
        );
        assert_eq!(
            run("one |two\nthree", "dw"),
            "one| \nthree",
            "dw stops at the line end"
        );
    }

    #[test]
    fn delete_to_the_end_of_the_line() {
        assert_eq!(run("foo b|ar baz", "d$"), "foo |b");
        assert_eq!(run("foo b|ar baz", "D"), "foo |b");
        assert_eq!(run("foo b|ar\nbaz", "D"), "foo |b\nbaz");
        assert_eq!(run("foo b|ar\nbaz", "2D"), "foo |b");
    }

    #[test]
    fn delete_lines() {
        assert_eq!(run("one\n|two\nthree", "dd"), "one\n|three");
        assert_eq!(run("one\ntwo\n|three", "dd"), "one\n|two");
        assert_eq!(run("|one", "dd"), "|");
        assert_eq!(run("|one\ntwo\nthree", "2dd"), "|three");
        assert_eq!(run("|one\ntwo\nthree", "d2d"), "|three");
        assert_eq!(run("|one\ntwo\nthree", "9dd"), "|");
        assert_eq!(run("one\n  |two\nthree", "dj"), "|one");
        assert_eq!(run("one\n|two\nthree", "dk"), "|three");
        assert_eq!(run("one\n|two\nthree", "dG"), "|one");
        assert_eq!(run("one\ntwo\n|three", "dgg"), "|");
        assert_eq!(run("  one\n|two", "dd"), "  |one");
    }

    #[test]
    fn change_with_motions() {
        assert_eq!(run("|foo bar", "cwxy<Esc>"), "x|y bar");
        assert_eq!(run("|foo bar", "cexy<Esc>"), "x|y bar");
        assert_eq!(run("|foo bar", "c2wxy<Esc>"), "x|y");
        assert_eq!(run("foo b|ar", "cbxy<Esc>"), "foo x|yar");
        assert_eq!(run("foo b|ar baz", "c$X<Esc>"), "foo b|X");
        assert_eq!(run("foo |bar baz", "CX<Esc>"), "foo |X");
        assert_eq!(run("one\n|two\nthree", "ccX<Esc>"), "one\n|X\nthree");
        assert_eq!(run("one\n|two\nthree", "SX<Esc>"), "one\n|X\nthree");
        assert_eq!(run("a|bc", "sX<Esc>"), "a|Xc");
        assert_eq!(run("a|bcd", "2sX<Esc>"), "a|Xd");
        assert_eq!(run("|foo  bar", "cwX<Esc>"), "|X  bar");
        assert_eq!(
            run("foo| bar", "cwX<Esc>"),
            "foo|Xbar",
            "cw on a blank changes the blank"
        );
        assert_eq!(run("|a b", "cwX<Esc>"), "|X b", "a one-letter word");
        assert_eq!(run("|foo\nbar", "cjX<Esc>"), "|X");
    }

    #[test]
    fn change_enters_insert_and_escape_leaves() {
        let mut r = Rig::new("|foo bar");
        r.keys("cw");
        assert_eq!(r.vim.mode(), Mode::Insert);
        r.keys("zz");
        assert_eq!(r.text(), "zz bar", "keys are typed in Insert mode");
        r.keys("<Esc>");
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(
            r.shown(),
            "z|z bar",
            "the caret steps back onto the last character"
        );
    }

    #[test]
    fn insert_commands() {
        assert_eq!(run("a|bc", "iX<Esc>"), "a|Xbc");
        assert_eq!(run("a|bc", "aX<Esc>"), "ab|Xc");
        assert_eq!(run("  a|bc", "IX<Esc>"), "  |Xabc");
        assert_eq!(run("a|bc\ndef", "AX<Esc>"), "abc|X\ndef");
        assert_eq!(run("a|bc\ndef", "oX<Esc>"), "abc\n|X\ndef");
        assert_eq!(run("abc\nd|ef", "OX<Esc>"), "abc\n|X\ndef");
        assert_eq!(run("|", "aX<Esc>"), "|X");
        assert_eq!(
            run("ab|c", "a<Esc>"),
            "ab|c",
            "a on the last character appends"
        );
        assert_eq!(
            run("|abc", "i<Esc>"),
            "|abc",
            "Escape at the line start stays"
        );
    }

    #[test]
    fn yank_and_put() {
        assert_eq!(run("|foo bar", "ywP"), "foo| foo bar");
        assert_eq!(run("|foo bar", "yep"), "ffo|ooo bar");
        assert_eq!(run("|foo bar", "y$p"), "ffoo ba|roo bar");
        assert_eq!(run("one\n|two\nthree", "yyp"), "one\ntwo\n|two\nthree");
        assert_eq!(run("one\n|two\nthree", "yyP"), "one\n|two\ntwo\nthree");
        assert_eq!(run("one\ntwo\n|three", "yyp"), "one\ntwo\nthree\n|three");
        assert_eq!(run("|one\ntwo", "yy2p"), "one\n|one\none\ntwo");
        assert_eq!(run("|foo bar", "dwp"), "bfoo| ar");
        assert_eq!(run("ab|c", "xP"), "a|cb");
        assert_eq!(run("|abc", "p"), "|abc", "an empty register puts nothing");
        assert_eq!(run("|one\ntwo", "ddp"), "two\n|one");
    }

    #[test]
    fn yank_leaves_the_text_alone() {
        let mut r = Rig::new("foo |bar");
        r.keys("yw");
        assert_eq!(r.shown(), "foo |bar");
        assert_eq!(r.vim.register(), ("bar", false));
        r.keys("yy");
        assert_eq!(r.vim.register(), ("foo bar", true));
        let mut r = Rig::new("foo ba|r");
        r.keys("yb");
        assert_eq!(r.vim.register(), ("ba", false));
        assert_eq!(
            r.shown(),
            "foo |bar",
            "yb moves the caret to where the yanked text starts"
        );
    }

    #[test]
    fn deletes_fill_the_register() {
        let mut r = Rig::new("|foo bar");
        r.keys("dw");
        assert_eq!(r.vim.register(), ("foo ", false));
        r.keys("x");
        assert_eq!(r.vim.register(), ("b", false));
        let mut r = Rig::new("one\n|two");
        r.keys("dd");
        assert_eq!(r.vim.register(), ("two", true));
    }

    #[test]
    fn replace_and_toggle_case() {
        assert_eq!(run("|abc", "rX"), "|Xbc");
        assert_eq!(run("|abc", "2rX"), "X|Xc");
        assert_eq!(run("|abc", "9rX"), "|abc", "not enough characters: nothing");
        assert_eq!(run("|abc", "r<Esc>"), "|abc");
        assert_eq!(run("|aBc", "~"), "A|Bc");
        assert_eq!(run("|aBc", "3~"), "Ab|C");
        assert_eq!(run("a|Bc", "99~"), "ab|C");
        assert_eq!(run("|аБ", "2~"), "А|б");
    }

    #[test]
    fn undo_and_redo() {
        let mut r = Rig::new("|foo bar");
        r.keys("dw");
        assert_eq!(r.text(), "bar");
        r.keys("x");
        assert_eq!(r.text(), "ar");
        r.keys("u");
        assert_eq!(r.text(), "bar");
        r.keys("u");
        assert_eq!(
            r.shown(),
            "|foo bar",
            "the caret goes back to where the change was"
        );
        r.keys("u");
        assert_eq!(r.text(), "foo bar", "nothing left to undo");
        r.keys("<C-r>");
        assert_eq!(r.text(), "bar");
        r.keys("<C-r><C-r>");
        assert_eq!(r.text(), "ar");
        r.keys("uu<C-r>");
        assert_eq!(r.text(), "bar");
    }

    #[test]
    fn a_change_with_its_typing_is_one_undo_step() {
        let mut r = Rig::new("|foo bar");
        r.keys("cwhello<Esc>");
        assert_eq!(r.text(), "hello bar");
        r.keys("u");
        assert_eq!(r.text(), "foo bar");
        r.keys("<C-r>");
        assert_eq!(r.text(), "hello bar");
        r.keys("oline two<Esc>");
        assert_eq!(r.text(), "hello bar\nline two");
        r.keys("u");
        assert_eq!(r.text(), "hello bar");
    }

    #[test]
    fn undo_takes_a_count() {
        let mut r = Rig::new("|abcdef");
        r.keys("xxx");
        assert_eq!(r.text(), "def");
        r.keys("2u");
        assert_eq!(r.text(), "bcdef");
    }

    #[test]
    fn dot_repeats_x_dw_cw_and_dd() {
        let mut r = Rig::new("|abcd");
        r.keys("x.");
        assert_eq!(r.text(), "cd");

        let mut r = Rig::new("|one two three four");
        r.keys("dw.");
        assert_eq!(r.text(), "three four");

        let mut r = Rig::new("|one two three");
        r.keys("cwX<Esc>w.");
        assert_eq!(r.text(), "X X three");
        r.keys("w.");
        assert_eq!(r.text(), "X X X");

        let mut r = Rig::new("|a\nb\nc\nd");
        r.keys("dd.");
        assert_eq!(r.text(), "c\nd");

        let mut r = Rig::new("|abc");
        r.keys("rXl.");
        assert_eq!(r.text(), "XXc");

        let mut r = Rig::new("|abc abc");
        r.keys("ixy<Esc>.");
        assert_eq!(r.text(), "xxyyabc abc");
    }

    #[test]
    fn dot_without_a_change_does_nothing() {
        let mut r = Rig::new("|abc");
        r.keys(".");
        assert_eq!(r.text(), "abc");
        r.keys("l.");
        assert_eq!(r.text(), "abc", "a motion is not a change");
        r.keys("x");
        r.keys("l.");
        assert_eq!(
            r.text(),
            "a",
            "the last change is the x, not the motion after it"
        );
    }

    #[test]
    fn a_dot_is_one_undo_step() {
        let mut r = Rig::new("|abcd");
        r.keys("x.");
        r.keys("u");
        assert_eq!(r.text(), "bcd");
    }

    #[test]
    fn visual_motions_extend_and_operators_act() {
        let mut r = Rig::new("|foo bar baz");
        r.keys("v");
        assert_eq!(r.vim.mode(), Mode::Visual);
        r.keys("e");
        assert_eq!(r.vim.selection(&r.input), Some(0..3));
        r.keys("w");
        assert_eq!(r.vim.selection(&r.input), Some(0..5));
        r.keys("d");
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(r.text(), "ar baz");
        assert_eq!(r.vim.register(), ("foo b", false));

        assert_eq!(run("|foo bar", "vey"), "|foo bar");
        assert_eq!(run("|foo bar", "veyP"), "fo|ofoo bar");
        assert_eq!(run("|foo bar", "vlx"), "|o bar");
        assert_eq!(run("|foo bar", "vecX<Esc>"), "|X bar");
        assert_eq!(run("foo b|ar", "vbd"), "foo |r");
        assert_eq!(run("|foo bar", "vls!<Esc>"), "|!o bar");
    }

    #[test]
    fn visual_escape_and_v_return_to_normal() {
        let mut r = Rig::new("|foo");
        r.keys("vl<Esc>");
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(r.vim.selection(&r.input), None);
        r.keys("vlv");
        assert_eq!(r.vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_o_swaps_the_ends_and_tilde_toggles() {
        let mut r = Rig::new("|foo bar");
        r.keys("vlllo");
        assert_eq!(r.input.cursor(), 0);
        assert_eq!(r.vim.selection(&r.input), Some(0..4));
        r.keys("~");
        assert_eq!(r.text(), "FOO bar");
        assert_eq!(r.vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_selection_spans_lines() {
        assert_eq!(run("a|bc\ndef", "vjd"), "a|f");
        assert_eq!(run("a|bc\ndef", "vjy"), "a|bc\ndef");
        let mut r = Rig::new("a|bc\ndef");
        r.keys("vjy");
        assert_eq!(r.vim.register(), ("bc\nde", false));
    }

    #[test]
    fn text_objects() {
        assert_eq!(run("foo b|ar baz", "diw"), "foo | baz");
        assert_eq!(run("foo b|ar baz", "daw"), "foo |baz");
        assert_eq!(run("foo bar b|az", "daw"), "foo ba|r");
        assert_eq!(run("foo b|ar baz", "ciwX<Esc>"), "foo |X baz");
        assert_eq!(run("foo b|ar baz", "yiwP"), "foo ba|rbar baz");
        assert_eq!(run("foo.b|ar", "diw"), "foo|.");
        assert_eq!(run("foo.b|ar", "diW"), "|");
        assert_eq!(run("fo|o bar", "viwd"), "| bar");
    }

    #[test]
    fn single_line_rules() {
        assert_eq!(Rig::single("ab|c").keys("j").shown(), "ab|c");
        assert_eq!(Rig::single("ab|c").keys("k").shown(), "ab|c");
        assert_eq!(Rig::single("ab|c").keys("dd").shown(), "|");
        assert_eq!(
            Rig::single("ab|c").keys("dd").vim.register(),
            ("abc", false)
        );
        assert_eq!(Rig::single("ab|c").keys("ccX<Esc>").shown(), "|X");
        assert_eq!(Rig::single("ab|c").keys("yyP").shown(), "ab|cabc");
        assert_eq!(Rig::single("a|bc").keys("dG").shown(), "|");
        assert_eq!(Rig::single("a|bc").keys("ox<Esc>").shown(), "abc|x");
        assert_eq!(Rig::single("a|bc").keys("Ox<Esc>").shown(), "|xabc");
    }

    #[test]
    fn single_line_paste_flattens_newlines() {
        let mut r = Rig::single("|ab");
        r.opts.use_system_clipboard = true;
        r.clip = Some("x\ny\n".into());
        r.keys("p");
        assert_eq!(r.text(), "ax yb");
    }

    #[test]
    fn enter_and_tab_and_escape_in_normal_are_the_fields_own() {
        let mut r = Rig::new("|abc");
        assert_eq!(r.key(Key::Enter), Outcome::Ignored);
        assert_eq!(r.key(Key::Tab), Outcome::Ignored);
        assert_eq!(r.key(Key::Escape), Outcome::Ignored);
        assert_eq!(
            r.key(Key::Char('z')),
            Outcome::Handled,
            "unknown keys are swallowed"
        );
        assert_eq!(r.text(), "abc");
    }

    #[test]
    fn escape_cancels_a_pending_command() {
        let mut r = Rig::new("|abc def");
        assert_eq!(r.key(Key::Char('d')), Outcome::Handled);
        assert_eq!(r.vim.pending_keys(), "d");
        assert_eq!(r.key(Key::Escape), Outcome::Handled);
        r.keys("x");
        assert_eq!(
            r.text(),
            "bc def",
            "the d was cancelled: x deletes one character"
        );
        let mut r = Rig::new("|abc def");
        r.keys("2<Esc>x");
        assert_eq!(r.text(), "bc def");
        r.keys("d2");
        assert_eq!(r.vim.pending_keys(), "d2");
    }

    #[test]
    fn insert_mode_types_and_escape_leaves() {
        let mut r = Rig::new("|");
        r.vim.mode = Mode::Insert;
        r.keys("ab<BS>c<Left>X");
        assert_eq!(r.shown(), "aX|c");
        assert_eq!(r.key(Key::Escape), Outcome::Handled);
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(r.shown(), "a|Xc");
    }

    #[test]
    fn insert_passthrough_only_takes_escape() {
        let mut r = Rig::new("ab|");
        r.vim.mode = Mode::Insert;
        r.opts.insert_passthrough = true;
        assert_eq!(r.key(Key::Char('x')), Outcome::Ignored);
        assert_eq!(r.text(), "ab");
        assert_eq!(r.key(Key::Escape), Outcome::Handled);
        assert_eq!(r.vim.mode(), Mode::Normal);
    }

    #[test]
    fn dot_repeats_text_a_field_typed_for_itself() {
        // The field types in Insert mode on its own; the text it typed is found by the caret.
        let mut r = Rig::new("|one two");
        r.opts.insert_passthrough = true;
        r.keys("cw");
        r.input.insert("1");
        r.input.insert("2");
        r.keys("<Esc>");
        assert_eq!(r.text(), "12 two");
        r.keys("w.");
        assert_eq!(r.text(), "12 12");
    }

    #[test]
    fn the_system_clipboard_follows_the_register() {
        let mut r = Rig::new("|foo bar");
        r.opts.use_system_clipboard = true;
        r.keys("dw");
        assert_eq!(r.clip.as_deref(), Some("foo "));
        r.keys("yy");
        assert_eq!(
            r.clip.as_deref(),
            Some("bar\n"),
            "a line goes with its newline"
        );
        r.clip = Some("from outside".into());
        r.keys("P");
        assert_eq!(r.text(), "from outsidebar");
    }

    #[test]
    fn clipboard_lines_paste_as_lines() {
        let mut r = Rig::new("one\n|two");
        r.opts.use_system_clipboard = true;
        r.clip = Some("new\n".into());
        r.keys("p");
        assert_eq!(r.text(), "one\ntwo\nnew");
        r.keys("P");
        assert_eq!(r.text(), "one\ntwo\nnew\nnew");
    }

    #[test]
    fn the_register_is_private_without_the_system_clipboard() {
        let mut r = Rig::new("|foo bar");
        r.opts.use_system_clipboard = false;
        r.clip = Some("outside".into());
        r.keys("dw");
        assert_eq!(r.clip.as_deref(), Some("outside"), "nothing is written");
        r.keys("P");
        assert_eq!(
            r.text(),
            "foo bar",
            "the register, not the clipboard, is put"
        );
    }

    #[test]
    fn the_caret_stays_on_a_character_after_every_command() {
        for keys in ["$", "x$x", "dw", "d$", "wwwx", "ddk", "Gdd"] {
            let mut r = Rig::new("|foo\nbar baz");
            r.keys(keys);
            let buf = Buf::new(r.text());
            let at = buf.idx(r.input.cursor());
            assert_eq!(buf.normal_clamp(at), at, "after {keys}: {}", r.shown());
        }
    }

    #[test]
    fn multibyte_text_survives_every_operator() {
        let text = "|héllo wörld ✓ ok\nsecond ünï line";
        for keys in [
            "dw",
            "de",
            "db",
            "x",
            "X",
            "D",
            "dd",
            "cwx<Esc>",
            "ccx<Esc>",
            "yyp",
            "ywP",
            "~",
            "rX",
            "wwwd$",
            "$dB",
            "jdk",
            "vjd",
            "wveyP",
            "u",
            "daw",
            "ciwX<Esc>",
            "fwx",
            "tld",
        ] {
            let mut r = Rig::new(text);
            r.keys(keys);
            assert!(
                r.input.text().is_char_boundary(r.input.cursor()),
                "after {keys}: {}",
                r.shown()
            );
        }
    }

    #[test]
    fn reset_returns_to_insert_and_keeps_the_register() {
        let mut r = Rig::new("|foo bar");
        r.keys("yw");
        r.keys("d");
        r.vim.reset();
        assert_eq!(r.vim.mode(), Mode::Insert);
        assert_eq!(r.vim.pending_keys(), "");
        assert_eq!(r.vim.register(), ("foo ", false));
    }
}
