# Noise suppression in calls

How Teams lowers the background noise of the microphone in a call, and how ngteams does the same. This is a client
feature: no request and no Trouter frame carries the level. Each client cleans its own microphone before it sends
the audio.

## What Teams does

The Teams desktop client has a "Noise suppression" setting in Settings > Devices and in the audio settings of a call
or meeting. Microsoft's help page gives these levels, in this order:

- **Auto (default)**: the client selects the level of suppression itself. Microsoft says that it is the best level
  for most situations.
- **Low**: the client lowers only low, steady background noise, for example a computer fan or an air conditioner.
  Microsoft recommends it when you play music to the other people in the meeting.
- **High**: the client removes all background sound that is not speech.

When the switch is off, the client does no noise suppression. Microsoft recommends this for a high-fidelity
microphone in a quiet room, for example a studio. The setting is on by default, at Auto, and the client keeps the
choice for the next call or meeting.

The web client (browser Teams, as the lab's echo-call runs since 2026-10-24 found it) shows only an on/off "Noise
suppression" switch, in the call's audio options (`#audio-button-configure`, then
`data-tid="background-suppression-switch"`). It has no levels. The lab's
echo-call scenario turns this switch off so that its test tone gets to the echo service
([calling-echo.md](calling-echo.md)). Microsoft does not list the web client among the platforms that have the
levels. Thus ngteams follows the desktop client: the four levels, with Off as the fourth item of the list.

## What ngteams does

The microphone's frames (10 ms, mono, 48 kHz, the device seam) go through these steps on the capture thread of the
call (`crates/teams-media/src/processing.rs`):

1. libwebrtc's echo canceller (AEC3), with libwebrtc's classic noise suppressor at Low only.
2. DeepFilterNet3 at Auto and High (`crates/teams-media/src/processing/deep.rs`).
3. libwebrtc's gain control (AGC2) and limiter. It comes after the noise suppression, so it sets the level of the
   clean voice and not of the noise.

The level of each step comes from these settings (`crates/teams-media/src/processing/noise.rs`):

| level | DeepFilterNet3 | classic suppressor | why |
|---|---|---|---|
| Auto | yes, limit 24 dB | no | Teams' Auto removes most noise but is not as hard as High. The 24 dB limit keeps about 6% of the noise's amplitude, so some of the room and of music stays and the voice sounds natural. |
| High | yes, no limit | no | Teams' High removes all sound that is not speech. |
| Low | no | yes | Teams' Low lowers only steady noise. The classic suppressor does exactly that. |
| Off | no | no | No noise suppression. Echo cancellation and gain control stay. |

The level is kept for each account in the settings file (the `noise_suppression` table, by the account's MRI). An
account that has no row is at Auto. The call reads the level at each frame. Thus a change in Settings or in the call's
audio menu applies to the next 10 ms frame. A change to or from Low makes libwebrtc's echo module again (it selects
its parts only when it is made). The echo canceller then converges again in about one second.

### DeepFilterNet3

DeepFilterNet3 is a neural network that keeps speech and removes other sounds, steady or not (keys, a dog, traffic).
ngteams runs it through `deep-filter-rt` (libDF's real-time inference on tract, pure Rust). The model is in the
crate, so it is in the `ngteams` binary of each release (Linux packages, Windows, macOS). No release script or
package file has to install a model file. The code and the model weights are MIT or Apache-2.0, as ngteams is. The
crate logs only its setup at `debug` (the load of the model, a change of the limit). Its per-frame line is at
`trace`, which the compile-time cap of `teams-log` removes. No line has audio in it.

The model loads one time for each process, on a thread of its own, when the first call's processing starts. Each call
then gets a clone of the model with a new state. Until the model is ready, the frames go through without
DeepFilterNet. The model's hop is 10 ms at 48 kHz, the frame of the device seam, so each frame goes in and out at
once. The `Reframer` adapts another hop at a fixed delay (`hop - gcd(hop, 480)` samples). DeepFilterNet3 adds its own
delay of 30 ms (10 ms of overlap and two frames of lookahead).

### CPU and GPU

Measured on the development host (Intel i7, 12 threads, NVIDIA GTX 1060 Mobile, load average about 17 during the
runs), a release build, one 10 ms frame at a time:

| path | mean ms per frame | p99 ms | CPU of one core |
|---|---|---|---|
| libDF on tract, High | 0.43 | 1.38 | 3.9% |
| libDF on tract, Auto | 0.40 | 1.04 | 3.7% |
| ONNX Runtime (`ort`) on the CPU | 1.60 | 4.59 | 14.7% |
| `ort` WebGPU on the GPU (Vulkan) | 23.6 | 124 | 162% |

The model loads in 143 ms. A clone for a call takes 0.02 ms. ONNX Runtime's CUDA provider did not load on this host
(its prebuilt binaries are for CUDA 13, which does not support this GPU, and cuBLAS was missing). On the GPU, the
transfers for each small frame cost more than the network itself, so the GPU path is slower than real time. Thus
ngteams runs only libDF on the CPU, with no GPU code. Low and Off add no cost to the classic processing.

`audio.cpp` (a ggml-style audio inference project) has only DeepFilterNet2, as a tool that cleans a whole file. It
has no real-time DeepFilterNet3, so it was not compared.
