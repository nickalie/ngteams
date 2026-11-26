---
worth: later
status: open
where: crates/teams-media/src/video_file.rs
added: 2026-11-26
---
# A video sticker's frame size is checked before decode only as the file declares it

`decode_webm` refuses a stream before decode only from the size in the Matroska track (`PixelWidth`/`PixelHeight`).
`decodebin` plugs `vp9parse`, but its caps keep the declared size (checked with `GStreamer` 1.28.9). The decoder takes
the size from the keyframe header. Thus a hostile file that declares a small size can make libvpx decode one large
frame (about 400 MB at 16384 px a side, two times through `vp9alphadecodebin`) before the appsink caps and the caps
probe stop it with `TooBig`. No frame above the limit is converted.

To do: read the frame size from each VP9 frame header (key and intra-only frames, and inter frames with an explicit
size; the alpha frame in `GstVideoCodecAlphaMeta` too) in a buffer probe before the decoder, and drop the stream
with `TooBig` when a size is above the limit. The review of task 655 (round 2) found it.
