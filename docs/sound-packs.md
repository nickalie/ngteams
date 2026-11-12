# Sound packs

Settings > Notifications > Sound chooses a pack as a whole. Each event then has its own switch, volume, and a pick of
another pack's sound or a file of your own. Calls > Ringtone is the same control for incoming calls.

## Making a pack

A pack is a folder, or a `.zip` of that folder, holding `pack.toml` and its sounds:

```toml
name = "Office chimes"   # required, up to 64 bytes
author = "You"           # optional

[sounds]
incoming-call = "ring.ogg"
message = "sounds/ping.mp3"
mention = "sounds/mention.wav"
```

Name any of these events; one you leave out plays the default pack's sound:

`message`, `mention`, `channel-message`, `missed-call`, `incoming-call`, `outgoing-call`, `call-joined`, `call-ended`,
`reconnecting`, `reconnected`, `person-joined`, `person-left`, `lobby-request`, `on-hold`, `resumed`,
`recording-started`, `recording-stopped`, `screen-share-started`, `screen-share-ended`, `hand-raised`, `live-reaction`,
`mic-muted`, `mic-unmuted`.

## What a sound can be

- WAV, MP3 or Ogg (Vorbis or Opus). PCM and 32-bit float WAVs are read directly. Other WAV encodings, MP3 and Ogg
  use the system's GStreamer plugins, which the app's packages install.
- From 10 milliseconds to 30 seconds, and up to 16 MB. A pack holds up to 64 MB, and a `.zip` up to 256 entries.
- Paths are relative to the pack, separated by `/`, without `..`.

An import checks every sound, then keeps its own copies: mono at 48 kHz with the peak limited, so a loud file can't
blast next to the app's own sounds. Packs live in `sound-packs/` beside the app's settings file. A sound that later
can't be read falls back to the chosen pack's and then the default pack's, so a call always rings.
