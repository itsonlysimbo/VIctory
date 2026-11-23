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
victory config
```

Logging: `VICTORY_LOG=victory_loader=trace,info`.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
