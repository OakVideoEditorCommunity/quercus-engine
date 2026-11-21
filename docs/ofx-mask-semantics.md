# OFX Optional Mask Clip Semantics

This document describes the contract between Quercus (`crates/quercus-ofx-plugin`) and OpenFX (OFX) plugins for optional mask clips.

## 1. Optional Mask Clips in OpenFX

In the [OpenFX specification](https://openfx.readthedocs.io/en/latest/), an effect may declare an optional mask clip (commonly named `Mask` or `Matte`) by setting `OfxImageClipPropOptional` and `OfxImageClipPropIsMask` on the clip descriptor (`crates/quercus-ofx-plugin/src/descriptor.rs`). Because the clip is optional, a user can leave it disconnected, and the plugin must still render using its documented unmasked behavior.

## 2. How Quercus Maps Optional Mask Clips

- **Node input creation**: During node construction, each non-`Output` descriptor clip becomes a `Texture` input on the node with the same name as the clip (`crates/quercus-ofx-plugin/src/node_factory.rs`).
- **Per-job attachment**: Before rendering a frame, the render driver iterates over the instance's non-`Output` clips and attaches the matching texture from the render job (`crates/quercus-ofx-plugin/src/render_driver.rs`).
- **Connection state**: `OfxImageClipPropConnected` tracks whether a clip currently has an input texture attached (`1` when connected, `0` when disconnected) via `ClipInstance::set_input_texture` (`crates/quercus-ofx-plugin/src/clip.rs`).
- **Disconnection lifecycle**: When a previously connected optional mask input is omitted from a subsequent render job, the clip is cleared so that `OfxImageClipPropConnected` reads `0` (pending #14 / #5 for the CPU texture path).

## 3. What the Plugin Must Do

Plugins that declare an optional mask clip must:

1. Check `clipGetIsConnected` (or read `OfxImageClipPropConnected`) before calling `clipGetImage` on the mask clip.
2. Use `clipGetRegionOfDefinition` to query the mask's spatial extent rather than assuming it matches the `Source` or `Output` RoD.
3. Handle an absent mask gracefully: `ClipInstance::fetch_image` returns `Error::NotFound` when no input texture is attached (`crates/quercus-ofx-plugin/src/clip.rs`).

## 4. What the Host Deliberately Does Not Do

Quercus intentionally leaves mask interpretation and processing to the plugin. The host does **not** perform:

- Mask inversion
- Feathering or edge blur
- Edge expansion / contraction
- Per-frame mask resampling

Plugins are responsible for implementing those behaviors according to their own parameter and clip contracts.

## 5. CPU vs. OpenGL Paths

- **CPU path (default)**: Input textures are passed as CPU image buffers via `set_input_texture` and fetched through `ClipInstance::fetch_image`.
- **OpenGL path (opt-in)**: Zero-copy GL texture support is gated behind `QUERCUS_OFX_GL=1` (see [OFX_GPU_HANDOFF.md](./OFX_GPU_HANDOFF.md)). Its pixel tests are gated, so GL mask behavior is documented as gated rather than guaranteed.

## 6. Known Gaps

- **Test fixture**: Optional-mask fixture coverage is tracked in #3.
- **Lifecycle**: Clearing stale CPU clip input state between render jobs is tracked in #14 and #5 (pending #14 / #5).
- **GL verification**: End-to-end GL masked-pixel verification remains open.
