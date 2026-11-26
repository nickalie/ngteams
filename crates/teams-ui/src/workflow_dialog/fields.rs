//! The card editor's fields as Teams draws them, by kind: comboboxes (team, channel, time, time zone, a connector's
//! list), a number (the weekly interval, "Enter a number between 1 and 71, with no decimals" when it isn't one), a date,
//! the week's days as seven round toggles, a message box with Teams' formatting bar, a folder picker that browses a
//! connector's folders ("Your folders", the app's name, then the folders browsed into, Outlook's well-known ones
//! first), and a people picker (people as chips; typing searches the directory for names and addresses starting with
//! the text). A connector's choices are read as Teams reads them: the operation's input definition names its dynamic
//! list or folder tree, which the field's own connection answers.
use super::*;
use crate::icons::Spinning as _;
use gpui_component::Sizable as _;
use gpui_component::date_picker::{DatePicker, DatePickerEvent, DatePickerState};
use teams_proto::TeamPerson;

use crate::editor::toolbar::FormatBar;
use crate::editor::{Editor, EditorEvent, EnterMode};

use super::combo::Combo;
use super::markup::{self, Kind};
use super::simple::{Choice, Choices, Field, Link, Simple};

const DAYS: [(&str, &str); 7] = [
    ("Sunday", "S"),
    ("Monday", "M"),
    ("Tuesday", "T"),
    ("Wednesday", "W"),
    ("Thursday", "T"),
    ("Friday", "F"),
    ("Saturday", "S"),
];

impl WorkflowDialog {
    /// The controls of the field `index`, with its value in them.
    pub(super) fn field_controls(simple: &Simple, index: usize, window: &mut Window, cx: &mut Context<Self>) -> Field {
        let mut field = Field::default();
        let Some(param) = simple.params.get(index) else {
            return field;
        };
        let selector = match param.kind {
            Kind::TeamsGroup => "workflow-simple-team".to_owned(),
            Kind::TeamsChannel => "workflow-simple-channel".to_owned(),
            _ => format!("workflow-field-{}", param.id),
        };
        match param.kind {
            Kind::TeamsGroup | Kind::TeamsChannel | Kind::TimeOfDay | Kind::TimeZone | Kind::Choice => {
                let placeholder = match param.kind {
                    Kind::TeamsGroup => "Type or select a team",
                    Kind::TeamsChannel => "Type or select a channel",
                    _ => "",
                };
                field.combo = Some(Combo::new(index, selector, placeholder, window, cx));
            }
            Kind::Interval | Kind::Text | Kind::People => {
                let placeholder = match param.kind {
                    Kind::Interval => "Specify the interval",
                    Kind::People => "Enter part of a name or email address to find people",
                    _ => "",
                };
                let text = match (&param.kind, &param.value) {
                    (Kind::People, _) => String::new(),
                    (_, Value::Number(number)) => number.to_string(),
                    (_, Value::String(text)) => text.clone(),
                    _ => String::new(),
                };
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(text));
                field.subscriptions.push(
                    cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, _, cx| {
                        this.text_event(index, event, cx);
                    }),
                );
                field.input = Some(input);
            }
            Kind::Message => {
                let text = param.text().unwrap_or_default().trim_end().to_owned();
                let editor = cx.new(|cx| {
                    let mut editor = Editor::new(window, cx).placeholder("Add message");
                    editor.set_enter_mode(EnterMode::NewLine, cx);
                    editor.set_heights(Some(px(96.0)), Some(px(240.0)), cx);
                    load_message(&mut editor, &text, cx);
                    editor
                });
                let bar = cx.new(|cx| FormatBar::new(editor.clone(), cx).without_discard());
                field
                    .subscriptions
                    .push(cx.subscribe(&editor, |_, _, event: &EditorEvent, cx| {
                        if matches!(event, EditorEvent::Changed) {
                            cx.notify();
                        }
                    }));
                field.message = Some((editor, bar));
            }
            Kind::StartDate => field = Self::start_date(param, index, field, window, cx),
            Kind::Fixed | Kind::WeekDays | Kind::Folder => {}
        }
        field
    }

    /// The date picker of a start date, on the field's date or today; no day before the earlier of the two.
    fn start_date(
        param: &markup::Param,
        index: usize,
        mut field: Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Field {
        let today = chrono::Local::now().date_naive();
        let picked = param.text().and_then(markup::parse_date).unwrap_or(today);
        let earliest = picked.min(today);
        let date = cx.new(|cx| {
            DatePickerState::new(window, cx)
                .date_format("%-m/%-d/%Y")
                .disabled_matcher(move |day: &jiff::civil::Date| {
                    crate::kit_date::from_kit(*day).is_some_and(|day| day < earliest)
                })
        });
        date.update(cx, |state, cx| {
            let day = gpui_component::calendar::Date::Single(crate::kit_date::to_kit(picked));
            state.set_date(day, window, cx);
        });
        field
            .subscriptions
            .push(cx.subscribe(&date, move |this, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(picked) = event;
                let day = crate::kit_date::picked_day(picked);
                this.date_picked(index, day, cx);
            }));
        field.date = Some(date);
        field
    }

    fn text_event(&mut self, index: usize, event: &InputEvent, cx: &mut Context<Self>) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        let (Some(param), Some(field)) = (simple.params.get_mut(index), simple.fields.get_mut(index)) else {
            return;
        };
        let Some(input) = &field.input else {
            return;
        };
        let text = input.read(cx).value().to_string();
        match (param.kind, event) {
            (Kind::People, InputEvent::Change) => {
                self.search_people(index, &text, cx);
                return;
            }
            (Kind::People, InputEvent::Blur) => field.found = None,
            (Kind::Interval, InputEvent::Change) => {
                param.value = text.trim().parse::<u64>().map_or(Value::String(text), Value::from);
                if markup::interval(&param.value).is_some() {
                    field.problem = None;
                }
            }
            // Teams says what is wrong once the field is left.
            (Kind::Interval, InputEvent::Blur) => {
                field.problem = (markup::interval(&param.value).is_none() && !param.empty())
                    .then_some("Enter a number between 1 and 71, with no decimals");
            }
            (Kind::Text, InputEvent::Change) => {
                param.value = if text.trim().is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                };
            }
            _ => {}
        }
        cx.notify();
    }

    fn date_picked(&mut self, index: usize, date: Option<chrono::NaiveDate>, cx: &mut Context<Self>) {
        if let Some(param) = self.simple.as_mut().and_then(|simple| simple.params.get_mut(index)) {
            if let Some(date) = date {
                let (value, shown) = markup::date_value(date);
                param.value = Value::String(value);
                param.display = Some(shown);
            } else {
                param.value = Value::Null;
                param.display = None;
            }
        }
        cx.notify();
    }

    fn toggle_day(&mut self, index: usize, day: &str, cx: &mut Context<Self>) {
        let Some(param) = self.simple.as_mut().and_then(|simple| simple.params.get_mut(index)) else {
            return;
        };
        let mut days: Vec<String> = param
            .value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if let Some(at) = days.iter().position(|chosen| chosen == day) {
            days.remove(at);
        } else {
            days.push(day.to_owned());
        }
        // In the week's order, as Teams sends them.
        days.sort_by_key(|chosen| DAYS.iter().position(|(name, _)| name == chosen));
        param.value = Value::from(days);
        param.display = Some(String::new());
        cx.notify();
    }

    /// Searches the directory for a people field's text: people whose name or address starts with it, as Teams asks.
    fn search_people(&mut self, index: usize, query: &str, cx: &mut Context<Self>) {
        let Some(field) = self.simple.as_mut().and_then(|simple| simple.fields.get_mut(index)) else {
            return;
        };
        field.asked = field.asked.wrapping_add(1);
        let asked = field.asked;
        let query = query.trim().to_owned();
        if query.is_empty() {
            field.found = None;
            cx.notify();
            return;
        }
        let reply = self.core.addable_people(query.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let people = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if let Some(field) = this.simple.as_mut().and_then(|simple| simple.fields.get_mut(index))
                    && field.asked == asked
                {
                    let people = owners::starting_with(people, &query);
                    field.found = Some((query, people));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add_person(&mut self, index: usize, person: &TeamPerson, window: &mut Window, cx: &mut Context<Self>) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        let (Some(param), Some(field)) = (simple.params.get_mut(index), simple.fields.get_mut(index)) else {
            return;
        };
        let email = person.email.clone().unwrap_or_default();
        let mut people = param.value.as_array().cloned().unwrap_or_default();
        if !people
            .iter()
            .any(|known| known.get("email").and_then(Value::as_str) == Some(email.as_str()))
        {
            people.push(serde_json::json!({
                "email": email,
                "name": person.name,
                "id": teams_proto::object_id(&person.mri).unwrap_or_default(),
                "userPrincipalName": email,
            }));
        }
        param.display = Some(people_names(&people));
        param.value = Value::from(people);
        field.found = None;
        if let Some(input) = &field.input {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn remove_person(&mut self, index: usize, email: &str, cx: &mut Context<Self>) {
        if let Some(param) = self.simple.as_mut().and_then(|simple| simple.params.get_mut(index)) {
            let people: Vec<Value> = param
                .value
                .as_array()
                .into_iter()
                .flatten()
                .filter(|person| person.get("email").and_then(Value::as_str) != Some(email))
                .cloned()
                .collect();
            param.display = Some(people_names(&people));
            param.value = if people.is_empty() {
                Value::Null
            } else {
                Value::from(people)
            };
        }
        cx.notify();
    }

    /// Reads a connector field's choices (a dynamic list, or a folder tree's top or `browse` folder), once its app is
    /// connected and the values its list refers to are chosen.
    pub(super) fn load_choices(&mut self, index: usize, cx: &mut Context<Self>) {
        self.load_choices_in(index, None, cx);
    }

    fn load_choices_in(&mut self, index: usize, browse: Option<Value>, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let Some(simple) = &mut self.simple else {
            return;
        };
        // Whatever this call does, a read started before for the field no longer answers it.
        self.choice_reads += 1;
        let read = self.choice_reads;
        if let Some(field) = simple.fields.get_mut(index) {
            field.listed = read;
        }
        let (api, connection, tree, mut body) = match source(simple, index) {
            Source::Ready {
                api,
                connection,
                tree,
                body,
            } => (api, connection, tree, body),
            Source::Operation(key) => {
                if !simple.operations.contains_key(&key) {
                    simple.operations.insert(key.clone(), None);
                    self.read_operation(environment, key, cx);
                }
                return;
            }
            Source::Waiting => {
                // A value it refers to isn't chosen yet: the field waits, empty, as Teams' Plan field does.
                if let Some(field) = simple.fields.get_mut(index) {
                    field.choices = Choices::Idle;
                }
                return;
            }
            Source::None => return,
        };
        if let Some(selection) = browse {
            body["selectionState"] = selection;
        }
        if let Some(field) = simple.fields.get_mut(index) {
            field.choices = Choices::Loading;
        }
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::Choices {
            environment,
            api,
            connection,
            tree,
            body,
        });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    if let Some(field) = this
                        .simple
                        .as_mut()
                        .and_then(|simple| simple.fields.get_mut(index))
                        .filter(|field| field.listed == read)
                    {
                        field.choices = match answer.and_then(|value| model::rows(&value).map(choices)) {
                            Ok(list) if tree => Choices::Ready(sorted_folders(list)),
                            Ok(list) => Choices::Ready(list),
                            Err(_) => Choices::Failed,
                        };
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    /// Reads a connector operation's input definition, then lists the fields that wait for it.
    fn read_operation(&mut self, environment: String, key: (String, String), cx: &mut Context<Self>) {
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::Operation {
            environment,
            api: key.0.clone(),
            operation: key.1.clone(),
        });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| this.operation_read(generation, key, answer, cx));
            }));
    }

    /// An operation's input definition was read: the fields that wait for it list their choices; if it could not be
    /// read, they say so (not "No options") and opening one asks again.
    fn operation_read(
        &mut self,
        generation: u64,
        key: (String, String),
        answer: WorkflowResponse,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }
        let Some(simple) = &mut self.simple else {
            return;
        };
        // The fields that wait for this definition: those whose source it is, with it unread.
        simple.operations.remove(&key);
        let waiting: Vec<usize> = (0..simple.params.len())
            .filter(|index| matches!(source(simple, *index), Source::Operation(ref wanted) if *wanted == key))
            .collect();
        match answer {
            Ok(value) => {
                let inputs = value.pointer("/properties/inputsDefinition/properties").cloned();
                simple.operations.insert(key, inputs);
                for index in waiting {
                    self.load_choices(index, cx);
                }
            }
            Err(_) => {
                for index in waiting {
                    if let Some(field) = simple.fields.get_mut(index) {
                        field.choices = Choices::Failed;
                    }
                }
            }
        }
        cx.notify();
    }

    fn toggle_folders(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(field) = self.simple.as_mut().and_then(|simple| simple.fields.get_mut(index)) else {
            return;
        };
        field.folder_open = !field.folder_open;
        let reload = field.folder_open && !matches!(field.choices, Choices::Ready(_) | Choices::Loading);
        if reload {
            field.folder_path.clear();
            self.load_choices(index, cx);
        }
        cx.notify();
    }

    fn browse_folder(&mut self, index: usize, to: Option<Choice>, cx: &mut Context<Self>) {
        let Some(field) = self.simple.as_mut().and_then(|simple| simple.fields.get_mut(index)) else {
            return;
        };
        let selection = if let Some(folder) = to {
            let selection = folder.selection.clone();
            field.folder_path.push(folder);
            Some(selection)
        } else {
            field.folder_path.clear();
            None
        };
        self.load_choices_in(index, selection, cx);
    }

    fn pick_folder(&mut self, index: usize, folder: &Choice, cx: &mut Context<Self>) {
        if let Some(simple) = &mut self.simple {
            if let Some(param) = simple.params.get_mut(index) {
                param.value = Value::String(folder.value.clone());
                param.display = Some(folder.full.clone());
            }
            if let Some(field) = simple.fields.get_mut(index) {
                field.folder_open = false;
            }
        }
        cx.notify();
    }

    /// The card editor: the back chevron and the heading (the workflow's name, or "Untitled workflow"), "Collapse
    /// all", and the card.
    pub(super) fn simple_editor(&self, simple: &Simple, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let heading = simple
            .flow
            .as_ref()
            .map_or_else(|| "Untitled workflow".to_owned(), |flow| flow.name.clone());
        div()
            .flex()
            .flex_col()
            .gap_8()
            .when(self.app.is_none(), gpui::Styled::px_12)
            .pt_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .min_w_0()
                            .child(selected_by(
                                "workflow-simple-back",
                                icon_button("workflow-simple-back", Glyph::CHEVRON_LEFT, "Back")
                                    .ghost()
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.leave_simple(cx))),
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(heading),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .when(!simple.message_action, |bar| {
                                bar.child(selected_by(
                                    "workflow-collapse-all",
                                    Button::new("workflow-collapse-all")
                                        .ghost()
                                        .small()
                                        .label(if simple.expanded { "Collapse all" } else { "Expand all" })
                                        .on_click(cx.listener(|this, _, _, cx| this.toggle_card(cx))),
                                ))
                            })
                            // the Workflows app's editor keeps Save at its top right, with no footer
                            .when(self.app.is_some(), |bar| bar.child(self.save_button(cx))),
                    ),
            )
            .child(self.simple_card(simple, palette, cx))
            .into_any_element()
    }

    fn simple_card(&self, simple: &Simple, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let failed = simple.links.iter().find_map(|(_, link)| match link {
            Link::Failed(error) => Some(error),
            _ => None,
        });
        div()
            .id("workflow-simple-card")
            .debug_selector(|| "workflow-simple-card".to_owned())
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(palette.border))
            .hover(|card| card.border_color(rgb(palette.foreground)))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(Self::chips(simple, palette))
                    .when(!simple.message_action, |top| {
                        top.child(selected_by(
                            "workflow-card-toggle",
                            icon_button(
                                "workflow-card-toggle",
                                if simple.expanded {
                                    Glyph::CHEVRON_UP
                                } else {
                                    Glyph::CHEVRON_DOWN
                                },
                                if simple.expanded { "Collapse" } else { "Expand" },
                            )
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_card(cx))),
                        ))
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "workflow-simple-title".to_owned())
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(simple.summary()),
            )
            .when(!simple.message_action, |card| {
                card.child(
                    div()
                        .text_color(rgb(palette.muted_foreground))
                        .child(simple.description.clone()),
                )
            })
            .when(simple.message_action, |card| {
                card.child(super::message::instructions(&simple.template_name(), palette))
            })
            .when(simple.expanded, |card| {
                card.children((0..simple.params.len()).filter_map(|index| self.field_view(simple, index, palette, cx)))
            })
            .when_some(failed, |card, error| {
                card.child(
                    div()
                        .debug_selector(|| "workflow-simple-problem".to_owned())
                        .text_sm()
                        .text_color(rgb(theme::DANGER))
                        .child(format!(
                            "An app this workflow uses could not be connected. {}",
                            model::problem(error)
                        )),
                )
            })
            .into_any_element()
    }

    /// The card's apps, each with its connector icon.
    fn chips(simple: &Simple, palette: Palette) -> gpui::Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(simple.links.iter().map(|(api, _)| {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .text_sm()
                    .bg(rgb(palette.hover))
                    .child(gpui::img(art::connector(api)).size(px(14.0)))
                    .child(super::simple::app_name(api).to_owned())
            }))
    }

    /// One field: its label (a red star when it is required) and its control.
    fn field_view(&self, simple: &Simple, index: usize, palette: Palette, cx: &Context<Self>) -> Option<AnyElement> {
        let param = simple.params.get(index)?;
        let field = simple.fields.get(index)?;
        let control = match param.kind {
            Kind::Fixed => return None,
            Kind::TeamsGroup | Kind::TeamsChannel | Kind::TimeOfDay | Kind::TimeZone | Kind::Choice => {
                self.combo_field(index, palette, cx)
            }
            Kind::Interval | Kind::Text => field.input.as_ref().map(|input| {
                selected_by(format!("workflow-field-{}", param.id), Input::new(input)).into_any_element()
            })?,
            Kind::Message => field
                .message
                .as_ref()
                .map(|(editor, bar)| Self::message_box(&param.id, editor, bar, palette))?,
            // Teams' calendar at the field's right is the kit's own (`AppAssets` serves it)
            Kind::StartDate => field.date.as_ref().map(|date| {
                selected_by(format!("workflow-field-{}", param.id), DatePicker::new(date).w_full()).into_any_element()
            })?,
            Kind::WeekDays => Self::days(index, param, palette, cx),
            Kind::Folder => Self::folder_field(index, param, field, palette, cx),
            Kind::People => Self::people_field(index, param, field, palette, cx),
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(param.label.clone())
                        .when(!param.optional, |label| {
                            label.child(div().text_color(rgb(theme::DANGER)).child("*"))
                        }),
                )
                .child(control)
                .when_some(field.problem, |view, problem| {
                    view.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(rgb(theme::DANGER))
                            .child(Glyph::X.icon().size(px(12.0)))
                            .child(problem),
                    )
                })
                .into_any_element(),
        )
    }

    /// The week's days as Teams draws them: seven round toggles, the chosen ones filled.
    fn days(index: usize, param: &markup::Param, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let chosen: Vec<&str> = param
            .value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        div()
            .flex()
            .gap_1p5()
            .children(DAYS.iter().map(|(day, letter)| {
                let on = chosen.contains(day);
                let selector = format!("workflow-day-{day}");
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .size(px(31.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .cursor_pointer()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .map(|toggle| {
                        if on {
                            toggle
                                .bg(rgb(palette.brand))
                                .border_color(rgb(palette.brand))
                                .text_color(rgb(palette.on_brand))
                        } else {
                            toggle
                                .border_color(rgb(palette.border))
                                .hover(|toggle| toggle.bg(rgb(palette.hover)))
                        }
                    })
                    .tooltip({
                        let day = (*day).to_owned();
                        move |window, cx| gpui_component::tooltip::Tooltip::new(day.clone()).build(window, cx)
                    })
                    .child(*letter)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_day(index, day, cx)))
            }))
            .into_any_element()
    }

    /// A folder field: the chosen folder with a chevron; open, "Your folders", the app's name and the folders browsed
    /// into, then each folder (choose it, or its chevron to look inside).
    fn folder_field(
        index: usize,
        param: &markup::Param,
        field: &Field,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let selector = format!("workflow-field-{}", param.id);
        let shown = param
            .display
            .clone()
            .or_else(|| param.text().map(str::to_owned))
            .unwrap_or_default();
        let face = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .flex()
            .items_center()
            .h(px(28.0))
            .px_3()
            .rounded_md()
            .text_sm()
            .bg(rgb(palette.hover))
            .border_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .child(div().flex_1().min_w_0().truncate().child(shown))
            .child(
                Glyph::CHEVRON_DOWN
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_folders(index, cx)));
        div()
            .relative()
            .child(face)
            .when(field.folder_open, |view| {
                view.child(
                    gpui::deferred(
                        div()
                            .absolute()
                            .top(px(32.0))
                            .left_0()
                            .right_0()
                            .child(Self::folder_list(index, param, field, &selector, (palette, cx))),
                    )
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    fn folder_list(
        index: usize,
        param: &markup::Param,
        field: &Field,
        selector: &str,
        (palette, cx): (Palette, &Context<Self>),
    ) -> AnyElement {
        let root = if param.type_name.starts_with("Outlook") {
            "Outlook"
        } else {
            "SharePoint"
        };
        let at_root = field.folder_path.is_empty();
        let mut crumbs = div().flex().items_center().gap_2().px_3().py_2().child(
            div()
                .id(SharedString::from(format!("{selector}-root")))
                .cursor_pointer()
                .when(at_root, |crumb| crumb.font_weight(gpui::FontWeight::BOLD))
                .child(root)
                .on_click(cx.listener(move |this, _, _, cx| this.browse_folder(index, None, cx))),
        );
        for (at, folder) in field.folder_path.iter().enumerate() {
            crumbs = crumbs.child(Glyph::CHEVRON_RIGHT.icon().size(px(14.0))).child(
                div()
                    .when(at + 1 == field.folder_path.len(), |crumb| {
                        crumb.font_weight(gpui::FontWeight::BOLD)
                    })
                    .child(folder.name.clone()),
            );
        }
        if at_root {
            crumbs = crumbs.child(Glyph::CHEVRON_RIGHT.icon().size(px(14.0)));
        }
        let mut list = div()
            .id(SharedString::from(format!("{selector}-folders")))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .py_1()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .occlude()
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .child("Your folders"),
            )
            .child(crumbs);
        match &field.choices {
            Choices::Ready(folders) if !folders.is_empty() => {
                list = list.children(
                    folders
                        .iter()
                        .map(|folder| Self::folder_row(index, folder, selector, palette, cx)),
                );
            }
            Choices::Loading => list = list.child(div().px_3().py_2().child("Loading…")),
            Choices::Failed => list = list.child(div().px_3().py_2().child("The folders could not be loaded.")),
            _ => list = list.child(div().px_3().py_2().child("No options available")),
        }
        list.into_any_element()
    }

    /// One folder: choose it, or its chevron (when it holds folders) to look inside.
    fn folder_row(
        index: usize,
        folder: &Choice,
        selector: &str,
        palette: Palette,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let row = format!("{selector}-{}", folder.name);
        let (picked, opened) = (folder.clone(), folder.clone());
        let open = cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.browse_folder(index, Some(opened.clone()), cx);
        });
        div()
            .id(SharedString::from(row.clone()))
            .debug_selector(move || row.clone())
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .cursor_pointer()
            .hover(|row| row.bg(rgb(palette.hover)))
            .child(Glyph::FOLDER.icon().size(px(14.0)))
            .child(div().flex_1().min_w_0().truncate().child(folder.name.clone()))
            .on_click(cx.listener(move |this, _, _, cx| this.pick_folder(index, &picked, cx)))
            .when(folder.parent, |row| {
                row.child(
                    div()
                        .id(SharedString::from(format!("{selector}-open-{}", folder.name)))
                        .px_2()
                        .border_l_1()
                        .border_color(rgb(palette.border))
                        .child(Glyph::CHEVRON_RIGHT.icon().size(px(14.0)))
                        .on_click(open),
                )
            })
    }

    /// A people field: the chosen people as chips in the box with the search; who the search found below it.
    fn people_field(
        index: usize,
        param: &markup::Param,
        field: &Field,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(input) = &field.input else {
            return div().into_any_element();
        };
        let chips = param
            .value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|person| {
                let name = person
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let email = person
                    .get("email")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let selector = format!("workflow-person-{email}");
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(palette.hover))
                    .text_sm()
                    .child(super::owners::initial(&name, palette).size(px(18.0)).text_xs())
                    .child(name)
                    .child(selected_by(
                        selector.clone(),
                        icon_button(SharedString::from(selector), Glyph::X, "Remove")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_person(index, &email, cx))),
                    ))
            });
        let selector = format!("workflow-field-{}", param.id);
        div()
            .relative()
            .child(div().flex().flex_wrap().items_center().gap_1().children(chips).child(
                div().flex_1().min_w(px(240.0)).child(selected_by(
                    selector,
                    Input::new(input).prefix(Glyph::USER.icon().size(px(14.0))),
                )),
            ))
            .when_some(field.found.as_ref(), |view, (query, people)| {
                view.child(
                    gpui::deferred(
                        div()
                            .absolute()
                            .top(px(38.0))
                            .left_0()
                            .right_0()
                            .child(Self::people_list(index, query, people, palette, cx)),
                    )
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    fn people_list(
        index: usize,
        query: &str,
        people: &[TeamPerson],
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let list = div()
            .p_1()
            .flex()
            .flex_col()
            .rounded_md()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .shadow_lg()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            });
        if people.is_empty() {
            return list
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .child(format!("{query} is not in your organization's directory.")),
                )
                .into_any_element();
        }
        list.children(people.iter().take(10).map(|person| {
            let selector = format!("workflow-person-pick-{}", person.mri);
            let chosen = person.clone();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(rgb(palette.hover)))
                .child(super::owners::initial(&person.name, palette))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().truncate().child(person.name.clone()))
                        .children(person.email.clone().map(|email| {
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted_foreground))
                                .truncate()
                                .child(email)
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.add_person(index, &chosen, window, cx)))
        }))
        .into_any_element()
    }

    /// The card editor's footer: Save and Back, as in Teams. Save stays off until every required field is filled.
    pub(super) fn simple_footer(&self, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap_2()
            .child(self.save_button(cx))
            .child(selected_by(
                "workflow-edit-cancel",
                Button::new("workflow-edit-cancel")
                    .outline()
                    .min_w(px(96.0))
                    .label("Back")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.leave_simple(cx))),
            ))
            .into_any_element()
    }

    /// The card editor's Save: off until every required field is filled.
    fn save_button(&self, cx: &Context<Self>) -> AnyElement {
        let message_empty = self.simple.as_ref().is_some_and(|simple| {
            simple.params.iter().zip(&simple.fields).any(|(param, field)| {
                !param.optional
                    && field
                        .message
                        .as_ref()
                        .is_some_and(|(editor, _)| editor.read(cx).is_blank())
            })
        });
        let unready = self
            .simple
            .as_ref()
            .is_none_or(|simple| simple.connecting() || !message_only(simple));
        selected_by(
            "workflow-save",
            Button::new("workflow-save")
                .primary()
                .min_w(px(96.0))
                .label(if self.busy { "Saving" } else { "Save" })
                .spinning(self.busy)
                .disabled(self.busy || unready || message_empty || self.uncertain.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.simple_save(cx))),
        )
        .into_any_element()
    }
}

impl WorkflowDialog {
    /// The message box as Teams' cards draw it: the formatting bar over the text, in one bordered box.
    fn message_box(id: &str, editor: &Entity<Editor>, bar: &Entity<FormatBar>, palette: Palette) -> AnyElement {
        let selector = format!("workflow-field-{id}");
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .child(
                div()
                    .px_1()
                    .py_0p5()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(bar.clone()),
            )
            .child(div().px_3().py_2().min_h(px(110.0)).child(editor.clone()))
            .into_any_element()
    }
}

/// Puts a kept message in the box: Teams' HTML with its formatting, or plain text.
fn load_message(editor: &mut Editor, text: &str, cx: &mut Context<Editor>) {
    if text.is_empty() {
        return;
    }
    if looks_like_html(text) {
        let rich = teams_proto::chat::parse_html(text);
        if editor
            .load(&rich.nodes, &std::collections::HashMap::<String, String>::new(), cx)
            .is_ok()
        {
            return;
        }
    }
    editor.insert(text, cx);
}

/// The tags a formatted message's HTML is made of: a kept message holding one is HTML; any other `<` (a plain
/// "a < b", "<3") is text.
const MESSAGE_TAGS: [&str; 30] = [
    "p",
    "br",
    "b",
    "i",
    "u",
    "s",
    "strong",
    "em",
    "a",
    "span",
    "div",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "code",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "img",
    "table",
    "tr",
    "td",
    "th",
    "hr",
    "del",
];

/// Whether a kept message is HTML: it holds one of a formatted message's tags (`<p>`, `</strong>`, `<br/>`,
/// `<a href=…>`), not merely a `<`.
pub(super) fn looks_like_html(text: &str) -> bool {
    text.match_indices('<').any(|(at, _)| {
        let rest = &text[at + 1..];
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
        let after = rest[name.len()..].chars().next();
        MESSAGE_TAGS.contains(&name.to_ascii_lowercase().as_str())
            && matches!(after, Some('>' | '/' | ' ' | '\t' | '\n'))
    })
}

/// What a message box sends: its plain text, as Teams sends an unformatted message, or the HTML of a formatted one.
pub(super) fn message_value(editor: &Editor) -> Value {
    let plain = editor.text();
    if plain.trim().is_empty() {
        return Value::Null;
    }
    match crate::compose::message_of(editor.doc(), &[]) {
        Ok(Some(message)) if formatted(message.html()) => Value::String(message.html().to_owned()),
        _ => Value::String(plain.trim_end().to_owned()),
    }
}

/// Whether a message's HTML holds more than paragraphs and line breaks.
fn formatted(html: &str) -> bool {
    ["<p>", "</p>", "<br>", "<br/>", "<br />"]
        .iter()
        .fold(html.to_owned(), |text, tag| text.replace(tag, ""))
        .contains('<')
}

/// Outlook's well-known folders, in the order Teams lists them before the others.
const WELL_KNOWN_FOLDERS: [&str; 6] = ["Deleted Items", "Sent Items", "Outbox", "Inbox", "Drafts", "Junk Email"];

/// A folder list as Teams shows it: Outlook's well-known folders first, in Teams' order, then the rest by name,
/// ignoring case (SharePoint's `_catalogs` before `FormServerTemplates` before `images`).
pub(super) fn sorted_folders(mut folders: Vec<Choice>) -> Vec<Choice> {
    folders.sort_by(|a, b| {
        let rank = |folder: &Choice| {
            WELL_KNOWN_FOLDERS
                .iter()
                .position(|name| *name == folder.name)
                .unwrap_or(WELL_KNOWN_FOLDERS.len())
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    folders
}

/// Whether the only fields left empty are message boxes, whose text the box holds until Save reads it.
fn message_only(simple: &Simple) -> bool {
    simple.params.iter().all(|param| match param.kind {
        Kind::Fixed | Kind::Message => true,
        Kind::Interval => markup::interval(&param.value).is_some(),
        _ => param.optional || !param.empty(),
    })
}

/// Where a connector field's choices come from, as far as is known yet.
enum Source {
    /// Not a connector field, or its app isn't connected yet.
    None,
    /// The operation's input definition is still to read.
    Operation((String, String)),
    /// A value its list refers to isn't chosen yet.
    Waiting,
    Ready {
        api: String,
        connection: String,
        tree: bool,
        body: Value,
    },
}

/// The connector request that lists a field's choices: its binding's action names the app and operation, whose input
/// definition names the dynamic list or folder tree.
fn source(simple: &Simple, index: usize) -> Source {
    let Some(param) = simple
        .params
        .get(index)
        .filter(|param| matches!(param.kind, Kind::Choice | Kind::Folder))
    else {
        return Source::None;
    };
    let Some(binding) = simple.bindings().iter().find(|binding| binding.param == param.id) else {
        return Source::None;
    };
    let host = markup::node(simple.definition(), &binding.node).and_then(|node| node.pointer("/inputs/host"));
    let api = host
        .and_then(|host| host.get("apiId"))
        .and_then(Value::as_str)
        .and_then(|id| id.rsplit('/').next());
    let operation = host.and_then(|host| host.get("operationId")).and_then(Value::as_str);
    let (Some(api), Some(operation)) = (api, operation) else {
        return Source::None;
    };
    let Some(connection) = simple.connection(api) else {
        return Source::None;
    };
    let Some(inputs) = simple.operations.get(&(api.to_owned(), operation.to_owned())) else {
        return Source::Operation((api.to_owned(), operation.to_owned()));
    };
    let Some(input) = binding
        .path
        .last()
        .and_then(|name| input_definition(inputs.as_ref()?, name))
    else {
        return Source::None;
    };
    let tree = input.get("x-ms-dynamic-tree").is_some();
    let Some(invocation) = input
        .get("x-ms-dynamic-tree")
        .or_else(|| input.get("x-ms-dynamic-list"))
    else {
        return Source::None;
    };
    match invocation_parameters(simple, &binding.node, invocation, tree) {
        Some(parameters) => Source::Ready {
            api: api.to_owned(),
            connection: connection.to_owned(),
            tree,
            body: serde_json::json!({"parameters": parameters, "dynamicInvocationDefinition": invocation}),
        },
        None => Source::Waiting,
    }
}

/// An operation input's definition by the name a binding fills: a top-level one (`groupId`), or one inside the body
/// (`body/groupId`, under `body.properties`), as Planner's Create a task names its group and plan.
pub(super) fn input_definition<'a>(inputs: &'a Value, name: &str) -> Option<&'a Value> {
    inputs.get(name).or_else(|| {
        let mut parts = name.split('/');
        let first = inputs.get(parts.next()?)?;
        parts.try_fold(first, |node, part| node.get("properties")?.get(part))
    })
}

/// The values a dynamic list or tree refers to, from the fields bound to the same action's inputs; `None` while a
/// required one isn't chosen.
fn invocation_parameters(simple: &Simple, node: &str, invocation: &Value, tree: bool) -> Option<Value> {
    let declared = if tree {
        invocation.pointer("/open/parameters")
    } else {
        invocation.get("parameters")
    };
    let mut values = serde_json::Map::new();
    for (name, spec) in declared.and_then(Value::as_object).into_iter().flatten() {
        if let Some(reference) = spec.get("parameterReference").and_then(Value::as_str) {
            let value = simple
                .bindings()
                .iter()
                .find(|binding| binding.node == node && binding.path.last().map(String::as_str) == Some(reference))
                .and_then(|binding| simple.params.iter().find(|param| param.id == binding.param))
                .filter(|param| !param.empty())
                .map(|param| param.value.clone());
            match value {
                Some(value) => {
                    values.insert(name.clone(), value);
                }
                None if spec.get("required").and_then(Value::as_bool) == Some(true) => return None,
                None => {}
            }
        }
        // A literal `value` stays in the invocation definition itself: Teams sends only the referred values.
    }
    Some(Value::Object(values))
}

/// The choices a connector answered: a list's (`value`, `displayName`) or a tree's folders.
fn choices(rows: &[Value]) -> Vec<Choice> {
    rows.iter()
        .filter_map(|row| {
            let value = match row.get("value")? {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let name = row
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(&value)
                .to_owned();
            Some(Choice {
                full: row
                    .get("fullyQualifiedDisplayName")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_owned(),
                parent: row.get("isParent").and_then(Value::as_bool) == Some(true),
                selection: row.get("selectionState").cloned().unwrap_or(Value::Null),
                value,
                name,
            })
        })
        .collect()
}

fn people_names(people: &[Value]) -> String {
    people
        .iter()
        .filter_map(|person| person.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> Choice {
        Choice {
            value: name.to_owned(),
            name: name.to_owned(),
            full: name.to_owned(),
            parent: true,
            selection: Value::Null,
        }
    }

    fn names(list: Vec<Choice>) -> Vec<String> {
        list.into_iter().map(|choice| choice.name).collect()
    }

    #[test]
    fn folders_list_outlooks_well_known_ones_first_then_by_name_ignoring_case() {
        let outlook = [
            "Archive",
            "Conversation History",
            "Deleted Items",
            "Drafts",
            "Inbox",
            "Junk Email",
            "Outbox",
            "Sent Items",
        ];
        assert_eq!(
            names(sorted_folders(outlook.iter().map(|name| folder(name)).collect())),
            [
                "Deleted Items",
                "Sent Items",
                "Outbox",
                "Inbox",
                "Drafts",
                "Junk Email",
                "Archive",
                "Conversation History"
            ]
        );
        let sharepoint = [
            "images",
            "_vti_pvt",
            "Shared Documents",
            "_cts",
            "FormServerTemplates",
            "_catalogs",
        ];
        assert_eq!(
            names(sorted_folders(sharepoint.iter().map(|name| folder(name)).collect())),
            [
                "_catalogs",
                "_cts",
                "_vti_pvt",
                "FormServerTemplates",
                "images",
                "Shared Documents"
            ]
        );
    }

    #[test]
    fn a_message_is_sent_plain_unless_formatted() {
        assert!(!formatted("<p>Weekly reminder</p>"));
        assert!(!formatted("<p>One</p><p>Two<br>three</p>"));
        assert!(formatted("<p><strong>Weekly</strong> reminder</p>"));
    }
}
