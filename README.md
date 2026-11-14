# Corpse

Your things stay where you died, for [Horizon](https://playhorizon.dev). When you die, everything you
drop is gathered into your corpse: your body, lying where you fell, in your own skin.

## How it plays

- **Die**, and the items you dropped go into a corpse at that spot, named "<you>'s corpse". Nothing is
  left lying on the ground to despawn. The chat tells you where it is, and tells you again when you respawn.
- **Right-click** the corpse to open it: six rows, the items in the first five. Click an item to take it,
  or the chest at the bottom to take everything that fits in your inventory.
- When it is empty, the body is gone.
- Corpses don't despawn, burn or take damage. They are kept in `<world>/corpse/corpses.txt`.
- Any player can open any corpse.

## Install

Install **Corpse** (`corpse_native`, the `.hnmod`) from the launcher's Mods page, then press Build. It is
a native mod: it runs with your account's permissions, so the launcher asks before building it. It needs
no content mod: the body is a vanilla mannequin.

## Building

`native/` is the mod's source, built by the Horizon launcher (`native-build`). `python3 tools/packnative.py
native corpse_native-1.0.2-horizon.1.hnmod` packs it. The unit tests run with `cargo test` in `native/api`.

## Credits

- Code by mxpbxb. MIT licensed: see `LICENSE`.
- Inspired by [Corpse](https://github.com/henkelmax/corpse) by Max Henkel. No code or assets of Corpse
  are in this mod: it was written from scratch for Horizon.
- No Minecraft code or assets are included.
