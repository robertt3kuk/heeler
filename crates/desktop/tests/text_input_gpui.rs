#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! TI-1: text reaches Hutan's fields through the platform's text input handler, so synthesized
//! Unicode (Cyrillic, CJK, emoji) lands in the field as typed, a plain key press inserts once,
//! and the editing shortcuts (⌘A, ⌘C, ⌘V) work in the Jump / command palette query.

mod common;

use common::{offline_connection, open_with};
use gpui::{ClipboardItem, Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::Workbench;

fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, None)
}

fn query(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| {
        wb.palette().map(|p| p.query.clone()).unwrap_or_default()
    })
}

#[gpui::test]
fn unicode_text_lands_in_the_palette_as_typed(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));

    // The characters of the text, not the layout's letter for the key code ("ффф").
    cx.simulate_input("ффф");
    assert_eq!(query(&view, cx), "ффф");
    cx.simulate_input("日本語");
    cx.simulate_input("😀");
    assert_eq!(query(&view, cx), "ффф日本語😀");

    // Backspace removes one character (an emoji is one), ⌘A then typing replaces everything.
    cx.simulate_keystrokes("backspace");
    assert_eq!(query(&view, cx), "ффф日本語");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("é");
    assert_eq!(query(&view, cx), "é");
}

#[gpui::test]
fn a_plain_key_press_inserts_once(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_keystrokes("s p l");
    assert_eq!(query(&view, cx), "spl");
    cx.simulate_keystrokes("left left");
    cx.simulate_input("X");
    assert_eq!(
        query(&view, cx),
        "sXpl",
        "the caret moves and the text goes in at it"
    );
}

#[gpui::test]
fn select_all_copy_and_paste_work_in_the_jump_query(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("split");
    cx.simulate_keystrokes("cmd-a cmd-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("split".to_owned())
    );
    cx.write_to_clipboard(ClipboardItem::new_string("a\nb".to_owned()));
    cx.simulate_keystrokes("cmd-a cmd-v");
    assert_eq!(
        query(&view, cx),
        "a",
        "a one-line field takes the first line of a paste"
    );
}
