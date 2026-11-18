# MiniRec

A small, portable screen recorder for Windows, written in Rust. One exe of about 1 MB that uses only what ships with Windows: no codecs, no runtimes, no installer.

## Features

- Record a whole monitor or a single program window. Program capture records only the program, without its title bar, even when other windows cover it. On Windows 10 the captured window shows a yellow border while it is recorded.
- Output at source size or 2160p, 1440p, 1080p, 720p or 480p, with the picture scaled to fit
- Hardware H.264 encoding through Media Foundation (NVIDIA, AMD or Intel)
- System audio and microphone, each from a device you pick
- Replay buffer kept in RAM, saved as a clip with a hotkey
- Trimmer that cuts one or more segments, either lossless or frame-exact
- Compressor with a quality mode that estimates the result size, or a target file size mode
- Rebindable global hotkeys, tray icon and live preview
- Closing the window keeps MiniRec running hidden with only its hotkeys active, using no CPU and a few MB of RAM. Run the exe again or press the Show hotkey to bring it back, and use Exit in the tray menu to quit. Turn on Start with Windows in the tray menu to have it start hidden when you log in.

## Requirements

- Windows 10 version 2004 or newer
- A GPU with a hardware H.264 encoder

## Download

Get `MiniRec.exe` from the [releases page](../../releases). Settings are saved in `MiniRec.ini` next to the exe.

## Building

Needs the Rust MSVC toolchain and the Windows 10/11 SDK (for `fxc.exe`, which compiles the cursor shader).

```
cargo build --release
```

The exe is written to `target/release/minirec.exe`.
