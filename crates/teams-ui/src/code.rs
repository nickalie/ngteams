//! A block of code in a message, drawn as Teams' code card: a bordered box whose header shows a code icon, the
//! language's name and a button that copies the code, over the code's lines, each numbered, in the monospace font,
//! its tokens coloured by the kind the sender's editor marked them (`teams_proto::chat::Token`). The code is laid out
//! from `teams-proto`'s model ([`CodeBlock::of`]), never from its markup.

use std::ops::Range;

use gpui::{
    AnyElement, App, ClipboardItem, FontStyle, HighlightStyle, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, TextDirection, Window, div, rgb,
};
use gpui_component::Sizable as _;
use teams_proto::chat::{Code, Token};

use crate::icons::{Glyph, icon_button, selected_by};
use crate::rich_text::tint;
use crate::selection::{self, Place, SelectableText};
use crate::theme::Palette;

/// The copy button's tooltip.
pub const COPY_CODE: &str = "Copy code";

/// The header's name of a block of code whose language its markup doesn't name.
pub const PLAIN_TEXT: &str = "Plain text";

/// What a tab in a block of code is drawn as (only drawn: it is copied as the tab it is).
const TAB: &str = "    ";

/// A block of code laid out to draw: its language's id and its lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeBlock {
    pub language: Option<String>,
    pub lines: Vec<CodeLine>,
}

/// One line of a block of code: its text as it came (a tab a tab) and its tokens' kinds (in order, not overlapping; the
/// rest plain).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeLine {
    pub text: String,
    pub tokens: Vec<(Range<usize>, Token)>,
}

impl CodeBlock {
    /// `code`'s runs split into lines at each line feed.
    #[must_use]
    pub fn of(code: &Code) -> Self {
        let mut lines = vec![CodeLine::default()];
        for run in &code.runs {
            for (at, piece) in run.text.split('\n').enumerate() {
                if at > 0 {
                    lines.push(CodeLine::default());
                }
                let Some(line) = lines.last_mut() else { continue };
                let start = line.text.len();
                line.text.push_str(piece);
                if run.token != Token::Plain && !piece.is_empty() {
                    line.tokens.push((start..line.text.len(), run.token));
                }
            }
        }
        Self {
            language: code.language.clone(),
            lines,
        }
    }

    /// The code as the copy button copies it: its lines as they came, a line feed between them.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The language's name in the card's header: Teams' name for a language its editor offers, else the id as it
    /// came; [`PLAIN_TEXT`] without one.
    #[must_use]
    pub fn label(&self) -> String {
        self.language.as_deref().map_or(PLAIN_TEXT.to_owned(), |id| {
            language_name(id).map_or(id.to_owned(), str::to_owned)
        })
    }
}

impl CodeLine {
    /// The line as it is drawn, each tab as [`TAB`], and its tokens' ranges in that text.
    #[must_use]
    pub fn shown(&self) -> (String, Vec<(Range<usize>, Token)>) {
        let extra = TAB.len() - 1;
        let at = |index: usize| index + extra * self.text[..index].matches('\t').count();
        let tokens = self
            .tokens
            .iter()
            .map(|(range, token)| (at(range.start)..at(range.end), *token))
            .collect();
        (self.text.replace('\t', TAB), tokens)
    }
}

/// Teams' name for a language id its code block editor uses.
#[must_use]
pub fn language_name(id: &str) -> Option<&'static str> {
    Some(match id {
        // one pasted block's class ran another word into the id (`language-plaintextskipProofing`)
        "text" | "plain" => PLAIN_TEXT,
        plain if plain.starts_with("plaintext") => PLAIN_TEXT,
        "bash" | "sh" => "Bash",
        "shell" => "Shell",
        "c" => "C",
        "cpp" | "c++" => "C++",
        "csharp" | "cs" => "C#",
        "css" => "CSS",
        "dart" => "Dart",
        "diff" => "Diff",
        "go" => "Go",
        "graphql" => "GraphQL",
        "html" => "HTML",
        "java" => "Java",
        "javascript" | "js" => "JavaScript",
        "json" => "JSON",
        "kotlin" => "Kotlin",
        "markdown" | "md" => "Markdown",
        "objectivec" => "Objective-C",
        "perl" => "Perl",
        "php" => "PHP",
        "powershell" => "PowerShell",
        "python" | "py" => "Python",
        "r" => "R",
        "ruby" => "Ruby",
        "rust" => "Rust",
        "scala" => "Scala",
        "sql" => "SQL",
        "swift" => "Swift",
        "typescript" | "ts" => "TypeScript",
        "vbnet" => "Visual Basic",
        "verilog" => "Verilog",
        "vhdl" => "VHDL",
        "xml" => "XML",
        "yaml" | "yml" => "YAML",
        _ => return None,
    })
}

/// Whether `palette` is a dark one: its background darker than mid-grey.
fn is_dark(palette: Palette) -> bool {
    let [_, red, green, blue] = palette.background.to_be_bytes();
    u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114 < 128_000
}

/// How a token of `kind` is drawn, as Teams colours it: a function's or a key's name red, a string green, a keyword
/// or a number in the brand colour, a comment muted and italic; plain text as the text around it.
#[must_use]
pub fn token_style(kind: Token, palette: Palette) -> HighlightStyle {
    let dark = is_dark(palette);
    let color = match kind {
        Token::Plain => return HighlightStyle::default(),
        Token::Function => {
            if dark {
                0x00e3_7c7f
            } else {
                0x00c4_314b
            }
        }
        Token::String => {
            if dark {
                0x0054_b054
            } else {
                0x0010_7c10
            }
        }
        Token::Keyword | Token::Number => palette.brand,
        Token::Comment => {
            return HighlightStyle {
                color: Some(rgb(palette.muted_foreground).into()),
                font_style: Some(FontStyle::Italic),
                ..HighlightStyle::default()
            };
        }
    };
    HighlightStyle {
        color: Some(rgb(color).into()),
        ..HighlightStyle::default()
    }
}

/// `block` drawn as Teams' code card; `id` keeps its copy button apart from another card's. Its lines are selectable
/// at `place` (each line the unit under it: [`crate::selection`]) when it has one.
#[must_use]
pub fn card(id: SharedString, block: &CodeBlock, palette: Palette, place: Option<Place>) -> AnyElement {
    CodeCard {
        id,
        block: block.clone(),
        palette,
        place,
    }
    .into_any_element()
}

/// Teams' code card (see the module docs).
#[derive(IntoElement)]
struct CodeCard {
    id: SharedString,
    block: CodeBlock,
    palette: Palette,
    place: Option<Place>,
}

impl CodeCard {
    fn header(&self) -> AnyElement {
        let palette = self.palette;
        let code = self.block.text();
        let copy = icon_button(SharedString::from(format!("{}-copy", self.id)), Glyph::COPY, COPY_CODE)
            .small()
            .on_click(move |_, _, cx: &mut App| cx.write_to_clipboard(ClipboardItem::new_string(code.clone())));
        div()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr_1()
            .py_1()
            .border_b_1()
            .border_color(tint(palette.foreground, 0.2))
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::CODE.icon().size_4())
            .child(
                div()
                    .debug_selector(|| "rich-code-language".to_owned())
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(self.block.label()),
            )
            .child(selected_by("rich-code-copy", copy))
            .into_any_element()
    }

    fn lines(&self, mono: Option<SharedString>) -> AnyElement {
        let palette = self.palette;
        let digits = self.block.lines.len().to_string().len();
        #[expect(clippy::cast_precision_loss, reason = "a line count's digits are a handful")]
        let number_width = gpui::rems(0.65 * digits as f32);
        let rows = self.block.lines.iter().enumerate().map(|(at, line)| {
            let (shown, tokens) = line.shown();
            let highlights = tokens
                .into_iter()
                .map(|(range, kind)| (range, token_style(kind, palette)))
                .collect();
            // an empty line keeps its height
            let text = if shown.is_empty() { " ".to_owned() } else { shown };
            let text = SelectableText::new(
                SharedString::from(format!("{}-line-{at}", self.id)),
                SharedString::from(text),
                highlights,
                Vec::new(),
                self.place.as_ref().map(|place| place.child(at)),
                selection::colour(palette),
            );
            div()
                .flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .w(number_width)
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_color(rgb(palette.muted_foreground))
                        .child((at + 1).to_string()),
                )
                .child(div().flex_1().min_w_0().child(text))
        });
        let body = div()
            .debug_selector(|| "rich-code-lines".to_owned())
            .pl_6()
            .pr_3()
            .py_4()
            .flex()
            .flex_col()
            .children(rows);
        match mono {
            Some(family) => body.font_family(family),
            None => body,
        }
        .into_any_element()
    }
}

impl RenderOnce for CodeCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // the kit's monospace family, which it keeps on a font the machine has; the text's own without the kit
        let mono = cx
            .try_global::<gpui_component::theme::Theme>()
            .map(|theme| theme.mono_font_family.clone());
        div()
            .debug_selector(|| "rich-code".to_owned())
            // code reads left to right, also in a right-to-left message
            .text_left()
            .text_direction(TextDirection::Ltr)
            .my_2()
            .w_full()
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(tint(self.palette.foreground, 0.2))
            .child(self.header())
            .child(self.lines(mono))
    }
}
