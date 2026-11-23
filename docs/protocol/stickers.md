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
An animation that uses a feature the renderer cannot draw is skipped with a reason. An animation that could make the
renderer do too much is skipped as unsupported too: precompositions nested more than 16 deep or in a cycle, more than
1000 layers in one layer list or 1000 assets, an asset id or a reference to one longer than 256 bytes, a parent chain
longer than 16, an expression on a shape path, more than 1000 repeater copies (with the repeater's offset added) or
polystar points, or more than 250,000 work in one frame. A layer, shape, path vertex and entry of a dash list is one
work, a keyframe one for each of its numbers (at least one), a static value one for each number, a polystar point two
(its outer and inner vertex). A mask, a layer with effects (disabled ones too), each effect and each canvas-sized
surface of a track matte (two for the matted layer, one for its matte source) is 1000, a Simple Choker 1000 for each
pixel of its window (a radius of 1 is nine pixels) in place of its 1000, each dash that a dashed stroke can draw 10,
and each 100 assets the renderer looks through for a precomposition one. Only the first visible stroke of a shape list
strokes it and the groups in it that have no visible stroke of their own. A gradient stroke that the renderer cannot
decode (too few colour numbers) leaves the stroke of the list around it in use, so a gradient stroke counts the larger
of its own dashes and that stroke's. The dashes of a path are the longest it can be (its size, radii and roundness,
or the control polygon of its static shape or of its keyframe ends) times the dash pairs over the shortest period that
the dash and gap values can ease to, plus one dash for each dash and gap entry, because the renderer can walk the
whole list on each path. A layer also counts the transforms of its parents. Each precomposition, matte and copy counts
each time it is drawn. A number that is not finite as a 32-bit float has no bound.

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

## A Signal pack from its link

Signal keeps each sticker pack encrypted on its CDN. The pack's link holds the pack's id and the key that decrypts it.
Anyone with the link can read the pack. No Signal account, credential or cookie is necessary.

### The link

| Form | Example |
| --- | --- |
| web | `https://signal.art/addstickers/#pack_id=<id>&pack_key=<key>` |
| app | `sgnl://addstickers/?pack_id=<id>&pack_key=<key>` |

- `pack_id` is 16 bytes as 32 hex digits. `pack_key` is 32 bytes as 64 hex digits.
- On `signal.art`, the parameters are in the fragment (after `#`). Thus a browser does not send them to `signal.art`.
- The import takes the link with or without `https://`, in either order of the parameters, and with hex digits of
  either case. It refuses another host or path, a parameter that is missing or given two times, and an id or a key of
  another length or with a character that is not a hex digit. The import sends nothing for a link that it refuses.
- The key never leaves the app. Only the id goes into the CDN's paths.

### The requests

The CDN is `https://cdn-ca.signal.org`. A public CA signs its certificate. `cdn.signal.org` serves the same paths with
a certificate of Signal's own CA, so the app does not use it. Rows are in
[endpoints.md](endpoints.md#signals-sticker-cdn-cdn-casignalorg).

1. `GET /stickers/<pack_id>/manifest.proto`: the pack's manifest, encrypted.
2. For each sticker of the manifest: `GET /stickers/<pack_id>/full/<sticker id>`: the sticker's picture, encrypted.

| Answer | Means | The import says |
| --- | --- | --- |
| 200 | the blob | (decrypts it) |
| 403 | no pack (or no sticker) of that id: the CDN gives 403, not 404, for a path that it does not hold | Signal has no sticker pack at that link. A missing sticker is skipped. |
| a redirect or another status | not expected | Signal's answer can't be read. Try again later. |
| no connection or no answer in 60 s | the CDN is not reachable | Signal can't be reached. Check your connection and try again. |

The client is `https` only. It follows no redirect, uses no proxy, tries each request one time, and sends no
credential and no Teams or own-server token. It reads a manifest of at most 1 MB and a sticker of at most 10 MB.
Nothing logs these requests.

### The keys and the blobs

- **Keys**: HKDF-SHA256 of the 32-byte pack key, with a salt of 32 zero bytes and the info `Sticker Pack`, gives 64
  bytes. Bytes 0 to 31 are the AES-256 key. Bytes 32 to 63 are the HMAC-SHA256 key.
- **A blob** (the manifest, each sticker) is the IV (16 bytes), then the AES-256-CBC ciphertext with PKCS#7 padding,
  then the HMAC-SHA256 of the IV and the ciphertext (32 bytes).
- The app examines the HMAC first, in constant time. It decrypts a blob only when its HMAC is correct.
  - A manifest that fails the HMAC means that the key in the link is wrong (or the blob changed): "That pack can't
    be opened with the key in its link. Check that you copied the whole link." Nothing is kept.
  - A sticker that fails the HMAC is skipped: "failed Signal's integrity check".

### The manifest

The manifest is a protobuf message:

```
Pack {
  title    = 1  (string)
  author   = 2  (string)
  cover    = 3  (Sticker)
  stickers = 4  (repeated Sticker)
}
Sticker {
  id          = 1  (uint32)
  emoji       = 2  (string)
  contentType = 3  (string: "image/webp", "image/png", "image/apng", or empty in an older pack)
}
```

The import passes over fields that it does not know. It refuses a known field of the wrong wire type, a string that is
not UTF-8 and a message that ends too early. It keeps each sticker id one time, and at most 200 stickers (the most
that Signal lets a pack hold).

### What the import makes

- The pack's title names the imported pack. Each sticker's emoji is its label.
- The import tells a picture by its first bytes, not by `contentType`. A PNG or a WebP becomes a still sticker (a
  PNG). An APNG becomes an animated sticker (a GIF), with its frames and their times. The import does the same as
  for a file, with the same limits.
- If the CDN stops in the middle of a pack (no connection, no answer), the import stops and keeps no part of the pack.
- The cover is a sticker of the pack. The import reads it from the manifest, but does not ask for it a second time.

### What a live run showed

Task 652 read one public pack, "Bandit the Cat" (from `signal.art`), with no account, on 2026-11-23:

- The manifest and all 24 stickers came back from `cdn-ca.signal.org` with a 200, with no credential. Each HMAC was
  correct, and each sticker decrypted to a still picture. The import kept 24 PNG stickers, each labelled with its
  emoji, and skipped none.
- A pack id that the CDN does not hold got a 403. The app says "Signal has no sticker pack at that link."
- The same pack with one hex digit of its key changed: the manifest came back, but its HMAC was wrong. The app says
  "That pack can't be opened with the key in its link. Check that you copied the whole link."
- The run did not read an animated (APNG) pack. The APNG path is tested on a synthetic pack only.

The tests use synthetic packs on a local mock CDN (`teams_mock::signal`), which encrypts each blob with its own
implementation of the steps above. No byte of a real pack is in the repository.
