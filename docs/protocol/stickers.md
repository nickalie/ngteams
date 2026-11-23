# Stickers from other apps

How the sticker picker imports packs that other apps make. Teams' own stickers and the form of a sent sticker are in
[chat.md](chat.md#stickers-team-manage-2026-11-19). An imported sticker is sent as Teams sends a picture in a sticker
element: a still sticker as a PNG, an animated one as a GIF.

## Telegram's sticker files

A Telegram pack has three kinds of stickers. The file of each kind is told by its first bytes, not by its name.

| Kind | File | First bytes | What the import does |
| --- | --- | --- | --- |
| still | `.webp`, 512 px on its longer side | `RIFF....WEBP` | decodes it as any WebP picture; a PNG sticker |
| animated | `.tgs`: a gzipped Lottie animation (JSON), 512 by 512, at most 3 s, often 60 frames a second | `1f 8b` (gzip) | renders it to frames, then a GIF sticker |
| video | `.webm`: VP9 with an alpha channel, at most 3 s | `1a 45 df a3` (Matroska) | skips it and says so (not imported yet) |

A Lottie animation names its canvas (`w`, `h`), its frame rate (`fr`) and its first frame and the frame after its
last (`ip`, `op`). Its layers are vector shapes. A `.tgs` from Telegram has no expressions and no raster pictures.

The import renders the animation with `rasterlottie` (pure Rust), at the size a sticker is kept at (250 px, never
larger than the canvas). It keeps at most 30 frames a second. A GIF's frame time is in hundredths of a second, and
browsers slow a faster frame down. Each frame's time is cut from the animation's clock, so the loop keeps its length:
a 3-second sticker at 60 frames a second becomes 90 frames of 30 or 40 ms, 3 s in all. A GIF has no partial
transparency. A pixel that is at least half covered is kept whole, and any other is clear, as for other animated
stickers.

Limits: the unpacked animation is at most 8 MB, its canvas at most 4096 px on a side, and at most 200 frames come out.
An animation that uses a feature the renderer cannot draw is skipped with a reason.

## A pack from its link: the Bot API

A pack's link is `https://t.me/addstickers/<name>` (also `telegram.me`, or `addemoji` for an emoji pack) or
`tg://addstickers?set=<name>`. The name has letters, digits and `_`, at most 64 characters. The t.me page shows only
the pack's title, so only the Bot API gives the files. The Bot API takes a bot's token, which a user gets from
@BotFather (`/newbot`). The bot needs no rights and joins no chat.

The requests (rows in [endpoints.md](endpoints.md#telegrams-bot-api-with-the-users-bot-token-apitelegramorg)):

1. `GET /bot<token>/getStickerSet?name=<name>`. The answer is `{"ok": true, "result": {...}}`. The result has `name`,
   `title`, `sticker_type` and `stickers`. Each sticker has `file_id`, `file_unique_id`, `type`, `width`, `height`,
   `is_animated` (a `.tgs`), `is_video` (a `.webm`), `emoji` and `file_size`.
2. For each sticker that is not a video: `GET /bot<token>/getFile?file_id=<id>`. The result has `file_path` (for
   example `stickers/file_12.webp`) and `file_size`.
3. `GET /file/bot<token>/<file_path>`: the file's bytes. The Bot API gives files of at most 20 MB. The import reads at
   most 10 MB.

A refusal is `{"ok": false, "error_code": <n>, "description": "..."}`:

| Answer | Means | The import says |
| --- | --- | --- |
| 401 `Unauthorized` (404 for a token of a wrong form) | the token is refused | Telegram refused the bot token. Check it, or replace it. |
| 400 `Bad Request: STICKERSET_INVALID` | no pack of that name | Telegram has no sticker pack at that link. |
| 429 | too many requests | Telegram asks to wait. Try again in a minute. |

A refusal of the token, a 429, a timeout or a lost connection on a sticker's file ends the import, and no part of the
pack is kept. A file that is too large or an answer that can't be read skips only that sticker.

The pack's title names the imported pack, and each sticker's emoji is its label. The app sends the token only to
`https://api.telegram.org`, only in the path. The client follows no redirect, uses no proxy, and logs none of these
requests. The token is kept in the system keyring (a lab alias and the read-only mode keep it in memory only).

What is not known from a live run: the agents' host has no bot token, so the requests above are checked against a
local mock that follows the Bot API's documentation (`teams_mock::telegram`), not against Telegram itself.
