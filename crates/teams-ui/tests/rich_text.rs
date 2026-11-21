//! Our own renderer of a message's rich text, node by node: marks, line breaks, paragraphs, mentions (the account's
//! own more strongly), links (opened through GPUI's `open_url`, here the test platform's record), quotes, lists (each
//! item beside its number or bullet, indented), images (a
//! placeholder), a pasted picture and a GIF from Teams' GIF picker (a block of its own, its bytes asked of the engine,
//! a GIF's every frame kept to play) and emoticons (inline at the
//! text's height, or large and without a bubble in a message of emoticons alone); laid out from `teams-proto`'s
//! sanitized model and drawn on GPUI's test context.
//! Every id, name and link is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use gpui::{
    Context, DevicePixels, Font, FontId, FontMetrics, FontRun, FontStyle, FontWeight, GlyphId, HighlightStyle,
    IntoElement, LineLayout, Modifiers, NoopTextSystem, ParentElement as _, Pixels, PlatformTextSystem, Render,
    RenderGlyphParams, SharedString, Size, Styled as _, TestAppContext, TestDispatcher, TextRenderingMode, Window, div,
    point, px,
};
use teams_proto::chat::Token;
use teams_proto::chat::{Marks, parse_html};
use teams_ui::code::token_style;
use teams_ui::inline::{PICTURE_BOX, WIDE_MAX_WIDTH};
use teams_ui::rich_text::{
    self, Block, IMAGE_PLACEHOLDER, LinkRange, ListBlock, Mentioned, Mentions, QuoteBlock, Rendered, Style, TextBlock,
    highlight, opens,
};
use teams_ui::theme::LIGHT_PALETTE;

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

fn laid_out(content: &str) -> Rendered {
    rich_text::layout(&parse_html(content), &Mentions::default())
}

/// The text block at `at` of the top level.
fn text_block(rendered: &Rendered, at: usize) -> &TextBlock {
    match &rendered.blocks[at] {
        Block::Text(text) => text,
        other => panic!("not text: {other:?}"),
    }
}

fn marks(style: Style) -> Marks {
    style.marks
}

#[test]
fn marks_style_their_pieces() {
    let rendered = laid_out("<p>plain <b>bold</b> <i>it</i> <u>under</u> <s>gone</s> <code>x = 1</code></p>");
    let block = text_block(&rendered, 0);
    assert_eq!(block.text, "plain bold it under gone x = 1");
    let styled: Vec<(&str, Marks)> = block
        .styled()
        .into_iter()
        .map(|(text, style)| (text, marks(style)))
        .collect();
    assert_eq!(
        styled,
        [
            ("bold", Marks::BOLD),
            ("it", Marks::ITALIC),
            ("under", Marks::UNDERLINE),
            ("gone", Marks::STRIKE),
            ("x = 1", Marks::CODE),
        ]
    );
    // the pieces cover the text in order, none overlapping
    let mut end = 0;
    for (range, _) in &block.pieces {
        assert_eq!(range.start, end);
        end = range.end;
    }
    assert_eq!(end, block.text.len());
}

#[test]
fn a_line_break_is_a_new_line_and_paragraphs_are_blocks() {
    let rendered = laid_out("<p>one<br>two</p><p>three</p><p>  </p>");
    assert_eq!(rendered.blocks.len(), 2, "an empty paragraph shows nothing");
    assert_eq!(text_block(&rendered, 0).text, "one\ntwo");
    assert_eq!(text_block(&rendered, 1).text, "three");
    // plain text, as a `Text` message comes
    let plain = rich_text::layout(&teams_proto::chat::RichText::plain("just words"), &Mentions::default());
    assert_eq!(text_block(&plain, 0).text, "just words");
}

#[test]
fn a_mention_is_highlighted_and_the_account_s_own_more_strongly() {
    let content = "<p>hi <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span> and \
                   <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"1\">Lab Bee</span></p>";
    // as read back: a JSON string of a list, the itemid a number
    let properties = format!(
        "{{\"mentions\":\"[{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":0,\\\"mri\\\":\\\"{ME}\\\",\\\"mentionType\\\":\\\"person\\\",\\\"displayName\\\":\\\"Lab A\\\"}},{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":1,\\\"mri\\\":\\\"{LAB_B}\\\",\\\"mentionType\\\":\\\"person\\\",\\\"displayName\\\":\\\"Lab Bee\\\"}}]\"}}"
    );
    let mentions = Mentions::of(Some(&properties), ME);
    let rendered = rich_text::layout(&parse_html(content), &mentions);
    let block = text_block(&rendered, 0);
    let mentioned: Vec<(&str, Option<Mentioned>)> = block
        .styled()
        .into_iter()
        .map(|(text, style)| (text, style.mention))
        .collect();
    assert_eq!(
        mentioned,
        [("Lab A", Some(Mentioned::Me)), ("Lab Bee", Some(Mentioned::Other))]
    );
    assert!(rendered.mentions_me(), "the account's own mention marks the message");

    // mentions in a form no capture showed name nobody: still highlighted, never as the account's own
    let unreadable = format!("{{\"mentions\":{{\"0\":\"{ME}\"}}}}");
    let rendered = rich_text::layout(&parse_html(content), &Mentions::of(Some(&unreadable), ME));
    let kinds: Vec<Option<Mentioned>> = text_block(&rendered, 0)
        .styled()
        .into_iter()
        .map(|(_, style)| style.mention)
        .collect();
    assert_eq!(kinds, [Some(Mentioned::Other), Some(Mentioned::Other)]);
    assert!(!rendered.mentions_me());
    assert_eq!(Mentions::of(None, ME), Mentions::default());
    assert_eq!(Mentions::of(Some("not json"), ME), Mentions::default());
}

/// A stored message of the group chat `GROUP` from `sender`, mentioning everyone in it as `name` (another language's
/// label, say), its `itemid` a number as read back.
fn everyone_message(sender: &str, name: &str) -> teams_store::Message {
    let properties = format!(
        "{{\"mentions\":\"[{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":0,\\\"mri\\\":\\\"{GROUP}\\\",\\\"mentionType\\\":\\\"everyone\\\",\\\"displayName\\\":\\\"{name}\\\"}}]\"}}"
    );
    teams_store::Message {
        conversation_id: GROUP.to_owned(),
        id: "1".to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: format!(
            "<p><span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">{name}</span> hi</p>"
        ),
        properties: Some(properties),
        arrival_time: 0,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

const GROUP: &str = "19:00000000000000000000000000000abc@thread.v2";

#[test]
fn a_mention_of_everyone_is_the_account_s_own_in_someone_else_s_message_and_shows_everyone() {
    let received = everyone_message(LAB_B, "Alle");
    let mentions = Mentions::of_message(&received, ME);
    assert!(mentions.names_me(), "it notifies the account");
    let rendered = rich_text::layout(&parse_html(&received.content), &mentions);
    // the name the sender's client wrote is shown as Everyone, the rest of the text as it is (unstyled, so not among
    // the styled pieces)
    let block = text_block(&rendered, 0);
    assert_eq!(block.text, "Everyone hi");
    let styled: Vec<(&str, Option<Mentioned>)> = block
        .styled()
        .into_iter()
        .map(|(text, style)| (text, style.mention))
        .collect();
    assert_eq!(styled, [("Everyone", Some(Mentioned::Me))]);
    assert!(rendered.mentions_me());

    let sent = everyone_message(ME, "Everyone");
    let mentions = Mentions::of_message(&sent, ME);
    assert!(!mentions.names_me(), "the sender isn't mentioned by its own message");
    let rendered = rich_text::layout(&parse_html(&sent.content), &mentions);
    assert_eq!(
        text_block(&rendered, 0).styled()[0],
        (
            "Everyone",
            Style {
                mention: Some(Mentioned::Other),
                ..Style::default()
            }
        )
    );
}

#[test]
fn a_link_is_a_clickable_range_and_only_the_web_s_and_mail_s_are_links() {
    let rendered = laid_out(
        "<p>see <a href=\"https://example.test/page\">the <b>page</b></a> or \
         <a href=\"javascript:alert(1)\">this</a> or <a href=\"mailto:someone@example.test\">mail</a></p>",
    );
    let block = text_block(&rendered, 0);
    assert_eq!(block.text, "see the page or this or mail");
    assert_eq!(
        block.links,
        [
            LinkRange {
                range: 4..12,
                href: "https://example.test/page".to_owned(),
            },
            LinkRange {
                range: 24..28,
                href: "mailto:someone@example.test".to_owned(),
            },
        ]
    );
    // the bold part of the link is both
    assert!(block.styled().contains(&(
        "page",
        Style {
            marks: Marks::BOLD,
            link: true,
            ..Style::default()
        }
    )));
    assert!(opens("https://example.test/") && opens("http://example.test/") && opens("mailto:x@example.test"));
    for refused in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,x",
        "ftp://example.test/",
        "page",
    ] {
        assert!(!opens(refused), "{refused}");
    }
}

#[test]
fn a_reply_s_quote_names_its_sender_and_a_plain_quote_none() {
    let rendered = laid_out(&format!(
        "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"1760000010000\">\
         <strong itemprop=\"mri\" itemid=\"{LAB_B}\">Lab Bee</strong><span itemprop=\"time\" itemid=\"1760000010000\">\
         </span><p itemprop=\"preview\">the original</p></blockquote><p>my answer</p>\
         <blockquote><p>plain words</p></blockquote>"
    ));
    assert_eq!(rendered.blocks.len(), 3);
    let Block::Quote(reply) = &rendered.blocks[0] else {
        panic!("a quote first: {rendered:?}");
    };
    assert_eq!(reply.sender_name.as_deref(), Some("Lab Bee"));
    assert_eq!(reply.message_id.as_deref(), Some("1760000010000"));
    assert_eq!(
        reply.time,
        Some(1_760_000_010_000),
        "its time, as the quote's itemid carries it"
    );
    assert_eq!(
        reply.blocks,
        [Block::Text(TextBlock {
            text: "the original".to_owned(),
            pieces: vec![(0..12, Style::default())],
            links: Vec::new(),
            emoticons: Vec::new(),
            ..TextBlock::default()
        })]
    );
    assert_eq!(text_block(&rendered, 1).text, "my answer");
    let Block::Quote(plain) = &rendered.blocks[2] else {
        panic!("a plain quote last: {rendered:?}");
    };
    assert_eq!(
        (
            plain.sender_name.as_ref(),
            plain.message_id.as_ref(),
            plain.time,
            plain.blocks.len()
        ),
        (None, None, None, 1)
    );
}

/// Words enough to wrap well past two lines in the test window.
fn long_words() -> String {
    "a made-up sentence that keeps going so the quote wraps onto many lines ".repeat(30)
}

/// `content` drawn once in a test window; the drawn quote's bounds and the text's line height.
fn quote_drawn(content: &str) -> (gpui::Bounds<gpui::Pixels>, gpui::Pixels) {
    let rendered = Arc::new(laid_out(content));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let quote = cx.debug_bounds("rich-quote").expect("the quote is drawn");
    let line = cx.update(|window, _| window.text_style().line_height_in_pixels(window.rem_size()));
    (quote, line)
}

#[test]
fn a_reply_s_preview_is_held_to_two_lines_and_an_emoticon_cut_off_with_it_is_not_drawn() {
    // the emoticon falls past the second line: cut off with its text, never placed, and so never painted (painting it
    // unplaced panics)
    let (quote, line) = quote_drawn(&format!(
        "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"1760000010000\">\
         <strong itemprop=\"mri\" itemid=\"{LAB_B}\">Lab Bee</strong><span itemprop=\"time\" \
         itemid=\"1760000010000\"></span><p itemprop=\"preview\">{}{}</p></blockquote>",
        long_words(),
        emoticon("smile", "\u{1f642}", 20)
    ));
    // its header and two lines of text, with the card's padding
    assert!(quote.size.height < line * 5.0, "{quote:?} at {line:?} a line");
}

#[test]
fn a_plain_quote_is_drawn_in_full() {
    let (quote, line) = quote_drawn(&format!("<blockquote><p>{}</p></blockquote>", long_words()));
    assert!(quote.size.height > line * 4.0, "{quote:?} at {line:?} a line");
}

#[test]
fn an_image_is_a_placeholder_and_never_its_source() {
    let rendered = laid_out(
        "<p>look <img src=\"https://images.example.test/cat.png\" alt=\"a cat\"> and \
         <img src=\"https://images.example.test/dog.png\"></p>",
    );
    let block = text_block(&rendered, 0);
    assert_eq!(
        block.text,
        format!("look [{IMAGE_PLACEHOLDER}: a cat] and [{IMAGE_PLACEHOLDER}]")
    );
    assert!(!block.text.contains("example.test"), "the source never shows");
    let images: Vec<&str> = block
        .styled()
        .into_iter()
        .filter(|(_, style)| style.image)
        .map(|(text, _)| text)
        .collect();
    assert_eq!(images.len(), 2);
    assert!(block.links.is_empty(), "an image is no link");
}

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

#[test]
fn a_chat_message_links_to_itself_in_teams_on_the_web() {
    assert_eq!(
        rich_text::message_web_link(CHAT, "1760000000123").as_deref(),
        Some(
            "https://teams.microsoft.com/l/message/19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-\
             00000000000b@unq.gbl.spaces/1760000000123?context=%7B%22contextType%22%3A%22chat%22%7D"
        )
    );
    assert!(rich_text::message_web_link("19:group0001@thread.v2", "1").is_some());
    for (chat, message) in [
        ("19:channel0001@thread.tacv2", "1"),
        ("19:group0001@thread.v2;messageid=1", "1"),
        ("19:a b@thread.v2", "1"),
        ("19:group0001@thread.v2/../x", "1"),
        ("48:notes", "1"),
        (CHAT, ""),
        (CHAT, "12a"),
        (CHAT, "1?x=2"),
    ] {
        assert_eq!(rich_text::message_web_link(chat, message), None, "{chat} {message}");
    }
}

#[test]
fn an_image_of_a_chat_message_is_followed_by_its_link_to_the_web() {
    let link = rich_text::message_web_link(CHAT, "1760000000123").unwrap();
    let text = parse_html(
        "<p>look <img src=\"https://images.example.test/cat.png\" alt=\"a cat\"> \
         <a href=\"https://example.test/page\"><img src=\"https://images.example.test/in-link.png\"></a></p>\
         <blockquote><p>quoted <img src=\"https://images.example.test/quoted.png\"></p></blockquote>",
    );
    let rendered = rich_text::layout_with_web_link(&text, &Mentions::default(), Some(&link));
    let block = text_block(&rendered, 0);
    assert!(
        block.text.starts_with(&format!(
            "look [{IMAGE_PLACEHOLDER}: a cat] {}",
            rich_text::IMAGE_WEB_LINK
        )),
        "{}",
        block.text
    );
    assert!(!block.text.contains("example.test"), "the source never shows");
    // the image's own link, then the anchor around the second image: never a second link inside it
    let hrefs: Vec<&str> = block.links.iter().map(|link| link.href.as_str()).collect();
    assert_eq!(hrefs, [link.as_str(), "https://example.test/page"]);
    assert_eq!(&block.text[block.links[0].range.clone()], rich_text::IMAGE_WEB_LINK);
    // a quoted image belongs to another message: no link
    let quoted = rendered
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Quote(quote) => Some(quote),
            Block::Text(_)
            | Block::Picture(_)
            | Block::List(_)
            | Block::Forward(_)
            | Block::Code(_)
            | Block::Rule
            | Block::Table(_) => None,
        })
        .unwrap();
    let Block::Text(inner) = &quoted.blocks[0] else {
        panic!("quoted text expected");
    };
    assert!(inner.links.is_empty(), "{inner:?}");
    // without a link, or with one the renderer wouldn't open, the placeholder stands alone
    for web_link in [None, Some("javascript:alert(1)")] {
        let alone = rich_text::layout_with_web_link(&text, &Mentions::default(), web_link);
        assert!(!text_block(&alone, 0).text.contains(rich_text::IMAGE_WEB_LINK));
        assert_eq!(alone, laid_out_text(&text));
    }
}

fn laid_out_text(text: &teams_proto::chat::RichText) -> Rendered {
    rich_text::layout(text, &Mentions::default())
}

#[test]
fn nothing_active_or_unknown_reaches_the_layout() {
    let rendered = laid_out(
        "<p onclick=\"steal()\">safe<script>alert(1)</script><style>p{}</style><iframe src=\"https://x.test\">\
         </iframe><marquee>moving</marquee></p>",
    );
    assert_eq!(rendered.blocks.len(), 1);
    assert_eq!(text_block(&rendered, 0).text, "safemoving");
    assert!(!rendered.truncated);
    let long = format!("<p>{}</p>", "x".repeat(70 * 1024));
    assert!(laid_out(&long).truncated);
}

/// A view that draws one rendered message, as the chat view does.
struct Holder {
    rendered: Arc<Rendered>,
}

impl Render for Holder {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let id = SharedString::from("message-1");
        div()
            .size_full()
            .child(rich_text::render(&id, &self.rendered, LIGHT_PALETTE))
    }
}

#[test]
fn a_list_s_items_are_numbered_or_bulleted_and_a_no_break_space_is_a_blank_line() {
    let rendered = laid_out(
        "<p>Pick one:</p><p>&nbsp;</p><ol start=\"2\"><li>External API</li><li>Internal API<ul><li>inner<ul><li>deep</li>\
         </ul></li></ul></li></ol>",
    );
    assert_eq!(text_block(&rendered, 0).text, "Pick one:");
    assert_eq!(
        text_block(&rendered, 1).text,
        "\u{a0}",
        "the web client's blank line stays"
    );
    let Block::List(list) = &rendered.blocks[2] else {
        panic!("a list: {:?}", rendered.blocks[2]);
    };
    assert!(list.ordered && list.level == 0);
    assert_eq!((list.marker(0), list.marker(1)), ("2.".to_owned(), "3.".to_owned()));
    let Block::List(inner) = &list.items[1][1] else {
        panic!("the nested list: {:?}", list.items[1]);
    };
    assert_eq!(
        (inner.ordered, inner.level, inner.marker(0)),
        (false, 1, "\u{25e6}".to_owned())
    );
    let bullet = |level| {
        ListBlock {
            level,
            ..ListBlock::default()
        }
        .marker(0)
    };
    assert_eq!((bullet(0), bullet(2)), ("\u{2022}".to_owned(), "\u{25aa}".to_owned()));
    // copied as a reader sees it: each item after its marker
    assert_eq!(
        rendered.blocks[2].plain_lines(),
        [
            "2. External API",
            "3. Internal API",
            "    \u{25e6} inner",
            "        \u{25aa} deep"
        ]
    );
}

/// `content` drawn once in a test window of the kit's theme; the window's context and the text's line height.
fn drawn(content: &str) -> (&'static mut gpui::VisualTestContext, gpui::Pixels) {
    let rendered = Arc::new(laid_out(content));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    app.update(teams_ui::theme::init);
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let line = cx.update(|window, _| window.text_style().line_height_in_pixels(window.rem_size()));
    (cx, line)
}

/// How tall `content` is drawn, in lines of its text.
fn drawn_lines(content: &str) -> f32 {
    let (cx, line) = drawn(content);
    let all = cx.debug_bounds("rich-all").expect("the text is drawn");
    all.size.height / line
}

#[test]
fn line_breaks_between_paragraphs_are_blank_lines_and_a_last_break_makes_none() {
    // an email's body: two `br`s between its paragraphs are two blank lines, as a browser draws them
    let rendered = laid_out("<p>Before</p><br><br><p>After two blank lines</p>");
    assert_eq!(rendered.blocks.len(), 3, "{:?}", rendered.blocks);
    assert_eq!(text_block(&rendered, 0).text, "Before");
    assert_eq!(text_block(&rendered, 2).text, "After two blank lines");
    let lines = drawn_lines("<p>Before</p><br><br><p>After two blank lines</p>");
    assert!((lines - 4.0).abs() < 0.1, "{lines} lines");
    let lines = drawn_lines("<p>Before</p><br><p>After one blank line</p>");
    assert!((lines - 3.0).abs() < 0.1, "{lines} lines");
    // a break that ends its paragraph starts no line, as a browser sets it; a line of its own still shows
    let lines = drawn_lines("<p>one<br></p><p>two</p>");
    assert!((lines - 2.0).abs() < 0.1, "{lines} lines");
    let lines = drawn_lines("<p>one<br><br>two</p>");
    assert!((lines - 3.0).abs() < 0.1, "{lines} lines");
    // the web client's blank line and markup whitespace stay as they were
    let lines = drawn_lines("<p>one</p><p>&nbsp;</p><p>two</p><p>  </p>");
    assert!((lines - 3.0).abs() < 0.1, "{lines} lines");
}

#[test]
fn a_table_s_cells_are_drawn_apart_in_rows_and_columns() {
    let content = "<p>Before</p><table><tr><th>Name</th><th>State</th></tr><tr><td>Cell one</td><td>Cell <b>two</b>\
                   </td></tr><tr><td>alone</td></tr></table><p>After</p>";
    let rendered = laid_out(content);
    let Block::Table(table) = &rendered.blocks[1] else {
        panic!("a table: {:?}", rendered.blocks);
    };
    assert_eq!(table.columns(), 2);
    assert!(table.rows[0].iter().all(|cell| cell.header));
    assert!(!table.rows[1][0].header);
    // copied as a reader sees it: a row a line, its cells apart
    assert_eq!(
        rendered.blocks[1].plain_lines(),
        ["Name\tState", "Cell one\tCell two", "alone"]
    );
    let (cx, _) = drawn(content);
    let mut cell = |row: usize, column: usize| {
        cx.debug_bounds(Box::leak(format!("rich-table-cell-{row}-{column}").into_boxed_str()))
            .unwrap_or_else(|| panic!("cell {row} {column} is drawn"))
    };
    let (one, two) = (cell(1, 0), cell(1, 1));
    assert!(one.right() <= two.left(), "{one:?} {two:?}");
    // the columns line up, row under row
    assert_eq!(cell(0, 1).left(), two.left());
    assert!(cell(0, 0).bottom() <= one.top() && one.bottom() <= cell(2, 0).top());
    // a short row keeps the table's columns
    assert_eq!(cell(2, 1).left(), two.left());
    let table = cx.debug_bounds("rich-table").expect("the table is drawn");
    assert!(table.left() <= one.left() && two.right() <= table.right());
}

#[test]
fn every_node_draws_and_a_link_opens_through_open_url() {
    let content = "<p><a href=\"https://example.test/opened\">example</a></p><p>a <b>bold</b> <code>c</code> \
                   <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span><br>\
                   <img src=\"https://images.example.test/x.png\" alt=\"x\"></p><blockquote><p>quoted</p></blockquote>\
                   <ol><li>listed</li></ol>";
    let properties = format!("{{\"mentions\":[{{\"itemid\":\"0\",\"mri\":\"{ME}\",\"mentionType\":\"person\"}}]}}");
    let rendered = Arc::new(rich_text::layout(
        &parse_html(content),
        &Mentions::of(Some(&properties), ME),
    ));
    assert!(matches!(&rendered.blocks[2], Block::Quote(QuoteBlock { .. })));
    let cx = TestAppContext::single();
    let app: &'static mut TestAppContext = Box::leak(Box::new(cx));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    for selector in ["rich-link", "rich-text", "rich-quote", "rich-list"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
    let link = cx.debug_bounds("rich-link").expect("the link is drawn");
    // a click on the link's first letters
    cx.simulate_click(link.origin + point(px(3.0), px(4.0)), Modifiers::none());
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.test/opened"));
}

#[test]
fn a_click_beside_a_link_opens_nothing() {
    let rendered = Arc::new(laid_out("<p><a href=\"https://example.test/\">x</a></p>"));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let link = cx.debug_bounds("rich-link").expect("the link is drawn");
    // far right of the line, past its one letter
    cx.simulate_click(
        point(link.origin.x + link.size.width - px(4.0), link.origin.y + px(4.0)),
        Modifiers::none(),
    );
    assert_eq!(cx.opened_url(), None);
}

/// The highlight a piece styled `style` is drawn with, in the light palette.
fn drawn_as(style: Style) -> HighlightStyle {
    highlight(style, LIGHT_PALETTE)
}

fn marked(marks: Marks) -> HighlightStyle {
    drawn_as(Style {
        marks,
        ..Style::default()
    })
}

fn mentioned(whom: Mentioned) -> HighlightStyle {
    drawn_as(Style {
        mention: Some(whom),
        ..Style::default()
    })
}

#[test]
fn each_mark_a_link_and_an_image_are_drawn_with_a_highlight_of_their_own() {
    assert_eq!(drawn_as(Style::default()), HighlightStyle::default());
    assert_eq!(marked(Marks::BOLD).font_weight, Some(FontWeight::BOLD));
    assert_eq!(marked(Marks::ITALIC).font_style, Some(FontStyle::Italic));
    assert!(marked(Marks::UNDERLINE).underline.is_some());
    assert!(marked(Marks::STRIKE).strikethrough.is_some());
    assert!(marked(Marks::CODE).background_color.is_some());
    let link = drawn_as(Style {
        link: true,
        ..Style::default()
    });
    assert!(link.underline.is_some() && link.color.is_some());
    let image = drawn_as(Style {
        image: true,
        ..Style::default()
    });
    assert_eq!(image.font_style, Some(FontStyle::Italic));
}

#[test]
fn a_mention_is_drawn_in_the_brand_colour_the_account_s_own_bold_in_the_mention_colour() {
    let (me, other) = (mentioned(Mentioned::Me), mentioned(Mentioned::Other));
    assert_eq!(
        other.color,
        Some(gpui::rgb(LIGHT_PALETTE.brand).into()),
        "someone else's in the brand colour"
    );
    assert_eq!((other.font_weight, other.background_color), (None, None));
    assert_eq!(
        me.color,
        Some(gpui::rgb(LIGHT_PALETTE.mention).into()),
        "the account's own in Teams' orange"
    );
    assert_eq!(me.font_weight, Some(FontWeight::BOLD), "the account's own is bold");
    assert_eq!(me.background_color, None, "no box behind a mention, as in Teams");

    // a block is drawn with its pieces' highlights
    let content =
        "<p>a <b>bold</b> <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span></p>";
    let properties = format!("{{\"mentions\":[{{\"itemid\":\"0\",\"mri\":\"{ME}\",\"mentionType\":\"person\"}}]}}");
    let rendered = rich_text::layout(&parse_html(content), &Mentions::of(Some(&properties), ME));
    let block = text_block(&rendered, 0);
    let drawn = rich_text::highlights(block, LIGHT_PALETTE);
    let of = |text: &str| {
        drawn
            .iter()
            .find(|(range, _)| &block.text[range.clone()] == text)
            .map(|(_, highlight)| *highlight)
    };
    assert_eq!(of("bold"), Some(marked(Marks::BOLD)));
    assert_eq!(of("Lab A"), Some(me));
    assert_eq!(drawn.len(), 2, "unstyled text has no highlight: {drawn:?}");
}

/// A pasted picture's `src`, its view on the media service's host (a made-up object).
const PICTURE: &str = "https://us-api.asm.skype.com/v1/objects/0-eus-d9-00000000000000000000000000000abc/views/imgo";

/// An emoticon's `<img>` as Teams sends it, wrapped in its span (`docs/protocol/chat.md`).
fn emoticon(id: &str, alt: &str, side: u32) -> String {
    format!(
        "<span title=\"{id}\" type=\"({id})\" class=\"animated-emoticon-{side}-{id}\" itemscope=\"\"><img itemscope=\"\" \
         itemtype=\"http://schema.skype.com/Emoji\" itemid=\"{id}\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/{id}/default/\
         20_f.png\" alt=\"{alt}\" style=\"width:{side}px; height:{side}px\"></span>"
    )
}

#[test]
fn an_emoticon_keeps_its_place_in_the_text_and_a_pasted_picture_is_a_block() {
    let content = format!(
        "<p>made-up board summary:</p>\
         <p><img src=\"{PICTURE}\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"884\" height=\"228\" \
         alt=\"image\"></p>\
         <p>12 made-up cards moved</p><p>7 made-up cards added{}</p>",
        emoticon("speechless", "\u{1f610}", 20)
    );
    let link = "https://teams.microsoft.com/l/message/19:x@thread.v2/1";
    let rendered = rich_text::layout_with_web_link(&parse_html(&content), &Mentions::default(), Some(link));
    assert_eq!(rendered.blocks.len(), 4, "{rendered:#?}");
    let Block::Picture(picture) = &rendered.blocks[1] else {
        panic!("a picture block: {rendered:#?}");
    };
    assert_eq!((picture.src.as_str(), picture.size), (PICTURE, Some((884, 228))));
    // drawn when it can't be shown: its placeholder and the link to the message
    assert_eq!(
        picture.fallback.text,
        format!("[{IMAGE_PLACEHOLDER}: image] {}", rich_text::IMAGE_WEB_LINK)
    );
    assert_eq!(picture.fallback.links[0].href, link);

    let last = text_block(&rendered, 3);
    assert_eq!(last.text, format!("7 made-up cards added{}", rich_text::EMOTICON_PLACE));
    assert_eq!(last.emoticons.len(), 1);
    assert_eq!(last.emoticons[0].id, "speechless");
    assert_eq!(&last.text[last.emoticons[0].range.clone()], rich_text::EMOTICON_PLACE);
    assert_eq!(last.plain(), "7 made-up cards added\u{1f610}");
    assert!(!last.text.contains(IMAGE_PLACEHOLDER), "no placeholder for an emoticon");
    assert!(
        rendered.emoticons_only().is_none() && !rendered.bare,
        "text with it: drawn in its bubble"
    );

    // a picture a quote shows, or one inside a link, stays a placeholder; so does an emoticon without its id
    let quoted = format!(
        "<blockquote><p><img src=\"{PICTURE}\" alt=\"q\"></p></blockquote>\
         <p><a href=\"https://example.test/page\"><img src=\"{PICTURE}\" alt=\"l\"></a> \
         <img itemtype=\"http://schema.skype.com/Emoji\" src=\"https://images.example.test/e.png\" alt=\"e\"></p>"
    );
    let rendered = rich_text::layout(&parse_html(&quoted), &Mentions::default());
    assert!(
        !rendered.blocks.iter().any(|block| matches!(block, Block::Picture(_))),
        "{rendered:#?}"
    );
    assert!(
        text_block(&rendered, 1)
            .text
            .contains(&format!("[{IMAGE_PLACEHOLDER}: e]"))
    );
}

#[test]
fn a_picture_on_an_own_servers_media_service_is_a_block() {
    let src = "https://chat.example:8443/ams/v1/objects/0-own-d1-0a56c46309364577b47881c59dae75a7/views/imgo";
    let content = format!(
        "<p><img src=\"{src}\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"240\" height=\"160\" \
         alt=\"image\"></p>"
    );
    let rendered = rich_text::layout(&parse_html(&content), &Mentions::default());
    let [Block::Picture(picture)] = &rendered.blocks[..] else {
        panic!("a picture block: {rendered:#?}");
    };
    assert_eq!((picture.src.as_str(), picture.size), (src, Some((240, 160))));
    // anything on that server that isn't a view of its media service stays a placeholder
    let other = "<p><img src=\"https://chat.example:8443/files/made-up.png\" alt=\"x\"></p>";
    let rendered = rich_text::layout(&parse_html(other), &Mentions::default());
    assert!(
        !rendered.blocks.iter().any(|block| matches!(block, Block::Picture(_))),
        "{rendered:#?}"
    );
}

/// A GIF from Teams' GIF picker, on Giphy's media host as a message names it (the id and Giphy's token made up).
const GIF: &str = "https://media2.giphy.com/media/v1.Y2lkPW1hZGUtdXA/MadeUpGif000/giphy.gif";

#[test]
fn a_gif_from_the_gif_picker_is_a_picture_block_at_its_size() {
    // as the web client sends one: between two blank paragraphs, its size as attributes
    let content = format!(
        "<p>&nbsp;</p>\n<img src=\"{GIF}\" width=\"250\" height=\"200\" alt=\"Made Up GIF (GIF Image)\" \
         style=\"height:auto; margin-top:4px; max-width:100%\">\n<p>&nbsp;</p>"
    );
    let link = "https://teams.microsoft.com/l/message/19:x@thread.v2/1";
    let rendered = rich_text::layout_with_web_link(&parse_html(&content), &Mentions::default(), Some(link));
    let pictures: Vec<_> = rendered
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Picture(picture) => Some(picture),
            Block::Text(_)
            | Block::Quote(_)
            | Block::List(_)
            | Block::Forward(_)
            | Block::Code(_)
            | Block::Rule
            | Block::Table(_) => None,
        })
        .collect();
    assert_eq!(pictures.len(), 1, "{rendered:#?}");
    assert_eq!((pictures[0].src.as_str(), pictures[0].size), (GIF, Some((250, 200))));
    assert!(
        rendered.blocks.iter().all(|block| match block {
            Block::Text(text) => !text.text.contains(IMAGE_PLACEHOLDER),
            Block::Picture(_)
            | Block::Quote(_)
            | Block::List(_)
            | Block::Forward(_)
            | Block::Code(_)
            | Block::Rule
            | Block::Table(_) => true,
        }),
        "no placeholder beside it: {rendered:#?}"
    );
    // drawn when it can't be read: its placeholder and the link to the message
    assert_eq!(
        pictures[0].fallback.text,
        format!(
            "[{IMAGE_PLACEHOLDER}: Made Up GIF (GIF Image)] {}",
            rich_text::IMAGE_WEB_LINK
        )
    );

    // the Giphy `itemtype`'s form, its size in its style
    let typed = format!(
        "<div>Confirmed. <br><img src=\"{GIF}\" alt=\"made up GIF\" itemscope=\"\" \
         itemtype=\"http://schema.skype.com/Giphy\" style=\"width:400px; height:300px\"></div>"
    );
    let rendered = laid_out(&typed);
    let Some(Block::Picture(picture)) = rendered.blocks.last() else {
        panic!("a picture block: {rendered:#?}");
    };
    assert_eq!((picture.src.as_str(), picture.size), (GIF, Some((400, 300))));

    // another host's GIF stays a placeholder
    let elsewhere = laid_out("<p><img src=\"https://images.example.test/made-up.gif\" alt=\"g\"></p>");
    assert!(
        !elsewhere.blocks.iter().any(|block| matches!(block, Block::Picture(_))),
        "{elsewhere:#?}"
    );
}

#[test]
fn a_forwarded_messages_content_is_laid_out_as_the_messages_own_in_a_block_of_its_own() {
    // as the service gives one back: a blockquote of itemtype Forward, a picture and a line in it
    let content = format!(
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p><img src=\"{PICTURE}\" \
         itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" width=\"127.40112994350282\" height=\"250\" \
         alt=\"image\"></p>\r\n\r\n\r\n<p>A made-up remark <a href=\"https://example.test/page\">here</a></p>\r\n\
         </blockquote>"
    );
    let web_link = "https://teams.microsoft.com/l/message/19:x@thread.v2/1";
    let rendered = rich_text::layout_with_web_link(&parse_html(&content), &Mentions::default(), Some(web_link));
    let [Block::Forward(forward)] = &rendered.blocks[..] else {
        panic!("one forward: {rendered:#?}");
    };
    let [Block::Picture(picture), Block::Text(line)] = &forward.blocks[..] else {
        panic!("its picture drawn and its line: {forward:#?}");
    };
    assert_eq!((picture.src.as_str(), picture.size), (PICTURE, Some((127, 250))));
    assert_eq!(
        picture.fallback.links[0].href, web_link,
        "the placeholder's link, as a message's own"
    );
    assert_eq!(line.text, "A made-up remark here");
    assert_eq!(line.links[0].href, "https://example.test/page");
    assert!(!rendered.bare && rendered.emoticons_only().is_none());
    // copied as its content
    assert_eq!(rendered.blocks[0].plain_lines(), ["A made-up remark here"]);

    // a mention in it is the original's, never the account's
    let mentioned = "<blockquote itemtype=\"http://schema.skype.com/Forward\"><p><span itemtype=\
                     \"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span></p></blockquote>";
    let properties = format!("{{\"mentions\":[{{\"itemid\":\"0\",\"mri\":\"{ME}\",\"mentionType\":\"person\"}}]}}");
    let mentions = Mentions::of(Some(&properties), ME);
    assert!(mentions.names_me());
    let rendered = rich_text::layout(&parse_html(mentioned), &mentions);
    assert!(!rendered.mentions_me());

    // a plain quote around a picture keeps it a placeholder, as before
    let quoted = format!("<blockquote><p><img src=\"{PICTURE}\" alt=\"q\"></p></blockquote>");
    let rendered = rich_text::layout(&parse_html(&quoted), &Mentions::default());
    assert!(matches!(&rendered.blocks[..], [Block::Quote(_)]), "{rendered:#?}");
}

#[test]
fn a_message_of_emoticons_alone_is_drawn_bare() {
    let one = format!("<div>{}</div>", emoticon("1f44b_wavinghand", "\u{1f44b}", 58));
    let rendered = rich_text::layout(&parse_html(&one), &Mentions::default());
    let emoticons = rendered.emoticons_only().unwrap();
    assert_eq!(emoticons.len(), 1);
    assert_eq!(emoticons[0].id, "1f44b_wavinghand");
    assert!(rendered.bare, "no bubble behind it");

    let three = format!(
        "<p>{} &nbsp;{}</p><p>{}</p>",
        emoticon("smile", "\u{1f642}", 20),
        emoticon("hi", "\u{1f44b}", 20),
        emoticon("yes", "\u{1f44d}", 20)
    );
    let rendered = rich_text::layout(&parse_html(&three), &Mentions::default());
    assert_eq!(rendered.emoticons_only().map(|all| all.len()), Some(3));
    assert!(rendered.bare);

    for not_alone in [
        format!("<p>hi {}</p>", emoticon("smile", "\u{1f642}", 20)),
        format!(
            "<p>{}</p><p><img src=\"{PICTURE}\"></p>",
            emoticon("smile", "\u{1f642}", 20)
        ),
        format!(
            "<p><a href=\"https://example.test/\">{}</a></p>",
            emoticon("smile", "\u{1f642}", 20)
        ),
        format!(
            "<p>{}</p>",
            emoticon("smile", "\u{1f642}", 20).repeat(rich_text::MOST_LARGE_EMOTICONS + 1)
        ),
        "<p>just text</p>".to_owned(),
    ] {
        let rendered = rich_text::layout(&parse_html(&not_alone), &Mentions::default());
        assert!(rendered.emoticons_only().is_none() && !rendered.bare, "{not_alone}");
    }
}

#[test]
fn a_message_of_pictures_alone_is_drawn_bare() {
    let alone = format!(
        "<p><img alt=\"Media\" itemscope=\"\" itemtype=\"http://schema.skype.com/AMSImage\" src=\"{PICTURE}\" \
         width=\"1080\" height=\"2520\"></p>"
    );
    let rendered = rich_text::layout(&parse_html(&alone), &Mentions::default());
    assert!(rendered.pictures_only() && rendered.bare, "{rendered:#?}");

    let with_text = format!("<p>look</p><p><img src=\"{PICTURE}\"></p>");
    let rendered = rich_text::layout(&parse_html(&with_text), &Mentions::default());
    assert!(!rendered.pictures_only() && !rendered.bare, "{rendered:#?}");
}

/// A made-up sticker on Teams' sticker CDN, as a message names it.
const STICKER: &str = "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif?v=5";

#[test]
fn a_message_of_a_sticker_alone_is_the_sticker_bare_at_its_size() {
    // read back: the bare `img` between two blank paragraphs; as sent: in its `readonly` element
    let read_back = format!(
        "<p>&nbsp;</p>\r\n<img src=\"{STICKER}\" height=\"180\" alt=\"A made-up sticker\" width=\"180\" \
         itemtype=\"http://schema.skype.com/Sticker\">\r\n<p>&nbsp;</p>"
    );
    let sent = format!(
        "<p>&nbsp;</p><readonly aria-label=\"A made-up label\" contenteditable=\"false\" \
         itemtype=\"http://schema.skype.com/Sticker\" title=\"A made-up label\"><img src=\"{STICKER}\" height=\"180\" \
         alt=\"A made-up label\" width=\"180\" data-image-type=\"standard\" data-image-mode=\"single\" \
         itemtype=\"http://schema.skype.com/Sticker\"></readonly><p>&nbsp;</p>"
    );
    for (content, label) in [(read_back, "A made-up sticker"), (sent, "A made-up label")] {
        let rendered = laid_out(&content);
        assert!(rendered.bare, "no bubble behind it: {rendered:#?}");
        let [Block::Picture(picture)] = &rendered.blocks[..] else {
            panic!("the sticker alone, no blank lines: {rendered:#?}");
        };
        assert!(picture.sticker);
        assert_eq!((picture.src.as_str(), picture.size), (STICKER, Some((180, 180))));
        assert_eq!(picture.alt.as_deref(), Some(label));
        assert_eq!(picture.fallback.text, format!("[{IMAGE_PLACEHOLDER}: {label}]"));
        assert_eq!(rendered.sticker_alone(), Some(picture));
        assert!(rendered.pictures().is_empty(), "no click opens a sticker in the viewer");
    }
}

#[test]
fn a_sticker_beside_text_or_in_a_quote_is_no_bare_message() {
    let sticker = format!(
        "<img src=\"{STICKER}\" width=\"180\" height=\"180\" alt=\"s\" itemtype=\"http://schema.skype.com/Sticker\">"
    );
    // with words: the sticker in the bubble, under them
    let with_text = laid_out(&format!("<p>made-up words</p>{sticker}"));
    assert!(!with_text.bare && with_text.sticker_alone().is_none(), "{with_text:#?}");
    assert!(matches!(&with_text.blocks[..], [Block::Text(_), Block::Picture(picture)] if picture.sticker));
    // two stickers: neither alone
    let two = laid_out(&format!("{sticker}{sticker}"));
    assert!(two.sticker_alone().is_none());
    // a quoted one is its placeholder, as another message's picture is
    let quoted = laid_out(&format!("<blockquote>{sticker}</blockquote><p>made-up reply</p>"));
    assert!(quoted.sticker_alone().is_none() && !quoted.bare);
    let Some(Block::Quote(quote)) = quoted.blocks.first() else {
        panic!("a quote: {quoted:#?}");
    };
    assert!(matches!(&quote.blocks[..], [Block::Text(text)] if text.text == format!("[{IMAGE_PLACEHOLDER}: s]")));
    // off Teams' sticker CDN and the media service: its placeholder, no sticker
    let elsewhere = laid_out(
        "<img src=\"https://images.example.test/a.gif\" alt=\"e\" itemtype=\"http://schema.skype.com/Sticker\">",
    );
    assert!(elsewhere.sticker_alone().is_none() && !elsewhere.bare, "{elsewhere:#?}");
}

/// The engine's side a message's pictures and emoticons are asked of: answers from its own pictures, no animation,
/// and records what it was asked.
#[derive(Default)]
struct FakePictures {
    pictures: Mutex<std::collections::HashMap<String, teams_core::PictureBytes>>,
    asked: Mutex<Vec<&'static str>>,
}

impl rich_text::PictureEngine for FakePictures {
    fn me(&self) -> &str {
        ME
    }

    fn message_picture(&self, src: String) -> teams_core::Reply<Option<teams_core::PictureBytes>> {
        self.asked.lock().unwrap().push("message_picture");
        teams_core::Reply::ready(Ok(self.pictures.lock().unwrap().get(&src).cloned()))
    }

    fn reaction_animation(&self, _: String) -> teams_core::Reply<Option<teams_core::Strip>> {
        self.asked.lock().unwrap().push("reaction_animation");
        teams_core::Reply::ready(Ok(None))
    }

    fn engine(&self) -> *const () {
        std::ptr::from_ref(self).cast()
    }
}

impl FakePictures {
    fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().clone()
    }
}

/// A made-up picture's PNG, `width` by `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

#[test]
fn a_pasted_picture_is_asked_of_the_engine_and_drawn_within_the_width_there_is() {
    let core = Arc::new(FakePictures::default());
    core.pictures.lock().unwrap().insert(
        PICTURE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png(40, 10),
        }),
    );
    // the message gives it a size wider than any window: drawn no wider than the width there is, a quarter as high
    let content = format!("<p>look</p><p><img src=\"{PICTURE}\" width=\"40000\" height=\"10000\"></p>");
    let source = rich_text::PictureSource::of(core.clone());
    let rendered = Arc::new(rich_text::layout(&parse_html(&content), &Mentions::default()).with_source(source));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    assert!(core.asked().contains(&"message_picture"));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let drawn = cx.debug_bounds("rich-picture-drawn").expect("the picture is drawn");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(drawn.size.width <= window.width, "{drawn:?} in {window:?}");
    // its frame's border aside, a quarter as high as wide
    let (width, height) = (drawn.size.width - px(2.0), drawn.size.height - px(2.0));
    assert!((f32::from(height) - f32::from(width) / 4.0).abs() < 2.0, "{drawn:?}");
}

/// A made-up animated GIF, `width` by `height`, of `frames` frames in turn red and blue.
fn gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
        for at in 0..frames {
            let colour = if at % 2 == 0 {
                [200, 40, 40, 255]
            } else {
                [40, 40, 200, 255]
            };
            let frame = image::RgbaImage::from_pixel(u32::from(width), u32::from(height), image::Rgba(colour));
            encoder
                .encode_frame(image::Frame::from_parts(
                    frame,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
    }
    bytes
}

#[test]
fn a_gif_is_asked_of_the_engine_and_drawn_with_every_frame_at_its_size() {
    let picture = Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Gif,
        bytes: gif(50, 40, 3),
    });
    let (image, natural) = teams_ui::inline::decode(&picture).expect("the GIF decodes");
    assert_eq!(
        (image.frame_count(), natural),
        (3, (50, 40)),
        "every frame kept to play"
    );

    let core = Arc::new(FakePictures::default());
    core.pictures.lock().unwrap().insert(GIF.to_owned(), picture);
    let content = format!("<p><img src=\"{GIF}\" width=\"250\" height=\"200\" alt=\"made up GIF\"></p>");
    let source = rich_text::PictureSource::of(core.clone());
    let rendered = Arc::new(rich_text::layout(&parse_html(&content), &Mentions::default()).with_source(source));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    assert!(core.asked().contains(&"message_picture"));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let drawn = cx.debug_bounds("rich-picture-drawn").expect("the GIF is drawn");
    // as Teams draws a picture neither long nor small: as high as its frame's side, its aspect ratio kept, the
    // frame's border aside
    assert_eq!(
        (drawn.size.width, drawn.size.height),
        (px(225.0 + 2.0), px(PICTURE_BOX + 2.0)),
        "{drawn:?}"
    );
}

#[test]
fn an_emoticon_alone_is_drawn_large_and_one_in_text_at_the_text_s_height() {
    let core = Arc::new(FakePictures::default());
    let alone = format!("<div>{}</div>", emoticon("1f44b_wavinghand", "\u{1f44b}", 58));
    let rendered = Arc::new(
        rich_text::layout(&parse_html(&alone), &Mentions::default())
            .with_source(rich_text::PictureSource::of(core.clone())),
    );
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let large = cx.debug_bounds("rich-emoticons").expect("drawn large");
    assert!(large.size.height >= px(58.0), "{large:?}");
    assert!(core.asked().contains(&"reaction_animation"), "its animation asked for");

    let inline = format!("<p>hi there{}</p>", emoticon("smile", "\u{1f642}", 20));
    let rendered = Arc::new(laid_out(&inline));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let line = cx.debug_bounds("rich-text").expect("the line is drawn");
    assert!(line.size.height < px(40.0), "one line of text: {line:?}");
    // Teams' 20 px beside its 14 px text, in the app's text size (to the pixel GPUI rounds a layout to), the line
    // keeping its height; standing on the text's baseline, not hung from the line's top (where it is exactly is
    // `inline`'s test: the test platform's font metrics are made up)
    let (rem, line_height) = cx.update(|window, _| {
        (
            window.rem_size(),
            window.text_style().line_height_in_pixels(window.rem_size()),
        )
    });
    let drawn = cx.debug_bounds("rich-inline-emoticon").expect("the emoticon is drawn");
    let side = rem * teams_ui::inline::INLINE_EMOTICON_REMS;
    assert!(
        (drawn.size.width - side).abs() <= px(1.0) && (drawn.size.height - side).abs() <= px(1.0),
        "{drawn:?}"
    );
    assert_eq!(line.size.height, line_height, "the line keeps its height: {line:?}");
    assert!(drawn.top() >= line.top() + px(2.0), "{drawn:?} on {line:?}");
}

/// A line, a pasted picture given `width` by `height` (its bytes twice as dense, as one pasted from a dense screen),
/// and a line, as Teams sends them; the picture's bytes put on `core`.
fn picture_between_lines(core: &FakePictures, width: u32, height: u32) -> String {
    core.pictures.lock().unwrap().insert(
        PICTURE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png(width * 2, height * 2),
        }),
    );
    format!(
        "<p>before</p>\n<p><img src=\"{PICTURE}\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"{width}\" \
         height=\"{height}\" alt=\"image\"></p>\n<p>after</p>"
    )
}

/// `content` drawn in a window of its own, the kit's theme set up, its pictures asked of `core` and read.
fn drawn_with(core: &Arc<FakePictures>, content: &str) -> &'static mut gpui::VisualTestContext {
    let source = rich_text::PictureSource::of(core.clone());
    let rendered = Arc::new(rich_text::layout(&parse_html(content), &Mentions::default()).with_source(source));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    app.update(teams_ui::theme::init);
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx
}

#[test]
fn a_thin_picture_is_drawn_at_its_own_size_inside_its_block() {
    let core = Arc::new(FakePictures::default());
    let content = picture_between_lines(&core, 198, 53);
    let cx = drawn_with(&core, &content);
    let window = cx.update(|window, _| window.viewport_size());
    assert!(window.width > px(400.0), "{window:?}");
    // its block spans the width there is, the picture in it only its own size (in its frame's border) at the start of
    // its line, never stretched to the block's width nor drawn past its bottom (over the line after it)
    let block = cx.debug_bounds("rich-picture").expect("its block");
    let drawn = cx.debug_bounds("rich-picture-drawn").expect("the picture is drawn");
    assert_eq!(
        (drawn.size.width, drawn.size.height),
        (px(198.0 + 2.0), px(53.0 + 2.0)),
        "{drawn:?}"
    );
    assert_eq!(drawn.origin.x, block.origin.x, "{drawn:?} in {block:?}");
    assert!(
        drawn.top() >= block.top() && drawn.bottom() <= block.bottom(),
        "{drawn:?} in {block:?}"
    );
}

#[test]
fn a_large_wide_picture_is_bounded_as_teams_bounds_it_inside_its_block() {
    let core = Arc::new(FakePictures::default());
    let content = picture_between_lines(&core, 4000, 1000);
    let cx = drawn_with(&core, &content);
    let block = cx.debug_bounds("rich-picture").expect("its block");
    let drawn = cx.debug_bounds("rich-picture-drawn").expect("the picture is drawn");
    // at most Teams' widest (its frame's border aside), and no wider than its block
    let widest = px(WIDE_MAX_WIDTH + 2.0).min(block.size.width);
    assert!((drawn.size.width - widest).abs() < px(1.0), "{drawn:?} in {block:?}");
    assert!(
        (f32::from(drawn.size.height - px(2.0)) - f32::from(drawn.size.width - px(2.0)) / 4.0).abs() < 1.0,
        "its aspect ratio kept: {drawn:?}"
    );
    assert!(drawn.bottom() <= block.bottom(), "{drawn:?} in {block:?}");
}

#[test]
fn a_tall_picture_is_fitted_into_a_square_frame_never_upscaled() {
    let core = Arc::new(FakePictures::default());
    // a phone's screenshot, as Teams sends one: 1080 by 2520
    let content = picture_between_lines(&core, 1080, 2520);
    let cx = drawn_with(&core, &content);
    let drawn = cx.debug_bounds("rich-picture-drawn").expect("the picture is drawn");
    assert_eq!(
        (drawn.size.width, drawn.size.height),
        (px(PICTURE_BOX + 2.0), px(PICTURE_BOX + 2.0)),
        "a square frame: {drawn:?}"
    );
}

/// A block of code as Teams' code block editor sends one, every value made up.
const CODE_BLOCK: &str = "<p>as this object:</p>\n<p></p>\n\
    <p itemtype=\"http://schema.skype.com/CodeBlockEditor\" id=\"x_codeBlockEditor-made-up\">\n&nbsp;</p>\n\
    <pre class=\"language-json language-was-manually-selected\" itemid=\"codeBlockEditor-made-up\"><code>{<br> \
    &nbsp;&nbsp; <span class=\"hljs-function\">&quot;kind&quot;</span>:&nbsp;<span class=\"hljs-string\">\
    &quot;made&nbsp;up&quot;</span>,<br> &nbsp;&nbsp; <span class=\"hljs-function\">&quot;next&quot;</span>:&nbsp;\
    <span class=\"hljs-keyword\">null</span><br>}</code></pre>\n<p></p>\n<p>and nothing else</p>";

#[test]
fn a_code_block_is_laid_out_as_numbered_lines_of_tokens_under_its_language() {
    let rendered = laid_out(CODE_BLOCK);
    // the editor's own paragraph and the empty ones are no blocks
    assert_eq!(rendered.blocks.len(), 3, "{rendered:#?}");
    let Block::Code(code) = &rendered.blocks[1] else {
        panic!("a code block: {rendered:#?}");
    };
    assert_eq!(code.label(), "JSON");
    let lines: Vec<&str> = code.lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(lines, ["{", "    \"kind\": \"made up\",", "    \"next\": null", "}"]);
    let second = &code.lines[1];
    let tokens: Vec<(&str, Token)> = second
        .tokens
        .iter()
        .map(|(range, token)| (&second.text[range.clone()], *token))
        .collect();
    assert_eq!(tokens, [("\"kind\"", Token::Function), ("\"made up\"", Token::String)]);
    assert_eq!(code.text(), "{\n    \"kind\": \"made up\",\n    \"next\": null\n}");
    assert_eq!(rendered.blocks[1].plain_lines(), lines);
}

#[test]
fn a_code_block_s_tokens_are_coloured_its_tabs_kept_and_its_language_named() {
    // the kinds drawn in colours of their own, a comment muted and italic, plain text as the text around it
    for kind in [Token::Function, Token::String, Token::Keyword] {
        assert!(token_style(kind, LIGHT_PALETTE).color.is_some(), "{kind:?}");
    }
    assert_ne!(
        token_style(Token::Function, LIGHT_PALETTE).color,
        token_style(Token::String, LIGHT_PALETTE).color
    );
    assert_eq!(
        token_style(Token::Comment, LIGHT_PALETTE).font_style,
        Some(FontStyle::Italic)
    );
    assert_eq!(token_style(Token::Plain, LIGHT_PALETTE), HighlightStyle::default());
    // a tab is drawn as four spaces, its tokens moved with it, and copied as the tab it is
    let Some(Block::Code(tabbed)) = laid_out("<pre>all:\n\t<span class=\"hljs-keyword\">cc</span> x.c</pre>")
        .blocks
        .first()
        .cloned()
    else {
        panic!("a code block");
    };
    assert_eq!(tabbed.text(), "all:\n\tcc x.c");
    assert_eq!(
        rich_text::Block::Code(tabbed.clone()).plain_lines(),
        ["all:", "\tcc x.c"]
    );
    let (shown, tokens) = tabbed.lines[1].shown();
    assert_eq!(shown, "    cc x.c");
    assert_eq!(tokens, [(4..6, Token::Keyword)]);
    // a block of code without a language is plain text; one of a language Teams doesn't name keeps its id
    for (html, label) in [
        ("<pre>x</pre>", "Plain text"),
        ("<pre class=\"language-plaintext\">x</pre>", "Plain text"),
        ("<pre class=\"language-madeup\">x</pre>", "madeup"),
        ("<pre class=\"language-csharp\">x</pre>", "C#"),
    ] {
        let Some(Block::Code(code)) = laid_out(html).blocks.first().cloned() else {
            panic!("{html}");
        };
        assert_eq!(code.label(), label, "{html}");
    }
}

#[test]
fn a_code_block_is_drawn_as_teams_card_and_its_button_copies_the_code() {
    let core = Arc::new(FakePictures::default());
    let cx = drawn_with(&core, CODE_BLOCK);
    let card = cx.debug_bounds("rich-code").expect("the card is drawn");
    let lines = cx.debug_bounds("rich-code-lines").expect("its lines are drawn");
    let language = cx.debug_bounds("rich-code-language").expect("its language is drawn");
    assert!(language.bottom() <= lines.top(), "the header over the lines");
    assert!(lines.bottom() <= card.bottom() && lines.top() >= card.top());
    let copy = cx.debug_bounds("rich-code-copy").expect("its copy button is drawn");
    assert!(copy.left() > language.left(), "the copy button on the header's right");
    cx.simulate_click(copy.center(), Modifiers::none());
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("{\n    \"kind\": \"made up\",\n    \"next\": null\n}")
    );
}

#[test]
fn a_message_takes_the_direction_of_its_first_strong_letter_as_teams_dir_auto_does() {
    let reply = format!(
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"1\">\
         <strong itemprop=\"mri\" itemid=\"{LAB_B}\">Made-up Name</strong><span itemprop=\"time\" itemid=\"1\"></span>\
         <p itemprop=\"preview\">مرحبا</p></blockquote><p>Fine, thanks.</p>"
    );
    for (content, rtl) in [
        ("<p>مرحبا بكم</p><p>Hello</p>", true),
        ("<p>Hello</p><p>مرحبا بكم</p>", false),
        ("<p>12 — \"!</p><p>שלום</p>", true),
        ("<p>12, 13.</p>", false),
        ("<ul><li>عربي</li></ul>", true),
        ("<table><tr><td>עברית</td></tr></table>", true),
        ("<blockquote><p>عربي</p></blockquote><p>English</p>", true),
        (reply.as_str(), false),
        ("<hr><p>عربي</p>", true),
        (
            "<blockquote itemtype=\"http://schema.skype.com/Forward\"><p>عربي</p></blockquote>",
            true,
        ),
        ("<p>(12) ابت</p>", true),
        ("<p>(12)</p><p>ابت</p>", true),
        ("<p>(12)</p>", false),
        ("<pre>let x = 1;</pre><p>ابت</p>", true),
    ] {
        assert_eq!(laid_out(content).right_to_left(), rtl, "{content}");
    }
}

#[test]
fn a_right_to_left_message_puts_its_list_markers_at_the_right() {
    for (content, rtl) in [
        ("<ul><li>البند الأول</li></ul>", true),
        ("<ul><li>First item</li></ul>", false),
    ] {
        let (cx, _) = drawn(content);
        let marker = cx.debug_bounds("rich-list-marker").expect("the marker is drawn");
        let item = cx.debug_bounds("rich-text").expect("the item is drawn");
        if rtl {
            assert!(
                marker.left() >= item.right(),
                "{content}: marker {marker:?}, item {item:?}"
            );
        } else {
            assert!(
                marker.right() <= item.left(),
                "{content}: marker {marker:?}, item {item:?}"
            );
        }
    }
}

/// The emoticon of `inline` drawn, its line, and the advance of each character of the test platform (0.6 em).
fn drawn_emoticon(inline: &str) -> (gpui::Bounds<gpui::Pixels>, gpui::Bounds<gpui::Pixels>, gpui::Pixels) {
    let rendered = Arc::new(laid_out(inline));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let advance = cx.update(|window, _| window.text_style().font_size.to_pixels(window.rem_size()) * 0.6);
    let line = cx.debug_bounds("rich-text").expect("the line is drawn");
    let drawn = cx.debug_bounds("rich-inline-emoticon").expect("the emoticon is drawn");
    (drawn, line, advance)
}

#[test]
fn an_emoticon_before_a_word_that_reads_the_other_way_stands_on_its_own_place() {
    let smile = emoticon("smile", "\u{1f642}", 20);
    // left to right: "hello" at the left, then the place's two spaces, then the Hebrew word
    let (drawn, line, advance) = drawn_emoticon(&format!("<p>hello{smile}שלום</p>"));
    let place = (line.left() + advance * 5.0)..(line.left() + advance * 7.0);
    let middle = drawn.center().x;
    assert!(
        middle >= place.start - px(1.0) && middle <= place.end + px(1.0),
        "{drawn:?} on {place:?} of {line:?}"
    );
    // right to left and aligned right: the Hebrew word at the right, then the place, then "hello" at its left
    let (drawn, line, advance) = drawn_emoticon(&format!("<p>שלום{smile}hello</p>"));
    let place = (line.right() - advance * 6.0)..(line.right() - advance * 4.0);
    let middle = drawn.center().x;
    assert!(
        middle >= place.start - px(1.0) && middle <= place.end + px(1.0),
        "{drawn:?} on {place:?} of {line:?}"
    );
}

/// The test platform's shaper with an en space half as wide as the other characters, as a real font has it.
struct NarrowEnSpace(NoopTextSystem);

impl PlatformTextSystem for NarrowEnSpace {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> gpui::Result<()> {
        self.0.add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        self.0.all_font_names()
    }

    fn font_id(&self, descriptor: &Font) -> gpui::Result<FontId> {
        self.0.font_id(descriptor)
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.0.font_metrics(font_id)
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> gpui::Result<gpui::Bounds<f32>> {
        self.0.typographic_bounds(font_id, glyph_id)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> gpui::Result<Size<f32>> {
        self.0.advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> gpui::Result<gpui::Bounds<DevicePixels>> {
        self.0.glyph_raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: gpui::Bounds<DevicePixels>,
    ) -> gpui::Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let mut layout = self.0.layout_line(text, font_size, runs);
        let narrowed = font_size * 0.3;
        let mut shift = px(0.);
        for glyph in layout.runs.iter_mut().flat_map(|run| &mut run.glyphs) {
            glyph.position.x -= shift;
            if text[glyph.index..].starts_with('\u{2002}') {
                shift += narrowed;
            }
        }
        layout.width -= shift;
        layout
    }
    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.0.recommended_rendering_mode(font_id, font_size)
    }
}

#[test]
fn an_emoticon_before_a_word_that_reads_the_other_way_is_centred_on_its_two_spaces() {
    let smile = emoticon("smile", "\u{1f642}", 20);
    for (inline, centre_from_edge) in [
        // "hello", the em space and the en space, then the Hebrew word
        (format!("<p>hello{smile}שלום</p>"), 5.75),
        // right to left: the Hebrew word at the right, the em space, the en space, then "hello"
        (format!("<p>שלום{smile}hello</p>"), -4.75),
    ] {
        let rendered = Arc::new(laid_out(&inline));
        let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::build_with_text_system(
            TestDispatcher::new(0),
            None,
            Arc::new(NarrowEnSpace(NoopTextSystem)),
        )));
        let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let advance = cx.update(|window, _| window.text_style().font_size.to_pixels(window.rem_size()) * 0.6);
        let line = cx.debug_bounds("rich-text").expect("the line is drawn");
        let drawn = cx.debug_bounds("rich-inline-emoticon").expect("the emoticon is drawn");
        let edge = if centre_from_edge > 0.0 {
            line.left()
        } else {
            line.right()
        };
        let centre = edge + advance * centre_from_edge;
        assert!(
            (drawn.center().x - centre).abs() <= px(0.5),
            "{drawn:?} centred at {centre:?} of {line:?} for {inline:?}"
        );
    }
}

#[test]
fn an_emoticon_inside_right_to_left_text_stands_on_its_place() {
    // the stand-in shaper of the test platform gives each character 0.6 em
    let inline = format!("<p>ابت{}ثج</p>", emoticon("smile", "\u{1f642}", 20));
    let rendered = Arc::new(laid_out(&inline));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let advance = cx.update(|window, _| window.text_style().font_size.to_pixels(window.rem_size()) * 0.6);
    let line = cx.debug_bounds("rich-text").expect("the line is drawn");
    let drawn = cx.debug_bounds("rich-inline-emoticon").expect("the emoticon is drawn");
    // right to left and aligned right: the first word's three letters at the right, then the place's two spaces
    let place = (line.right() - advance * 5.0)..(line.right() - advance * 3.0);
    let middle = drawn.center().x;
    assert!(
        middle >= place.start - px(1.0) && middle <= place.end + px(1.0),
        "{drawn:?} on {place:?} of {line:?}"
    );
}
