# VIctory

Runs a console title (Xbox Series X|S) natively on Windows, from files
the user supplies.

> **Rule #1: nothing from the game is ever stored in this repository**:
> no assets, no code, nothing derived from them. Everything is built on the
> user's machine from the user's own files. See
> [docs/asset-policy.md](docs/asset-policy.md).

The title's x86-64 code runs unchanged inside our process; VIctory supplies
the platform underneath it and converts the title's data, ahead of time where
it can and on first use where it must (shaders). See
[docs/architecture.md](docs/architecture.md).

## Getting started

Requirements: Windows 10/11 x64, a CPU with AVX2, Rust (pinned in
`rust-toolchain.toml`).

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
copy victory.example.toml victory.toml   # then set your paths (outside the repo)
cargo test --workspace                   # needs no game data
cargo run -p victory -- config           # check your paths
```

## Commands

```
victory inspect <exe|dll> [--imports] [--exports]
victory census [paths...] [--missing] [--strict]
victory lib-exports <import.lib> [--list]
victory shader-info [paths...] [--verbose] [--disasm]
victory run <exe> [--execute] [--unimplemented trap|zero] [--strict]
            [--console series-x|series-s] [-- <title arguments>]
victory config
```

Logging: `VICTORY_LOG=victory_loader=trace,info`.

Debugging `run --execute` (builds with `--features diagnostics` only): `VICTORY_STACKS=20,60` prints every thread's stack at those seconds;
`VICTORY_WATCH=<rva>,...` logs a title u32 when it changes; `VICTORY_KEYS=30:Enter,35:X` presses keys
(focus or not); `VICTORY_CRASH_SAVE=<rva>:<len>,...` saves title memory to the cache dir on a crash;
`VICTORY_AUDIO_CAPTURE=<file.wav>` records what the title plays (our process only, from a child process);
`VICTORY_XMA_DUMP=<dir>` saves the first 32 XMA voices' context and input (`VICTORY_XMA_STREAMS=<dir>` runs the ignored `xma` test on them);
`VICTORY_D3D12_DEBUG=1` runs the host debug layer, `VICTORY_D3D12_DRED=1` adds device-removed data to a GPU failure
(the allocation a page fault hit, the commands in flight with their pipelines, user data and root views);
`VICTORY_D3D12_WARP=1` puts the host device on the software adapter (a shader's wild access cannot reset the driver);
`VICTORY_DUMP_TARGETS=1` with `VICTORY_CAPTURE` saves every render target and written texture of the frames in `VICTORY_CAPTURE_FRAMES`.
Every build appends crashes and host GPU failures to `crashes.log` in the title's data folder.

Input: XInput pads. With none connected the keyboard is a pad while the title's window has focus: WASD/IJKL sticks,
arrows D-pad, Enter A, Backspace B, X X, Y Y, Q/E shoulders, Z/C triggers, F/H stick clicks, Esc Menu, Tab View.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
