<div align="center">

<img src="packaging/icons/hicolor/128x128/apps/com.nickalie.ngteams.png" width="96" height="96" alt="ngteams icon">

# ngteams

**A native Microsoft Teams app for Linux and Windows, with a macOS preview.**

Chats, channels, calendar, calls and meetings for your work account,
built for your desktop instead of a web page in a wrapper.

[Install](#install) · [Features](#what-you-can-do) · [Shortcuts](#keyboard-shortcuts) · [Privacy](#your-data) · [Good to know](#good-to-know)

</div>

![The ngteams chat view in the dark theme](docs/images/chat-dark.png)

## Why ngteams

- **It's a real desktop app.** Native windows, desktop notifications, a tray icon with an unread badge, and `msteams:` links from your browser open right in the app.
- **Nothing to relearn.** The layout follows Teams, so chats, channels and call controls are where you expect them.
- **All your accounts, one window.** Sign in to several work accounts and switch between them with a click. Every account gets its notifications and calls.
- **Made for the keyboard.** Jump to any chat, search, reply, react and control a call without touching the mouse.
- **Scriptable.** `ngteamsctl` lets your scripts read chats, send messages and react to what happens in Teams.

## What you can do

**Chat**
- One-on-one and group chats, with formatting, emoji, @mentions, replies, reactions, edits and deletes
- Drop in files, or paste screenshots and pictures straight into a message
- Search messages and people, and keep unread counts in sync with your other devices
- Click or hover anyone's picture or name for their profile card, as in Teams: their availability, work hours and local time, contact details, who they work with and their org chart, with Chat, a quick message, and video and audio calls right there
- Drafts stay put while you move around

**Teams and channels**
- Channel posts and threads, favorites, notification preferences and channel management
- Cards and Workflows, as they appear in Teams

**Sounds**
- A sound, a volume and an off switch for each of 23 events, far beyond Teams' three dropdowns: messages, mentions, channel posts, missed calls, ringing, people joining and leaving, recording, screen sharing, raised hands and more
- Built-in sound packs, your own packs from a folder or a `.zip`, or any WAV, OGG or MP3 of yours for a single sound ([how to make a pack](docs/sound-packs.md))

**Calls and meetings**
- One-on-one and group audio and video calls, and scheduled or instant meetings
- Screen sharing, background blur and virtual backgrounds
- Gallery and speaker views, raise hand, reactions, hold, transfer and inviting people mid-call
- Recording and transcription, live captions, and meeting recaps with recordings and transcripts

**Calendar**
- Day, work week and month views
- Create and edit meetings, including repeating ones
- Join from the calendar through the pre-join screen, or with a meeting ID

**Optional extras on your computer**
- Read and write a chat in another language, with translated messages kept for next time
- Have a draft corrected before you send it, in any language: spelling, grammar and the word that fits best, previewed word by word
- Translate the message you are writing into any language, in place, keeping its mentions, links and formatting (Ctrl+Z brings it back)
- Translate live captions as people speak
- Keep every call's captions as a searchable transcript, saved only on this computer
- Get an AI summary of a meeting: purpose, key takeaways, topics, next steps and open questions

Translation, corrections and summaries use your own [Claude Code](https://claude.com/product/claude-code) or Codex CLI, and stay off until you turn them on.

**Make it yours**
- Teams Light and Dark, High Contrast, Solarized Light and Dark, Nord and Dracula themes

<table>
  <tr>
    <td width="50%"><a href="docs/images/channel-thread.png"><img src="docs/images/channel-thread.png" alt="A channel with a thread open beside it"></a></td>
    <td width="50%"><a href="docs/images/calendar.png"><img src="docs/images/calendar.png" alt="The calendar's month view"></a></td>
  </tr>
  <tr>
    <td align="center">Channels and threads</td>
    <td align="center">Calendar</td>
  </tr>
  <tr>
    <td width="50%"><a href="docs/images/call.png"><img src="docs/images/call.png" alt="A call window beside the main window"></a></td>
    <td width="50%"><a href="docs/images/theme-nord.png"><img src="docs/images/theme-nord.png" alt="The chat view in the Nord theme"></a></td>
  </tr>
  <tr>
    <td align="center">Keep working during a call</td>
    <td align="center">Themes</td>
  </tr>
</table>

<sub>Screenshots show made-up demo data.</sub>

## Install

**You'll need:**

- A Linux desktop running on **Wayland**, or **Windows 11 x64**
- A Microsoft 365 **work or school** account with Teams (personal Microsoft accounts aren't supported)
- On Linux, a desktop keyring, such as GNOME Keyring, to hold your sign-in
- On Linux, PipeWire for calls, which most current desktops already use

Every release is on the [releases page](https://github.com/nickalie/ngteams/releases/latest). Linux x86_64 and Windows x64.

| On | Download | Install with |
|---|---|---|
| Windows 11 x64 | `ngteams-*-windows-x86_64-setup.exe` | run it (see below) |
| | `ngteams-*-windows-x86_64.zip` | extract, then run `Install.ps1` in PowerShell |
| Debian 13+, Ubuntu 24.04+ | `ngteams_*_amd64.deb` | `sudo apt install ./ngteams_*_amd64.deb` |
| Fedora, RHEL-likes | `ngteams-*.x86_64.rpm` | `sudo dnf install ./ngteams-*.x86_64.rpm` |
| openSUSE | `ngteams-*.x86_64.rpm` | `sudo zypper install --allow-unsigned-rpm ./ngteams-*.x86_64.rpm` |
| Arch Linux | `ngteams-*-x86_64.pkg.tar.zst` | `sudo pacman -U ./ngteams-*-x86_64.pkg.tar.zst` |
| Anything with glibc 2.39 or newer | `ngteams-*-linux-x86_64.AppImage` | `chmod +x` it and run it |
| | `ngteams-*-linux-x86_64.tar.gz` | unpack it and run `ngteams/AppRun` |

**Updates**: ngteams checks for a newer release once an hour. When there is one, **Install update** appears in its title bar: select it and ngteams downloads the release, checks it against the release's `SHA256SUMS`, installs it and restarts. A `.deb`, `.rpm` or Arch package asks for your password first; on Windows, ngteams closes and the new installer upgrades it and opens it again. If a call is going on or you have a message you haven't sent, the restart waits until you select **Restart to update**. To update by hand, install the newer package the same way. The unpacked `.tar.gz` (or an extracted AppImage) updates itself only while it stays in its own folder named `ngteams`; the Windows zip doesn't update itself.

**On Windows**, run `ngteams-*-windows-x86_64-setup.exe` as your ordinary user: it needs no administrator rights.
It installs ngteams for you alone under `%LOCALAPPDATA%\Programs\ngteams`, adds it to Start and to Settings → Apps →
Installed apps, and installs Microsoft's WebView2 Runtime if it is missing (downloaded from Microsoft and checked to
be signed by Microsoft). To open Teams links here, choose ngteams for MSTeams in Settings → Apps → Default apps;
setup offers it there but never changes your current choice. Running a newer installer upgrades ngteams in place.
Uninstalling it from Installed apps removes the program and keeps your accounts and data
(`%LOCALAPPDATA%\ngteams`, and the sign-ins in Credential Manager).

The installer and the app aren't code-signed yet, so **Windows SmartScreen** may say it "protected your PC" from an
unrecognized app: check the download against the release's `SHA256SUMS` (or `gh attestation verify <file> --repo
nickalie/ngteams`), then choose **More info → Run anyway**.

Without the installer, extract the zip, open PowerShell in its `ngteams` folder and run
`powershell -NoProfile -ExecutionPolicy Bypass -File .\Install.ps1` as your ordinary user. The archive's
`README.txt` covers portable use, verification and removal.

**On Linux, the sign-in window** runs on WPE WebKit 2.54 or newer. If your system has it (the Arch package installs it, Debian testing and openSUSE Tumbleweed have it), ngteams uses it. Otherwise it downloads a verified copy of about 120 MB once, the first time you sign in.

**On Ubuntu 24.04 and later, prefer the `.deb`.** Those systems restrict the sandbox the sign-in window needs, and the `.deb` installs the AppArmor profile that allows it. To run an AppImage there, extract it to a permanent place and install the profile it carries:

```sh
mkdir -p ~/.local/opt
./ngteams-*-linux-x86_64.AppImage --appimage-extract && mv squashfs-root ~/.local/opt/ngteams
sed "s|@NGTEAMS_BIN@|$HOME/.local/opt/ngteams/usr/bin/ngteams|" ~/.local/opt/ngteams/usr/share/doc/ngteams/apparmor.profile.in \
    | sudo tee /etc/apparmor.d/ngteams > /dev/null
sudo apparmor_parser -r /etc/apparmor.d/ngteams
~/.local/opt/ngteams/AppRun
```

For the tarball, do the same with its `ngteams` folder.

**From source** on Arch, the system it's developed on:

```sh
sudo pacman -S --needed base-devel git rustup clang cmake wayland libxkbcommon fontconfig freetype2 \
    pipewire wpewebkit gstreamer gst-plugins-base gst-plugins-good gst-libav
git clone https://github.com/nickalie/ngteams.git && cd ngteams
scripts/install.sh
```

The app installs into `~/.local`; the first build downloads about 150 MB of call components. Other distributions need the equivalent packages, including WPE WebKit 2.54 or newer. A build from source doesn't update itself: to update, run `git pull && scripts/install.sh` in that folder.

**On a Mac (Apple silicon, macOS 14 or later)** ngteams is a preview. Download
`ngteams-<version>-macos-arm64.dmg` from the [latest release](https://github.com/nickalie/ngteams/releases/latest),
open it and drag **ngteams** to **Applications** (the `.zip` holds the same app). Until releases are signed with an
Apple Developer ID, open it the first time with right-click → **Open**. It keeps your sign-in in the macOS Keychain
and its files in `~/Library/Application Support/ngteams`, asks for the camera and microphone the first time a call
uses them, and updates itself from later releases. Closing the window keeps it in the Dock (the badge
shows your unread chats), a click on the Dock icon brings the window back, and **⌘Q** quits. It also builds from
source with Xcode's command line tools, the repository's Rust and GStreamer's official macOS runtime
(`scripts/macos-gstreamer.sh`, no administrator, no Homebrew); the
[development guide](docs/development.md#macos-host-runner) has the commands.

## Getting started

1. Open **ngteams** from your app launcher, or run `ngteams` in a terminal. Make sure `~/.local/bin` is on your `PATH` for the terminal.
2. Enter your work email and sign in on Microsoft's own page, including any multi-factor prompt.
3. Check the first-run screen. It explains the things the app does on its own (typing indicators, read receipts and your availability) and lets you decide before any of them are sent.

To add another account, press **+** under your avatar on the left.

## Keyboard shortcuts

| Keys | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Jump to a chat or command |
| <kbd>Ctrl</kbd>+<kbd>N</kbd> | New message |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Search |
| <kbd>Ctrl</kbd>+<kbd>2</kbd> | Go to the chat list |
| <kbd>Ctrl</kbd>+<kbd>4</kbd> | Open the calendar |
| <kbd>Enter</kbd> / <kbd>Shift</kbd>+<kbd>Enter</kbd> | Send / new line |
| <kbd>R</kbd>, <kbd>A</kbd>, <kbd>E</kbd> on a selected message | Reply, react, edit |
| <kbd>Left</kbd> / <kbd>Right</kbd>, <kbd>Space</kbd> on a selected message | See who reacted, add or remove your reaction |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>M</kbd> | Mute or unmute in a call |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd> | Turn the camera on or off |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>K</kbd> | Raise or lower your hand |
| <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Live captions |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>H</kbd> | Leave the call |

On a Mac, press <kbd>⌘</kbd> (Cmd) where the table says <kbd>Ctrl</kbd>.

## Automate it

`ngteamsctl` talks to the running app from your terminal or scripts:

```sh
ngteamsctl unread                             # what's waiting for you
ngteamsctl send "Project 1" 'On my way'       # send a message
ngteamsctl mute toggle                        # mute from a hotkey
ngteamsctl wait message --mentions            # wait until someone mentions you
```

The [full command list](docs/development.md#ngteamsctl) covers chats, calls, the calendar and a live stream of events.

## Your data

- There's no ngteams server or ngteams account. The app talks to Teams directly as you.
- Your messages and settings are stored on your computer, and your sign-in is kept in your desktop keyring.
- Your camera starts off, and only you can turn it on.
- The update check asks GitHub for ngteams' latest release. It sends no account data and no sign-in, only the app's name and version.
- If you turn on translation or AI summaries, or ask for a correction, that text goes to the tool you picked and its provider. Translating a chat also sends its earlier messages, its title and its members' names; a correction also sends the chat's recent messages. Translating the message you are writing sends only its words and the language.
- Logs at the debug level contain message text, so don't post them publicly.

## Good to know

- **ngteams is an independent, unofficial project.** It isn't affiliated with or endorsed by Microsoft. Microsoft Teams is a trademark of Microsoft.
- **Things can break.** ngteams relies on Teams' internal services, which Microsoft can change at any time.
- **Your organization's rules still apply.** If your company only allows sign-in from managed devices, sign-in will be refused and ngteams won't work around it. Security keys, device-bound sign-in and client certificates aren't supported. Your administrators can see the sign-in as a Teams desktop client, so check with IT if you're unsure.
- **It's early.** ngteams is at version 0.1 and under active development.

## For developers

Architecture, building, testing and the protocol notes are in the [development guide](docs/development.md).

## License

ngteams is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option. Some parts carry their own licences: code taken from other projects and the `vendor/` sources keep
theirs, as their crates and licence files state.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as
defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
