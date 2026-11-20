> **Internal crate — do not depend on it directly.**
>
> This crate is an implementation detail of `quercus-engine` and is
> published only to satisfy Cargo's dependency resolution. Its API is
> **not stable** and may change in any release without notice. Use
> `quercus-engine-rs` (or the C ABI via `quercus-engine`) instead.

# quercusplugin Rust crate (M11 phase 1+2 implementation status)

> Status: **M11 phases 1 and 2 implemented** (phase 2: GL render path +
> ofxColour + pluginrenderer render driver absorbed). The original
> declaration draft (types and function signatures + doc comments as the
> contract) has been filled in by the implementer; there is no `todo!()`
> left in `src/`.
> Plan: docs/zh/plans/riir/M11-ofx-host.md.

## Structure

```
src/
  lib.rs        crate docs, module map, FFI discipline
  error.rs      error codes (one-to-one with include/plugin/error.h)
  handle.rs     RefBox boundary container (Host::create_instance return type)
  property.rs   PropertySet: storage and typed read/write of OFX property sets
  suites/       C function tables plugins call in (unsafe trampoline layer)
    mod.rs      fetchSuite registry + render/GL context TLS
    property.rs memory.rs image_effect.rs param.rs
    message.rs  progress.rs timeline.rs multithread.rs
    gl_render.rs OfxImageEffectOpenGLRenderSuiteV1 (new in M11 §4)
    draw.rs     OfxDrawSuiteV1 (OFX 1.5, interact drawing; real GL rendering)
    interact.rs OfxInteractSuiteV1 + Interact host object (interact host)
  host.rs       Host singleton: bundle scanning, plugin cache, action dispatch
  descriptor.rs EffectDescriptor/ClipDescriptor (products of describe)
  instance.rs   Instance: action call surface (render/negotiation/RoD/RoI/
                isIdentity/render_gl/GetOutputColourspace)
  render_driver.rs pluginrenderer.cpp render flow absorbed (new in M11 §4)
  clip.rs       ClipInstance: clip↔quercusrender texture bridge (render.rs)
  image.rs      Image: OFX view of framebuffers/textures
  param.rs      12 parameter instances + param↔quercusnode bridge (node.rs/undo)
  progress.rs   PluginProgressReporter
  node.rs       quercusnode bridge: node value POD, identity registry, undoable write-back
  render.rs     quercusrender bridge: Texture/Frame/Renderer value type call surface
```

## Plugin search paths

`host::PluginCache::scan` scans in the following order (duplicate paths
deduplicated, nonexistent directories skipped; bundles scanned recursively,
depth 3):

| Level | Path |
| --- | --- |
| User-level (`$HOME`) | `$HOME/.OFX/Plugins`, `$HOME/.local/share/OFX/Plugins`, `$HOME/.local/share/olive/ofx/Plugins`; macOS additionally includes `$HOME/Library/OFX/Plugins` |
| System-level (OFX spec) | macOS `/Library/OFX/Plugins`; Linux `/usr/OFX/Plugins`, `/usr/local/OFX/Plugins`; Windows `%ProgramFiles%\Common Files\OFX\Plugins` (falls back to `C:\Program Files\Common Files\OFX\Plugins` when `%ProgramFiles%` is unset) |
| app-relative (Olive alignment) | `../OFX/Plugins`, `../share/olive/ofx/Plugins`, `../lib/olive/ofx/Plugins` |
| Environment variables | `OFX_PLUGIN_PATH` (official OFX), `OLIVE_OFX_PLUGIN_PATH`, `OLIVE_PLUGIN_PATH` (Olive extensions); platform path separator (Unix `:` / Windows `;`) |

## Bridge layout decisions (frozen in M11 phase 1; extended in phase 2; single-library revision)

After the single-library consolidation, `bridge/` (C ABI imports) and
`ffi.rs` (export layer) were deleted: quercusundo/quercusnode/quercusrender
are now direct path dependencies, and the bridges use direct Rust types.

- **`node::Value`** = the `quercusnode_value` POD from
  `include/node/node.h:93`, fields character-for-character identical
  (`type`/`num`/`den`/`f[4]`; for `type` values see `node_value_type`).
  String-family inputs (k_file/k_text/k_font/k_str_combo) have no POD
  representation; they go through `set_input_string_undoable`.
  `Value::to_node_value`/`Value::from_node_value` convert to/from
  `quercusnode::value::NodeValue` (by POD kind; STRING family has no data).
- **Node binding and write-back** (`node.rs`): process-level identity
  registry `register_node(project, id)` → packed identity
  (`quercusnode::id::NodeId::identity()`) →
  `Instance::bind_node(identity)`; `node_from_identity` looks up
  `NodeRef { project, id }` by identity (weak-reference map, naturally
  invalidated after the project is released).
  `set_input_undoable`/`set_input_string_undoable` build an unexecuted
  `quercusundo::undocommand::UndoCommand` (closure-vtable pattern,
  isomorphic to `quercusnode::ops::set_value_at_time_command`): redo locks
  the project, `graph.get_mut(id)`, `NodeCore::set_standard_value`; undo
  replays the old value snapshotted at creation. Failure (unknown input/
  invalidated node/non-string-family input) returns `NodeBridgeError`, and
  the caller treats it as a no-op.
- **Texture/frame/renderer value types** (`render.rs`):
  `Texture = quercusrender::texture::Texture`, `Frame =
  quercusrender::texture::Frame` (value semantics, drop automatically
  releases the backend token, the original `texture_free`/`frame_free`
  call surface is deleted),
  `Renderer = Arc<dyn quercusrender::backend::GpuContextLike>` (the
  renderer is the backend context). `texture_create(params, pixels,
  linesize)` constructs a CPU wrapper texture (line-stride-aware copy,
  GPU upload deferred to the backend);
  `texture_get_frame`/`texture_get_params`/
  `renderer_is_open_gl` are real implementations. **Retained stub**:
  `texture_id` always 0 — the wgpu backend has no OpenGL texture name, so
  the GL suite's `OpenGLTextureIndex` property and the use_opengl decision
  fall back to the CPU path accordingly.
  After `ClipInstance::set_output_texture` is attached, `store_output_image`
  takes a full-frame copy of the CPU frame (F32 end to end, clear error on
  size mismatch; GPU targets are written back via `GpuContextLike::upload`).
- **undo packaging** = directly through `quercusundo::undocommand::UndoCommand`;
  the paramEditBegin/End edit transaction maintains a depth and accumulated
  multi on `Instance` (C++ `submit_undo_command` semantics: subcommands
  inside a transaction redo immediately and are merged into multi, and
  editEnd redos the whole thing and releases it).
- **param↔node value conversion** (`param.rs`): `set_from_node` (node→plugin,
  mapped by parameter type, dimension truncated/zero-padded),
  `to_node_value` (plugin→node, RGB padded with alpha=1, consistent with
  C++ `RGBInstance::set`). String parameters go through
  `node::set_input_string_undoable`.
- **Node binding**: `Instance::bind_node(identity)` (the Rust side of
  C++ `set_node_handle`); write-back only triggers on
  `ChangeReason::PluginEdited`, and is a no-op when unbound or when the
  identity is not found.

## M11 phase 2: GL path + ofxColour + render driver

### GL render path

- `suites/gl_render.rs`: OfxImageEffectOpenGLRenderSuiteV1
  (clipLoadTexture / clipFreeTexture / flushResources), semantics
  cross-checked against ofxGPURender.h (vendored) and the HostSupport
  plugin side (ofxhImageEffect.cpp:2296-2367). The texture handle is a
  property set (12 properties: OpenGLTextureIndex/OpenGLTextureTarget/
  PixelDepth/Components/PreMultiplication/RenderScale/PixelAspectRatio/
  Bounds/ROD/RowBytes/Field/UniqueIdentifier); the live table holds
  Box<PropertySet> (handle address stable). **OpenGLTextureIndex is a real
  GL texture name** (GL mode; 0 = CPU fallback). The render target binding
  of the Output clip is done by the host (equivalent to C++
  `attach_output_texture`); clipFreeTexture does not delete the Output
  texture (the host deletes it after read-back).
- GL render action: `Instance::render_gl` (coexisting with the CPU `render`)
  — action sequence kOfxActionOpenGLContextAttached → render (in args carry
  kOfxImageEffectPropOpenGLEnabled=1) → OpenGLContextDetached; the render
  result stays on the output GL texture attached to the FBO, and after
  render returns the host **reads it back with glReadPixels** to assemble
  the frame (approach B, see below).
- **GL context rules** (ofxGPURender.h "OpenGL Current Context"): the host
  requires the context current only during
  Render/Begin/EndSequenceRender/Attach/Detach — this implementation's
  convention is that the render driver calls `gl_bridge::acquire` once for
  the whole GL render action (context current until the guard drops), and
  it is always current during suite callbacks.
- Format negotiation: the plugin descriptor declares
  kOfxImageEffectPropOpenGLRenderSupported ("false"/"true"/"needed") and
  kOfxOpenGLPropPixelDepth (optional bit-depth list); the host's
  `pick_gl_pixel_depth` selects according to the pipeline F32 constraint
  (declared list without Float → GL mode infeasible, fall back to CPU).
  The use_opengl decision is in render_driver (plugin GL declaration +
  depth negotiation + bridge availability).

### GL interop bridge (approach B landed, `gl_bridge.rs`)

- **macOS real implementation** (Core OpenGL / CGL, linked directly against
  `OpenGL.framework`, no new crate): process-level shared offscreen context
  (3.2 core profile, offline renderer allowed), `acquire()` globally
  serialized + makes current on the calling thread. For each GL render,
  create an output GL texture (size = target frame; F32 → RGBA32F,
  U8 → RGBA8) + FBO attachment; the plugin draws directly into the FBO;
  after render returns, `glReadPixels` reads back (vertical flip,
  U8 normalized to F32), matching the CPU path output format.
- Input clips in GL mode are uploaded through the bridge as real GL textures
  (deleted by clipFreeTexture).
- GL failure falls back to CPU (consistent with existing failure semantics).
  Linux (EGL)/Windows (WGL) have reserved cfg stubs.
- Verification: `gl_bridge.rs` unit tests (clear a known color → read back
  and assert per pixel, gated by `QUERCUS_GPU_TESTS`) + `tests/gl_render_test.rs`
  end to end (the GL test plugin really takes the GL path, outputs a known
  color).

### ofxColour (OFX 1.4)

- The host descriptor declares OCIO color management
  (kOfxImageEffectPropColourManagementStyle = OCIO + AvailableConfigs =
  ofx-native-v1.5_aces-v1.3_ocio-v2.3); instance-time negotiated properties
  (style/config/OCIOConfig=ocio://default, working space ACEScg conveyed
  via clip color space properties).
- The host writes the input clip's kOfxImageClipPropColourspace as ACEScg;
  GetOutputColourspace action (`Instance::get_output_colourspace`):
  adopt the preference list + resolve "OfxColourspace_<clip>" cross-references
  (`resolve_colourspace`), then write the result back to the output clip
  (`set_output_colourspace`); plugin not implementing it → the color space
  of the first input clip (spec default).
- Descriptor predefines GL/colour properties (ofxGPURender.h/ofxColour.h):
  HostSupport's propSet does not create properties (returns ErrUnknown), the
  host predefines the property universe — phase 1 lacked GL/colour
  declarations, phase 2 filled them in (see "Phase 1 fixes").

### render_driver (pluginrenderer.cpp absorbed)

- `render_driver.rs`: Rust port of `PluginRenderer::render_plugin` (the
  render-flow portion of the 1857-line C++) — instance lock, use_opengl
  decision, multi-input clip collection (effect_input_id / inputs table /
  SimpleSource fallback), getClipPreferences, RoI/RoD setup,
  getRegionOfInterest (on failure continues with the default full frame,
  as C++ does for BadHandle), output clip format, isIdentity short-circuit
  (identity frames copied directly without calling the render action),
  parameter overrides (apply_param_overrides port), CPU/GL rendering and
  output assembly.
- Sequence brackets: `render_begin_sequence`/`render_end_sequence` C ABI
  (quercusrender begins once and then ends after a batch of frames for the
  same instance, with per-frame `render_job` in between).
- Target shape: quercusrender's PluginJob degrades to a single C ABI call
  `quercusplugin_instance_render_job` (new in instance.h, see below).

### include/plugin/instance.h additions (M11 §4, existing signatures unchanged)

- `quercusplugin_instance_render_begin_sequence` / `render_end_sequence`:
  sequence brackets.
- `quercusplugin_instance_render_job(instance, dst, time, clear, interactive,
  effect_input_id, src, inputs[], values[], renderer)`: the single call for
  rendering one frame; two PODs come with it: `quercusplugin_job_value`
  (parameter overrides) / `quercusplugin_job_texture` (input clip textures).
  The GL contract (context current + output attached) is in the header's
  doc comment.

## OFX Interact host (interact + Draw suite, M11 §5)

Host side of plugin custom UI: Interact instances are created in the main
process (UI event host), coexisting with render instances in worker
processes (OFX allows multiple instances of the same plugin).

- **`suites/interact.rs`**:
  - `Instance::new_interact` (task contract): take the plugin-declared
    overlay interact entry (kOfxImageEffectPluginPropOverlayInteractV2
    preferred, V1 next; if undeclared, the plugin main entry) → create
    `Interact` (property table + tagged INTERACT handle) → send
    `kOfxActionNewInteract`. Plugin returns OK → Some; returns
    ReplyDefault with no declared overlay entry (no interact) → None;
    error → None.
  - Lifecycle: `describe` (kOfxActionDescribe == kOfxActionDescribeInteract)
    → `create_instance` (kOfxActionCreateInstance) → events → `destroy`
    (kOfxActionDestroyInstance); **same lifecycle as the effect instance** —
    when the instance is destroyed (`Instance::notify_destroy`), the
    interact is destroyed first along with it.
  - Call surface (host→plugin, return values passed through as-is):
    `draw(viewport, pixel_scale, time, background)`,
    `pen_motion(pen_pos, pen_down)`, `pen_down`, `pen_up`,
    `key_down(key_sym, key_string)`, `key_up`, `idle` (the task contract's
    kOfxInteractActionIdle, a host extension), `gain_focus`/`lose_focus`.
    Pen coordinates are viewport pixels; inArgs additionally carry the
    canonical position (viewport/pixel ratio), viewport position, pressure
    (two-state pen maps to 0/1); key carries kOfxPropKeySym (key codes from
    ofxKeySyms.h) + kOfxPropKeyString. Returns kOfxStatOK = the plugin
    handled it (the host no longer gives the event to other objects),
    ReplyDefault = not handled.
  - `OfxInteractSuiteV1` (ofxInteract.h:534): interactSwapBuffers /
    interactRedraw record the request into `swap_requested`/`redraw_requested`
    (app-side polling surface); interactGetPropertySet returns the interact
    property set handle.
  - The interact property set uses the existing property suite: PixelScale /
    ViewportSize / BackgroundImage / SuggestedColour / SlaveToParam /
    BackgroundColour / BitDepth / HasAlpha (PropertiesInteract of
    ofxInteract.h; ViewportSize uses the OFX 1.3 name "OfxInteractPropViewport",
    BackgroundImage and Idle are task-contract extensions).
- **`suites/draw.rs`** (`OfxDrawSuiteV1`, OFX 1.5, vendored ofxDrawSuite.h):
  getColour / setColour / setLineWidth / setLineStipple / draw / drawText.
  State (colour/lineWidth/stipple) lives in `DrawContext` (created per draw
  action, registered in the live table, removed when draw returns — calling
  outside draw → kOfxStatFailed). GL drawing is **real rendering**: gl_bridge's
  CGL context is 3.2 core (no fixed pipeline), so draw uses a minimal shader
  + VAO/VBO to draw lines/rectangles/polygons/ellipses under an orthographic
  projection, with non-opaque colors composited "over". drawText has no font
  rasterizer → returns kOfxStatErrUnsupported as-is. Non-macOS has no GL
  bridge → kOfxStatFailed.
- **GL context model**: before calling draw, the host keeps the context
  current for the whole action via `gl_bridge::acquire` (same model as the
  WG1 render path); `acquire` supports same-thread reentrancy (test: "one
  acquire covers FBO setup → draw → read-back").
- New vendored headers: `ofxDrawSuite.h`, `ofxKeySyms.h` (kOfxPropKeySym/
  kOfxPropKeyString + kOfxKey_*), `ofxPixels.h` (OfxRGBAColourF) — from
  official openfx (BSD-3-Clause), same origin as the existing vendored
  headers.
- Verification: `tests/interact_test.rs` (lifecycle + item-by-item assertions
  of event parameters against the plugin-side marker records; ReplyDefault
  pass-through of Escape key_down; destroy on instance teardown) + GL draw
  end to end (`QUERCUS_GPU_TESTS` gate: plugin glClear dark background +
  Draw suite setColour(0.9,0.1,0.2,1) + draw(Rectangle) → read back and
  assert rectangle color and background).

app wiring (WG3b) public API: `Instance::new_interact` / `Instance::interact` /
`Instance::describe_interact`, `Interact::{describe, create_instance, draw,
pen_motion, pen_down, pen_up, key_down, key_up, idle, gain_focus, lose_focus,
destroy, props, swap_requested, redraw_requested}`, `host::KEY_*` key codes,
`fetch_suite("OfxInteractSuite"/"OfxDrawSuite", 1)`.

## Comparison with M11 §3.5 acceptance (phase 1+2 status)

| Acceptance item | Status |
| --- | --- |
| Phase 0 golden master all green (empty descriptor diff, CPU render bit-identical) | **Not achieved**: phase 0 infrastructure (snapshot/frame library under `tests/ofx/`) has not been generated yet, the corresponding cases in `golden_test.rs` are `#[ignore]` (reason in each case's doc); the render golden additionally depends on the bridge to the real libquercusrender. |
| Test plugin round-trip + CImg full describe/negotiation smoke test | **Partial**: test plugin round-trip green (suites/ffi/lifecycle/negotiation/colour/gl/driver); the CImg full smoke test is blocked by the local CImg bundle (Natron branch, incompatible property-suite handle semantics) — `#[ignore]`; `system_misc_ofx_bundle_smoke` skips on the local Misc bundle (also a Natron branch, describe returns MissingHostFeature), machines with a compatible bundle assert as usual. |
| Lifecycle: create/destroy pairing, repeated scan, mid-render cancel, no leak while alive | **Achieved**: lifecycle_test all green (including 256 iterations and stub render cancellation). |
| nm: libquercusnode/libquercusrender→libquercusplugin C++ symbols = 0; HostSupport moved out of the build | Verified by the outer CMake wiring (on this crate's side: ffi.rs only exports the C ABI). |
| cargo test (host internal unit tests) + ctest (C ABI layer) both green | **Achieved** (cargo test all green in both modes; ctest belongs to the outer standalone tree). |
| M11 §4: GL path + ofxColour + pluginrenderer absorbed | **Achieved** (verified locally): OpenGLRender suite v1 + render_gl (GL context/format negotiation rules per ofxGPURender.h); ofxColour property family + GetOutputColourspace (ACEScg working space); render_driver absorbed + render_job C ABI (see above). GL golden (EXR tolerance) needs phase 0 infrastructure + manual confirmation on the local GPU (`QUERCUS_GPU_TESTS` gate). |

## Implementation discipline (mandatory reading for implementers)

1. panic must not cross the plugin FFI: the suite dispatch table
   (`suites/mod.rs`) and the host-side plugin call sites each `catch_unwind`
   as a backstop and map error codes.
2. Instances are passed as `Arc<RefBox<Instance>>` (`RefBox` is the facade
   boundary type, see `handle.rs`); raw pointers crossing FFI only point to
   heap-stable objects (props/tag tagging), and their address meaning is
   never exposed.
3. Shared state (plugin cache, instance registry, thread table) is always
   `Mutex`; a plugin may call back into any suite from a thread it starts
   (MultiThread suite lifetime).
4. OFX semantics take HostSupport behavior as the reference frame; each
   negotiation/timing implementation point must be commented with the
   corresponding HostSupport file line numbers.
5. Error codes, handle layouts, two-part strings are character-for-character
   consistent with `include/plugin/*.h`.
6. Dependency policy: **prefer mature third-party crates** (serde/quick-xml/
   thiserror etc.), don't reinvent the wheel. Selection requirements:
   maintained on crates.io, license MIT/Apache-2.0/BSD (GPL-compatible);
   new dependencies register name and rationale in README. Large existing
   C++ libraries like OTIO are never rewritten — continue to use them via
   their C ABI/bridge layer.
7. Cross-phase handles (texture/image handles returned by suites) must point
   to **heap-stable objects** (Box/Arc) — stack temporaries dangle after the
   suite returns (hit in the initial gl_render version of M11 phase 2, see
   "Phase 1 fixes").

## Known technical points (for implementers)

- **C varargs**: `paramGetValue`/`paramSetValue` etc. are variadic.
  Stable Rust cannot *define* C-variadic functions (`c_variadic` is still
  unstable). Two options: (a) use nightly `c_variadic` for these few
  functions; (b) compile a few-dozen-line C shim with build.rs that carries
  only the variadic entry and forwards. Choose (b) to stay on the stable
  toolchain. The `core::ffi::VaList` type itself is stable; only "defining a
  variadic fn" is restricted.
  **Landed**: the message suite's v1/v2 entries are the C shim
  (cbits/ofx_message_shim.c, compiled by build.rs via `cc`; `cc` is a
  build-only dependency, rationale below). The param suite's variadic works
  the same way (same approach when the time comes).
- **Dependency registration**: `cc` (build-dependencies) — the only reliable
  way to compile the C shim (hand-rolled `Command::new("cc")` cannot handle
  cross-platform flags/cross compilation; zero runtime dependencies
  unchanged). M11 phase 2 added no new dependencies.
- **Dependency registration**: `thiserror` (dependencies) — for deriving the
  conventional `Display` + `std::error::Error` impls for the crate error
  enum (src/error.rs); other modules in the project (quercusotio etc.)
  already use the same version ("2"), avoiding hand-written boilerplate.
- **Dependency registration**: `libloading` + `libc` (Linux target only) —
  real GPU zero-copy for OpenFX GL needs EGL/GL dynamic loading (avoiding
  hard links in build environments without GL headers/libraries) and
  `dup(2)` to export fds; both compile only on Linux, without affecting
  macOS/Windows builds.
- Suite function tables are exposed via accessors like `suite_v1()`
  (`static` initialization lives in lazy/OnceLock), `fetch_suite` only looks
  up the table.
- **GL context ownership**: quercusplugin does not own a GL context (the
  C ABI has no make-current function); the GL contract of render_job
  requires the caller (quercusrender) to make the context current and attach
  the output texture. GL texture upload/read-back goes through the
  quercusrender C ABI (which ensures the context internally).

## Phase 1 fixes (phase-1 defects found and fixed during M11 phase 2)

1. **ffi::quercusplugin_instance_render leaks the frame handle**: on the
   render failure path the early `?` return does not release `frame` (the
   retained reference from texture_get_frame). Fix: uniformly go through
   `render_driver::write_output_frame`, which releases on all paths.
2. **CPU copy assumes tight rows**: fetch_image / store_output_image / ffi
   render copy with tight row width — real quercusrender frames may have row
   padding. Fix: add the `frame_linesize_bytes` import, row-first copy by
   linesize.
3. **Descriptor does not predefine GL/colour properties**: the property
   suite's propSet returns ErrUnknown for undefined properties (consistent
   with HostSupport: the host predefines the property universe); the phase 1
   descriptor table lacked kOfxImageEffectPropOpenGLRenderSupported/
   kOfxOpenGLPropPixelDepth/ofxColour family → plugins could not declare
   GL/colour capability. Fix: `init_descriptor_props` fills in the predefined
   ones (default "false"/empty array/None).

## Stage 6a: OpenFX engine wiring (quercusnode/quercusrender/quercusplugin absorbed)

The wiring layer that connects OFX plugins into the quercusnode node factory
and the quercusrender evaluation loop (previously the plugin side had only
host/suite/render driver and did not enter the node graph). Everything lands
in crates/, src/ (app) and gpui/ untouched.

- **`node_factory.rs` (new)**:
  - Instance registry: `register_instance/instance_from_id/unregister_instance`
    (u64 key ↔ `Arc<RefBox<Instance>>`; process-level lifetime, aligned with
    C++ factory ownership; nodes hold the key via
    [`quercusnode::nodes::plugin::PluginInstanceHandle`]).
  - `register_plugin_nodes()`: iterate the host plugin cache, filter context
    preferred (otherwise first), register dynamic node entries via
    `Factory::register_dynamic` (skip existing ids, aligned with C++
    existing_ids). Returns the list of newly registered ids.
  - Parameter translation `build_core`: 15 kinds of OFX parameters →
    quercusnode inputs (type table, default value cache, color semantics
    heuristic, combo ChoiceOrder sorting, secret→hidden, ui_group/ui_page,
    min/max/tooltip, clip→texture input, effect_input selection), aligned
    item by item with engine/node/plugins/plugin.cpp.
  - `install_render_executor()`: install render_driver into
    `quercusrender::eval::set_plugin_executor` (dependency inversion), and
    duplicator into `quercusnode::nodes::plugin::set_plugin_duplicator`.
  - `set_project_extent(w,h)`: basis for converting normalized coordinate
    defaults.
- **`gl_bridge.rs` (approach B landed)**: conclusion of the GL interop
  evaluation for the `texture_id` stub — approach A (wgpu-hal GL interop) is
  infeasible on macOS (the Metal backend has no GL namespace); **approach B
  (independent offscreen GL context + read-back) has landed**: macOS CGL
  offscreen context (process-level shared, `acquire()` serialized + current),
  output GL texture/FBO attachment, glReadPixels read-back frame assembly
  (vertical flip, format conversion), real GL texture upload for input clips.
  The use_opengl decision and the GL suite's OpenGLTextureIndex are wired to
  real GL names. Linux (EGL)/Windows (WGL) reserved cfg stubs. Verification:
  unit tests (clear → read-back assertion) + end to end (GL test plugin real
  rendering) behind the `QUERCUS_GPU_TESTS` gate.
- **`progress.rs`**: new `UiProgressReporter` trait +
  `ReporterFactory` + `set_reporter_factory` (app injection point). When no
  C callback is installed at render time, a silent reporter is used;
  progressStart carries (label,message) and creates a UI reporter on the fly
  through the factory; update returning false means cancel.
- **`suites/timeline.rs`**: new `ViewerTimeInfo` +
  `ActiveViewerProvider` + `set_active_viewer_provider` (app injection
  point). The timeline suite's getTime/getTimeBounds fall back to the active
  viewer time source when the render context is missing.
- **`render_driver.rs`**: `apply_param_overrides` gains Double scalar
  NaN/Inf scrubbing (falls back to default and warns) + Min/Max clamping
  (aligned with pluginrenderer.cpp:155-177).
- **`clip.rs`**: `fetch_image` supports normalizing U8/U16/F16 input frames
  to F32 (aligned with the format conversion path of oliveclip.cpp
  setInputTexture); NaN/Inf scrubbed to 0 during conversion (oliveclip.cpp
  copy_pixels scrub); new hand-written `f16_to_f32` bit conversion.
- **quercusrender `eval.rs`**: `JobSpec::Plugin` expanded to carry
  instance/time/effect_input_id/inputs/values; new `PluginExecutor` +
  `set_plugin_executor` dependency inversion slot; `process_plugin_job`
  produces frames via the executor, falls back to a purple frame (1,0,1,1)
  on failure; `RenderEvalHooks` implements `RenderHooks::resolve` to unwrap
  the `PluginJobPayload` box and execute.
- **quercusnode**: `factory.rs` dynamic registration surface
  (`register_dynamic`/`dynamic_entries`/`create_any` etc.); `nodes/plugin.rs`
  rewritten as the PluginJobPayload value model + duplicator slot;
  `traverser.rs` texture inputs pass-through; `handle.rs` `get_checked<T>`
  discriminates box types by TypeId.

app wiring (stage 6b, out of this crate's scope) connects through these
public entry points:
`node_factory::register_plugin_nodes`,
`progress::set_reporter_factory`,
`suites::timeline::set_active_viewer_provider`,
`node_factory::set_project_extent`,
`quercusnode::factory::Factory::global().dynamic_entries/create_any`.

## Tests

Run (full, including the quercusrender test stubs for the render pixel path):

```sh
cargo test --features test-stubs
cargo tarpaulin --out stdout --features test-stubs   # coverage gate
```

- Default `cargo test` also works: the bridge resolves quercusrender/quercusnode/
  quercusundo symbols via dlsym (gives explainable errors at the render/
  write-back boundary when missing), stub-related cases are not compiled;
- `--features test-stubs`: in-library quercusrender/quercusnode/quercusundo
  stubs (`bridge::*::stub`), all bridge calls in ffi/clip/param (including
  pixel read/write, node write-back, undo packaging) are runnable end to end
  — **coverage is measured in this mode** (M11 phase 1 measured 82.93% line
  coverage; phase 2 gate ≥80%, see COVERAGE);
- Minimal test plugin (cbits/quercus_test_plugin.c, four entries:
  org.quercus.test-plugin / org.quercus.test-plugin.gl /
  org.quercus.test-plugin.identity / org.quercus.test-plugin.interact) is
  compiled by build.rs into a shared library, and
  `common::test_plugin_dir` assembles it into a bundle at run time; related
  cases skip when unavailable;
- The host singleton is lock-free: cases that touch the host surface are
  serialized via `common::with_host`.
- **System-level smoke test**: `bridge_test::system_misc_ofx_bundle_smoke`
  scans→describe→create_instance→negotiate when
  `/Library/OFX/Plugins/Misc.ofx.bundle` exists; skips when the local bundle
  is a Natron branch (describe returns MissingHostFeature) (already recorded
  in README), machines with a compatible bundle assert as usual.
- **GL test case strategy** (`gl_render_test.rs`): in `test-stubs` mode the
  stub GL renderer (renderer_is_open_gl/texture_create/texture_id etc.)
  simulates the GPU — suite round-trips, the GL render path (attach/detach
  pairing, plugin reports texture index via message), and error paths are
  all runnable end to end; default mode (no libquercusrender) asserts
  graceful degradation (GL suite without context → MissingHostFeature,
  render falls back to CPU). The real GPU golden is behind the
  `QUERCUS_GPU_TESTS` gate (`common::gpu_available`), skipped in CI.
- **golden cases** (`golden_test.rs`) are `#[ignore]` because M11 phase 0
  infrastructure (the snapshot and frame library under tests/ofx/) has not
  been generated yet (reason written in each case's doc); the CImg full
  smoke test is additionally blocked by the Natron branch handle semantics
  of the local CImg bundle (see the case comments); the real plugin path is
  covered by the Misc system smoke test.

TDD: test declarations and implementation declarations are frozen together
(tests/, function bodies `todo!()`):

- `handle_test.rs` / `property_test.rs` — infrastructure contract (RefBox
  container, property set semantics, concurrency).
- `suites_test.rs` — round-trip for the eight suites (via the minimal test
  plugin, "plugin perspective" HostSupport compatibility endorsement).
- `ffi_host_test.rs` / `ffi_instance_test.rs` — C ABI export contract (one
  normal and one error path per function; two-part string boundaries).
- `lifecycle_test.rs` — create/destroy pairing, repeated scans, render
  cancellation atomicity, 256 iterations without leaks, concurrent instances.
- `negotiation_test.rs` — the negotiation hard-hit area (component/bit-depth
  matrix, RoD/RoI, isIdentity, field pass-through, sequence brackets).
- `colour_test.rs` — ofxColour property family + GetOutputColourspace
  round-trip (preference adoption, cross-reference resolution, output
  write-back; ACEScg working space).
- `gl_render_test.rs` — GL suite round-trip/error paths/pixel depth
  negotiation matrix + GL render path end to end (graceful skip without GPU,
  see above).
- `interact_test.rs` — Interact host: lifecycle + event call surface
  (item-by-item assertions on the plugin-side marker), Escape state
  pass-through, instance teardown, draw real GL rendering assertions
  (`QUERCUS_GPU_TESTS` gate).
- `render_driver_test.rs` — render_job CPU path (sequence brackets, multiple
  inputs, parameter overrides, isIdentity pass-through pixel assertions,
  degradation without stubs).
- `bridge_test.rs` — the node/render/undo three bridges
  (`--features test-stubs` full chain; default mode takes the degraded path
  of missing dlsym) + Misc system smoke test.
- `golden_test.rs` — descriptor snapshot diff + CPU bit-exact/GL tolerance
  render golden + F32+ACEScg chain assertions (phase 0 infrastructure not
  landed, see above).
- `common/mod.rs` — test plugin location, golden directory, GPU gate, skip
  conventions.
