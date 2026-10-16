//! The GPUI side of Vim mode (`editor.vim_mode`, `docs/design.md` § Vim mode): keystrokes into
//! the modal layer of `chat::vim`, the state of text kept as a plain `String`, the mode pill and
//! the caret.
//!
//! Fields that own a [`TextInput`] (the composer, the Tasks form, the attach form) keep a
//! [`Vim`] beside it and call [`apply`]. Fields that keep a `String` (the palette, prompts, the
//! sidebar filter) keep a [`FieldEdit`], which mirrors the string in a `TextInput` for the
//! caret, the undo history and the modal state.

use gpui::{
    AnyElement, App, ClipboardItem, Context, IntoElement, KeyDownEvent, ParentElement,
    SharedString, Styled, StyledText, TextRun, div, font, px,
};
use hutan_settings::SettingsStore;

use super::chat::TextInput;
use super::chat::vim::{Clipboard, Key, Mode, Options, Outcome, Vim};
use super::modal::Modal;
use super::ui;
use super::view::Workbench;
use crate::settings::EditorSettings;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// `editor` settings as the inputs read them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VimConfig {
    pub enabled: bool,
    pub use_system_clipboard: bool,
}

impl VimConfig {
    pub fn of(settings: &SettingsStore) -> Self {
        let editor = settings.get::<EditorSettings>();
        Self {
            enabled: editor.vim_mode,
            use_system_clipboard: editor.vim_use_system_clipboard,
        }
    }

    /// The modal layer's options for a field.
    pub fn options(self, single_line: bool, insert_passthrough: bool) -> Options {
        Options {
            single_line,
            insert_passthrough,
            use_system_clipboard: self.use_system_clipboard,
        }
    }
}

/// The key the modal layer sees for `event`; `None` for ⌘ and fn combinations (shortcuts, not
/// typing) and for keys it has no name for.
pub fn key_of(event: &KeyDownEvent) -> Option<Key> {
    let ks = &event.keystroke;
    let mods = &ks.modifiers;
    if mods.platform || mods.function {
        return None;
    }
    let named = match ks.key.as_str() {
        "escape" => Some(Key::Escape),
        "enter" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "delete" => Some(Key::Delete),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        _ => None,
    };
    if let Some(key) = named {
        return (!mods.control && !mods.alt).then_some(key);
    }
    if mods.control {
        let mut chars = ks.key.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() && !mods.alt => Some(Key::Ctrl(c)),
            _ => None,
        };
    }
    let text = ks.key_char.as_deref()?;
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(Key::Char(c)),
        _ => None,
    }
}

struct AppClipboard<'a>(&'a mut App);

impl Clipboard for AppClipboard<'_> {
    fn read(&mut self) -> Option<String> {
        self.0.read_from_clipboard().and_then(|item| item.text())
    }

    fn write(&mut self, text: &str) {
        self.0
            .write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    }
}

/// Feeds `event` to the modal layer over `input`.
pub fn apply(
    vim: &mut Vim,
    input: &mut TextInput,
    event: &KeyDownEvent,
    options: Options,
    cx: &mut App,
) -> Outcome {
    match key_of(event) {
        Some(key) => vim.key(input, key, options, &mut AppClipboard(cx)),
        None => Outcome::Ignored,
    }
}

/// Which way the caret is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretShape {
    /// A thin bar between two characters (Insert).
    Bar,
    /// A block over the character under the caret (Normal, Visual).
    Block,
}

impl CaretShape {
    pub fn of(mode: Mode) -> Self {
        if mode.block_caret() {
            CaretShape::Block
        } else {
            CaretShape::Bar
        }
    }
}

/// Where the caret and the selection of a field are, to draw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caret {
    pub cursor: usize,
    pub mode: Mode,
    pub selection: Option<std::ops::Range<usize>>,
}

impl Caret {
    pub fn of(vim: &Vim, input: &TextInput) -> Self {
        Self {
            cursor: input.cursor().min(input.text().len()),
            mode: vim.mode(),
            selection: vim.selection(input),
        }
    }
}

/// How a run of a field's text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Plain,
    Selected,
    /// The block caret.
    Block,
}

/// The text to show for a block caret and how to paint it: a character under the caret to
/// highlight (a space when the caret is at the end of a line), the selection tinted.
/// The runs' lengths add up to the shown text's length.
pub fn block_layout(text: &str, caret: &Caret) -> (String, Vec<(usize, Paint)>) {
    let cursor = caret.cursor.min(text.len());
    let under = text[cursor..].chars().next().filter(|c| *c != '\n');
    let mut shown = text.to_owned();
    let caret_len = match under {
        Some(c) => c.len_utf8(),
        None => {
            shown.insert(cursor, ' ');
            1
        }
    };
    // Offsets of `text` after the inserted space move along.
    let at = |offset: usize| {
        if under.is_none() && offset >= cursor {
            offset + 1
        } else {
            offset
        }
    };
    let (sel_start, sel_end) = caret
        .selection
        .as_ref()
        .map_or((0, 0), |range| (at(range.start), at(range.end)));
    let mut cuts = vec![
        0,
        shown.len(),
        cursor,
        cursor + caret_len,
        sel_start,
        sel_end,
    ];
    cuts.retain(|cut| *cut <= shown.len() && shown.is_char_boundary(*cut));
    cuts.sort_unstable();
    cuts.dedup();
    let paint_at = |start: usize| {
        if (cursor..cursor + caret_len).contains(&start) {
            Paint::Block
        } else if (sel_start..sel_end).contains(&start) {
            Paint::Selected
        } else {
            Paint::Plain
        }
    };
    let runs = cuts
        .windows(2)
        .map(|pair| (pair[1] - pair[0], paint_at(pair[0])))
        .collect();
    (shown, runs)
}

/// The text of a field with its caret, for Normal and Visual mode: the character under the
/// caret in a block, the selection tinted.
pub fn block_text(text: &str, caret: &Caret, theme: &Theme) -> AnyElement {
    let (shown, runs) = block_layout(text, caret);
    let run = |len: usize, paint: Paint| TextRun {
        len,
        font: font(".SystemUIFont"),
        color: if paint == Paint::Block {
            theme.bg
        } else {
            theme.fg
        },
        background_color: match paint {
            Paint::Plain => None,
            Paint::Selected => Some(theme.accent_soft),
            Paint::Block => Some(theme.accent),
        },
        underline: None,
        strikethrough: None,
    };
    let runs: Vec<TextRun> = runs
        .into_iter()
        .map(|(len, paint)| run(len, paint))
        .collect();
    div()
        .child(StyledText::new(SharedString::from(shown)).with_runs(runs))
        .into_any_element()
}

/// The text of a single-line field with its caret in the shape of the mode: a bar between
/// the characters in Insert, a block over one in Normal and Visual.
pub fn field_text(text: &str, caret: &Caret, theme: &Theme, tokens: &Tokens) -> AnyElement {
    if caret.mode.block_caret() {
        return block_text(text, caret, theme);
    }
    let cursor = caret.cursor.min(text.len());
    let (before, after) = text.split_at(cursor);
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .min_w_0()
        .child(SharedString::from(before.to_owned()))
        .child(
            div()
                .w(px(1.0))
                .h(px(tokens.font_size + 2.0))
                .bg(theme.accent),
        )
        .child(SharedString::from(after.to_owned()))
        .into_any_element()
}

/// The `NORMAL` / `INSERT` / `VISUAL` pill.
pub fn mode_pill(mode: Mode, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    let color = match mode {
        Mode::Insert => theme.muted,
        Mode::Normal => theme.accent,
        Mode::Visual => theme.warning,
    };
    ui::pill(mode.label(), color, tokens)
}

/// The state of one plain-`String` field under Vim mode: the text as a [`TextInput`] (caret,
/// undo history) and the modal layer.
#[derive(Debug, Clone, Default)]
pub struct FieldEdit {
    input: TextInput,
    vim: Vim,
}

impl FieldEdit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// A field that opens for typing: Insert mode, the history forgotten (the register stays).
    pub fn reset(&mut self) {
        self.vim.reset();
        self.input = TextInput::new();
    }

    /// Follows `text` when it changed behind this field's back (set by code, not typed).
    fn sync(&mut self, text: &str) {
        if self.input.text() != text {
            self.input.reset_to(text);
        }
    }

    /// The caret and selection for drawing `text`.
    pub fn caret(&self, text: &str) -> Caret {
        if self.input.text() == text {
            Caret::of(&self.vim, &self.input)
        } else {
            Caret {
                cursor: text.len(),
                mode: self.vim.mode(),
                selection: None,
            }
        }
    }

    /// Applies `event` to `text`. [`Outcome::Ignored`] means the field's own key handling
    /// applies (Enter, Tab, Escape in Normal mode, ↑ ↓ in Insert mode).
    pub fn key(
        &mut self,
        text: &mut String,
        event: &KeyDownEvent,
        config: VimConfig,
        single_line: bool,
        cx: &mut App,
    ) -> Outcome {
        self.sync(text);
        let ks = &event.keystroke;
        let outcome = if ks.modifiers.platform {
            self.command_key(event, single_line, cx)
        } else {
            apply(
                &mut self.vim,
                &mut self.input,
                event,
                config.options(single_line, false),
                cx,
            )
        };
        if self.input.text() != text.as_str() {
            *text = self.input.text().to_owned();
        }
        outcome
    }

    /// ⌘V, ⌘Z and ⌘⇧Z.
    fn command_key(&mut self, event: &KeyDownEvent, single_line: bool, cx: &mut App) -> Outcome {
        let ks = &event.keystroke;
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.function {
            return Outcome::Ignored;
        }
        match ks.key.as_str() {
            "v" => {
                let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                    return Outcome::Handled;
                };
                let text = if single_line {
                    text.replace(['\r', '\n'], " ")
                } else {
                    text
                };
                self.input.insert(&text);
                Outcome::Handled
            }
            "z" => {
                if ks.modifiers.shift {
                    self.input.redo();
                } else {
                    self.input.undo();
                }
                Outcome::Handled
            }
            _ => Outcome::Ignored,
        }
    }

    /// Escape for the fields that handle it as an action (palette, prompts): `true` when the
    /// modal layer used it (Insert or Visual to Normal, a pending command dropped), `false`
    /// when the field's own Escape applies (close it).
    pub fn escape(&mut self, text: &str) -> bool {
        self.sync(text);
        self.vim
            .key(&mut self.input, Key::Escape, Options::default(), &mut ())
            == Outcome::Handled
    }
}

/// What a single-line field of a panel shows under Vim mode: the text with its caret in the
/// shape of the mode (growing to fill the field) and the mode pill. `None` while Vim mode is
/// off, when the field draws itself as ever.
pub fn field_content(
    settings: &SettingsStore,
    edit: &FieldEdit,
    text: &str,
    theme: &Theme,
    tokens: &Tokens,
) -> Option<Vec<AnyElement>> {
    if !VimConfig::of(settings).enabled {
        return None;
    }
    let caret = edit.caret(text);
    Some(vec![
        div()
            .flex_1()
            .min_w_0()
            .child(field_text(text, &caret, theme, tokens))
            .into_any_element(),
        mode_pill(caret.mode, theme, tokens).into_any_element(),
    ])
}

// ---- the workbench's overlays ----------------------------------------------------------------

/// The Vim state of every text field the workbench itself owns (its overlays); only one of
/// them has the keyboard at a time.
#[derive(Debug, Default)]
pub struct OverlayEdits {
    pub palette: FieldEdit,
    pub picker: FieldEdit,
    pub agent: FieldEdit,
    pub modal: FieldEdit,
    pub sheet_query: FieldEdit,
    pub sheet_value: FieldEdit,
}

impl Workbench {
    pub(crate) fn vim_config(&self) -> VimConfig {
        VimConfig::of(&self.settings)
    }

    /// The Vim mode of the open palette's filter; `None` when it is closed or Vim mode is off.
    pub fn palette_vim_mode(&self) -> Option<Mode> {
        (self.palette.is_some() && self.vim_config().enabled).then(|| self.edits.palette.mode())
    }

    /// ⌘K "Toggle Vim Mode": flips `editor.vim_mode` in the user's settings (live).
    pub(crate) fn toggle_vim_mode(&mut self, cx: &mut Context<Self>) {
        let on = !self.vim_config().enabled;
        match self
            .settings
            .write_user("editor.vim_mode", toml::Value::Boolean(on))
        {
            Ok(()) => self.set_notice(if on { "Vim mode on" } else { "Vim mode off" }, cx),
            Err(err) => {
                self.set_notice(format!("Cannot save editor.vim_mode: {err}"), cx);
            }
        }
        cx.notify();
    }

    /// The text of a single-line field of an overlay and the mode pill, when Vim mode is on.
    pub(crate) fn vim_text(&self, edit: &FieldEdit, text: &str) -> Option<(AnyElement, gpui::Div)> {
        if !self.vim_config().enabled {
            return None;
        }
        let tokens = self.density.ui();
        let caret = edit.caret(text);
        Some((
            field_text(text, &caret, &self.theme, &tokens),
            mode_pill(caret.mode, &self.theme, &tokens),
        ))
    }

    /// A key in the command palette's filter.
    pub(crate) fn palette_vim_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let config = self.vim_config();
        let Some(mut query) = self.palette.as_ref().map(|p| p.query.clone()) else {
            return false;
        };
        if !config.enabled {
            return false;
        }
        self.edits.palette.key(&mut query, event, config, true, cx);
        if self.palette.as_ref().is_some_and(|p| p.query != query) {
            self.set_palette_query(&query, cx);
        }
        cx.notify();
        true
    }

    /// A key in the ⌘O / ⌘J picker's filter.
    pub(crate) fn picker_vim_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let config = self.vim_config();
        let Some(mut query) = self.picker.as_ref().map(|p| p.query.clone()) else {
            return false;
        };
        if !config.enabled {
            return false;
        }
        self.edits.picker.key(&mut query, event, config, true, cx);
        if self.picker.as_ref().is_some_and(|p| p.query != query) {
            self.set_picker_query(&query, cx);
        }
        cx.notify();
        true
    }

    /// A key in the model filter of the new-agent dialog.
    pub(crate) fn agent_vim_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let config = self.vim_config();
        let Some(picker) = &mut self.agent_picker else {
            return false;
        };
        if !config.enabled {
            return false;
        }
        let before = picker.model_query.clone();
        self.edits
            .agent
            .key(&mut picker.model_query, event, config, true, cx);
        if picker.model_query != before {
            picker.selected = 0;
        }
        cx.notify();
        true
    }

    /// A key in a prompt dialog's input (not in a secret's: those are typed as ever).
    pub(crate) fn modal_vim_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let config = self.vim_config();
        if !config.enabled {
            return false;
        }
        let Some(Modal {
            input: Some(input),
            secure_input: false,
            ..
        }) = &mut self.modal
        else {
            return false;
        };
        self.edits.modal.key(input, event, config, true, cx);
        cx.notify();
        true
    }

    /// Escape in the palette, the pickers: Insert goes to Normal first. `true` when Vim used it.
    pub(crate) fn overlay_vim_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.vim_config().enabled {
            return false;
        }
        let used = if let Some(palette) = &self.palette {
            self.edits.palette.escape(&palette.query)
        } else if let Some(picker) = &self.picker {
            self.edits.picker.escape(&picker.query)
        } else if let Some(picker) = &self.agent_picker {
            self.edits.agent.escape(&picker.model_query)
        } else {
            false
        };
        if used {
            cx.notify();
        }
        used
    }

    /// Escape in a prompt dialog.
    pub(crate) fn modal_vim_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.vim_config().enabled {
            return false;
        }
        let Some(Modal {
            input: Some(input),
            secure_input: false,
            ..
        }) = &self.modal
        else {
            return false;
        };
        let used = self.edits.modal.escape(input);
        if used {
            cx.notify();
        }
        used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(cursor: usize, selection: Option<std::ops::Range<usize>>) -> Caret {
        Caret {
            cursor,
            mode: Mode::Normal,
            selection,
        }
    }

    #[test]
    fn the_block_covers_the_character_under_the_caret() {
        let (shown, runs) = block_layout("héllo", &caret(1, None));
        assert_eq!(shown, "héllo");
        assert_eq!(
            runs,
            vec![(1, Paint::Plain), (2, Paint::Block), (3, Paint::Plain)]
        );
    }

    #[test]
    fn the_block_at_a_line_end_is_a_space() {
        let (shown, runs) = block_layout("ab\ncd", &caret(2, None));
        assert_eq!(shown, "ab \ncd");
        assert_eq!(
            runs,
            vec![(2, Paint::Plain), (1, Paint::Block), (3, Paint::Plain)]
        );
        let (shown, runs) = block_layout("", &caret(0, None));
        assert_eq!(shown, " ");
        assert_eq!(runs, vec![(1, Paint::Block)]);
    }

    #[test]
    fn the_selection_is_tinted_around_the_block() {
        let (_, runs) = block_layout("abcdef", &caret(3, Some(1..4)));
        assert_eq!(
            runs,
            vec![
                (1, Paint::Plain),
                (2, Paint::Selected),
                (1, Paint::Block),
                (2, Paint::Plain)
            ]
        );
        assert_eq!(runs.iter().map(|(n, _)| n).sum::<usize>(), 6);
    }

    #[test]
    fn caret_shape_follows_the_mode() {
        assert_eq!(CaretShape::of(Mode::Insert), CaretShape::Bar);
        assert_eq!(CaretShape::of(Mode::Normal), CaretShape::Block);
        assert_eq!(CaretShape::of(Mode::Visual), CaretShape::Block);
    }
}
