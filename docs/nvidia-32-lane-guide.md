# Running VIctory on a 32-lane GPU (NVIDIA): guide and status

This branch (`nvidia-32-lane`) is an unofficial fork of
[Marvisak/VIctory](https://github.com/Marvisak/VIctory). It collects open
upstream pull requests and further changes made while testing on an NVIDIA
RTX 4070, a GPU without 64-lane waves. It is shared so others can reproduce
the results and build on them. **It is not playable yet** (see Status).

## Legal

- **You must supply the game yourself**, from a copy you own, dumped from
  your own console. This repository contains no game files and never will:
  no executables, assets, shaders, shader dumps, captures or anything
  derived from them ([asset-policy.md](asset-policy.md)). Please don't open
  issues asking where to get the game, and don't post game files, dumps or
  links to them anywhere in this repository.
- Not affiliated with, endorsed by or connected to the game's publisher or Microsoft. All product names are their owners'
  owners' trademarks, used here only to say what the software is for.
- The code is licensed GPL-3.0-or-later ([LICENSE](../LICENSE)), as
  upstream. The one non-code file inherited from upstream,
  `crates/victory-formats/src/bcpack-tables.bin`, is not covered by that
  licence (see [asset-policy.md](asset-policy.md)).
- The game is run offline; VIctory gives it no network access.

### Modifications (GPL-3.0 §5a)

Modified from upstream `main` from 2026-11-23 to 2026-11-24. Besides
commits from open upstream pull requests (authors kept), the changes are:

- Shader translator: console wave64 programs on 32-lane hosts, including
  groups of several console waves (see [wave64-on-wave32.md](wave64-on-wave32.md));
  SGPR spills; cube sample gradients and level queries; compute
  `sampleLevel`; mesh shader caps; console descriptor words read from a
  12-dword runtime slot table; translated loops limited to 2^12 iterations.
- Rectangle-list draws completed by a generated geometry shader that picks
  the right-angle corner.
- Compute-written 16-bit console depth kept as a 16-bit texture (D3D12
  forbids depth-stencil and UAV on one resource); read-only depth tests on it
  dropped.
- Diagnostics: per-frame draw and dispatch tallies with drop reasons, busy
  frame target dumps, shader dumps, serialized draws, runtime shader hashes
  in `translate --list`.

## Requirements

Upstream's requirements (see the [README](../README.md)), plus for an
NVIDIA card:

- A recent driver with shader model 6.6 and mesh shaders (tested on an
  RTX 4070).
- A longer GPU timeout. World frames take seconds, and Windows resets the
  driver after 2 s by default. As administrator, then reboot:

  ```powershell
  reg add "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrDelay /t REG_DWORD /d 30 /f
  ```

  ```powershell
  reg add "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrDdiDelay /t REG_DWORD /d 30 /f
  ```

  Delete both values to undo this.
- Graphics Tools (Windows optional feature) if you want the D3D12 debug
  layer.
- Several GiB of free commit memory (see upstream's README).

## Build and run

```powershell
copy victory.example.toml victory.toml
```

Set the paths in `victory.toml` to folders outside the repository. Then:

```powershell
cargo build --release -p victory
```

```powershell
target\release\victory.exe run "<your dump>\<the title's exe>" --execute
```

For investigating, build with diagnostics:

```powershell
cargo build --profile diag -p victory --features diagnostics
```

To get past the intro and menus without a pad, send Enter every 3 s for the
first minute (presses made while loading are lost, so keep sending them):

```powershell
$env:VICTORY_KEYS = ((3..60 | ? { $_ % 3 -eq 0 } | % { "${_}:Enter" }) -join ',')
```

Useful diagnostics switches (diag build), on top of upstream's list:

| Variable | Effect |
|---|---|
| `VICTORY_DUMP_BUSY=<draws>[:<n>]` | with `VICTORY_CAPTURE=<dir>` (folder must exist): dumps the targets of the first `n` (3) frames with at least `draws` draws, with a per-depth-target draw and dispatch tally and the reasons draws were dropped |
| `VICTORY_DUMP_SHADERS=<hash>,...` + `VICTORY_DUMP_DIR=<dir>` | saves the listed shader containers when the title first uses them |
| `VICTORY_SERIALIZE_DRAWS=1` | a UAV barrier after every draw and dispatch |

Keep dumps and logs on your own machine: they contain game data.

To check how much of your copy's shader code translates (no GPU run needed,
about a minute):

```powershell
target\diag\victory.exe translate --validate --host-lanes 32-32 --list "<your dump folder>"
```

`--host-lanes 32-32` describes a GPU without 64-lane waves. The listing
names each refused program by its runtime hash and gives refusal reasons
with counts at the end.

## Status (2026-11-24, RTX 4070)

| Stage | State |
|---|---|
| Boot, menus, intro video, start screen, saves | work |
| Loading into the world | works |
| The world | reached; subtitles and UI draw, **the 3D scene is black**; frames take about a second; the title quits after a few minutes, probably its own watchdog |

What is known about the black scene:

- The G-buffer and the light-pass depth now get data; the tiled light
  draws run, but the lighting target stays zero.
- Still refused by the translator, in rough order of impact:
  - most world mesh shaders, which read raw console buffer descriptors
    from per-instance data (needs flat title memory rather than a
    translator rule);
  - compute programs with whole-wave sections in wave64 code (32-lane hosts
    only);
  - compute programs whose memory addresses do not come from a root
    parameter;
  - 64-bit image atomics;
  - the ray-tracing structure builder's shaders.
- The pairwise wave64 exchange (groups of several console waves) passes the
  GPU tests but is untested in game.

Contributions that make any of these work are welcome, ideally as pull
requests upstream.
