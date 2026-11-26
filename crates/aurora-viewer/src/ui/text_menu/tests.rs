use super::*;
use egui::RawInput;
use egui::text::CCursor;

#[test]
fn entries_follow_firestorm_rules() {
    let field = |editable, password, selection| Field {
        editable,
        password,
        selection,
    };
    let all = |f: Field, clipboard: bool| ENTRIES.map(|(entry, _)| enabled(entry, f, clipboard));
    // Cut, Copy, Paste, Delete, Select All
    assert_eq!(all(field(true, false, true), true), [true, true, true, true, true]);
    assert_eq!(all(field(true, false, false), true), [false, false, true, true, true]);
    assert_eq!(all(field(true, false, true), false), [true, true, false, true, true]);
    // a password is never copied out (LLLineEditor::mDrawAsterixes)
    assert_eq!(all(field(true, true, true), true), [false, false, true, true, true]);
    // read-only: only copying and selecting
    assert_eq!(all(field(false, false, true), true), [false, true, false, false, true]);
    assert_eq!(all(field(false, false, false), true), [false, false, false, false, true]);
}

#[test]
fn a_right_click_inside_the_selection_keeps_it() {
    let selection = CCursorRange::two(CCursor::new(2), CCursor::new(8));
    let kept = after_right_press(selection, CCursorRange::one(CCursor::new(5)));
    assert_eq!(kept.as_sorted_char_range(), selection.as_sorted_char_range());
    assert_eq!(kept.primary.index, CCursor::new(8).index, "cursor at the selection's end");
    // outside it: the cursor where the click put it, nothing selected
    let moved = CCursorRange::one(CCursor::new(10));
    assert_eq!(after_right_press(selection, moved), moved);
    // nothing was selected
    let none = CCursorRange::one(CCursor::new(3));
    assert_eq!(after_right_press(none, moved), moved);
}

const FIELD: &str = "text_menu_test_field";

enum Kind {
    Editable,
    Password,
    ReadOnly,
    /// The field has a right-click menu of its own.
    OwnMenu,
    /// Spell checked (`ui::spelling::field`).
    Spelled,
}

struct Bench {
    ctx: Context,
    menu: TextMenu,
    text: String,
    kind: Kind,
    clipboard: Option<String>,
    copied: Vec<String>,
}

impl Bench {
    fn new(text: &str, kind: Kind) -> Self {
        let ctx = Context::default();
        crate::theme::Theme::default().apply(&ctx, 1.0);
        let mut bench = Bench {
            ctx,
            menu: TextMenu::default(),
            text: text.into(),
            kind,
            clipboard: None,
            copied: Vec::new(),
        };
        // laid out once, then focused like a click would
        bench.frame(Vec::new());
        bench.ctx.memory_mut(|m| m.request_focus(Id::unique(FIELD)));
        bench.frame(Vec::new());
        bench
    }

    fn frame(&mut self, events: Vec<Event>) {
        let raw = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 400.0))),
            events,
            ..Default::default()
        };
        let clipboard = self.clipboard.clone();
        let Bench { ctx, menu, text, kind, .. } = self;
        let p = crate::theme::Theme::default().palette();
        let mut output = ctx.run_ui(raw, |ui| {
            menu.before_widgets(ui.ctx(), || clipboard.clone(), None);
            let id = Id::unique(FIELD);
            match kind {
                Kind::Editable => {
                    ui.add(egui::TextEdit::singleline(text).id(id));
                }
                Kind::Password => {
                    ui.add(egui::TextEdit::singleline(text).id(id).password(true));
                }
                Kind::ReadOnly => {
                    ui.add(egui::TextEdit::singleline(&mut text.as_str()).id(id));
                }
                Kind::Spelled => {
                    let mut out = egui::TextEdit::singleline(text).id(id).show(ui);
                    crate::ui::spelling::field(ui, &p, &mut out, text, false);
                }
                Kind::OwnMenu => {
                    let response = ui.add(egui::TextEdit::singleline(text).id(id));
                    crate::ui::menu::context_menu(&response, &p, |ui| {
                        ui.label("own");
                    });
                }
            }
            menu.after(ui.ctx(), &p);
        });
        for command in output.platform_output.commands.drain(..) {
            if let egui::OutputCommand::CopyText(text) = command {
                self.copied.push(text);
            }
        }
        output.textures_delta.clear();
    }

    fn center(&self) -> Pos2 {
        self.ctx.read_response(Id::unique(FIELD)).expect("field laid out").rect.center()
    }

    fn click(&mut self, pos: Pos2, button: PointerButton) {
        let event = |pressed| Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![Event::PointerMoved(pos), event(true)]);
        self.frame(vec![event(false)]);
    }

    fn select_all(&mut self) {
        self.frame(vec![key(Key::A, Modifiers::COMMAND)]);
    }

    fn selection(&self) -> std::ops::Range<usize> {
        let range = TextEditState::load(&self.ctx, Id::unique(FIELD))
            .and_then(|s| s.cursor.char_range())
            .expect("cursor")
            .as_sorted_char_range();
        range.start.0..range.end.0
    }

    fn opened(&self) -> Option<Field> {
        self.menu.open.as_ref().map(|open| open.can)
    }
}

#[test]
fn a_right_click_on_a_field_opens_the_menu_and_keeps_the_selection() {
    let mut b = Bench::new("hello world", Kind::Editable);
    b.select_all();
    assert_eq!(b.selection(), 0..11);
    b.click(b.center(), PointerButton::Secondary);
    assert_eq!(
        b.opened(),
        Some(Field {
            editable: true,
            password: false,
            selection: true,
        })
    );
    assert_eq!(b.selection(), 0..11, "the right click kept the selection");
    // the popup is an egui area: drawn from its second frame
    b.frame(Vec::new());
    let open = b.menu.open.as_ref().expect("still open");
    let layer = egui::LayerId::new(egui::Order::Foreground, popup_id(&b.ctx, open.serial));
    assert!(b.ctx.memory(|m| m.areas().is_visible(&layer)));
    // a click outside closes it
    b.click(Pos2::new(480.0, 380.0), PointerButton::Primary);
    assert!(b.menu.open.is_none());
}

#[test]
fn the_field_menu_closes_when_its_window_loses_focus() {
    let mut b = Bench::new("draft", Kind::Editable);
    b.click(b.center(), PointerButton::Secondary);
    assert!(b.menu.open.is_some());
    b.frame(vec![Event::WindowFocused(false)]);
    assert!(b.menu.open.is_none());
}

#[test]
fn chosen_entries_run_in_the_field() {
    let mut b = Bench::new("hello world", Kind::Editable);
    b.select_all();
    b.click(b.center(), PointerButton::Secondary);
    b.frame(Vec::new());
    // click the first row, Cut: the menu takes the focus, the field gets it back
    let open = b.menu.open.as_ref().expect("open");
    let id = popup_id(&b.ctx, open.serial);
    let rect = b.ctx.memory(|m| m.area_rect(id)).expect("menu laid out");
    let first = Pos2::new(rect.center().x, rect.top() + rect.height() / 10.0);
    b.click(first, PointerButton::Primary);
    b.frame(Vec::new());
    assert_eq!(b.text, "");
    assert_eq!(b.copied, ["hello world"]);
    // Paste plays the clipboard's text at the cursor
    b.clipboard = Some("pasted".into());
    b.menu.pending = Some((Id::unique(FIELD), Pending::Entry(Entry::Paste)));
    b.frame(Vec::new());
    assert_eq!(b.text, "pasted");
    // Select All then Delete
    b.menu.pending = Some((Id::unique(FIELD), Pending::Entry(Entry::SelectAll)));
    b.frame(Vec::new());
    assert_eq!(b.selection(), 0..6);
    b.menu.pending = Some((Id::unique(FIELD), Pending::Entry(Entry::Delete)));
    b.frame(Vec::new());
    assert_eq!(b.text, "");
}

/// Native Conversations runs its UI within the root pass. The child owns the draft,
/// keyboard focus and menu, while clipboard commands leave with its platform output.
#[test]
fn the_detached_composer_cuts_and_pastes_its_own_selection() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    let ctx = Context::default();
    let theme = crate::theme::Theme::default();
    theme.apply(&ctx, 1.0);
    let palette = theme.palette();
    ctx.set_embed_viewports(false);
    let child = egui::ViewportId::from_hash_of("detached conversations");
    let events = Rc::new(Cell::new(Vec::<Event>::new()));
    let clipboard = Rc::new(RefCell::new(None::<String>));
    let input = events.clone();
    let copied = clipboard.clone();
    Context::set_immediate_viewport_renderer(move |ctx, viewport| {
        let egui::ImmediateViewport {
            ids, mut viewport_ui_cb, ..
        } = viewport;
        let info = egui::ViewportInfo {
            parent: Some(ids.parent),
            ..Default::default()
        };
        let raw = RawInput {
            viewport_id: ids.this,
            viewports: [(ids.this, info)].into_iter().collect(),
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 400.0))),
            events: input.take(),
            ..Default::default()
        };
        let max_passes = ctx.options(|o| o.max_passes);
        ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::MIN);
        let mut out = ctx.run_ui(raw, |ui| viewport_ui_cb(ui));
        ctx.options_mut(|o| o.max_passes = max_passes);
        for command in out.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                *copied.borrow_mut() = Some(text);
            }
        }
        out.textures_delta.clear();
    });

    let mut root_menu = TextMenu::default();
    let mut child_menu = TextMenu::default();
    let mut root_draft = String::from("main window");
    let mut child_draft = String::from("nearby draft");
    let field = Id::unique("detached composer");
    let mut frame = |child_events: Vec<Event>| {
        events.set(child_events);
        let mut composer = egui::Rect::NOTHING;
        let mut popup = None;
        let mut out = ctx.run_ui(RawInput::default(), |ui| {
            root_menu.before_widgets(ui.ctx(), || clipboard.borrow().clone(), None);
            let root = ui.add(egui::TextEdit::singleline(&mut root_draft));
            root.request_focus();
            ui.ctx().show_viewport_immediate(child, egui::ViewportBuilder::default(), |ui, _| {
                child_menu.before_widgets(ui.ctx(), || clipboard.borrow().clone(), None);
                composer = ui.add(egui::TextEdit::multiline(&mut child_draft).id(field)).rect;
                child_menu.after(ui.ctx(), &palette);
                if let Some(open) = &child_menu.open {
                    assert_eq!(open.field, field);
                    let id = popup_id(ui.ctx(), open.serial);
                    popup = ui.ctx().memory(|m| m.area_rect(id));
                }
            });
            root_menu.after(ui.ctx(), &palette);
            assert!(root_menu.open.is_none());
        });
        out.textures_delta.clear();
        (composer.left_top() + egui::vec2(12.0, 10.0), popup, child_draft.is_empty())
    };

    let click = |at, button| {
        [true, false].map(|pressed| {
            vec![
                Event::PointerMoved(at),
                Event::PointerButton {
                    pos: at,
                    button,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ]
        })
    };
    let (at, _, _) = frame(Vec::new());
    // Start the selection elsewhere in the text so the following right click
    // is a single click, rather than egui's double click selecting one word.
    for events in click(at + egui::vec2(40.0, 0.0), PointerButton::Primary) {
        frame(events);
    }
    frame(vec![key(Key::A, Modifiers::COMMAND)]);
    for (row, empty_after) in [(0, true), (2, false)] {
        for events in click(at, PointerButton::Secondary) {
            frame(events);
        }
        let (_, menu, _) = frame(Vec::new());
        let menu = menu.expect("the child draws the editing menu");
        // The five equally high rows are Cut, Copy, Paste, Delete and Select All.
        let y = menu.top() + menu.height() * (row as f32 + 0.5) / ENTRIES.len() as f32;
        for events in click(Pos2::new(menu.center().x, y), PointerButton::Primary) {
            frame(events);
        }
        let (_, popup, empty) = frame(Vec::new());
        assert!(popup.is_none());
        assert_eq!(empty, empty_after);
        assert_eq!(clipboard.borrow().as_deref(), Some("nearby draft"));
    }
    assert_eq!(child_draft, "nearby draft");
    assert_eq!(root_draft, "main window");
}

#[test]
fn passwords_and_read_only_fields_are_recognised() {
    let mut b = Bench::new("secret", Kind::Password);
    b.select_all();
    b.click(b.center(), PointerButton::Secondary);
    assert_eq!(
        b.opened(),
        Some(Field {
            editable: true,
            password: true,
            selection: true,
        })
    );
    // even run, Copy leaves a password out of the clipboard
    b.menu.pending = Some((Id::unique(FIELD), Pending::Entry(Entry::Copy)));
    b.frame(Vec::new());
    assert!(b.copied.is_empty());

    let mut b = Bench::new("read only", Kind::ReadOnly);
    b.select_all();
    b.click(b.center(), PointerButton::Secondary);
    assert_eq!(
        b.opened(),
        Some(Field {
            editable: false,
            password: false,
            selection: true,
        })
    );
}

#[test]
fn a_field_with_its_own_menu_keeps_it() {
    let mut b = Bench::new("script", Kind::OwnMenu);
    b.click(b.center(), PointerButton::Secondary);
    assert!(b.menu.open.is_none());
}

#[test]
fn a_right_click_elsewhere_opens_nothing() {
    let mut b = Bench::new("hello", Kind::Editable);
    b.click(Pos2::new(480.0, 380.0), PointerButton::Secondary);
    assert!(b.menu.open.is_none());
}

#[test]
fn a_misspelled_word_gets_its_suggestions() {
    crate::spelling::tests::ready();
    let mut b = Bench::new("Hello wrold", Kind::Spelled);
    let field = Id::unique(FIELD);
    let marked = crate::ui::spelling::marked(&b.ctx, field).expect("checked");
    assert_eq!(marked.as_slice(), [(6..11, "wrold".to_owned())]);
    // the click right of the text puts the cursor after the word: still it
    b.click(b.center(), PointerButton::Secondary);
    let spell = b.menu.open.as_ref().and_then(|open| open.spell.clone()).expect("on the word");
    assert_eq!(spell.word, "wrold");
    let started = std::time::Instant::now();
    let suggestions = loop {
        if let Some(found) = crate::spelling::suggestions("wrold") {
            break found;
        }
        assert!(started.elapsed().as_secs() < 30, "suggestions arrive");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(suggestions.contains(&"world".to_owned()));
    // the menu is drawn with them, then the suggestion replaces the word
    b.frame(Vec::new());
    b.menu.pending = Some((field, Pending::Replace(spell, "world".into())));
    b.frame(Vec::new());
    assert_eq!(b.text, "Hello world");
    b.frame(Vec::new());
    assert!(crate::ui::spelling::marked(&b.ctx, field).expect("checked").is_empty());
}

#[test]
fn a_suggestion_waits_for_its_word() {
    crate::spelling::tests::ready();
    let mut b = Bench::new("Hello wrold", Kind::Spelled);
    let field = Id::unique(FIELD);
    b.click(b.center(), PointerButton::Secondary);
    let spell = b.menu.open.as_ref().and_then(|open| open.spell.clone()).expect("on the word");
    // the text changed meanwhile: nothing is pasted over another word
    b.text = "Bye".into();
    b.frame(Vec::new());
    b.menu.pending = Some((field, Pending::Replace(spell, "world".into())));
    b.frame(Vec::new());
    assert_eq!(b.text, "Bye");
}

#[test]
fn a_field_without_spelling_has_no_suggestions() {
    crate::spelling::tests::ready();
    let mut b = Bench::new("Hello wrold", Kind::Editable);
    b.click(b.center(), PointerButton::Secondary);
    assert!(b.menu.open.as_ref().expect("opened").spell.is_none());
}
