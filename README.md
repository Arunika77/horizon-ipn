# Inventory Profiles Next (`ipn`) for Horizon

Clean-room Rust/WASM inventory management mod for **Horizon** (Minecraft Java 26.3, Mod API `0.2`), inspired by [Inventory Profiles Next (IPN)](https://codeberg.org/Inventory-Profiles-Next-Mod/IPN).

## Features

- **Locked Slots:** Press `L` (or `Alt+Click`, or toggle `Lock` mode on the panel) to lock player slots. Locked slots display an amber border + padlock badge rendered cleanly beneath item tooltips (`screen_render_foreground`) and are protected from sorting, dumping, moving, auto-refill, and throwing (optional `Protect Locked Slots on Click` setting blocks `Q` and `Shift+Click` too).
- **Smart Lock-Aware Sort (`R`):** Merges partial stacks and sorts container storage or player inventory without touching locked slots. Supports **Category + Name**, **Display Name (A-Z)**, **Item ID (A-Z)**, and **Stack Count (High-Low)** in **Row by Row** or **Column by Column** order.
- **Container Transfers (Item + NBT Aware):**
  - **Move Match (`M`):** Matches both `ItemKey` and full NBT/Components patch (`match_group`), preventing custom-NBT variants sharing a vanilla ID from filling empty chest slots. Configurable between **`Match Item + NBT (Allow Empty Slots)`** and strict **`Top-Up Existing Stacks Only (No Empty)`** in **Config**.
  - **Dump All (`G`)**, **Pull Match (`U`)**, **Pull All (`Shift+U`)**.
- **Hover Matching Highlight (`H` / `HL`):** Toggleable cyan highlight across both chest and player inventory matching the hovered item's ID and NBT (aligned with Minecraft's 18x18 slot hover hitbox).
- **Bulk Throw Hotkeys:**
  - **Throw Matching (`Shift+Q`):** Throws all stacks matching the hovered slot's Item + NBT on the hovered side (chest or unlocked player inventory).
  - **Throw All (`Ctrl+Shift+Q`):** Throws all container stacks or unlocked player inventory stacks in bounded per-tick click batches (`MAX_CLICKS_PER_TICK = 12`).
- **Auto-Refill & Tool Saver:** Automatically replaces depleted stacks or tools reaching your configurable durability threshold (`refill_threshold`) from unlocked main inventory slots.
- **5 Loadout Profiles (`P1`–`P5`):** Capture (`Shift+P`) and equip (`P`) hotbar, armor, and offhand loadouts; cycle with `]`.
- **Dockable / Draggable Side Panel:** Supports `Left (Auto-dodge)` (moves clear of the recipe book), `Left`, `Right (Shift Effects)` (shifts active beacon/potion effects out of the way), plus free dragging via the `IPN` header bar (right-click header to reset).

## Default Controls

| Action | Default Keybind |
| --- | --- |
| Sort Container / Inventory | `R` |
| Lock / Unlock Hovered Slot | `L` (or `Alt+Left Click`) |
| Toggle Matching Highlight | `H` (or `HL` button) |
| Move Matching to Container | `M` |
| Dump Inventory to Container | `G` |
| Pull Matching from Container | `U` |
| Pull All from Container | `Shift+U` |
| Throw Matching (Hovered) | `Shift+Q` |
| Throw All (Chest / Inventory) | `Ctrl+Shift+Q` |
| Apply Active Profile | `P` |
| Save Active Profile | `Shift+P` |
| Cycle Active Profile | `]` |

All bindings and options can be customized under **Options -> Controls** and **Options -> Mods -> Inventory Profiles Next -> Config**.

## Installation (`.hmod` Release)

1. Download `ipn.hmod` from [Releases](https://github.com/Arunika77/horizon-ipn/releases).
2. Place `ipn.hmod` into your Horizon `game-data/horizon-mods/` folder.
3. Enable `mod.ipn:true` in `game-data/horizon-mods.txt` (or via **Options -> Mods**).

## Building from Source

Because Horizon mods depend on the local `horizon-mod-api` crate (`path = "../../crates/horizon-mod-api"`), place this repository at `<horizon-checkout>/mods-custom/ipn`:

```powershell
# Run offline unit tests
cargo test --offline

# Build ipn.hmod WASM package
cargo-horizon-mod build --wasm
```

The packaged `.hmod` is written to `target/horizon-mod/ipn.hmod`.
