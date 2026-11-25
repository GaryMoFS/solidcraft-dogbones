# SolidCraft Dogbones Plugin

CNC dogbone corner-relief plugin for **SolidCraft 0.48+** (`craft:plugin@0.2.0`), modeled after Brian Ekins' *Nifty Dogbone* workflow.

## Features

- **Two commands** (grouped under one split-dropdown button on the **Plugins** tab):
  - **Create Dogbones…** (`dogbones`)
  - **Update Dogbones** (`update-dogbones`) — recomputes stored dogbone recipes via `storage@0.1` after upstream sketch/model edits
- **Three selection modes** (with custom 64×64 SVG icons):
  - **Body Select** — auto-infers CNC panel normal from largest planar face group
  - **Upper Face Select** — targets only internal corners accessible from the selected planar Z-up face
  - **Edge Select** — selective dogboning on specific internal corner edges
- **Four dogbone styles:**
  - **Corner** (classic bisector dogbone, zero corner interference)
  - **Minimal Corner** (FabLab Denmark style with configurable **Corner Interference**)
  - **Along Long Side** (T-bone tangent to the longer wall)
  - **Along Short Side** (T-bone tangent to the shorter wall)
- **Additional Clearance** (`D + clearance`) to avoid CAM floating-point rejection
- **Collapsible Advanced section** with live-calculated **Visible Gap (90 deg)** via `dialog@0.1`, **Corner Angle Range (+/-)**, and **Feature Name** (auto-numbered across repeated runs)
- **Blind pocket & through-slot aware:** groups corners by `(body, machining_normal, z_top, z_bottom)` into one Sketch + one Extrude Cut per depth group while preserving blind pocket floors.

## Build & Package (Windows GNU Toolchain)

```powershell
$env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu"

# 1. Build WASM core module
cd plugin
cargo build --release --target wasm32-unknown-unknown

# 2. Encode WebAssembly component & package dist/dogbones.zip
cd ../packager
cargo run --release
```

Install `dist/dogbones.zip` in SolidCraft via **Plugins › Install plugin…**.

## Integration Tests

Requires a local SolidCraft 0.48+ checkout referenced by `packager/Cargo.toml`:

```powershell
$env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu"
cd packager
cargo test --no-run
$exe = (Get-ChildItem ".\target\debug\deps\dogbones_packager-*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
strip --strip-debug $exe
& $exe --nocapture
```
