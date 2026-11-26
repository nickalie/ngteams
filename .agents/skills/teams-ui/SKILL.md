---
name: teams-ui
description: Build or change ngteams' native Teams UI (Rust, GPUI + gpui-component) in Claude Code or Codex, matching Microsoft Teams and checking sandbox screenshots. Use for any change to what the app shows or how it is used, including views, controls, settings, calls, themes, layouts, UI bugs, and features with implied UI such as pinned chats or reactions.
---

# Teams UI

The app is meant to replace Microsoft Teams for daily use, so a view is done when it looks and behaves like the real
client, not when it merely works. The user checks the personal build by hand, so every visible slip costs them a
rebuild and a restart. This skill applies in both Claude Code and Codex.

Work from the repository root. Read `AGENTS.md` explicitly if the host has not already loaded it; its project rules,
checks and design invariants apply to every UI change, alongside more specific directory instructions and user
instructions.
This skill adds the UI side. The canonical copy is `.agents/skills/teams-ui/SKILL.md`; Claude discovers the same
skill through the `.claude/skills/teams-ui` symlink. Keep updates in the shared copy.

## Before you build

- **Look at it in browser Teams first, never invent it.** Before building a feature or a screen, open it in the web
  client with a lab account: `proto-lab run team-manage --headless` under the exclusive live lock and an outer timeout,
  driven over `PROTO_LAB_STEPS_SOCKET` (`shot <name>.png` after each step; `xy <x> <y>` and `wheel <x> <y> <dy>` reach
  cross-origin frames such as Workflows), after the lab-contact check. Screenshot every step: entry, each state,
  menus opened, validation and empty states, and what the final action leads to. Keep the shots out of the repo
  (they hold tenant data), build from them, and compare them side by side with the sandbox shots before handing over.
- **Find the Teams reference.** When the user shows a screenshot or says "like Teams", list every behavior it shows
  (layout, sections, scrolling, hover, empty and fallback states) and build all of it. A feature real Teams has and
  the app lacks is a gap to fix, not "by design". A control drawn disabled, a "not available yet" note or a "Not done"
  list doesn't finish the job. If a part needs a capture, a read or a lab run
  nobody has made yet, make it. Leave a part out only when it truly can't be built now, and say what blocks it.
- **Look at the current state first**: run the UI tour (below) and read the shots of the surfaces you will touch.
- **Reuse what exists** before writing new widgets: `crates/teams-ui/src/icons.rs` (icons, icon buttons),
  `settings_view.rs` (`group`, `row`, `segmented`, `picker`, `theme_tile`, `caption`), `shell.rs` (`menu_row`,
  `pane_header`, `no_chat`), `call_view.rs` (`control`: solid red/green call buttons). `../ngterm/crates/ngterm-gpui/src`
  (the user's GPUI terminal) is a reference for clean GPUI patterns, e.g. `settings/draw.rs`.

## Guidelines

### Controls

- **An action in a toolbar, header, bar or hover strip is an icon button with a tooltip**, not a text button:
  `icon_button(id, Glyph::X, "Tip")`, or `icon_button_with_key(id, glyph, "Tip", &Action)` when a key is bound, so
  the tooltip shows the key. The tooltip names the action in plain words ("Turn camera off", "Cancel reply"); it is
  also the button's accessible name.
- **A word stays** where the word is the point: a dialog's confirm/cancel, the one primary action of a place (Teams'
  "Join", "Leave", "Answer", "Decline" carry an icon *and* a word), "Send again", "Copy diagnostics".
- **A toggle shows its state**: swap the glyph (mic/mic-off) and mark it `.selected(true)` while it is in the "off"
  state Teams highlights (muted, camera off).
- **Destructive and call actions use Teams' colours**: `theme::DANGER` (leave, decline, delete, unread badge),
  `theme::SUCCESS` (answer). The kit's `.danger()`, `.success()` and `.custom(..)` button variants are drawn only
  tinted (a custom colour is mixed 80% toward transparent), so a solid red/green button is our own `div` — see
  `call_view.rs`'s `control`. A quiet destructive icon (delete in a hover bar) is a ghost button with a red foreground.
- **The read-only mode** (`Core::read_only`): a new control that writes (sends, reacts, calls, sets a status, adds or
  removes an account) offers nothing there, the way the compose box shows `READ_ONLY_APP` and a message offers only
  Copy (`ChatWrites::AppReadOnly`); the engine refuses it anyway, but the view shouldn't offer it.
- **Icons are Lucide SVGs** (ISC, `crates/teams-ui/assets/icons/LICENSE-LUCIDE`). Copy a new one from
  `~/.cargo/registry/src/*/gpui-kit-assets-*/assets/icons/<name>.svg` into `crates/teams-ui/assets/icons/` and add it
  as a `Glyph` constant rather than drawing one ad hoc. Sizes: 14 px in menus and small buttons, 16–18 px in toolbars
  and nav, 22 px on the rail.

### Structure

- **Settings-like content is pages of titled boxes of rows**: a box (`group`) is a rounded, bordered surface with a
  hairline between rows; a row (`row`) has its name and a one-line description of what it does on the left and the
  control on the right. Group related options; a page per area, listed in the settings' own nav (the settings take the
  list pane's place, as in Teams).
- **Pick the control by the choice**: a switch for on/off; a segmented control (`segmented`) for 2–4 short options; a
  dropdown (`picker`) for longer lists or long labels (statuses, durations, devices); preview tiles for visual choices
  (themes); a stepper with −/+/reset icons for a number. Avoid walls of radio buttons.
- **Menus** (account menu, pickers) are rows of icon/mark + label + a check on the chosen one, on a raised surface with
  a shadow — `menu_row` in `shell.rs`.
- **Headers line up**: the list header and the conversation header are both 48 px with a bottom border; a chat header
  shows the chat's avatar, its name (semibold, ellipsized), presence, then actions on the right.
- **Empty and error states give direction**: an empty pane shows an icon in a tinted circle, a short title and what to
  do (with the key as a `Kbd`). A status that is fine says nothing (the connection appears only when it isn't up);
  a problem says what is wrong and how to fix it, in a warning box (`problems` in `settings_view.rs`).

### Reach

- **Everything the pointer reaches, the keyboard reaches.** A new list of pages, tabs or rows gets a focus handle and
  Up/Down/Home/End (the settings' page column: `SettingsView::page_key`), focused when it opens; a new action gets a
  key or a Ctrl+K command where it fits.
- **An element shown only while its row is hovered keeps itself shown and takes its own clicks.** GPUI's hover is the
  row's own hitbox: a hover bar that reaches past its row (Teams' message bar floats over the header or the message
  above) vanishes as the pointer leaves the row unless the bar keeps the hover itself, and a click on it falls through
  to the row underneath unless it blocks the mouse. `chat_view.rs`'s hover bar does both; a test moves the pointer to
  the bar's top edge, over the message above, and clicks there.
- **A dropdown or popup scrolls** (`menu.scrollable(true).max_h(..)`): a long device list or a short window must still
  reach every row.

### Look

- **Colours come from the palette** (`theme::current(cx)`: `foreground`, `muted_foreground`, `surface`, `background`,
  `border`, `brand`, `hover`, `selected`, `on_brand`, `rail`) so every theme (Teams Light/Dark, High Contrast,
  Solarized, Nord, Dracula) works; the only fixed colours are `theme::DANGER*`/`SUCCESS*`. Check a change in a dark
  theme as well as Teams Light.
- **Type**: titles semibold (page title ~22 px, pane titles `text_lg`), body default, descriptions and captions
  `text_sm`/`text_xs` in `muted_foreground`. Sentence case everywhere.
- **Spacing and shape**: GPUI's scale (`gap_2/3`, `px_3/4`, `py_2p5`); boxes and menus `rounded_lg`, controls
  `rounded_md`, avatars and dots `rounded_full`. One border colour (`palette.border`).

### Words

Write from the user's side, plainly: "Report my availability", "Stop reporting me available after", "Turn camera on" —
not the internal names (fence, outbox, endpoint, trace). Active verbs, and one name for an action everywhere it is
offered: the call bar's Leave (tooltip "Leave call"), the Ctrl+K command "Leave call" and the README all say leave, not
"hang up" in one place and "leave" in another. When you rename an action, grep the switcher (`switcher.rs`), the
notifications, the README and the tests for the old name. Errors say what happened and what to do, without apologising.

## Kit pitfalls (gpui-component 0.11)

- `Button` takes no `debug_selector`: wrap it with `selected_by(id, button)` so tests can still find it.
- The kit draws some icons by path (`IconName`: carets, submenu chevrons, the date field's calendar, spinners);
  `teams_ui::AppAssets` serves them, so every GPUI start (the app's, a sandbox test's) runs
  `.with_assets(teams_ui::AppAssets)`, or those icons are blank.
- A loading `Button` shows its spinner only in its icon's place: use `.spinning(busy)` (`icons::Spinning`), not
  `.loading(busy)`, on a button with a label alone.
- `.dropdown_caret(true)` doesn't show when the button's content is a custom child: add a `Glyph::CHEVRON_DOWN` to the
  face yourself (see `picker`).
- A dropdown's rows are `PopupMenuItem::element(..)` whose element carries the row's `debug_selector`, so a test can
  click the picker, then the row.
- `Tooltip::new(text).action(&action, None)` shows a key on a plain `div`'s `.tooltip(..)` (needs
  `StatefulInteractiveElement`, so the div needs an `.id`).
- Don't use the kit's Markdown/HTML views for message text (it is drawn only from sanitized `RichText`, AGENTS.md's
  design invariants), and don't call `ThemeRegistry::watch_dir`.

## Keep tests and the trace working

- **Keep every existing `debug_selector`** (tests click and measure by them: `call-mute`, `status-menu-busy`,
  `idle-15`, `microphone-usb-mic`, …). When a control moves into a page or a dropdown, update the test to navigate
  there first (`settings-page-<key>`, then the picker's selector, then the row's) rather than renaming the selector.
- Give every new control a stable selector of the same shape (`<group>-<key>`).
- The view trace records view and change names and service ids only, never displayed text.

## Check it rendered

A change to what the app shows is checked by looking at it. Pick the shots that cover the surfaces you touched; the
rest of the checks follow AGENTS.md's "Checks".

1. `GPUI_SMOKE_REQUIRE=1 cargo test -p ngteams --test ui_tour` (run in the background; it builds and drives the app
   on the mock in the UI sandbox) writes `target/ui-shots/tour-*.png`: no chat, a chat with a message's actions (shown
   by selecting it, so hover itself isn't checked: that is the view tests' job), a reply, the account menu, every
   settings page (Calls with made-up devices), appearance and chat in Teams Dark. A new surface gets a step in
   `STEPS` in `crates/ngteams/tests/ui_tour.rs`.
2. Calls: `cargo test -p ngteams --test call_window` writes `ngteams-call-*.png` (incoming, connected, video).
3. **Read the screenshots** using Claude Code's `Read` tool or Codex's `view_image` tool with the local image's
   absolute path, and fix what looks off — alignment, clipped text, a control
   that looks disabled, a colour that vanished in dark mode. Say plainly what a still shot can't show (hover, open
   menus, animation) and that you haven't seen it.
4. **The user's real chats** (a bug only real data shows, a layout with real names and long chats): run the installed
   personal build read-only with `scripts/run-personal-read-only.sh <upn>` (`--shot <name>`, `--key <spec>`,
   `--type <text>`, `--click <x> <y>` drive it, `--stop` ends it and deletes the snapshot). It shows what the
   *installed* build does, not your change. The user's chats are private: describe what you saw, never paste names or
   message text.

Render functions grow fast: split one over clippy's line limit into helpers (a `controls`, a `no_chat`) rather than
excusing it. When reporting, list the surfaces changed and which shots you looked at.
