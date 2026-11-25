//! The rich-text editor (`teams_ui::editor`): its document's edits, what it sends (Teams' editor's HTML, read back by
//! `teams-proto` as the same rich text), what it loads, and the keys as a user types them on GPUI's test context:
//! formats toggled on and typed with, lists continued by Enter, Enter in each mode, undo, a rich copy and paste.
//! Every text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::HashMap;

use gpui::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px,
};
use teams_proto::chat::{FontSize, Marks, MessageText, Node, Rgb, RichText, SpanStyle, parse_html};
use teams_ui::editor::convert::{Load, from_nodes, to_nodes};
use teams_ui::editor::doc::{Attrs, Block, Doc, Kind, Pos, Run};
use teams_ui::editor::{Editor, EditorEvent, EnterMode};

fn html_of(doc: &Doc) -> String {
    let (nodes, mentioned) = to_nodes(doc);
    MessageText::new(
        RichText {
            nodes,
            truncated: false,
        },
        mentioned,
    )
    .unwrap()
    .html()
    .to_owned()
}

fn bold() -> Attrs {
    Attrs {
        marks: Marks::BOLD,
        ..Attrs::default()
    }
}

#[test]
fn a_document_is_sent_as_teams_editor_writes_it() {
    let lilac_blue = Attrs {
        span: SpanStyle {
            color: Some(Rgb(0x1E_53A3)),
            highlight: Some(Rgb(0xEB_D3E1)),
            size: None,
        },
        ..bold()
    };
    let doc = Doc::new(vec![
        Block::new(Kind::Heading(2), 0, vec![Run::new("Title", Attrs::default())]),
        Block::new(
            Kind::Paragraph,
            0,
            vec![
                Run::new("plain ", Attrs::default()),
                Run::new("both", lilac_blue),
                Run::new(" and ", Attrs::default()),
                Run::new(
                    "a link",
                    Attrs {
                        link: Some("https://example.test/a".to_owned()),
                        ..Attrs::default()
                    },
                ),
            ],
        ),
        Block::new(Kind::Paragraph, 0, Vec::new()),
        Block::new(Kind::Bullet, 0, vec![Run::new("one", Attrs::default())]),
        Block::new(Kind::Bullet, 1, vec![Run::new("inner", Attrs::default())]),
        Block::new(Kind::Bullet, 0, vec![Run::new("two", Attrs::default())]),
        Block::new(Kind::Numbered, 0, vec![Run::new("first", Attrs::default())]),
        Block::new(Kind::Quote, 0, vec![Run::new("quoted", Attrs::default())]),
        Block::new(Kind::Paragraph, 1, vec![Run::new("indented", Attrs::default())]),
        Block::new(Kind::Rule, 0, Vec::new()),
        Block::new(Kind::Paragraph, 0, vec![Run::new("a  b ", Attrs::default())]),
    ]);
    assert_eq!(
        html_of(&doc),
        "<h2>Title</h2>\
         <p>plain <span style=\"background-color:#EBD3E1; color:#1E53A3\"><strong>both</strong></span> and \
         <a href=\"https://example.test/a\" rel=\"noreferrer noopener\" title=\"https://example.test/a\" \
         target=\"_blank\">a link</a></p>\
         <p>&nbsp;</p>\
         <ul><li>one<ul><li>inner</li></ul></li><li>two</li></ul>\
         <ol><li>first</li></ol>\
         <blockquote><p>quoted</p></blockquote>\
         <p style=\"margin-left:40px\">indented</p>\
         <hr>\
         <p>a &nbsp;b&nbsp;</p>"
    );
}

#[test]
fn what_is_sent_reads_back_as_the_same_document() {
    let large_red = Attrs {
        span: SpanStyle {
            color: Some(Rgb(0xB6_424C)),
            highlight: None,
            size: Some(FontSize::Large),
        },
        marks: Marks::ITALIC.with(Marks::UNDERLINE),
        ..Attrs::default()
    };
    let doc = Doc::new(vec![
        Block::new(Kind::Heading(1), 0, vec![Run::new("Head", Attrs::default())]),
        Block::new(
            Kind::Paragraph,
            0,
            vec![
                Run::new("big red", large_red),
                Run::new(" then ", Attrs::default()),
                Run::new(
                    "struck",
                    Attrs {
                        marks: Marks::STRIKE,
                        ..Attrs::default()
                    },
                ),
                Run::new(" and ", Attrs::default()),
                Run::new(
                    "code",
                    Attrs {
                        marks: Marks::CODE,
                        ..Attrs::default()
                    },
                ),
            ],
        ),
        Block::new(Kind::Numbered, 0, vec![Run::new("one", Attrs::default())]),
        Block::new(Kind::Numbered, 1, vec![Run::new("one.one", Attrs::default())]),
        Block::new(Kind::Quote, 0, vec![Run::new("a line\nbroken", Attrs::default())]),
        Block::new(
            Kind::Code,
            0,
            vec![Run::new("fn main() {\n    go();\n}", Attrs::default())],
        ),
        Block::new(Kind::Rule, 0, Vec::new()),
        Block::new(Kind::Paragraph, 2, vec![Run::new("deep", Attrs::default())]),
    ]);
    let html = html_of(&doc);
    let read = from_nodes(&parse_html(&html).nodes, &HashMap::<String, String>::new(), Load::Edit).unwrap();
    assert_eq!(read, doc, "{html}");
}

#[test]
fn a_mention_is_an_atom_deleted_whole() {
    let mut doc = Doc::new(vec![Block::new(
        Kind::Paragraph,
        0,
        vec![
            Run::new("hi ", Attrs::default()),
            Run::new(
                "Lab Bee",
                Attrs {
                    mention: Some("8:orgid:00000000-0000-4000-8000-00000000000b".to_owned()),
                    ..Attrs::default()
                },
            ),
            Run::new("!", Attrs::default()),
        ],
    )]);
    assert_eq!(
        doc.clamp(Pos::new(0, 6), false),
        Pos::new(0, 3),
        "no place inside a mention"
    );
    assert_eq!(
        doc.before(Pos::new(0, 10)),
        Some(Pos::new(0, 3)),
        "a step back goes over it whole"
    );
    let at = doc.delete(Pos::new(0, 5), Pos::new(0, 11));
    assert_eq!((doc.plain_text(), at), ("hi ".to_owned(), Pos::new(0, 3)));
}

#[test]
fn a_fragment_pasted_joins_the_blocks_at_its_edges() {
    let mut doc = Doc::new(vec![Block::paragraph("startend")]);
    let fragment = vec![
        Block::new(Kind::Paragraph, 0, vec![Run::new("one", bold())]),
        Block::new(Kind::Bullet, 0, vec![Run::new("two", Attrs::default())]),
    ];
    let at = doc.insert_fragment(Pos::new(0, 5), fragment);
    assert_eq!(doc.plain_text(), "startone\ntwoend");
    assert_eq!(at, Pos::new(1, 3));
    assert_eq!(doc.blocks[1].kind, Kind::Bullet);
    assert_eq!(doc.blocks[0].runs[1].attrs, bold());

    let mut ruled = Doc::new(vec![Block::paragraph("ab")]);
    let at = ruled.insert_fragment(Pos::new(0, 1), vec![Block::new(Kind::Rule, 0, Vec::new())]);
    let kinds: Vec<Kind> = ruled.blocks.iter().map(|block| block.kind).collect();
    assert_eq!(kinds, [Kind::Paragraph, Kind::Rule, Kind::Paragraph]);
    assert_eq!((ruled.plain_text(), at), ("a\n\nb".to_owned(), Pos::new(2, 0)));
}

#[test]
fn pasted_html_keeps_what_the_editor_writes() {
    let html = "<div><p><b>Bold</b> <span style=\"color:rgb(205,89,55);font-size:24px\">orange</span></p>\
                <ul><li>item</li></ul><h3>Head</h3><table><tr><td>cell</td></tr></table><script>x()</script></div>";
    let doc = from_nodes(&parse_html(html).nodes, &HashMap::<String, String>::new(), Load::Paste).unwrap();
    assert_eq!(doc.plain_text(), "Bold orange\nitem\nHead\ncell");
    assert_eq!(doc.blocks[0].runs[0].attrs.marks, Marks::BOLD);
    assert_eq!(doc.blocks[0].runs[2].attrs.span.color, Some(Rgb(0xCD_5937)));
    assert_eq!(doc.blocks[0].runs[2].attrs.span.size, Some(FontSize::Large));
    assert_eq!(doc.blocks[1].kind, Kind::Bullet);
    assert_eq!(doc.blocks[2].kind, Kind::Heading(3));
}

#[test]
fn a_table_pastes_as_a_line_a_row_and_an_edit_of_one_is_refused() {
    let nodes =
        parse_html("<table><tr><th>Area</th><th>State</th></tr><tr><td>Notes</td><td><b>Ready</b></td></tr></table>")
            .nodes;
    let doc = from_nodes(&nodes, &HashMap::<String, String>::new(), Load::Paste).unwrap();
    assert_eq!(doc.plain_text(), "Area\tState\nNotes\tReady");
    assert!(doc.blocks[1].runs.iter().any(|run| run.attrs.marks == Marks::BOLD));
    // the box has no table: an edit would send it back as lines
    assert!(from_nodes(&nodes, &HashMap::<String, String>::new(), Load::Edit).is_err());
}

// --- typed, on GPUI's test context ---

/// The editor in a window, as its owner holds it.
struct Host {
    editor: Entity<Editor>,
    submitted: usize,
    _events: gpui::Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(500.0)).child(self.editor.clone())
    }
}

fn editor_window() -> (Entity<Host>, &'static mut VisualTestContext) {
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    app.update(|cx| {
        gpui_component::init(cx);
        teams_ui::theme::init(cx);
    });
    let (host, cx) = app.add_window_view(|window, cx| {
        let editor = cx.new(|cx| Editor::new(window, cx).placeholder("Type a message"));
        let events = cx.subscribe(&editor, |host: &mut Host, _, event, _| {
            if *event == EditorEvent::Submit {
                host.submitted += 1;
            }
        });
        Host {
            editor,
            submitted: 0,
            _events: events,
        }
    });
    let editor = cx.update(|_, cx| host.read(cx).editor.clone());
    cx.update(|window, cx| {
        let focus = editor.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    });
    draw(cx);
    (host, cx)
}

/// Draws the window a few times, as the views' tests do, so the input handler is in place.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn editor_of(host: &Entity<Host>, cx: &mut VisualTestContext) -> Entity<Editor> {
    cx.update(|_, cx| host.read(cx).editor.clone())
}

fn doc_of(host: &Entity<Host>, cx: &mut VisualTestContext) -> Doc {
    let editor = editor_of(host, cx);
    cx.update(|_, cx| editor.read(cx).doc().clone())
}

#[test]
fn ctrl_b_turns_bold_on_for_what_is_typed_next_and_off_again() {
    let (host, cx) = editor_window();
    cx.simulate_input("plain ");
    cx.simulate_keystrokes("ctrl-b");
    cx.simulate_input("bold");
    cx.simulate_keystrokes("ctrl-b");
    cx.simulate_input(" again");
    let doc = doc_of(&host, cx);
    let runs: Vec<(&str, bool)> = doc.blocks[0]
        .runs
        .iter()
        .map(|run| (run.text.as_str(), run.attrs.marks.contains(Marks::BOLD)))
        .collect();
    assert_eq!(runs, [("plain ", false), ("bold", true), (" again", false)]);
}

#[test]
fn a_selection_made_italic_and_underlined_by_its_keys() {
    let (host, cx) = editor_window();
    cx.simulate_input("one two");
    cx.simulate_keystrokes("shift-left shift-left shift-left ctrl-i ctrl-u");
    let doc = doc_of(&host, cx);
    let last = doc.blocks[0].runs.last().unwrap();
    assert_eq!(last.text, "two");
    assert!(last.attrs.marks.contains(Marks::ITALIC) && last.attrs.marks.contains(Marks::UNDERLINE));
    cx.simulate_keystrokes("ctrl-z ctrl-z");
    assert_eq!(doc_of(&host, cx).blocks[0].runs.len(), 1, "undone");
}

#[test]
fn enter_in_a_list_starts_the_next_item_and_on_an_empty_one_ends_the_list() {
    let (host, cx) = editor_window();
    cx.simulate_keystrokes("ctrl-shift-8");
    cx.simulate_input("one");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("two");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("more");
    cx.simulate_keystrokes("enter enter");
    cx.simulate_input("after");
    let doc = doc_of(&host, cx);
    let blocks: Vec<(Kind, String)> = doc.blocks.iter().map(|block| (block.kind, block.text())).collect();
    assert_eq!(
        blocks,
        [
            (Kind::Bullet, "one".to_owned()),
            (Kind::Bullet, "two\nmore".to_owned()),
            (Kind::Paragraph, "after".to_owned()),
        ]
    );
    assert_eq!(
        cx.update(|_, cx| host.read(cx).submitted),
        0,
        "Enter in a list sends nothing"
    );
}

#[test]
fn enter_sends_until_the_box_is_expanded_then_ctrl_enter_does() {
    let (host, cx) = editor_window();
    cx.simulate_input("line");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("next");
    cx.simulate_keystrokes("enter");
    assert_eq!(cx.update(|_, cx| host.read(cx).submitted), 1);
    assert_eq!(doc_of(&host, cx).blocks.len(), 2, "Shift+Enter starts a paragraph");

    let editor = editor_of(&host, cx);
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_enter_mode(EnterMode::NewLine, cx)));
    cx.simulate_keystrokes("enter");
    cx.simulate_input("third");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("broken");
    assert_eq!(
        cx.update(|_, cx| host.read(cx).submitted),
        1,
        "expanded, Enter is a new paragraph"
    );
    let doc = doc_of(&host, cx);
    assert_eq!(doc.blocks.len(), 3);
    assert_eq!(doc.blocks[2].text(), "third\nbroken", "and Shift+Enter breaks the line");
    cx.simulate_keystrokes("ctrl-enter");
    assert_eq!(cx.update(|_, cx| host.read(cx).submitted), 2);
}

#[test]
fn a_rich_copy_pastes_with_its_formats() {
    let (host, cx) = editor_window();
    cx.simulate_keystrokes("ctrl-b");
    cx.simulate_input("strong");
    cx.simulate_keystrokes("ctrl-b");
    cx.simulate_input(" text");
    cx.simulate_keystrokes("ctrl-a ctrl-c ctrl-end shift-enter ctrl-v");
    let doc = doc_of(&host, cx);
    assert_eq!(doc.plain_text(), "strong text\nstrong text");
    assert!(
        doc.blocks[1].runs[0].attrs.marks.contains(Marks::BOLD),
        "bold survives the paste"
    );
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        copied.as_deref(),
        Some("strong text"),
        "and its plain text is on the clipboard"
    );
}

#[test]
fn backspace_at_a_list_item_s_start_makes_it_a_paragraph() {
    let (host, cx) = editor_window();
    cx.simulate_keystrokes("ctrl-shift-7");
    cx.simulate_input("x");
    cx.simulate_keystrokes("backspace backspace");
    let doc = doc_of(&host, cx);
    assert_eq!((doc.blocks[0].kind, doc.plain_text()), (Kind::Paragraph, String::new()));
    let editor = editor_of(&host, cx);
    let empty = cx.update(|_, cx| editor.read(cx).is_empty());
    assert!(empty);
}

#[test]
fn a_node_tree_of_the_web_client_loads_into_blocks() {
    let nodes = vec![Node::List(teams_proto::chat::List {
        ordered: false,
        start: 1,
        items: vec![vec![
            Node::Text {
                text: "a".to_owned(),
                marks: Marks::NONE,
            },
            Node::List(teams_proto::chat::List {
                ordered: true,
                start: 1,
                items: vec![vec![Node::Text {
                    text: "b".to_owned(),
                    marks: Marks::NONE,
                }]],
            }),
        ]],
    })];
    let doc = from_nodes(&nodes, &HashMap::<String, String>::new(), Load::Edit).unwrap();
    let blocks: Vec<(Kind, u8, String)> = doc
        .blocks
        .iter()
        .map(|block| (block.kind, block.indent, block.text()))
        .collect();
    assert_eq!(
        blocks,
        [(Kind::Bullet, 0, "a".to_owned()), (Kind::Numbered, 1, "b".to_owned())]
    );
}

#[test]
fn a_block_of_code_pasted_on_an_empty_line_is_one_and_other_text_takes_the_line_s_kind() {
    let code = Block::new(Kind::Code, 0, vec![Run::new("a\nb", Attrs::default())]);
    let mut doc = Doc::default();
    doc.insert_fragment(Pos::new(0, 0), vec![code.clone()]);
    assert_eq!(doc.blocks, std::slice::from_ref(&code));
    // on a line with text, its text joins the line
    let mut doc = Doc::new(vec![Block::paragraph("x")]);
    doc.insert_fragment(Pos::new(0, 1), vec![code]);
    assert_eq!(
        (doc.blocks[0].kind, doc.plain_text()),
        (Kind::Paragraph, "xa\nb".to_owned())
    );
    // a paragraph's text on an empty list item leaves it an item
    let mut doc = Doc::new(vec![Block::new(Kind::Bullet, 1, Vec::new())]);
    doc.insert_fragment(Pos::new(0, 0), vec![Block::paragraph("word")]);
    assert_eq!((doc.blocks[0].kind, doc.blocks[0].indent), (Kind::Bullet, 1));
}

#[test]
fn blank_lines_at_either_end_of_a_message_load_as_none() {
    let nbsp = || {
        Node::Paragraph(vec![Node::Text {
            text: "\u{a0}".to_owned(),
            marks: Marks::NONE,
        }])
    };
    let text = |text: &str| {
        Node::Paragraph(vec![Node::Text {
            text: text.to_owned(),
            marks: Marks::NONE,
        }])
    };
    let nodes = vec![nbsp(), nbsp(), text("a"), nbsp(), text("b"), nbsp()];
    let doc = from_nodes(&nodes, &HashMap::<String, String>::new(), Load::Edit).unwrap();
    assert_eq!(doc.plain_text(), "a\n \nb");
    let blank = from_nodes(&[nbsp(), nbsp()], &HashMap::<String, String>::new(), Load::Edit).unwrap();
    assert_eq!(blank.blocks.len(), 1, "one block stays");
}

#[test]
fn a_rich_paste_into_a_block_of_code_is_its_plain_text() {
    let (host, cx) = editor_window();
    cx.simulate_keystrokes("ctrl-b");
    cx.simulate_input("one");
    cx.simulate_keystrokes("ctrl-b shift-enter");
    cx.simulate_input("two");
    cx.simulate_keystrokes("ctrl-a ctrl-c ctrl-end shift-enter ctrl-shift-b ctrl-v");
    let doc = doc_of(&host, cx);
    let blocks: Vec<(Kind, String)> = doc.blocks.iter().map(|block| (block.kind, block.text())).collect();
    assert_eq!(
        blocks,
        [
            (Kind::Paragraph, "one".to_owned()),
            (Kind::Paragraph, "two".to_owned()),
            (Kind::Code, "one\ntwo".to_owned()),
        ]
    );
    assert_eq!(doc.blocks[2].runs[0].attrs, Attrs::default(), "with no formats");
}

#[test]
fn a_paragraph_break_copied_alone_pastes_as_one() {
    let (host, cx) = editor_window();
    cx.simulate_input("a");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("b");
    cx.simulate_keystrokes("ctrl-home right shift-right ctrl-c ctrl-end ctrl-v");
    assert_eq!(doc_of(&host, cx).plain_text(), "a\nb\n");
}

#[test]
fn lines_pasted_into_a_list_item_are_items_of_its_list() {
    let (host, cx) = editor_window();
    cx.simulate_keystrokes("ctrl-shift-8");
    cx.simulate_input("one");
    let editor = editor_of(&host, cx);
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.paste_plain("two\nthree", cx)));
    let doc = doc_of(&host, cx);
    let blocks: Vec<(Kind, String)> = doc.blocks.iter().map(|block| (block.kind, block.text())).collect();
    assert_eq!(
        blocks,
        [(Kind::Bullet, "onetwo".to_owned()), (Kind::Bullet, "three".to_owned())]
    );
}

#[test]
fn typing_right_after_a_load_is_a_step_of_its_own() {
    let (host, cx) = editor_window();
    cx.simulate_input("sent");
    let editor = editor_of(&host, cx);
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_doc(Doc::default(), cx)));
    cx.simulate_input("next");
    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(doc_of(&host, cx).plain_text(), "", "the typing undone");
}

/// The editor of `host` held to `max` characters, holding `text` with the cursor at its end.
fn limited_to(max: usize, text: &str, host: &Entity<Host>, cx: &mut VisualTestContext) -> Entity<Editor> {
    let editor = editor_of(host, cx);
    cx.update(|_, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_max_chars(Some(max));
            editor.set_doc(Doc::new(vec![Block::paragraph(text)]), cx);
        });
    });
    draw(cx);
    editor
}

fn text_of(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| editor.read(cx).text())
}

#[test]
fn a_limit_keeps_what_fits_and_refuses_the_keys_past_it() {
    let (host, cx) = editor_window();
    let editor = limited_to(10, "abcdefghi", &host, cx);
    cx.simulate_input("jk");
    assert_eq!(
        text_of(&editor, cx),
        "abcdefghij",
        "of two keys, only the one that fits goes in"
    );
    cx.simulate_input("l");
    assert_eq!(text_of(&editor, cx), "abcdefghij", "a key past the limit is refused");
    // at the start, a key past the limit does not cut the end
    cx.simulate_keystrokes("home");
    cx.simulate_input("Z");
    assert_eq!(text_of(&editor, cx), "abcdefghij");
    // a new line counts as a character, and Enter past the limit is refused
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(text_of(&editor, cx), "abcdefghij");
    cx.simulate_keystrokes("end backspace backspace");
    cx.simulate_input("XYZ");
    assert_eq!(
        text_of(&editor, cx),
        "abcdefghXY",
        "with room again, a key goes in where the cursor is"
    );
}

#[test]
fn a_paste_past_the_limit_keeps_what_fits_and_counts_characters() {
    let (host, cx) = editor_window();
    let editor = limited_to(6, "ab", &host, cx);
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.paste_plain("éèêë-more", cx)));
    assert_eq!(text_of(&editor, cx), "abéèêë", "four characters of eight bytes fit");
    // a rich paste that does not fit is refused whole
    cx.update(|_, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_doc(Doc::new(vec![Block::paragraph("abcd")]), cx);
            editor.insert("e", cx);
            editor.paste_doc(Doc::new(vec![Block::paragraph("xyz")]), cx);
        });
    });
    assert_eq!(text_of(&editor, cx), "abcde");
    cx.update(|_, cx| editor.update(cx, Editor::undo));
    assert_eq!(text_of(&editor, cx), "abcd", "a refused edit leaves no step of undo");
}

/// Puts a bold bulleted list of `TOP` and `BOTTOM` on the clipboard as the editor copies it.
fn copy_a_rich_list(host: &Entity<Host>, cx: &mut VisualTestContext) {
    let editor = editor_of(host, cx);
    let bold = Attrs {
        marks: Marks::BOLD,
        ..Attrs::default()
    };
    let items = ["TOP", "BOTTOM"].map(|text| Block::new(Kind::Bullet, 0, vec![Run::new(text, bold.clone())]));
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_doc(Doc::new(items.to_vec()), cx)));
    cx.simulate_keystrokes("ctrl-a ctrl-c");
}

/// Puts `make` in `host`'s place of its editor and focuses it.
fn swap_editor(
    host: &Entity<Host>,
    cx: &mut VisualTestContext,
    make: impl FnOnce(Editor) -> Editor + 'static,
) -> Entity<Editor> {
    cx.update(|window, cx| {
        host.update(cx, |host, cx| host.editor = cx.new(|cx| make(Editor::new(window, cx))));
        let focus = host.read(cx).editor.focus_handle(cx);
        window.focus(&focus, cx);
    });
    draw(cx);
    editor_of(host, cx)
}

#[test]
fn a_plain_text_field_pastes_a_rich_copy_as_plain_text_up_to_its_limit() {
    let (host, cx) = editor_window();
    copy_a_rich_list(&host, cx);
    let editor = swap_editor(&host, cx, |editor| editor.max_chars(8).plain_text_only());
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(text_of(&editor, cx), "TOP\nBOTT", "what fits of the rich copy goes in");
    let doc = doc_of(&host, cx);
    assert!(doc.blocks.iter().all(|block| block.kind == Kind::Paragraph), "no list");
    assert!(
        doc.blocks
            .iter()
            .flat_map(|block| &block.runs)
            .all(|run| !run.attrs.marks.contains(Marks::BOLD)),
        "no format"
    );
}

#[test]
fn a_single_line_field_pastes_and_types_line_breaks_as_spaces() {
    let (host, cx) = editor_window();
    copy_a_rich_list(&host, cx);
    let editor = swap_editor(&host, cx, |editor| editor.max_chars(25).single_line());
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(text_of(&editor, cx), "TOP BOTTOM");
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.paste_plain("\r\nup\ndown", cx)));
    assert_eq!(text_of(&editor, cx), "TOP BOTTOM up down");
    assert_eq!(doc_of(&host, cx).blocks.len(), 1, "one line");
}

#[test]
fn a_composition_cut_by_the_limit_keeps_the_caret_in_what_went_in() {
    use gpui::EntityInputHandler as _;
    let (host, cx) = editor_window();
    let editor = limited_to(6, "abcd", &host, cx);
    let selected = cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_and_mark_text_in_range(None, "xyz", Some(3..3), window, cx);
            (
                editor
                    .selected_text_range(false, window, cx)
                    .map(|selection| selection.range),
                editor.marked_text_range(window, cx),
            )
        })
    });
    assert_eq!(text_of(&editor, cx), "abcdxy");
    assert_eq!(selected, (Some(6..6), Some(4..6)), "the caret ends at the cut text");
}
