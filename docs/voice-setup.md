# Voice setup

DeepSeekCustom can listen and speak. Speech to text runs through
whisper-rs. Speech to speech runs through Kokoro. Both are local models.
Nothing is sent to a cloud speech service.

This doc covers what to download, where the files go, and how the
harness finds them.

The browser Settings workspace owns voice controls. Push-to-talk keyboard
events call the Rust voice service through the typed application port. Audio
capture, transcription, synthesis, and playback stay in the local Rust process.

## What you need

Download three things:

1. The whisper GGML model, for speech to text.
2. The Kokoro ONNX model, for text to speech.
3. Seven Kokoro voice packs, one `.bin` file per voice.

## Whisper model

Download `ggml-base.en.bin` from:

```
https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

It is about 148 MB. Put it at `models/ggml-base.en.bin` under the
project root.

## Kokoro model

Kokoro ships three ONNX variants on Hugging Face:

```
https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX
```

The files live under `onnx/` in that repo. Download `model.onnx`, the
fp32 build. It is about 325 MB. Put it at `models/model.onnx`.

Measured on this machine, per synthesized line:

| Variant | Size | CUDA | CPU |
|---|---|---|---|
| `model.onnx` (fp32) | 325 MB | 0.25s | 1.0s |
| `model_quantized.onnx` (int8) | 92 MB | not tested | 3.7s |

The fp32 build is both the fastest and the best sounding of the two on
this hardware. Use it as the default. Keep `model_quantized.onnx`
around if you want it, but do not make it the default.

There is a third variant, `model_q8f16.onnx`. Do not download it. It
crashes ONNX Runtime with an access violation while the session is
being built.

## Kokoro voice packs

Download the voice packs from the `voices/` folder in the same
Hugging Face repo. Each voice is one small `.bin` file. This project
ships with seven English voices already:

```
af_bella.bin
af_heart.bin
af_nicole.bin
am_michael.bin
am_puck.bin
bf_emma.bin
bm_george.bin
```

Put them all in a `voices/` folder at the project root, next to
`models/`, not inside it. `af_heart` is the default voice.

## On-disk layout

```
<project root>/
  models/
    ggml-base.en.bin        whisper, speech to text
    model.onnx               Kokoro fp32, speech from text, the default
    model_quantized.onnx     Kokoro int8, kept but never the default
  voices/
    af_bella.bin
    af_heart.bin
    af_nicole.bin
    am_michael.bin
    am_puck.bin
    bf_emma.bin
    bm_george.bin
```

## Where the harness looks

For each model, and for the voices folder, the harness checks three
places in order and uses the first one that exists:

1. The path set in `settings.json`, under `voice.stt_model_path`,
   `voice.tts_model_path`, or `voice.tts_voices_path`.
2. `models/` under the project root (or `voices/` at the project
   root, for the voice packs).
3. `%LOCALAPPDATA%\DeepSeekCustom\models\` (or
   `%LOCALAPPDATA%\DeepSeekCustom\voices\`).

If a model is missing everywhere, the harness logs a warning. The
warning names every path it tried, plus the download URL. Startup
does not fail. Speech to text and speech to speech turn off
independently. A missing whisper model does not stop speech output.
A missing Kokoro model does not stop speech recognition.

## Configuring paths in settings.json

You do not need to set anything if the files sit in the default
layout above. To point at a different location, add a `voice` block:

```json
{
  "voice": {
    "enabled": true,
    "stt_enabled": true,
    "tts_enabled": true,
    "stt_model_path": "D:/models/ggml-base.en.bin",
    "tts_model_path": "D:/models/model.onnx",
    "tts_voices_path": "D:/voices",
    "tts_voice": "af_heart",
    "tts_speed": 1.0
  }
}
```

## GPU acceleration (optional)

A GPU is optional. Without one, both models run on CPU, at the
speeds in the table above. To use CUDA, install these Python wheels:

```
pip install nvidia-cudnn-cu12 nvidia-cublas-cu12 nvidia-cufft-cu12 nvidia-cuda-runtime-cu12
```

`src/voice/cuda_dlls.rs` finds the NVIDIA runtime DLLs inside those
wheels on its own. You do not need to add anything to `PATH`.

## Running the model-dependent tests

Two integration tests need the real model files above on disk. They sit in
`crates/deepseek-custom-tests` behind the `voice-models` Cargo feature. The
feature is off by default. A fresh checkout without models can run the normal
workspace tests.
Once the files are in place, run:

```powershell
cargo test --workspace --features deepseek-custom-tests/voice-models
```

## Build requirements

Building whisper-rs needs three tools installed:

- CMake
- A Visual Studio C++ toolset
- LLVM, for `libclang`

`.cargo/config.toml` pins `CMAKE` and `LIBCLANG_PATH` for this build.
If `cargo build` fails on whisper-rs or whisper.cpp, check these
tools are installed first.
