# MiniRec

A small, portable screen recorder for Windows, written in Rust. One exe of about 1 MB that uses only what ships with Windows: no codecs, no runtimes, no installer.

## Features

- Record a whole monitor or a single program window. Program capture records only the program, without its title bar, even when other windows cover it. On Windows 10 the captured window shows a yellow border while it is recorded.
- Output at source size or 2160p, 1440p, 1080p, 720p or 480p, with the picture scaled to fit
- Hardware H.264 encoding through Media Foundation (NVIDIA, AMD or Intel)
- System audio and microphone, each from a device you pick
- Replay buffer kept in RAM, saved as a clip with a hotkey
- Video player with a resizable window, auto-hiding controls, fullscreen, volume, speed and next/previous video in the folder. Press E or click Edit to switch to the trimmer, which cuts one or more segments lossless, frame-exact or compressed to a quality or file size. MiniRec adds itself to Open with for .mp4, .m4v and .mov files as MiniRec Trimmer
- Sounds when recording or the replay buffer starts and stops, a different pair for each
- Compressor with a quality mode that estimates the result size, or a target file size mode
- Rebindable global hotkeys, tray icon and live preview
- Closing the window keeps MiniRec running hidden with only its hotkeys active, using no CPU and a few MB of RAM. Exit in the tray menu does the same. Run the exe again or press the Show hotkey to bring it back. To stop it completely, end it in Task Manager. Recordings in progress are saved when Windows shuts down. Turn on Start with Windows in the tray menu to have it start hidden when you log in.

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
