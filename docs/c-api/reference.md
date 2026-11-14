# Quercus Engine C API Reference

> **Generated file — do not edit.** Regenerate with `python3 tooling/gen_c_api_docs.py --write`.

| Property | Value |
| --- | --- |
| Source header | `crates/quercus-engine/include/quercus_engine.h` |
| Header SHA-256 | `a413e89ed3c7bb0d581268749dbd271b8ea27117f9c88a20867aa55f9298f3c4` |
| Header lines | 1568 |
| Functions | 744 |
| Modules | 26 |
| Named POD structs | 60 |
| Additional inline structs | 1 (`QuercusMat3`) |
| Enums | 14 |
| Callback typedefs | 10 |
| Type aliases | 3 |

`cargo doc` (rustdoc) remains the reference for the **safe Rust SDK** (`quercus-engine-rs`), including wrapper types, constants, and examples. This page is a separate, generated reference for the **C ABI** declared by the checked-in C header, so C/C++ hosts and hand-written FFI bindings have one authoritative listing of every exported function and POD type. It intentionally describes the C surface, not Rust API semantics beyond what the SDK doc comments state.

For host build, linking, and threading guidance, see [`docs/c-api/README.md`](README.md).

## Regenerating and checking

```sh
python3 tooling/gen_c_api_docs.py --write   # regenerate this file
python3 tooling/gen_c_api_docs.py --check   # fail if it is stale
python3 tooling/gen_c_header.py --check     # verify the source header
```

## Conventions

- Time values are seconds: `double` where the prototype says `double`, and exact `Rational` (`num`/`den`) values where it says `Rational`.
- `QuercusRational` is an alias for `Rational`, `QuercusTimeRange` is an alias for `TimeRange`, and `QuercusNodeValue` is an alias for `NodeValue`. The `Quercus*` spellings are the engine-side names; the C ABI uses the shorter names.
- Opaque handles are `void*` / `const void*`. Ownership and lifetime rules are not encoded in C types; follow the owning Rust SDK documentation.
- Byte buffers are passed as a pointer plus an explicit length wherever the prototype shows a `(data, len)` pair. They are not implicitly NUL-terminated unless a `char*` parameter says otherwise.
- Boolean parameters and returns are C `bool` (`<stdbool.h>`) and cross the ABI as 0/1.

## Contents

| Module | Functions |
| --- | ---: |
| [`core`](#module-core) | 118 |
| [`editor`](#module-editor) | 97 |
| [`project`](#module-project) | 28 |
| [`footage`](#module-footage) | 13 |
| [`sequence`](#module-sequence) | 10 |
| [`track`](#module-track) | 17 |
| [`tracklist`](#module-tracklist) | 3 |
| [`block`](#module-block) | 20 |
| [`effect`](#module-effect) | 18 |
| [`timeline`](#module-timeline) | 14 |
| [`undo`](#module-undo) | 14 |
| [`node`](#module-node) | 13 |
| [`codec`](#module-codec) | 91 |
| [`audio`](#module-audio) | 31 |
| [`otio`](#module-otio) | 56 |
| [`ofx`](#module-ofx) | 91 |
| [`plugin`](#module-plugin) | 10 |
| [`render`](#module-render) | 32 |
| [`storage`](#module-storage) | 27 |
| [`task`](#module-task) | 22 |
| [`thumbnail`](#module-thumbnail) | 4 |
| [`waveform`](#module-waveform) | 4 |
| [`worker`](#module-worker) | 2 |
| [`generator`](#module-generator) | 1 |
| [`folder`](#module-folder) | 3 |
| [`video`](#module-video) | 5 |
| **Total** | **744** |

## Enums (14)

The header declares enums in two forms: C `typedef enum` definitions and integer typedefs whose values are macros (`QuercusEditOp`). Enumerator order and values are reproduced verbatim from the header.

### `Codec`

Export codecs, mirroring `quercus_codec::exportcodec::Codec` (the C ABI carries the `repr(i32)` discriminants).

C `enum` (storage type is chosen by the compiler).

```c
typedef enum Codec {
    QUERCUS_CODEC_DNXHD = 0,
    QUERCUS_CODEC_H264 = 1,
    QUERCUS_CODEC_H264RGB = 2,
    QUERCUS_CODEC_H265 = 3,
    QUERCUS_CODEC_OPENEXR = 4,
    QUERCUS_CODEC_PNG = 5,
    QUERCUS_CODEC_PRORES = 6,
    QUERCUS_CODEC_CINEFORM = 7,
    QUERCUS_CODEC_TIFF = 8,
    QUERCUS_CODEC_VP9 = 9,
    QUERCUS_CODEC_MP2 = 10,
    QUERCUS_CODEC_MP3 = 11,
    QUERCUS_CODEC_AAC = 12,
    QUERCUS_CODEC_PCM = 13,
    QUERCUS_CODEC_OPUS = 14,
    QUERCUS_CODEC_VORBIS = 15,
    QUERCUS_CODEC_FLAC = 16,
    QUERCUS_CODEC_SRT = 17,
    QUERCUS_CODEC_AV1 = 18
} Codec;
```

### `Format`

Export container formats, mirroring `quercus_codec::exportformat::Format`.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum Format {
    QUERCUS_FORMAT_DNXHD = 0,
    QUERCUS_FORMAT_MATROSKA = 1,
    QUERCUS_FORMAT_MPEG4VIDEO = 2,
    QUERCUS_FORMAT_OPENEXR = 3,
    QUERCUS_FORMAT_QUICKTIME = 4,
    QUERCUS_FORMAT_PNG = 5,
    QUERCUS_FORMAT_TIFF = 6,
    QUERCUS_FORMAT_WAV = 7,
    QUERCUS_FORMAT_AIFF = 8,
    QUERCUS_FORMAT_MP3 = 9,
    QUERCUS_FORMAT_FLAC = 10,
    QUERCUS_FORMAT_OGG = 11,
    QUERCUS_FORMAT_WEBM = 12,
    QUERCUS_FORMAT_SRT = 13,
    QUERCUS_FORMAT_MPEG4AUDIO = 14
} Format;
```

### `FrameOrigin`

Original producer of an owned frame's bytes.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum FrameOrigin {
    QUERCUS_FRAMEORIGIN_CPU = 0,
    QUERCUS_FRAMEORIGIN_GPU = 1,
    QUERCUS_FRAMEORIGIN_SHAREDMEMORY = 2
} FrameOrigin;
```

### `GpuBackend`

The engine's wgpu backend selection (the C ABI `BackendKind` values).

C `enum` (storage type is chosen by the compiler).

```c
typedef enum GpuBackend {
    QUERCUS_GPUBACKEND_AUTO = 0,
    QUERCUS_GPUBACKEND_METAL = 1,
    QUERCUS_GPUBACKEND_VULKAN = 2,
    QUERCUS_GPUBACKEND_GL = 3,
    QUERCUS_GPUBACKEND_CPU = 4
} GpuBackend;
```

### `NodeCategory`

A node category, as the C++ `Node::CategoryID` ordinal the engine reports for a factory entry.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum NodeCategory {
    QUERCUS_NODECATEGORY_OUTPUT = 0,
    QUERCUS_NODECATEGORY_EFFECT = 1,
    QUERCUS_NODECATEGORY_GENERATOR = 2,
    QUERCUS_NODECATEGORY_INPUT = 3,
    QUERCUS_NODECATEGORY_MATH = 4,
    QUERCUS_NODECATEGORY_COLOR = 5,
    QUERCUS_NODECATEGORY_DISTORT = 6,
    QUERCUS_NODECATEGORY_FILTER = 7,
    QUERCUS_NODECATEGORY_KEYING = 8,
    QUERCUS_NODECATEGORY_OPENFX = 9,
    QUERCUS_NODECATEGORY_TIMELINE = 10,
    QUERCUS_NODECATEGORY_GROUP = 11
} NodeCategory;
```

### `NodeKind`

A node's coarse kind.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum NodeKind {
    QUERCUS_NODEKIND_FOLDER = 1,
    QUERCUS_NODEKIND_FOOTAGE = 2,
    QUERCUS_NODEKIND_SEQUENCE = 3,
    QUERCUS_NODEKIND_TRACK = 4,
    QUERCUS_NODEKIND_CLIP = 5,
    QUERCUS_NODEKIND_GAP = 6,
    QUERCUS_NODEKIND_TRANSITION = 7,
    QUERCUS_NODEKIND_ADJUSTMENT = 8,
    QUERCUS_NODEKIND_TRACKLIST = 9
} NodeKind;
```

### `PixelFormat`

Pixel formats (values identical to quercus-core's `PixelFormat`, carried as i32 over the ABI).

C `enum` (storage type is chosen by the compiler).

```c
typedef enum PixelFormat {
    QUERCUS_PIXELFORMAT_INVALID = -1,
    QUERCUS_PIXELFORMAT_U8 = 0,
    QUERCUS_PIXELFORMAT_U10 = 1,
    QUERCUS_PIXELFORMAT_U16 = 2,
    QUERCUS_PIXELFORMAT_F16 = 3,
    QUERCUS_PIXELFORMAT_F32 = 4
} PixelFormat;
```

### `ProxyState`

On-disk state of a proxy file.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum ProxyState {
    QUERCUS_PROXYSTATE_MISSING = 0,
    QUERCUS_PROXYSTATE_GENERATING = 1,
    QUERCUS_PROXYSTATE_READY = 2,
    QUERCUS_PROXYSTATE_FAILED = 3
} ProxyState;
```

### `QuercusEditOp`

Numeric operation selector; discriminants are part of the native ABI.

Underlying C type: `uint32_t`; values are `#define` constants.

```c
typedef uint32_t QuercusEditOp;

#define QUERCUS_EDITOP_ADDTRACK 0U
#define QUERCUS_EDITOP_REMOVETRACK 1U
#define QUERCUS_EDITOP_SETTRACKHEIGHT 2U
#define QUERCUS_EDITOP_SETTRACKMUTED 3U
#define QUERCUS_EDITOP_SETTRACKLOCKED 4U
#define QUERCUS_EDITOP_CREATESEQUENCE 5U
#define QUERCUS_EDITOP_SETSEQUENCEPARAMETERS 6U
#define QUERCUS_EDITOP_SETSEQUENCEPLAYHEAD 7U
#define QUERCUS_EDITOP_CREATEFOLDER 8U
#define QUERCUS_EDITOP_CREATETEXTFOOTAGE 9U
#define QUERCUS_EDITOP_IMPORTFOOTAGE 10U
#define QUERCUS_EDITOP_ENSURESEQUENCESMOUNTED 11U
#define QUERCUS_EDITOP_REPROBEUNPROBEDFOOTAGE 12U
#define QUERCUS_EDITOP_CREATEADJUSTMENTLAYER 13U
#define QUERCUS_EDITOP_PLACEFOOTAGECLIP 14U
#define QUERCUS_EDITOP_PLACENESTEDSEQUENCECLIP 15U
#define QUERCUS_EDITOP_PLACETEXTCLIP 16U
#define QUERCUS_EDITOP_PLACEGENERATORCLIP 17U
#define QUERCUS_EDITOP_PLACEFOOTAGECLIPSLINKED 18U
#define QUERCUS_EDITOP_ADDTRANSITIONATSEAM 19U
#define QUERCUS_EDITOP_ADDTRANSITIONATEDGE 20U
#define QUERCUS_EDITOP_ADDDEFAULTTRANSITION 21U
#define QUERCUS_EDITOP_SETTRANSITIONLENGTH 22U
#define QUERCUS_EDITOP_SPLITCLIP 23U
#define QUERCUS_EDITOP_SPLITCLIPSPRESERVINGLINKS 24U
#define QUERCUS_EDITOP_TRIMCLIP 25U
#define QUERCUS_EDITOP_RIPPLETRIMCLIP 26U
#define QUERCUS_EDITOP_ROLLEDIT 27U
#define QUERCUS_EDITOP_SLIDECLIP 28U
#define QUERCUS_EDITOP_SLIPCLIP 29U
#define QUERCUS_EDITOP_MOVECLIP 30U
#define QUERCUS_EDITOP_MOVECLIPTOTRACK 31U
#define QUERCUS_EDITOP_MOVECLIPWITHLINKS 32U
#define QUERCUS_EDITOP_SETCLIPSLINKED 33U
#define QUERCUS_EDITOP_DELETECLIP 34U
#define QUERCUS_EDITOP_RIPPLEDELETECLIP 35U
#define QUERCUS_EDITOP_DELETEAREA 36U
#define QUERCUS_EDITOP_ROLLCLIPEDGETO 37U
#define QUERCUS_EDITOP_MARKERADD 38U
#define QUERCUS_EDITOP_MARKERREMOVE 39U
#define QUERCUS_EDITOP_WORKAREASET 40U
#define QUERCUS_EDITOP_WORKAREASETUNDOABLE 41U
#define QUERCUS_EDITOP_REMOVENODE 42U
#define QUERCUS_EDITOP_CONNECT 43U
#define QUERCUS_EDITOP_DISCONNECT 44U
#define QUERCUS_EDITOP_SETCONTEXTPOSITION 45U
#define QUERCUS_EDITOP_COPYCLIPS 46U
#define QUERCUS_EDITOP_PASTECLIPS 47U
#define QUERCUS_EDITOP_SETBLOCKSENABLED 48U
#define QUERCUS_EDITOP_SETCLIPSSPEED 49U
#define QUERCUS_EDITOP_RENAMEBLOCKS 50U
#define QUERCUS_EDITOP_DUPLICATECLIPS 51U
#define QUERCUS_EDITOP_PASTECLIPSINSERT 52U
#define QUERCUS_EDITOP_NESTCLIPS 53U
#define QUERCUS_EDITOP_EFFECTINSERT 54U
#define QUERCUS_EDITOP_EFFECTREMOVE 55U
#define QUERCUS_EDITOP_EFFECTMOVE 56U
#define QUERCUS_EDITOP_EFFECTSETENABLED 57U
#define QUERCUS_EDITOP_SETNODESTANDARDVALUE 58U
#define QUERCUS_EDITOP_UPSERTNODEKEYFRAME 59U
#define QUERCUS_EDITOP_REMOVENODEKEYFRAME 60U
#define QUERCUS_EDITOP_SETNODEVALUEATTIME 61U
#define QUERCUS_EDITOP_PUSHNODEBUTTON 62U
#define QUERCUS_EDITOP_SETNODETEXTVALUE 63U
#define QUERCUS_EDITOP_REPLACEFOOTAGE 64U
#define QUERCUS_EDITOP_DELETEENTRY 65U
#define QUERCUS_EDITOP_SETFOOTAGEPROXYPARAMS 66U
#define QUERCUS_EDITOP_CLEARFOOTAGEPROXYPARAMS 67U
#define QUERCUS_EDITOP_CREATESEQUENCEFROMFOOTAGE 68U
#define QUERCUS_EDITOP_DELETEPROXY 69U
#define QUERCUS_EDITOP_SETPROXYENABLED 70U
#define QUERCUS_EDITOP_SETMULTICAMENABLED 71U
#define QUERCUS_EDITOP_SWITCHMULTICAM 72U
#define QUERCUS_EDITOP_CREATEMULTICAMSEQUENCE 73U
#define QUERCUS_EDITOP_CREATENODE 74U
```

### `RetrieveAudioStatus`

The outcome of an audio retrieve.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum RetrieveAudioStatus {
    QUERCUS_RETRIEVEAUDIOSTATUS_SUCCESS = 0,
    QUERCUS_RETRIEVEAUDIOSTATUS_INVALIDRANGE = 1,
    QUERCUS_RETRIEVEAUDIOSTATUS_UNSUPPORTED = 2,
    QUERCUS_RETRIEVEAUDIOSTATUS_CONFORMNEEDED = 3,
    QUERCUS_RETRIEVEAUDIOSTATUS_DECODERERROR = 4
} RetrieveAudioStatus;
```

### `SampleFormat`

Sample formats, mirroring `quercus_core::SampleFormat`'s explicit discriminants (the C ABI carries them as i32).

C `enum` (storage type is chosen by the compiler).

```c
typedef enum SampleFormat {
    QUERCUS_SAMPLEFORMAT_INVALID = -1,
    QUERCUS_SAMPLEFORMAT_U8PLANAR = 0,
    QUERCUS_SAMPLEFORMAT_S16PLANAR = 1,
    QUERCUS_SAMPLEFORMAT_S32PLANAR = 2,
    QUERCUS_SAMPLEFORMAT_S64PLANAR = 3,
    QUERCUS_SAMPLEFORMAT_F32PLANAR = 4,
    QUERCUS_SAMPLEFORMAT_F64PLANAR = 5,
    QUERCUS_SAMPLEFORMAT_U8 = 6,
    QUERCUS_SAMPLEFORMAT_S16 = 7,
    QUERCUS_SAMPLEFORMAT_S32 = 8,
    QUERCUS_SAMPLEFORMAT_S64 = 9,
    QUERCUS_SAMPLEFORMAT_F32 = 10,
    QUERCUS_SAMPLEFORMAT_F64 = 11
} SampleFormat;
```

### `StreamType`

A stream's kind within a `FootageDescription`.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum StreamType {
    QUERCUS_STREAMTYPE_VIDEO = 0,
    QUERCUS_STREAMTYPE_AUDIO = 1,
    QUERCUS_STREAMTYPE_SUBTITLE = 2
} StreamType;
```

### `TaskKind`

Kinds of background tasks the codec module can request.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum TaskKind {
    QUERCUS_TASKKIND_CONFORM = 0,
    QUERCUS_TASKKIND_PROXY = 1
} TaskKind;
```

### `VideoPixelFormat`

Pixel layout of an owned frame's byte buffer.

C `enum` (storage type is chosen by the compiler).

```c
typedef enum VideoPixelFormat {
    QUERCUS_VIDEOPIXELFORMAT_F32RGBA = 0,
    QUERCUS_VIDEOPIXELFORMAT_BGRA8 = 1
} VideoPixelFormat;
```

## Structs (60 named POD structs)

All structs are plain data (`#[repr(C)]` in Rust). Field declarations are reproduced verbatim from the header. The header also type-checks the layout of `Rational` (16 bytes), `TimeRange` (32 bytes), `NodeValue` (56 bytes), and `StorageProjectHandle` (32 bytes) with static assertions.

### `AudioBlockInfo`

Timing, output layout and interleaved-element count of an audio block.

```c
struct AudioBlockInfo {
    TimeRange range;
    int32_t sample_rate;
    uint64_t channel_layout;
    int32_t channel_count;
    size_t sample_count;
};
```

### `AudioParams`

Audio stream parameters. `repr(C)` mirror of the engine's `QuercusAudioParams`; passed by value over the ABI.

```c
struct AudioParams {
    int32_t sample_rate;
    uint64_t channel_layout;
    int32_t format;
};
```

### `AudioStreamParams`

An audio stream's parameters as reported by a probe. Layout matches the engine's `QuercusAudioStreamParams`.

```c
struct AudioStreamParams {
    int32_t sample_rate;
    uint64_t channel_layout;
    int32_t format;
    int32_t stream_index;
    int64_t duration;
    int32_t time_base_num;
    int32_t time_base_den;
};
```

### `ChannelStats`

Per-channel level statistics.

```c
struct ChannelStats {
    double peak_linear;
    double peak_db;
    double rms_linear;
    double rms_db;
    double vu_db;
};
```

### `ClipSyncInfo`

Timeline/media timing and optional source timecode for one block.

```c
struct ClipSyncInfo {
    Rational timeline_in;
    Rational timeline_out;
    Rational media_in;
    Rational source_start_time;
    bool has_source_start_time;
    double media_length_seconds;
};
```

### `ControlPoint`

A curve control point: `key` is the parametric position, `value` the result, `slope` the first derivative (Hermite tangent; recomputed automatically by the mutating calls).

```c
struct ControlPoint {
    double key;
    double value;
    double slope;
};
```

### `EditorMarkerRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct EditorMarkerRaw {
    TimeRange time;
    int32_t color;
};
```

### `EncodingParams`

`repr(C)` mirror of the engine's encoding parameters for `export`. Zeroed fields are disabled/defaults.

```c
struct EncodingParams {
    int32_t format;
    bool video_enabled;
    int32_t video_codec;
    int32_t video_width;
    int32_t video_height;
    int32_t video_time_base_num;
    int32_t video_time_base_den;
    int32_t video_pixel_format;
    bool audio_enabled;
    int32_t audio_codec;
    int32_t audio_sample_rate;
    uint64_t audio_channel_layout;
    bool subtitles_enabled;
    int32_t export_length_num;
    int32_t export_length_den;
    bool has_custom_range;
    int32_t custom_range_in_num;
    int32_t custom_range_in_den;
    int32_t custom_range_out_num;
    int32_t custom_range_out_den;
    int64_t video_bit_rate;
    int64_t audio_bit_rate;
    bool color_override_enabled;
    int32_t color_primaries;
    int32_t color_trc;
    int32_t color_space;
};
```

### `GraphEdgeRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct GraphEdgeRaw {
    uint64_t from;
    uint64_t to;
    int32_t element;
    bool synthetic;
};
```

### `GraphNodeRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct GraphNodeRaw {
    uint64_t id;
    int32_t role;
    bool protected_;
    bool enabled;
    bool has_position;
    double x;
    double y;
    int32_t input_count;
    bool has_output;
    bool output_connected;
};
```

### `GraphPortRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct GraphPortRaw {
    size_t declaration_index;
    int32_t media_type;
    bool connected;
    bool connectable;
};
```

### `KeyframeRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct KeyframeRaw {
    Rational time;
    ValueRaw value;
    int32_t interpolation;
    double bezier_in_x;
    double bezier_in_y;
    double bezier_out_x;
    double bezier_out_y;
};
```

### `MinMax`

One channel's minimum and maximum amplitude within one window.

```c
struct MinMax {
    float min;
    float max;
};
```

### `NodeValue`

A node parameter value (POD; layout matches the engine's `QuercusNodeValue`). String-typed values are not bridged.

```c
struct NodeValue {
    int32_t kind;
    int64_t num;
    int64_t den;
    double f[4];
};
```

### `OffsetEstimate`

Candidate offset and normalized correlation confidence.

```c
struct OffsetEstimate {
    int64_t offset_samples;
    double confidence;
    bool valid;
};
```

### `ParamRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct ParamRaw {
    int32_t value_type;
    uint32_t flags;
    int32_t property_count;
    int32_t value_count;
    int32_t track_count;
};
```

### `Primaries`

A set of RGB primaries plus the white point.

```c
struct Primaries {
    Xy red;
    Xy green;
    Xy blue;
    Xy white;
};
```

### `PropertyRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct PropertyRaw {
    ValueRaw value;
};
```

### `ProxyParams`

Proxy generation parameters. `repr(C)` mirror of the engine's `ProxyParams`; crosses the ABI by value. The string fields are fixed 32-byte NUL-terminated buffers, exactly like the C struct.

```c
struct ProxyParams {
    int32_t width;
    int32_t height;
    int32_t divider;
    int32_t version;
    int32_t crf;
    int32_t include_audio;
    uint8_t extension[32];
    uint8_t preset[32];
};
```

### `QuercusClipAudioEffectInfo`

No Rust SDK doc comment is available for this pod struct.

```c
struct QuercusClipAudioEffectInfo {
    uint8_t type_id[128];
    bool enabled;
};
```

### `QuercusClipAudioState`

No Rust SDK doc comment is available for this pod struct.

```c
struct QuercusClipAudioState {
    double gain;
    double pan;
    int64_t fade_in_num;
    int64_t fade_in_den;
    int64_t fade_out_num;
    int64_t fade_out_den;
    int32_t effect_count;
};
```

### `QuercusClipboardClip`

Native clipboard entry; timestamps use the source sequence's frame timebase.

```c
struct QuercusClipboardClip {
    uint64_t footage;
    int32_t kind;
    size_t track_index;
    int64_t media_in_ts;
    int64_t start_ts;
    int64_t length_ts;
    double speed;
};
```

### `QuercusCodecEncodingParams`

Encoding parameters. Layout matches the engine's `EncodingParams` (which mirrors quercus-codec's `repr(C)` struct field for field); the three inner enums cross as their i32 discriminants (`crate::types::PixelFormat`, `crate::audio::SampleFormat`, scaling method).

```c
struct QuercusCodecEncodingParams {
    uint8_t filename[1024];
    int32_t format;
    int32_t video_enabled;
    int32_t video_codec;
    int32_t video_width;
    int32_t video_height;
    int32_t video_time_base_num;
    int32_t video_time_base_den;
    int32_t video_pixel_format;
    int32_t video_interlacing;
    int32_t video_pixel_aspect_num;
    int32_t video_pixel_aspect_den;
    int64_t video_bit_rate;
    int64_t video_min_bit_rate;
    int64_t video_max_bit_rate;
    int64_t video_buffer_size;
    int32_t video_threads;
    uint8_t video_pix_fmt[64];
    int32_t video_is_image_sequence;
    int32_t video_scaling_method;
    int32_t audio_enabled;
    int32_t audio_codec;
    int32_t audio_sample_rate;
    uint64_t audio_channel_layout;
    int32_t audio_sample_format;
    int64_t audio_bit_rate;
    int32_t subtitles_enabled;
    int32_t subtitles_codec;
    int32_t subtitles_are_sidecar;
    int32_t subtitles_sidecar_format;
    uint8_t color_transform_output[256];
    int32_t export_length_num;
    int32_t export_length_den;
    int32_t has_custom_range;
    int64_t custom_range_in_num;
    int64_t custom_range_in_den;
    int64_t custom_range_out_num;
    int64_t custom_range_out_den;
    int32_t color_primaries;
    int32_t color_trc;
    int32_t color_space;
    int32_t color_range;
};
```

### `QuercusCodecProxyResult`

`repr(C)` mirror of the engine's `QuercusCodecProxyResult`.

```c
struct QuercusCodecProxyResult {
    int32_t state;
    uint8_t filename[1024];
};
```

### `QuercusCodecTaskRequest`

`repr(C)` mirror of the engine's `QuercusCodecTaskRequest`.

```c
struct QuercusCodecTaskRequest {
    int32_t kind;
    const char* input_filename;
    const char* output_filename;
    int32_t stream_index;
    int32_t sample_rate;
    uint64_t channel_layout;
    int32_t sample_format;
    int32_t proxy_width;
    int32_t proxy_height;
};
```

### `QuercusConfigSourceVTable`

`repr(C)` mirror of the engine's `QuercusConfigSourceVTable`. The field order is the ABI and must stay byte-identical with the engine struct.

```c
struct QuercusConfigSourceVTable {
    uint32_t struct_size;
    uint32_t abi_version;
    int32_t (* load)(void*, uint8_t*, size_t);
    bool (* save)(void*, const uint8_t*, size_t);
};
```

### `QuercusEditParams`

Generic operation parameters and borrowed input/output buffers.

```c
struct QuercusEditParams {
    uint64_t a;
    uint64_t b;
    uint64_t c;
    uint64_t d;
    int64_t in_ts;
    int64_t out_ts;
    int64_t media_in_ts;
    int64_t new_frame;
    int32_t kind;
    size_t track_index;
    bool enabled;
    bool start_edge;
    bool linked;
    bool ripple;
    double height;
    double speed;
    double x;
    double y;
    Rational rate;
    TimeRange range;
    TimeRange old_range;
    int32_t element;
    NodeValue node_value;
    double bezier_in_x;
    double bezier_in_y;
    double bezier_out_x;
    double bezier_out_y;
    ProxyParams proxy_params;
    const uint8_t* name;
    size_t name_len;
    const uint8_t* path;
    size_t path_len;
    const uint8_t* input_id;
    size_t input_id_len;
    const uint64_t* ids;
    size_t ids_len;
    const QuercusTrackPlacement* placements;
    size_t placements_len;
    const double* values;
    size_t values_len;
    uint64_t* result_ids;
    size_t result_ids_capacity;
    QuercusClipboardClip* result_clipboard;
    size_t result_clipboard_capacity;
};
```

### `QuercusEditResult`

Fixed-size result; variable arrays are copied into `QuercusEditParams` buffers.

```c
struct QuercusEditResult {
    uint64_t primary;
    int64_t value;
    Rational rational;
    int64_t revision;
    size_t ids_len;
    size_t clipboard_len;
};
```

### `QuercusLevelStats`

No Rust SDK doc comment is available for this pod struct.

```c
struct QuercusLevelStats {
    double max_peak_linear;
    double integrated_lufs;
    int32_t silence;
};
```

### `QuercusLoudness`

The engine's loudness measurement POD. Layout matches `QuercusLoudness` in the engine C ABI.

```c
struct QuercusLoudness {
    double integrated_lufs;
    double peak_dbfs;
};
```

### `QuercusMediaIoError`

`repr(C)` mirror of the engine's `QuercusMediaIoError`.

```c
struct QuercusMediaIoError {
    int32_t code;
    uint8_t message[512];
};
```

### `QuercusMediaIoVTable`

`repr(C)` mirror of the engine's `QuercusMediaIoVTable`. The field order is the ABI and must stay byte-identical with the engine struct.

```c
struct QuercusMediaIoVTable {
    uint32_t struct_size;
    uint32_t abi_version;
    const char* name;
    int32_t (* can_open)(void*, const uint8_t*, size_t, QuercusMediaIoError*);
    int32_t (* open)(void*, const uint8_t*, size_t, void**, QuercusMediaIoError*);
    int64_t (* read)(void*, void*, uint8_t*, size_t, QuercusMediaIoError*);
    int64_t (* seek)(void*, void*, int64_t, int32_t, QuercusMediaIoError*);
    void (* close)(void*, void*);
};
```

### `QuercusOfxCurvePoint`

No Rust SDK doc comment is available for this pod struct.

```c
struct QuercusOfxCurvePoint {
    double key;
    double value;
    double slope;
};
```

### `QuercusOfxRectD`

No Rust SDK doc comment is available for this pod struct.

```c
struct QuercusOfxRectD {
    double x1;
    double y1;
    double x2;
    double y2;
};
```

### `QuercusOtioTimeRange`

A duration and its start time (OTIO `TimeRange`).

```c
struct QuercusOtioTimeRange {
    RationalTime start_time;
    RationalTime duration;
};
```

### `QuercusPluginDescriptor`

`repr(C)` mirror of the engine's `QuercusPluginDescriptor`.

```c
struct QuercusPluginDescriptor {
    uint8_t id[128];
    uint8_t name[256];
    uint8_t version[64];
    uint8_t author[256];
    uint8_t description[1024];
};
```

### `QuercusStorageError`

`repr(C)` mirror of the engine's `QuercusStorageError`.

```c
struct QuercusStorageError {
    int32_t code;
    uint8_t message[512];
};
```

### `QuercusStorageProjectInfo`

`repr(C)` mirror of the engine's `QuercusStorageProjectInfo` (a library row in a caller-owned array). Field order is the ABI.

```c
struct QuercusStorageProjectInfo {
    uint8_t uuid[40];
    uint8_t name[256];
    int64_t created_at;
    int64_t modified_at;
    int32_t schema_ver;
    int32_t reserved;
};
```

### `QuercusStorageTransportVTable`

`repr(C)` mirror of the engine-side transport vtable.

```c
struct QuercusStorageTransportVTable {
    uint32_t struct_size;
    uint32_t abi_version;
    const char* name;
    const char* scheme;
    uint32_t write_policy;
    int32_t (* can_handle)(void*, const uint8_t*, size_t, QuercusStorageError*);
    int32_t (* load)(void*, const uint8_t*, size_t, uint8_t**, size_t*, int32_t*, QuercusStorageError*);
    void (* free_bytes)(void*, uint8_t*, size_t);
    int32_t (* save)(void*, const uint8_t*, size_t, const uint8_t*, size_t, uint32_t, QuercusStorageError*);
    int32_t (* library_list)(void*, const uint8_t*, size_t, QuercusStorageProjectInfo*, size_t, size_t*, QuercusStorageError*);
    int32_t (* library_delete)(void*, const uint8_t*, size_t, const uint8_t*, size_t, QuercusStorageError*);
    int32_t (* library_rename)(void*, const uint8_t*, size_t, const uint8_t*, size_t, const uint8_t*, size_t, QuercusStorageError*);
    int32_t (* library_snapshot)(void*, const uint8_t*, size_t, const uint8_t*, size_t, QuercusStorageError*);
};
```

### `QuercusSubtitleStyle`

POD mirror of the engine's `QuercusSubtitleStyle` C struct.

```c
struct QuercusSubtitleStyle {
    uint8_t font_family[128];
    float font_size;
    double color[4];
    double outline_color[4];
    bool outline_enabled;
    float outline_width;
    int32_t align;
    float margin_h;
    float margin_v;
    float line_spacing;
};
```

### `QuercusTrackPlacement`

Native `(track kind, track index)` pair used by linked placement.

```c
struct QuercusTrackPlacement {
    int32_t kind;
    size_t index;
};
```

### `Rational`

A rational number, always reduced with a non-negative denominator; `0/0` is the null/invalid sentinel. Layout matches the engine's `QuercusRational`.

```c
struct Rational {
    int64_t num;
    int64_t den;
};
```

### `RationalTime`

A time value and its frame rate (OTIO `RationalTime`).

```c
struct RationalTime {
    double value;
    double rate;
};
```

### `RawMulticamState`

No Rust SDK doc comment is available for this pod struct.

```c
struct RawMulticamState {
    uint64_t sequence;
    uint64_t multicam;
    uint64_t clip;
    int32_t source_count;
    int32_t current_source;
};
```

### `RawProxyStatus`

No Rust SDK doc comment is available for this pod struct.

```c
struct RawProxyStatus {
    int32_t state;
    bool enabled;
    bool has_custom_params;
    bool can_generate;
    bool has_proxy;
};
```

### `RectD`

A rectangle in canonical coordinates.

```c
struct RectD {
    double x1;
    double y1;
    double x2;
    double y2;
};
```

### `SourceClip`

Source-timecode and media-in metadata used for timeline alignment.

```c
struct SourceClip {
    Rational source_start_time;
    Rational media_in;
    bool has_source_start_time;
};
```

### `StorageProjectHandle`

Owned C-compatible storage project handle used only for same-instance migration. Layout matches `quercus_core::handle::CHandle` exactly.

```c
struct StorageProjectHandle {
    void* ctx;
    void (* addref)(void*);
    void (* release)(void*);
    uint32_t abi_version;
};
```

### `StreamAudioParams`

An audio stream's parameters as carried by a footage node.

```c
struct StreamAudioParams {
    int32_t sample_rate;
    uint64_t channel_layout;
    int32_t format;
};
```

### `StreamVideoParams`

A video stream's parameters as carried by a footage node.

```c
struct StreamVideoParams {
    int32_t width;
    int32_t height;
    Rational frame_rate;
    int32_t pixel_format;
    int32_t channels;
    int32_t interlaced;
};
```

### `StretchEstimate`

Candidate playback-rate correction plus offset and confidence.

```c
struct StretchEstimate {
    double rate;
    int64_t offset_samples;
    double confidence;
    bool valid;
};
```

### `SubtitleParams`

A subtitle stream's parameters as reported by a probe (the subtitle entries themselves are content, not exposed here). Layout matches the engine's `QuercusSubtitleParams`.

```c
struct SubtitleParams {
    int32_t stream_index;
    int32_t enabled;
    int32_t entry_count;
};
```

### `SyncPlacement`

Validated timeline placement produced by a synchronization policy.

```c
struct SyncPlacement {
    Rational timeline_in;
    bool valid;
};
```

### `ThumbnailInfo`

Thumbnail dimensions and packed pixel-buffer length.

```c
struct ThumbnailInfo {
    int32_t width;
    int32_t height;
    size_t pixel_count;
};
```

### `TimeRange`

A half-open time range [in, out) (normalized so `in <= out`). Layout matches the engine's `QuercusTimeRange`.

```c
struct TimeRange {
    Rational in_;
    Rational out;
};
```

### `ValueRaw`

No Rust SDK doc comment is available for this pod struct.

```c
struct ValueRaw {
    bool has_pod;
    NodeValue pod;
    bool has_text;
};
```

### `VideoFrameInfo`

Geometry, origin, pixel format and byte count of an owned frame.

```c
struct VideoFrameInfo {
    int32_t width;
    int32_t height;
    int32_t linesize;
    FrameOrigin origin;
    VideoPixelFormat pixel_format;
    size_t byte_count;
};
```

### `VideoParams`

A video parameter set. Layout matches the engine's `QuercusVideoParams`; enums cross as their discriminant ints (`format` is a `PixelFormat` code, `interlacing` 0/1/2 = none/top/bottom, `video_type` 0/1/2 = video/still/sequence, `color_range` 0/1 = limited/full, bools as 0/1).

```c
struct VideoParams {
    int32_t width;
    int32_t height;
    int32_t depth;
    int32_t time_base_num;
    int32_t time_base_den;
    int32_t frame_rate_num;
    int32_t frame_rate_den;
    int32_t pixel_aspect_num;
    int32_t pixel_aspect_den;
    int32_t format;
    int32_t channel_count;
    int32_t interlacing;
    int32_t divider;
    int32_t enabled;
    float x;
    float y;
    int32_t stream_index;
    int32_t video_type;
    int64_t start_time;
    int64_t duration;
    int32_t premultiplied_alpha;
    int32_t color_range;
    int32_t color_primaries;
    int32_t color_transfer;
    uint8_t colorspace[64];
};
```

### `WaveformInfo`

Waveform layout and point-count metadata.

```c
struct WaveformInfo {
    int32_t channels;
    int32_t sample_rate;
    int32_t samples_per_point;
    size_t point_count;
};
```

### `Xy`

A chromaticity coordinate (white point / primary).

```c
struct Xy {
    float x;
    float y;
};
```

### `QuercusMat3`

Inline struct definition (declared and defined in one typedef).

3x3 color matrix returned by the colormath helpers.

```c
typedef struct QuercusMat3 { float v[3][3]; } QuercusMat3;
```

## Type aliases (3)

### `QuercusNodeValue`

Alias for `NodeValue`; the engine-side spelling used in prose.

```c
typedef NodeValue QuercusNodeValue;
```

### `QuercusRational`

Alias for `Rational`; the engine-side spelling used in prose.

```c
typedef Rational QuercusRational;
```

### `QuercusTimeRange`

Alias for `TimeRange`; the engine-side spelling used in prose.

```c
typedef TimeRange QuercusTimeRange;
```

## Callback typedefs (10)

Callbacks cross the ABI as plain C function pointers. `userdata` is passed through unchanged; it is never dereferenced by the engine.

### `CommandFn`

No Rust SDK doc comment is available for this callback typedef.

```c
typedef void (*CommandFn)(const char* command_id, void* userdata);
```

### `LogDropFn`

Raw sink-drop callback signature.

```c
typedef void (*LogDropFn)(void* userdata);
```

### `LogFn`

Raw sink-callback signature (`repr(C)`-free: plain pointers and ints).

```c
typedef void (*LogFn)(int32_t level, const uint8_t* message, size_t len, void* userdata);
```

### `ProgressEndFn`

No Rust SDK doc comment is available for this callback typedef.

```c
typedef void (*ProgressEndFn)(void* userdata);
```

### `ProgressFn`

No Rust SDK doc comment is available for this callback typedef.

```c
typedef bool (*ProgressFn)(const char* label, const char* message, double fraction, void* userdata);
```

### `QuercusConfigDropFn`

Raw config-source cleanup callback signature.

```c
typedef void (*QuercusConfigDropFn)(void* userdata);
```

### `QuercusConfigLoadFn`

Raw config-source load callback signature: snprintf-style, returning the full TOML byte length excluding the NUL.

```c
typedef int32_t (*QuercusConfigLoadFn)(void* userdata, uint8_t* buf, size_t cap);
```

### `QuercusConfigSaveFn`

Raw config-source save callback signature (borrowed UTF-8 TOML).

```c
typedef bool (*QuercusConfigSaveFn)(void* userdata, const uint8_t* data, size_t len);
```

### `TaskSubmitFn`

No Rust SDK doc comment is available for this callback typedef.

```c
typedef int32_t (*TaskSubmitFn)(const QuercusCodecTaskRequest* req, void* userdata);
```

### `UndoObserverFn`

No Rust SDK doc comment is available for this callback typedef.

```c
typedef void (*UndoObserverFn)(void* userdata);
```

<a id="module-core"></a>
## Module: `core` (118 functions)

### `quercus_core_cancelatom_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_core_cancelatom_add_ref(const void* this_);
```
### `quercus_core_cancelatom_cancel` — Set the token's cancel flag so every clone observes cancellation.
```c
void quercus_core_cancelatom_cancel(const void* this_);
```
### `quercus_core_cancelatom_heard_cancel` — Whether any consumer has observed the cancel flag.
```c
bool quercus_core_cancelatom_heard_cancel(const void* this_);
```
### `quercus_core_cancelatom_is_cancelled` — Reads the cancel flag (reading a set flag records the cancellation as heard).
```c
bool quercus_core_cancelatom_is_cancelled(const void* this_);
```
### `quercus_core_cancelatom_new` — A not-cancelled token.
```c
void* quercus_core_cancelatom_new(void);
```
### `quercus_core_cancelatom_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_core_cancelatom_release(const void* this_);
```
### `quercus_core_color_config_path` — The active OCIO config path; `None` when none is loaded (or the bundled config is in use).
```c
int32_t quercus_core_color_config_path(uint8_t* buf, size_t buf_len);
```
### `quercus_core_color_get_output_gamut` — The output gamut's setting string.
```c
int32_t quercus_core_color_get_output_gamut(uint8_t* buf, size_t buf_len);
```
### `quercus_core_color_get_output_transfer` — The output transfer's setting string.
```c
int32_t quercus_core_color_get_output_transfer(uint8_t* buf, size_t buf_len);
```
### `quercus_core_color_get_working_space` — The working color space's setting string.
```c
int32_t quercus_core_color_get_working_space(uint8_t* buf, size_t buf_len);
```
### `quercus_core_color_set_pipeline_settings` — Sets the pipeline color settings from their persisted setting strings ("acescg"/"srgb_legacy", "srgb"/"displayp3"/"bt2020", and the transfer names).
```c
bool quercus_core_color_set_pipeline_settings(const uint8_t* working, size_t working_len, const uint8_t* gamut, size_t gamut_len, const uint8_t* transfer, size_t transfer_len);
```
### `quercus_core_color_set_up_default_config` — Loads the default OCIO config: `Some(path)` for an explicit file, `None` for `$OCIO`/the bundled default.
```c
bool quercus_core_color_set_up_default_config(void);
```
### `quercus_core_color_set_up_default_config_from` — Loads the default OCIO config: `Some(path)` for an explicit file, `None` for `$OCIO`/the bundled default.
```c
bool quercus_core_color_set_up_default_config_from(const uint8_t* path, size_t path_len);
```
### `quercus_core_colormath_chromatic_adaptation` — The Bradford chromatic adaptation matrix from `src_white` to `dst_white`.
```c
QuercusMat3 quercus_core_colormath_chromatic_adaptation(Xy src_white, Xy dst_white);
```
### `quercus_core_colormath_gamma_eotf` — Decodes one gamma-encoded component to linear light: `sign(v)*|v|^gamma`.
```c
float quercus_core_colormath_gamma_eotf(float v, float gamma);
```
### `quercus_core_colormath_gamma_oetf` — Encodes one linear component with a gamma curve: `sign(v)*|v|^(1/gamma)`.
```c
float quercus_core_colormath_gamma_oetf(float v, float gamma);
```
### `quercus_core_colormath_hlg_eotf` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
float quercus_core_colormath_hlg_eotf(float v);
```
### `quercus_core_colormath_hlg_oetf` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
float quercus_core_colormath_hlg_oetf(float v);
```
### `quercus_core_colormath_output_spec_to_xyz_d65` — Converts packed f32 samples from the output spec into linear XYZ (D65) in place.
```c
bool quercus_core_colormath_output_spec_to_xyz_d65(float* data, size_t len, const uint8_t* gamut, size_t gamut_len, const uint8_t* transfer, size_t transfer_len);
```
### `quercus_core_colormath_pq_eotf` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
float quercus_core_colormath_pq_eotf(float v);
```
### `quercus_core_colormath_pq_oetf` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
float quercus_core_colormath_pq_oetf(float v);
```
### `quercus_core_colormath_primaries_ap1` — The built-in ACES AP1 (D60) primaries.
```c
Primaries quercus_core_colormath_primaries_ap1(void);
```
### `quercus_core_colormath_primaries_bt2020` — The built-in Rec.2020 (D65) primaries.
```c
Primaries quercus_core_colormath_primaries_bt2020(void);
```
### `quercus_core_colormath_primaries_display_p3` — The built-in Display P3 (D65) primaries.
```c
Primaries quercus_core_colormath_primaries_display_p3(void);
```
### `quercus_core_colormath_primaries_srgb` — The built-in sRGB (Rec.709, D65) primaries.
```c
Primaries quercus_core_colormath_primaries_srgb(void);
```
### `quercus_core_colormath_rgb_to_rgb` — The 3x3 matrix converting RGB values from `src` primaries to `dst`.
```c
QuercusMat3 quercus_core_colormath_rgb_to_rgb(Primaries src, Primaries dst);
```
### `quercus_core_colormath_rgb_to_xyz` — The RGB→XYZ matrix of a set of primaries.
```c
QuercusMat3 quercus_core_colormath_rgb_to_xyz(Primaries primaries);
```
### `quercus_core_colormath_srgb_eotf` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
float quercus_core_colormath_srgb_eotf(float v);
```
### `quercus_core_colormath_srgb_oetf` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
float quercus_core_colormath_srgb_oetf(float v);
```
### `quercus_core_colormath_working_to_display_target` — Applies the working→display transform in place over packed f32 samples. Settings are the persisted strings (see `crate::state::color_set_pipeline_settings`). Infallible for valid settings from Rust (strings are always UTF-8).
```c
bool quercus_core_colormath_working_to_display_target(float* data, size_t len, const uint8_t* working, size_t working_len, const uint8_t* gamut, size_t gamut_len, const uint8_t* transfer, size_t transfer_len);
```
### `quercus_core_colorproc_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_core_colorproc_add_ref(const void* this_);
```
### `quercus_core_colorproc_cache_id` — Returns the engine's cache id for this processor's OCIO transform.
```c
int32_t quercus_core_colorproc_cache_id(const void* this_, uint8_t* buf, size_t buf_len);
```
### `quercus_core_colorproc_convert_bgra8` — Converts `pixels` BGRA8 pixels in place to the processor's output.
```c
bool quercus_core_colorproc_convert_bgra8(const void* this_, uint8_t* data, int64_t pixels);
```
### `quercus_core_colorproc_convert_color` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_core_colorproc_convert_color(const void* this_, double* rgba);
```
### `quercus_core_colorproc_convert_f32_rgba` — Converts `pixels` RGBA f32 pixels in place to the processor's output.
```c
bool quercus_core_colorproc_convert_f32_rgba(const void* this_, float* data, int64_t pixels);
```
### `quercus_core_colorproc_create` — A processor from two colorspace names on the default OCIO config (`inverse` runs dst → src). `None` when no default config is loaded (see `crate::state::color_set_up_default_config`). OCIO failures yield a pass-through processor — check with `ColorProcessor::is_valid`.
```c
void* quercus_core_colorproc_create(const uint8_t* src_space, size_t src_space_len, const uint8_t* dst_transform, size_t dst_transform_len, bool inverse);
```
### `quercus_core_colorproc_create_display_icc` — A processor converting `src_space` to the display ICC profile at `icc_path`.
```c
void* quercus_core_colorproc_create_display_icc(const uint8_t* src_space, size_t src_space_len, const uint8_t* icc_path, size_t icc_path_len);
```
### `quercus_core_colorproc_create_display_icc_bgra8` — A BGRA8 processor converting `src_space` to the display ICC profile.
```c
void* quercus_core_colorproc_create_display_icc_bgra8(const uint8_t* src_space, size_t src_space_len, const uint8_t* icc_path, size_t icc_path_len);
```
### `quercus_core_colorproc_create_display_icc_xyz` — A processor converting CIE XYZ (D65) to the display ICC profile.
```c
void* quercus_core_colorproc_create_display_icc_xyz(const uint8_t* icc_path, size_t icc_path_len);
```
### `quercus_core_colorproc_create_display_icc_xyz_bgra8` — The BGRA8 variant of `ColorProcessor::create_display_icc_xyz`.
```c
void* quercus_core_colorproc_create_display_icc_xyz_bgra8(const uint8_t* icc_path, size_t icc_path_len);
```
### `quercus_core_colorproc_create_lut` — A processor applying the LUT file at `path` (`inverse` runs it backwards).
```c
void* quercus_core_colorproc_create_lut(const uint8_t* path, size_t path_len, bool inverse);
```
### `quercus_core_colorproc_is_valid` — Whether the processor has a real OCIO transform (false for the pass-through fallback produced on OCIO failures).
```c
bool quercus_core_colorproc_is_valid(const void* this_);
```
### `quercus_core_colorproc_pass_through` — A pass-through processor (no config needed).
```c
void* quercus_core_colorproc_pass_through(void);
```
### `quercus_core_colorproc_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_core_colorproc_release(const void* this_);
```
### `quercus_core_colortransform_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_core_colortransform_add_ref(const void* this_);
```
### `quercus_core_colortransform_display` — The display name ("" for output transforms).
```c
int32_t quercus_core_colortransform_display(const void* this_, uint8_t* buf, size_t buf_len);
```
### `quercus_core_colortransform_is_display` — Whether this is a display/view/look transform.
```c
bool quercus_core_colortransform_is_display(const void* this_);
```
### `quercus_core_colortransform_look` — Returns the transform's look name.
```c
int32_t quercus_core_colortransform_look(const void* this_, uint8_t* buf, size_t buf_len);
```
### `quercus_core_colortransform_new_display` — Creates a display/view/look transform descriptor.
```c
void* quercus_core_colortransform_new_display(const uint8_t* display, size_t display_len, const uint8_t* view, size_t view_len, const uint8_t* look, size_t look_len);
```
### `quercus_core_colortransform_new_output` — Creates an output-colorspace transform descriptor.
```c
void* quercus_core_colortransform_new_output(const uint8_t* output, size_t output_len);
```
### `quercus_core_colortransform_output` — The output colorspace name ("" for display transforms).
```c
int32_t quercus_core_colortransform_output(const void* this_, uint8_t* buf, size_t buf_len);
```
### `quercus_core_colortransform_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_core_colortransform_release(const void* this_);
```
### `quercus_core_colortransform_view` — Returns the transform's view name.
```c
int32_t quercus_core_colortransform_view(const void* this_, uint8_t* buf, size_t buf_len);
```
### `quercus_core_config_clear_source` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_core_config_clear_source(void);
```
### `quercus_core_config_get` — Reads a string config value; `None` when the key does not exist.
```c
int32_t quercus_core_config_get(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_config_get_bool` — Reads a boolean config value, `fallback` when unset (0/1 over the ABI, like the C++ store).
```c
int32_t quercus_core_config_get_bool(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, int32_t fallback);
```
### `quercus_core_config_get_double` — Read floating-point config `key` from `group`, or `fallback` when it is unavailable.
```c
double quercus_core_config_get_double(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, double fallback);
```
### `quercus_core_config_get_int64` — Read integer config `key` from `group`, or `fallback` when it is unavailable.
```c
int64_t quercus_core_config_get_int64(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, int64_t fallback);
```
### `quercus_core_config_has_source` — Whether a process-wide config persistence source is installed.
```c
bool quercus_core_config_has_source(void);
```
### `quercus_core_config_load` — Loads the config store from its persistence backend (the installed `ConfigSource` when there is one, else `config.toml`).
```c
bool quercus_core_config_load(void);
```
### `quercus_core_config_save` — Saves the config store to its persistence backend (the installed `ConfigSource` when there is one, else `config.toml`).
```c
bool quercus_core_config_save(void);
```
### `quercus_core_config_set` — Sets a string config value (`group` = None for the global group).
```c
bool quercus_core_config_set(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, const uint8_t* value, size_t value_len);
```
### `quercus_core_config_set_bool` — Write boolean `value` to config `key` in `group`.
```c
bool quercus_core_config_set_bool(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, int32_t value);
```
### `quercus_core_config_set_double` — Write floating-point `value` to config `key` in `group`.
```c
bool quercus_core_config_set_double(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, double value);
```
### `quercus_core_config_set_int64` — Write integer `value` to config `key` in `group`.
```c
bool quercus_core_config_set_int64(const uint8_t* group, size_t group_len, const uint8_t* key, size_t key_len, int64_t value);
```
### `quercus_core_config_set_source` — Installs `source` as the process-wide config persistence backend, replacing (and dropping) any previously installed source.
```c
bool quercus_core_config_set_source(const QuercusConfigSourceVTable* vt, void* userdata, QuercusConfigDropFn drop_cb);
```
### `quercus_core_displayicc_system_display_icc` — The system display ICC profile path; `None` when unavailable (headless, unsupported platform).
```c
int32_t quercus_core_displayicc_system_display_icc(uint8_t* buf, size_t buf_len);
```
### `quercus_core_displayicc_system_display_icc_for_monitor` — The display ICC profile path for the monitor identified by `fingerprint`; `None` when unknown/unavailable.
```c
int32_t quercus_core_displayicc_system_display_icc_for_monitor(const uint8_t* fingerprint, size_t fingerprint_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_displayicc_windows_acm_active` — Whether Windows ACM (Auto Color Management) is active.
```c
bool quercus_core_displayicc_windows_acm_active(void);
```
### `quercus_core_displayicc_windows_monitor_fingerprint` — The monitor fingerprint for a Windows HMONITOR (cast to u64).
```c
int32_t quercus_core_displayicc_windows_monitor_fingerprint(uint64_t hmonitor, uint8_t* buf, size_t buf_len);
```
### `quercus_core_displayicc_x11_monitor_fingerprint_at` — The fingerprint of the X11 monitor covering (`x`, `y`) in global coordinates.
```c
int32_t quercus_core_displayicc_x11_monitor_fingerprint_at(double x, double y, uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_default_disk_cache_path` — The default disk cache path.
```c
int32_t quercus_core_file_default_disk_cache_path(uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_directory_is_valid` — Whether `dir` is a valid directory; with `try_to_create`, attempts to create it first.
```c
bool quercus_core_file_directory_is_valid(const uint8_t* dir, size_t dir_len, bool try_to_create);
```
### `quercus_core_file_ensure_filename_extension` — Appends `extension` to `filename` when missing; `None` on engine error.
```c
int32_t quercus_core_file_ensure_filename_extension(const uint8_t* filename, size_t filename_len, const uint8_t* extension, size_t extension_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_application_path` — The directory containing the running application executable.
```c
int32_t quercus_core_file_get_application_path(uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_auto_recovery_root` — The auto-recovery root directory.
```c
int32_t quercus_core_file_get_auto_recovery_root(uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_configuration_location` — The per-user configuration directory.
```c
int32_t quercus_core_file_get_configuration_location(uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_formatted_executable_for_platform` — The platform-specific executable name for `unformatted` (adds ".exe" on Windows).
```c
int32_t quercus_core_file_get_formatted_executable_for_platform(const uint8_t* unformatted, size_t unformatted_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_safe_temporary_filename` — A collision-free temporary filename derived from `original`.
```c
int32_t quercus_core_file_get_safe_temporary_filename(const uint8_t* original, size_t original_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_temp_file_path` — The process-wide temporary directory used for intermediate files.
```c
int32_t quercus_core_file_get_temp_file_path(uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_get_unique_file_identifier` — The unique file identifier for `filename` (empty for missing files); `None` on engine error.
```c
int32_t quercus_core_file_get_unique_file_identifier(const uint8_t* filename, size_t filename_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_read_file_as_string` — Reads a whole text file. Unreadable/missing files read as an empty string (C++ parity); `None` only when the engine rejects the call.
```c
int32_t quercus_core_file_read_file_as_string(const uint8_t* filename, size_t filename_len, uint8_t* buf, size_t buf_len);
```
### `quercus_core_file_rename_file_allow_overwrite` — Renames `from` to `to`, overwriting an existing `to`.
```c
bool quercus_core_file_rename_file_allow_overwrite(const uint8_t* from, size_t from_len, const uint8_t* to, size_t to_len);
```
### `quercus_core_log_get_level` — The engine's current minimum log level (0 = DEBUG … 4 = FATAL).
```c
int32_t quercus_core_log_get_level(void);
```
### `quercus_core_log_set_level` — Sets the engine's minimum log level (0 = DEBUG, 1 = INFO, 2 = WARNING, 3 = ERROR, 4 = FATAL); `false` when the code is out of range.
```c
bool quercus_core_log_set_level(int32_t level);
```
### `quercus_core_log_set_sink` — Installs (or with `None`, clears) the engine's process-wide log sink: `sink(level, message)` receives lines already formatted as `"[LEVEL] message"`. Replacing or clearing a sink drops the previous closure. Returns `false` if the engine rejected the registration (not expected).
```c
void quercus_core_log_set_sink(LogFn sink, void* userdata, LogDropFn drop_cb);
```
### `quercus_core_log_write` — Writes `message` through the engine's process-wide log at `level` (0 = DEBUG … 4 = FATAL). Out-of-range levels are printed with the engine's `UNKNOWN` tag.
```c
void quercus_core_log_write(int32_t level, const uint8_t* msg, size_t len);
```
### `quercus_core_lut_display_domain` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_core_lut_display_domain(float* lo, float* hi);
```
### `quercus_core_lut_display_edge` — The display LUT lattice edge length (65).
```c
uint32_t quercus_core_lut_display_edge(void);
```
### `quercus_core_pixelformat_bytes_per_channel` — Bytes per channel for this format: 1 for `U8`, 2 for `U16`/`F16` and 4 for `F32`.
```c
int32_t quercus_core_pixelformat_bytes_per_channel(int32_t format);
```
### `quercus_core_pixelformat_bytes_per_pixel` — Bytes occupied by one pixel of this format with `channels` channels.
```c
int32_t quercus_core_pixelformat_bytes_per_pixel(int32_t format, int32_t channels);
```
### `quercus_core_pixelformat_is_valid` — The discriminant for a raw ABI value; `None` outside -1..=4.
```c
bool quercus_core_pixelformat_is_valid(int32_t format);
```
### `quercus_core_rational_add` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
Rational quercus_core_rational_add(Rational a, Rational b);
```
### `quercus_core_rational_cmp` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
int32_t quercus_core_rational_cmp(Rational a, Rational b);
```
### `quercus_core_rational_div` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
Rational quercus_core_rational_div(Rational a, Rational b);
```
### `quercus_core_rational_from_f64` — The rational closest to `value` within quercus-core's precision.
```c
Rational quercus_core_rational_from_f64(double value);
```
### `quercus_core_rational_from_string` — Parses the text form; invalid input yields the null sentinel.
```c
Rational quercus_core_rational_from_string(const uint8_t* s, size_t len);
```
### `quercus_core_rational_is_nan` — True for the NaN sentinel (produced by overflowed arithmetic).
```c
bool quercus_core_rational_is_nan(Rational r);
```
### `quercus_core_rational_is_null` — Whether this is the null/invalid sentinel, detected by a zero numerator.
```c
bool quercus_core_rational_is_null(Rational r);
```
### `quercus_core_rational_mul` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
Rational quercus_core_rational_mul(Rational a, Rational b);
```
### `quercus_core_rational_new` — A reduced `num/den` rational (0/0 when `den` is 0). Reduction runs in the engine, so the result is bit-identical to quercus-core's.
```c
Rational quercus_core_rational_new(int64_t num, int64_t den);
```
### `quercus_core_rational_null` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
Rational quercus_core_rational_null(void);
```
### `quercus_core_rational_sub` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
Rational quercus_core_rational_sub(Rational a, Rational b);
```
### `quercus_core_rational_time_to_timestamp` — Rational `time` as an integer timestamp in `self` as timebase.
```c
int64_t quercus_core_rational_time_to_timestamp(Rational timebase, Rational time);
```
### `quercus_core_rational_timestamp_to_time` — Integer timestamp `ts` back to rational time in `self` as timebase.
```c
Rational quercus_core_rational_timestamp_to_time(Rational timebase, int64_t ts);
```
### `quercus_core_rational_to_f64` — The rational as an `f64`, i.e. `num / den`.
```c
double quercus_core_rational_to_f64(Rational r);
```
### `quercus_core_rational_to_string` — The project-XML text form (e.g. "30000/1001").
```c
int32_t quercus_core_rational_to_string(Rational r, uint8_t* buf, size_t buf_len);
```
### `quercus_core_timerange_combined` — The smallest range covering both inputs.
```c
TimeRange quercus_core_timerange_combined(TimeRange a, TimeRange b);
```
### `quercus_core_timerange_contains` — Whether instant `t` lies in the half-open range `[in, out)`.
```c
bool quercus_core_timerange_contains(TimeRange r, Rational t);
```
### `quercus_core_timerange_intersected` — The overlap of two ranges, empty when they are disjoint.
```c
TimeRange quercus_core_timerange_intersected(TimeRange a, TimeRange b);
```
### `quercus_core_timerange_length` — `out - in`.
```c
Rational quercus_core_timerange_length(TimeRange r);
```
### `quercus_core_timerange_new` — A normalized range: the endpoints are swapped when `out < in`.
```c
TimeRange quercus_core_timerange_new(Rational in_, Rational out);
```
### `quercus_core_videoparams_buffer_size` — Total frame buffer size in bytes.
```c
int32_t quercus_core_videoparams_buffer_size(const VideoParams* params);
```
### `quercus_core_videoparams_bytes_per_pixel` — Bytes per pixel for this parameter set's format and channel count.
```c
int32_t quercus_core_videoparams_bytes_per_pixel(const VideoParams* params);
```
### `quercus_core_videoparams_default` — The default (invalid) parameter set.
```c
VideoParams quercus_core_videoparams_default(void);
```
### `quercus_core_videoparams_effective_height` — The height after applying the resolution divider.
```c
int32_t quercus_core_videoparams_effective_height(const VideoParams* params);
```
### `quercus_core_videoparams_effective_width` — The width after applying the resolution divider.
```c
int32_t quercus_core_videoparams_effective_width(const VideoParams* params);
```
### `quercus_core_videoparams_is_valid` — Whether the parameter set describes a valid video stream.
```c
bool quercus_core_videoparams_is_valid(const VideoParams* params);
```
<a id="module-editor"></a>
## Module: `editor` (97 functions)

### `quercus_editor_add_audio_bus_effect` — Appends an effect to the current sequence's master-bus chain at `index` (clamped to the chain length) as one undo entry.
```c
bool quercus_editor_add_audio_bus_effect(const void* session, const uint8_t* type_id, size_t type_id_len, int32_t index);
```
### `quercus_editor_add_clip_audio_effect` — Attaches an audio effect to a clip's chain at `index` (clamped to the chain length) as one undo entry. Returns false for an empty type id.
```c
bool quercus_editor_add_clip_audio_effect(const void* session, uint64_t clip, const uint8_t* type_id, size_t type_id_len, int32_t index);
```
### `quercus_editor_add_ref` — Clones the handle, not the project; both values refer to the same session.
```c
void quercus_editor_add_ref(const void* session);
```
### `quercus_editor_adopt_storage_project_handle` — Adopts an owned storage handle into a session without reopening the project.
```c
void* quercus_editor_adopt_storage_project_handle(StorageProjectHandle project);
```
### `quercus_editor_audio_bus_effect_count` — Read the current sequence's master-bus effect chain.
```c
int32_t quercus_editor_audio_bus_effect_count(const void* session);
```
### `quercus_editor_audio_bus_effect_info` — Read the current sequence's master-bus effect chain.
```c
bool quercus_editor_audio_bus_effect_info(const void* session, int32_t index, QuercusClipAudioEffectInfo* out);
```
### `quercus_editor_audio_bus_effect_param` — Read the current sequence's master-bus effect chain.
```c
bool quercus_editor_audio_bus_effect_param(const void* session, int32_t index, const uint8_t* input, size_t input_len, double* out);
```
### `quercus_editor_bind_project_to_library` — Registers this exact project for write-through persistence in the default library. Later successful undo-path edits can be written back automatically.
```c
bool quercus_editor_bind_project_to_library(void* session);
```
### `quercus_editor_clip_audio_effect_info` — Read the audio mix state and effect chain of one clip.
```c
bool quercus_editor_clip_audio_effect_info(const void* session, uint64_t clip, int32_t index, QuercusClipAudioEffectInfo* out);
```
### `quercus_editor_clip_audio_effect_param` — Read the audio mix state and effect chain of one clip.
```c
bool quercus_editor_clip_audio_effect_param(const void* session, uint64_t clip, int32_t index, const uint8_t* input, size_t input_len, double* out);
```
### `quercus_editor_clip_audio_state` — Read the audio mix state and effect chain of one clip.
```c
bool quercus_editor_clip_audio_state(const void* session, uint64_t clip, QuercusClipAudioState* out);
```
### `quercus_editor_clip_media_filename` — Returns the first upstream footage filename without exposing the graph.
```c
int32_t quercus_editor_clip_media_filename(const void* session, uint64_t clip, uint8_t* buf, size_t buf_len);
```
### `quercus_editor_clip_sync_info` — Resolves a block and its upstream footage into an owned sync DTO.
```c
bool quercus_editor_clip_sync_info(const void* session, uint64_t clip, ClipSyncInfo* out);
```
### `quercus_editor_create_library` — Creates a blank row in the default library without reloading it.
```c
void* quercus_editor_create_library(const uint8_t* name, size_t name_len, int32_t* error);
```
### `quercus_editor_create_sequence` — Creates a sequence with the engine's default track lists and selects it.
```c
uint64_t quercus_editor_create_sequence(void* session, const uint8_t* name, size_t name_len);
```
### `quercus_editor_current_sequence` — Returns the sequence targeted by timeline commands without an explicit ID.
```c
uint64_t quercus_editor_current_sequence(const void* session);
```
### `quercus_editor_edit` — Executes exactly one semantic edit and returns owned results.
```c
int32_t quercus_editor_edit(void* session, uint32_t op, const QuercusEditParams* params, QuercusEditResult* result, uint8_t* error, size_t error_len);
```
### `quercus_editor_edit_subscribe` — Subscribes to successful semantic commands executed on this session.
```c
uint64_t quercus_editor_edit_subscribe(void* session, void (* callback)(void*, uint32_t, int64_t), void* user_data, void (* drop_callback)(void*));
```
### `quercus_editor_edit_unsubscribe` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_editor_edit_unsubscribe(void* session, uint64_t id);
```
### `quercus_editor_export_subtitles` — Exports the current sequence's Text clips as an SRT file.
```c
int32_t quercus_editor_export_subtitles(const void* session, const uint8_t* path, size_t path_len);
```
### `quercus_editor_from_project_handle` — Creates a fresh session around an existing project handle without reopening or copying the document. The handle is consumed exactly once.
```c
void* quercus_editor_from_project_handle(void* project);
```
### `quercus_editor_graph_can_connect` — Validate a proposed stream connection against the current live graph.
```c
bool quercus_editor_graph_can_connect(void* session, uint64_t from, uint64_t to, const uint8_t* input_id, size_t input_id_len, int32_t element);
```
### `quercus_editor_graph_edge_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
bool quercus_editor_graph_edge_at(const void* graph, int32_t index, GraphEdgeRaw* out);
```
### `quercus_editor_graph_edge_count` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_edge_count(const void* graph);
```
### `quercus_editor_graph_edge_input_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_edge_input_at(const void* graph, int32_t index, uint8_t* buf, size_t len);
```
### `quercus_editor_graph_node_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
bool quercus_editor_graph_node_at(const void* graph, int32_t index, GraphNodeRaw* out);
```
### `quercus_editor_graph_node_count` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_node_count(const void* graph);
```
### `quercus_editor_graph_node_label_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_node_label_at(const void* graph, int32_t index, uint8_t* buf, size_t len);
```
### `quercus_editor_graph_node_type_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_node_type_at(const void* graph, int32_t index, uint8_t* buf, size_t len);
```
### `quercus_editor_graph_port_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
bool quercus_editor_graph_port_at(const void* graph, int32_t node, int32_t port, GraphPortRaw* out);
```
### `quercus_editor_graph_port_id_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_port_id_at(const void* graph, int32_t node, int32_t port, uint8_t* buf, size_t len);
```
### `quercus_editor_graph_port_label_at` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int32_t quercus_editor_graph_port_label_at(const void* graph, int32_t node, int32_t port, uint8_t* buf, size_t len);
```
### `quercus_editor_graph_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_editor_graph_release(const void* graph);
```
### `quercus_editor_graph_revision` — Read a complete snapshot out of a raw graph-snapshot handle.
```c
int64_t quercus_editor_graph_revision(const void* graph);
```
### `quercus_editor_graph_snapshot` — Captures a full sequence graph, or only a clip's `footage → effects → clip` context when `clip` is `Some`. Invalid IDs return `None`.
```c
void* quercus_editor_graph_snapshot(void* session, uint64_t sequence, uint64_t clip);
```
### `quercus_editor_import_subtitles` — Imports an SRT file as Text clips on the current sequence and track.
```c
int32_t quercus_editor_import_subtitles(const void* session, const uint8_t* path, size_t path_len, int32_t track_index);
```
### `quercus_editor_import_subtitles_styled` — Imports an SRT file as styled Text clips on the current sequence and `track_index`. Returns the cue count.
```c
int32_t quercus_editor_import_subtitles_styled(const void* session, const uint8_t* path, size_t path_len, int32_t track_index, const QuercusSubtitleStyle* style);
```
### `quercus_editor_marker_at` — The session's markers (owned DTOs).
```c
bool quercus_editor_marker_at(const void* session, int32_t index, EditorMarkerRaw* out);
```
### `quercus_editor_marker_count` — The session's markers (owned DTOs).
```c
int32_t quercus_editor_marker_count(const void* session);
```
### `quercus_editor_marker_name_at` — The session's markers (owned DTOs).
```c
int32_t quercus_editor_marker_name_at(const void* session, int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_editor_measure_loudness` — Mixes `range` through the normal preview-audio path and measures its approximate integrated LUFS and true peak. Returns `None` when the session has no current sequence or the audio render fails.
```c
bool quercus_editor_measure_loudness(const void* session, const TimeRange* range, QuercusLoudness* out);
```
### `quercus_editor_multicam_clip_at` — Finds the multicam clip covering `time` on a sequence's video tracks.
```c
uint64_t quercus_editor_multicam_clip_at(const void* session, uint64_t sequence, Rational time);
```
### `quercus_editor_multicam_connected_sequence` — Returns the first sequence feeding the clip's texture chain.
```c
uint64_t quercus_editor_multicam_connected_sequence(const void* session, uint64_t clip);
```
### `quercus_editor_multicam_current_source` — Returns a multicam node's current angle, or `None` when it is invalid.
```c
int32_t quercus_editor_multicam_current_source(const void* session, uint64_t multicam);
```
### `quercus_editor_multicam_grid` — Angle grid layout `(rows, cols)` for `sources`, or `None` when the count is not positive.
```c
bool quercus_editor_multicam_grid(int32_t source_count, int32_t* rows, int32_t* cols);
```
### `quercus_editor_multicam_is_clip` — Whether a clip's *direct* texture source is a multicam node.
```c
bool quercus_editor_multicam_is_clip(const void* session, uint64_t clip);
```
### `quercus_editor_multicam_sequence` — The source sequence connected to a multicam node, if it resolves.
```c
uint64_t quercus_editor_multicam_sequence(const void* session, uint64_t multicam);
```
### `quercus_editor_multicam_source_at` — Hit-test normalized display coordinates against the angle grid.
```c
int32_t quercus_editor_multicam_source_at(int32_t source_count, double x, double y);
```
### `quercus_editor_multicam_source_count` — Returns a multicam node's angle count, or `None` when it is invalid.
```c
int32_t quercus_editor_multicam_source_count(const void* session, uint64_t multicam);
```
### `quercus_editor_multicam_source_name` — Query one angle's display name through the C ABI.
```c
int32_t quercus_editor_multicam_source_name(const void* session, uint64_t multicam, int32_t source, uint8_t* buf, size_t buf_len);
```
### `quercus_editor_multicam_source_track` — Returns one angle's track inside the connected source sequence.
```c
uint64_t quercus_editor_multicam_source_track(const void* session, uint64_t multicam, int32_t source);
```
### `quercus_editor_multicam_state` — Resolve a clip's multicam state: its source sequence, the multicam node, the angle count and the current angle.
```c
bool quercus_editor_multicam_state(const void* session, uint64_t clip, RawMulticamState* out);
```
### `quercus_editor_new` — Creates a new project and binds it when a default library is configured.
```c
void* quercus_editor_new(void);
```
### `quercus_editor_open` — Opens any storage URI and binds the loaded project.
```c
void* quercus_editor_open(const uint8_t* uri, size_t uri_len, int32_t* version_info);
```
### `quercus_editor_open_project_from_library` — Opens the project identified by `uuid` in the configured default library.
```c
void* quercus_editor_open_project_from_library(const uint8_t* uuid, size_t uuid_len, int32_t* error);
```
### `quercus_editor_open_project_from_library_uri` — Opens the project `uuid` from an explicit library URI (any backend the registry can resolve, not only the configured default library).
```c
void* quercus_editor_open_project_from_library_uri(const uint8_t* library_uri, size_t library_uri_len, const uint8_t* uuid, size_t uuid_len, int32_t* error);
```
### `quercus_editor_params_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
bool quercus_editor_params_at(const void* params, int32_t index, ParamRaw* out);
```
### `quercus_editor_params_count` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_count(const void* params);
```
### `quercus_editor_params_input_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_input_at(const void* params, int32_t index, uint8_t* buf, size_t len);
```
### `quercus_editor_params_keyframe_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
bool quercus_editor_params_keyframe_at(const void* params, int32_t param, int32_t element, int32_t index, KeyframeRaw* out);
```
### `quercus_editor_params_keyframe_count` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_keyframe_count(const void* params, int32_t param, int32_t element);
```
### `quercus_editor_params_keyframe_text_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_keyframe_text_at(const void* params, int32_t param, int32_t element, int32_t index, uint8_t* buf, size_t len);
```
### `quercus_editor_params_label_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_label_at(const void* params, int32_t index, uint8_t* buf, size_t len);
```
### `quercus_editor_params_property_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
bool quercus_editor_params_property_at(const void* params, int32_t param, int32_t property, PropertyRaw* out);
```
### `quercus_editor_params_property_key_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_property_key_at(const void* params, int32_t param, int32_t property, uint8_t* buf, size_t len);
```
### `quercus_editor_params_property_text_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_property_text_at(const void* params, int32_t param, int32_t property, uint8_t* buf, size_t len);
```
### `quercus_editor_params_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_editor_params_release(const void* params);
```
### `quercus_editor_params_revision` — Reads an owned snapshot out of a C-side parameter handle.
```c
int64_t quercus_editor_params_revision(const void* params);
```
### `quercus_editor_params_snapshot` — Captures all visible parameters for `node`, including static values, properties and every currently stored keyframe track. Invalid IDs return `None`.
```c
void* quercus_editor_params_snapshot(void* session, uint64_t node);
```
### `quercus_editor_params_track_element_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_track_element_at(const void* params, int32_t param, int32_t index);
```
### `quercus_editor_params_value_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
bool quercus_editor_params_value_at(const void* params, int32_t param, int32_t element, ValueRaw* out);
```
### `quercus_editor_params_value_element_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_value_element_at(const void* params, int32_t param, int32_t index);
```
### `quercus_editor_params_value_text_at` — Reads an owned snapshot out of a C-side parameter handle.
```c
int32_t quercus_editor_params_value_text_at(const void* params, int32_t param, int32_t element, uint8_t* buf, size_t len);
```
### `quercus_editor_project` — A new safe handle to the same live project used by storage binding.
```c
void* quercus_editor_project(const void* session);
```
### `quercus_editor_proxy_complete` — Finalizes only the matching active task, preventing stale-worker updates.
```c
bool quercus_editor_proxy_complete(const void* session, uint64_t footage, uint64_t task_id, bool succeeded);
```
### `quercus_editor_proxy_generate` — Start an asynchronous proxy transcode for `footage`.
```c
uint64_t quercus_editor_proxy_generate(const void* session, uint64_t footage, const uint8_t* cache_path, size_t cache_path_len, uint8_t* error, size_t error_len);
```
### `quercus_editor_proxy_path` — Returns the recorded proxy filename, or `None` for invalid footage.
```c
int32_t quercus_editor_proxy_path(const void* session, uint64_t footage, uint8_t* buf, size_t buf_len);
```
### `quercus_editor_proxy_status` — Reads proxy lifecycle and footage capability flags without exposing codec or node behavior types through the SDK.
```c
bool quercus_editor_proxy_status(const void* session, uint64_t footage, RawProxyStatus* out);
```
### `quercus_editor_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_editor_release(const void* session);
```
### `quercus_editor_remove_audio_bus_effect` — Removes one entry of the current sequence's master-bus chain.
```c
bool quercus_editor_remove_audio_bus_effect(const void* session, int32_t index);
```
### `quercus_editor_remove_clip_audio_effect` — Removes one entry of a clip's audio effect chain.
```c
bool quercus_editor_remove_clip_audio_effect(const void* session, uint64_t clip, int32_t index);
```
### `quercus_editor_save` — Saves through the storage backend selected by the path extension.
```c
int32_t quercus_editor_save(const void* session, const uint8_t* path, size_t path_len, uint32_t options);
```
### `quercus_editor_set_audio_bus_effect_enabled` — Enable or disable one entry of the current sequence's master-bus effect chain, as one undo row.
```c
bool quercus_editor_set_audio_bus_effect_enabled(const void* session, int32_t index, bool enabled);
```
### `quercus_editor_set_audio_bus_effect_param` — Set one static (non-keyframed) scalar parameter of a master-bus effect entry.
```c
bool quercus_editor_set_audio_bus_effect_param(const void* session, int32_t index, const uint8_t* input_id, size_t input_id_len, double value);
```
### `quercus_editor_set_clip_audio_effect_enabled` — Enables or disables one entry of a clip's audio effect chain.
```c
bool quercus_editor_set_clip_audio_effect_enabled(const void* session, uint64_t clip, int32_t index, bool enabled);
```
### `quercus_editor_set_clip_audio_effect_param` — Sets one static float parameter of a clip's audio effect (Phase 1: Volume `volume_in`, Pan `panning_in`).
```c
bool quercus_editor_set_clip_audio_effect_param(const void* session, uint64_t clip, int32_t index, const uint8_t* input_id, size_t input_id_len, double value);
```
### `quercus_editor_set_clip_fade` — Sets a clip's linear fade-in/fade-out durations as one undo entry.
```c
bool quercus_editor_set_clip_fade(const void* session, uint64_t clip, int64_t fade_in_num, int64_t fade_in_den, int64_t fade_out_num, int64_t fade_out_den);
```
### `quercus_editor_set_clip_gain` — Sets a clip's audio gain (0..=4, 1 = unity) as one undo entry.
```c
bool quercus_editor_set_clip_gain(const void* session, uint64_t clip, double gain);
```
### `quercus_editor_set_clip_pan` — Sets a clip's constant-power stereo pan (-1..=1, 0 = centre) as one undo entry.
```c
bool quercus_editor_set_clip_pan(const void* session, uint64_t clip, double pan);
```
### `quercus_editor_set_current_sequence` — Selects the target sequence; false means the ID is not a session sequence.
```c
bool quercus_editor_set_current_sequence(void* session, uint64_t id);
```
### `quercus_editor_set_subtitle_style` — Applies `style` to the Text clip `clip` as one undoable edit.
```c
bool quercus_editor_set_subtitle_style(const void* session, uint64_t clip, const QuercusSubtitleStyle* style);
```
### `quercus_editor_storage_bound` — Queries whether the project currently has an active write-through binding.
```c
bool quercus_editor_storage_bound(const void* session);
```
### `quercus_editor_storage_last_error` — Returns the most recent write-through error, if the backend supplied one.
```c
int32_t quercus_editor_storage_last_error(const void* session, uint8_t* buf, size_t buf_len);
```
### `quercus_editor_subtitle_style` — Reads the style applied to `clip`'s Text source, or `None` when the clip has no Text generator.
```c
bool quercus_editor_subtitle_style(const void* session, uint64_t clip, QuercusSubtitleStyle* out);
```
### `quercus_editor_subtitle_style_default` — The engine's default subtitle style (the text3 defaults).
```c
QuercusSubtitleStyle quercus_editor_subtitle_style_default(void);
```
### `quercus_editor_unbind` — Explicitly flushes pending writes and removes the library binding.
```c
bool quercus_editor_unbind(void* session);
```
### `quercus_editor_workarea` — The session's work area: the enabled flag and its range (the range is meaningful even while disabled).
```c
bool quercus_editor_workarea(const void* session, TimeRange* out);
```
<a id="module-project"></a>
## Module: `project` (28 functions)

### `quercus_project_active` — The process-wide active project (`None` when the host has none open).
```c
void* quercus_project_active(void);
```
### `quercus_project_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_project_add_ref(const void* project);
```
### `quercus_project_cache_location` — `(cache mode, custom path)`; mode 2 uses the custom path.
```c
int32_t quercus_project_cache_location(const void* project, int32_t* setting, uint8_t* path, size_t path_len);
```
### `quercus_project_deep_copy` — A deep copy of the project (background-render isolation).
```c
void* quercus_project_deep_copy(const void* project);
```
### `quercus_project_filename` — The project file path ("" when unsaved).
```c
int32_t quercus_project_filename(const void* project, uint8_t* buf, size_t buf_len);
```
### `quercus_project_graph_endpoints` — The graph's virtual input/output endpoints, when present.
```c
bool quercus_project_graph_endpoints(const void* project, uint64_t* input, uint64_t* output);
```
### `quercus_project_into_storage_handle` — Converts this owned handle into the storage migration representation used by legacy hosts. The same live Arc is retained; this handle is consumed.
```c
StorageProjectHandle quercus_project_into_storage_handle(void* project);
```
### `quercus_project_load` — Loads a project from an XML file. Not made active automatically.
```c
void* quercus_project_load(const uint8_t* path, size_t path_len);
```
### `quercus_project_load_from_bytes` — Parses a project from its XML byte representation (as produced by `Project::to_xml_bytes` or `Project::save`).
```c
void* quercus_project_load_from_bytes(const uint8_t* bytes, size_t len, int32_t* version_info, int32_t* error);
```
### `quercus_project_modified` — Whether the project has unsaved changes.
```c
bool quercus_project_modified(const void* project);
```
### `quercus_project_name` — The project display name.
```c
int32_t quercus_project_name(const void* project, uint8_t* buf, size_t buf_len);
```
### `quercus_project_new` — A new, initialized project (empty graph with a root folder).
```c
void* quercus_project_new(void);
```
### `quercus_project_node_at` — Returns the token of the `index`-th live node in graph arena order, or `None` when `index` is past `Project::node_count`.
```c
uint64_t quercus_project_node_at(const void* project, int32_t index);
```
### `quercus_project_node_count` — Number of live nodes in the graph.
```c
int32_t quercus_project_node_count(const void* project);
```
### `quercus_project_pretty_filename` — The display-friendly file path.
```c
int32_t quercus_project_pretty_filename(const void* project, uint8_t* buf, size_t buf_len);
```
### `quercus_project_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_project_release(const void* project);
```
### `quercus_project_root` — The root folder of the bin tree.
```c
uint64_t quercus_project_root(const void* project);
```
### `quercus_project_save` — Saves the project as XML to `path` (`None` = the project's own filename; save-as semantics: the path becomes the filename).
```c
bool quercus_project_save(const void* project, const uint8_t* path, size_t path_len);
```
### `quercus_project_save_to_bytes` — Serializes the project to its XML byte representation, the same document `Project::save` writes. Returns `None` on a serialization failure. The bytes are UTF-8 project XML with no trailing NUL and do not depend on the project's filename, so an unsaved project round-trips too.
```c
int32_t quercus_project_save_to_bytes(const void* project, uint8_t* buf, size_t cap);
```
### `quercus_project_set_active` — Sets (or with `None` clears) the process-wide active project — the one plugins see through `active_project`.
```c
void quercus_project_set_active(const void* project);
```
### `quercus_project_set_cache_location` — Writes the project's cache policy and custom cache path, marking the project modified.
```c
bool quercus_project_set_cache_location(const void* project, int32_t setting, const uint8_t* path, size_t path_len);
```
### `quercus_project_set_filename` — Sets the project file path (used by save when no path is given).
```c
bool quercus_project_set_filename(const void* project, const uint8_t* path, size_t path_len);
```
### `quercus_project_set_output_color_spec` — Sets the output gamut/transfer (persisted + applied to the pipeline).
```c
bool quercus_project_set_output_color_spec(const void* project, const uint8_t* gamut, size_t gamut_len, const uint8_t* transfer, size_t transfer_len);
```
### `quercus_project_set_setting` — Writes the persisted project setting `key` to `value`, replacing any previous value, and marks the project modified.
```c
bool quercus_project_set_setting(const void* project, const uint8_t* key, size_t key_len, const uint8_t* value, size_t value_len);
```
### `quercus_project_set_working_color_space` — Sets the working color space: persisted into the project settings AND applied to the process-wide color pipeline.
```c
bool quercus_project_set_working_color_space(const void* project, const uint8_t* setting, size_t setting_len);
```
### `quercus_project_setting` — Reads the persisted project setting `key` from the project file's settings map.
```c
int32_t quercus_project_setting(const void* project, const uint8_t* key, size_t key_len, uint8_t* buf, size_t buf_len);
```
### `quercus_project_unset_setting` — Removes the persisted project setting `key`, marking the project modified when a value was present.
```c
bool quercus_project_unset_setting(const void* project, const uint8_t* key, size_t key_len);
```
### `quercus_project_uuid` — The stable uuid used by the project library.
```c
int32_t quercus_project_uuid(const void* project, uint8_t* buf, size_t buf_len);
```
<a id="module-footage"></a>
## Module: `footage` (13 functions)

### `quercus_footage_audio_params` — Returns the parameters of the `ordinal`-th audio stream of `footage` (zero-based; counts audio streams only).
```c
bool quercus_footage_audio_params(const void* project, uint64_t id, int32_t ordinal, StreamAudioParams* out);
```
### `quercus_footage_audio_stream_count` — Returns the number of audio streams in the footage's probed media; `0` for a non-footage token or an unprobed file. Stream ordinals for `Project::footage_audio_params` count audio streams only.
```c
int32_t quercus_footage_audio_stream_count(const void* project, uint64_t id);
```
### `quercus_footage_custom_proxy_params` — Returns the per-footage proxy override, or `None` when global defaults apply.
```c
bool quercus_footage_custom_proxy_params(void* project, uint64_t id, ProxyParams* out);
```
### `quercus_footage_duration` — Returns the footage's total duration in rational seconds: the longest stream duration reported by the probe.
```c
Rational quercus_footage_duration(const void* project, uint64_t id);
```
### `quercus_footage_effective_proxy_params` — Returns custom proxy parameters when present, otherwise global defaults.
```c
bool quercus_footage_effective_proxy_params(void* project, uint64_t id, ProxyParams* out);
```
### `quercus_footage_filename` — Returns the footage's absolute source file path, or `None` when `footage` is not a footage node. An unset path is returned as `Some("")`.
```c
int32_t quercus_footage_filename(const void* project, uint64_t id, uint8_t* buf, size_t buf_len);
```
### `quercus_footage_import` — Imports a media file into the bin (probes first; a failed probe leaves nothing behind). Undoable.
```c
uint64_t quercus_footage_import(const void* project, const uint8_t* filename, size_t filename_len);
```
### `quercus_footage_source_start_time` — Returns source-start timecode only when the footage carries it.
```c
bool quercus_footage_source_start_time(const void* project, uint64_t footage, Rational* out);
```
### `quercus_footage_subtitle_stream_count` — Returns the number of subtitle streams in the footage's probed media; `0` for a non-footage token, an unprobed file, or media without subtitle tracks.
```c
int32_t quercus_footage_subtitle_stream_count(const void* project, uint64_t id);
```
### `quercus_footage_total_stream_count` — Returns the total number of streams (video + audio + subtitle) in the footage's probed media.
```c
int32_t quercus_footage_total_stream_count(const void* project, uint64_t id);
```
### `quercus_footage_video_length` — Returns the duration of the footage's longest video stream in rational seconds, or the null rational (`den == 0`) when the token is not footage.
```c
Rational quercus_footage_video_length(const void* project, uint64_t id);
```
### `quercus_footage_video_params` — Returns the parameters of the `ordinal`-th video stream of `footage` (zero-based; counts video streams only).
```c
bool quercus_footage_video_params(const void* project, uint64_t id, int32_t ordinal, StreamVideoParams* out);
```
### `quercus_footage_video_stream_count` — Returns the number of video streams in the footage's probed media; `0` for a non-footage token or an unprobed file. Stream ordinals for `Project::footage_video_params` count video streams only.
```c
int32_t quercus_footage_video_stream_count(const void* project, uint64_t id);
```
<a id="module-sequence"></a>
## Module: `sequence` (10 functions)

### `quercus_sequence_create` — Creates a sequence with the default 2V+2A track layout, mounted in the bin root. (Creation itself is not undoable, like the app.)
```c
uint64_t quercus_sequence_create(const void* project, const uint8_t* name, size_t name_len);
```
### `quercus_sequence_frame_rate` — The sequence's frame rate (the null rational when it has no video params).
```c
Rational quercus_sequence_frame_rate(const void* project, uint64_t id);
```
### `quercus_sequence_geometry` — The sequence's output geometry (width, height); None when it has no video params.
```c
bool quercus_sequence_geometry(const void* project, uint64_t id, int32_t* width, int32_t* height);
```
### `quercus_sequence_interlaced` — Whether the sequence uses interlaced video (`None` for a stale id).
```c
int32_t quercus_sequence_interlaced(const void* project, uint64_t id);
```
### `quercus_sequence_length` — Returns the sequence's length in rational seconds.
```c
Rational quercus_sequence_length(const void* project, uint64_t id);
```
### `quercus_sequence_playhead` — Returns the sequence playhead position in rational seconds, relative to the sequence start.
```c
Rational quercus_sequence_playhead(const void* project, uint64_t id);
```
### `quercus_sequence_set_interlaced` — Sets the sequence's interlaced flag (true when it was a sequence).
```c
bool quercus_sequence_set_interlaced(const void* project, uint64_t id, bool interlaced);
```
### `quercus_sequence_set_video_params` — Sets the sequence's output video params (width/height/frame rate; pixel format stays F32 RGBA).
```c
bool quercus_sequence_set_video_params(const void* project, uint64_t id, int32_t width, int32_t height, int32_t frame_rate_num, int32_t frame_rate_den);
```
### `quercus_sequence_track_list_at` — Returns the token of the `index`-th track list of `sequence` (video lists first, then audio, then subtitle).
```c
uint64_t quercus_sequence_track_list_at(const void* project, uint64_t id, int32_t index);
```
### `quercus_sequence_track_list_count` — Returns the number of track lists in `sequence`; `0` for a non-sequence token.
```c
int32_t quercus_sequence_track_list_count(const void* project, uint64_t id);
```
<a id="module-track"></a>
## Module: `track` (17 functions)

### `quercus_track_add` — Adds a track to the sequence's list of the given media kind (`TrackType` code; 0 = video, 1 = audio, 2 = subtitle). Undoable.
```c
uint64_t quercus_track_add(const void* project, uint64_t sequence, int32_t kind);
```
### `quercus_track_append_block` — Places `block` on `track` and records `track` as the block's owning track. Undoable.
```c
bool quercus_track_append_block(const void* project, uint64_t track, uint64_t block);
```
### `quercus_track_block_at` — Returns the token of the `index`-th block on `track` in timeline order (`0` = the first/leftmost block).
```c
uint64_t quercus_track_block_at(const void* project, uint64_t id, int32_t index);
```
### `quercus_track_block_containing_time` — Returns the block on `track` whose half-open range `[in, out)` contains `time`, or `NodeId::INVALID` when `time` falls in a gap, on an out-point boundary, or the token is not a track.
```c
uint64_t quercus_track_block_containing_time(const void* project, uint64_t id, Rational time);
```
### `quercus_track_block_count` — Returns the number of blocks on `track` (clips, gaps, transitions and adjustments all count); `0` for a non-track token.
```c
int32_t quercus_track_block_count(const void* project, uint64_t id);
```
### `quercus_track_height` — Returns the track's height in the graph's internal height units (not pixels) as stored on the track.
```c
double quercus_track_height(const void* project, uint64_t id);
```
### `quercus_track_height_pixels` — The track's display height in pixels (24px floor, 64px default).
```c
double quercus_track_height_pixels(const void* project, uint64_t id);
```
### `quercus_track_index` — Returns the zero-based index of `track` in its owning track list's stack order (`0` = bottom of the stack).
```c
int32_t quercus_track_index(const void* project, uint64_t id);
```
### `quercus_track_insert_block_after` — Inserts `block` into `track` immediately after `before`, or at the front when `before` is `None` (the insert command treats a missing predecessor as a prepend). Undoable.
```c
bool quercus_track_insert_block_after(const void* project, uint64_t track, uint64_t block, uint64_t before);
```
### `quercus_track_internal_height_from_pixels` — Converts a pixel track height to the graph's internal height units.
```c
double quercus_track_internal_height_from_pixels(int32_t pixels);
```
### `quercus_track_kind` — Returns the track's media type as a `TrackType` code: `0` = video, `1` = audio, `2` = subtitle.
```c
int32_t quercus_track_kind(const void* project, uint64_t id);
```
### `quercus_track_length` — Returns the track's total length in rational seconds: the out-point of its last block.
```c
Rational quercus_track_length(const void* project, uint64_t id);
```
### `quercus_track_list` — Returns the track list that owns `track`, or `None` when the track is detached, stale, or not a track.
```c
uint64_t quercus_track_list(const void* project, uint64_t id);
```
### `quercus_track_locked` — Whether the track's locked flag is set. Editing commands that move or remove blocks skip locked tracks.
```c
bool quercus_track_locked(const void* project, uint64_t id);
```
### `quercus_track_muted` — Whether the track's muted flag is set: silenced on audio tracks, hidden on video/subtitle tracks (one flag drives both).
```c
bool quercus_track_muted(const void* project, uint64_t id);
```
### `quercus_track_replace_block` — Replaces the block `old` with `new` at `old`'s position on `track`, moving the track ownership to `new`. Undoable.
```c
bool quercus_track_replace_block(const void* project, uint64_t track, uint64_t old, uint64_t new_);
```
### `quercus_track_ripple_remove_block` — Removes `block` from `track`, closing the gap (ripple). Undoable.
```c
bool quercus_track_ripple_remove_block(const void* project, uint64_t track, uint64_t block);
```
<a id="module-tracklist"></a>
## Module: `tracklist` (3 functions)

### `quercus_tracklist_kind` — Returns the track list's media type as a `TrackType` code: `0` = video, `1` = audio, `2` = subtitle.
```c
int32_t quercus_tracklist_kind(const void* project, uint64_t id);
```
### `quercus_tracklist_track_at` — Returns the token of the `index`-th track of `tracklist` in stack order (`0` = bottom of the stack).
```c
uint64_t quercus_tracklist_track_at(const void* project, uint64_t id, int32_t index);
```
### `quercus_tracklist_track_count` — Returns the number of tracks in `tracklist`; `0` for a non-track-list token. Pairs with `Project::tracklist_track_at`.
```c
int32_t quercus_tracklist_track_count(const void* project, uint64_t id);
```
<a id="module-block"></a>
## Module: `block` (20 functions)

### `quercus_block_create_adjustment` — Creates an adjustment block.
```c
uint64_t quercus_block_create_adjustment(const void* project);
```
### `quercus_block_create_clip` — Creates a clip block (not on any track yet — placement is the undoable step).
```c
uint64_t quercus_block_create_clip(const void* project);
```
### `quercus_block_create_gap` — Creates a gap block.
```c
uint64_t quercus_block_create_gap(const void* project);
```
### `quercus_block_create_transition` — Creates a transition block.
```c
uint64_t quercus_block_create_transition(const void* project);
```
### `quercus_block_enabled` — Whether the block is enabled; disabled clips, gaps, transitions and adjustments contribute nothing to playback or render.
```c
bool quercus_block_enabled(const void* project, uint64_t id);
```
### `quercus_block_footage` — Returns the footage node that supplies `block`'s media, or `None` when `block` is not a clip, is stale, or has no footage connected yet.
```c
uint64_t quercus_block_footage(const void* project, uint64_t id);
```
### `quercus_block_in` — Returns the block's timeline in-point in rational seconds: where the block starts on the sequence timeline.
```c
Rational quercus_block_in(const void* project, uint64_t id);
```
### `quercus_block_length` — Returns the block's timeline length in rational seconds, equal to `block_out - block_in` for a normal block.
```c
Rational quercus_block_length(const void* project, uint64_t id);
```
### `quercus_block_link_at` — Returns the linked-block tokens recorded for `block` (its A/V link group), in stored order; stale partner tokens are filtered out.
```c
uint64_t quercus_block_link_at(const void* project, uint64_t id, int32_t index);
```
### `quercus_block_link_count` — Returns the linked-block tokens recorded for `block` (its A/V link group), in stored order; stale partner tokens are filtered out.
```c
int32_t quercus_block_link_count(const void* project, uint64_t id);
```
### `quercus_block_media_in` — Returns the block's media in-point in rational seconds: the position in the source media where playback starts.
```c
Rational quercus_block_media_in(const void* project, uint64_t id);
```
### `quercus_block_media_out` — Returns the block's media out-point in rational seconds: the source position where playback ends.
```c
Rational quercus_block_media_out(const void* project, uint64_t id);
```
### `quercus_block_multicam_source` — The multicam source the block reads from, when its texture input is a multicam node.
```c
uint64_t quercus_block_multicam_source(const void* project, uint64_t id);
```
### `quercus_block_out` — Returns the block's timeline out-point in rational seconds (the exclusive end of the block's timeline range).
```c
Rational quercus_block_out(const void* project, uint64_t id);
```
### `quercus_block_set_length` — Resizes the block from its right edge to `length`, leaving the timeline in-point and the media in-point fixed (the media out-point follows the new length). Undoable.
```c
bool quercus_block_set_length(const void* project, uint64_t block, Rational value);
```
### `quercus_block_set_length_and_media_in` — Sets the block's length, keeping the out point anchored in the media (left-edge resize). Undoable.
```c
bool quercus_block_set_length_and_media_in(const void* project, uint64_t block, Rational value);
```
### `quercus_block_set_media_in` — Sets the block's media in-point, changing which part of the source is shown while keeping the timeline placement and length. Undoable.
```c
bool quercus_block_set_media_in(const void* project, uint64_t block, Rational value);
```
### `quercus_block_speed` — Returns the block's playback speed multiplier: `1.0` = normal, `0.5` = half speed, `2.0` = double speed.
```c
double quercus_block_speed(const void* project, uint64_t id);
```
### `quercus_block_track` — Returns the track that currently holds `block`, or `None` when the block is not placed on a track, is stale, or is not a block.
```c
uint64_t quercus_block_track(const void* project, uint64_t id);
```
### `quercus_block_transition_width` — The width of the transition on one edge of the clip, if any.
```c
Rational quercus_block_transition_width(const void* project, uint64_t id, bool start_edge);
```
<a id="module-effect"></a>
## Module: `effect` (18 functions)

### `quercus_effect_chain_at` — The effect chain of `host`, closest-to-source first (signal order).
```c
uint64_t quercus_effect_chain_at(const void* project, uint64_t host, int32_t index);
```
### `quercus_effect_chain_count` — The effect chain of `host`, closest-to-source first (signal order).
```c
int32_t quercus_effect_chain_count(const void* project, uint64_t host);
```
### `quercus_effect_chain_insert` — Undoable insert ("Add Effect") of a new effect of `type_id` at chain position `index` (clamped to the ends; `usize::MAX` appends). Returns the new node, or `None` on failure (unknown host/type, or the host/type cannot host effects).
```c
uint64_t quercus_effect_chain_insert(const void* project, uint64_t host, int32_t index, const uint8_t* type_id, size_t type_id_len);
```
### `quercus_effect_chain_move` — Undoable reorder ("Reorder Effect") of `effect` to chain position `new_index` (an insertion index **after** removal).
```c
bool quercus_effect_chain_move(const void* project, uint64_t host, uint64_t effect, int32_t new_index);
```
### `quercus_effect_chain_remove` — Undoable removal ("Remove Effect") of `effect` from `host`'s chain. The node stays in the project graph, orphaned (documented engine limitation: node transfer is one-way).
```c
bool quercus_effect_chain_remove(const void* project, uint64_t host, uint64_t effect);
```
### `quercus_effect_factory_category_at` — The built-in factory table (registration order, C++ parity).
```c
int32_t quercus_effect_factory_category_at(int32_t index);
```
### `quercus_effect_factory_count` — The built-in factory table (registration order, C++ parity).
```c
int32_t quercus_effect_factory_count(void);
```
### `quercus_effect_factory_dynamic_count` — The runtime-registered (OpenFX plugin) factory table, in registration order. Empty until `crate::ofx::scan` + `register_plugin_nodes` ran.
```c
int32_t quercus_effect_factory_dynamic_count(void);
```
### `quercus_effect_factory_dynamic_name_at` — The runtime-registered (OpenFX plugin) factory table, in registration order. Empty until `crate::ofx::scan` + `register_plugin_nodes` ran.
```c
int32_t quercus_effect_factory_dynamic_name_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_effect_factory_dynamic_sub_category_at` — The runtime-registered (OpenFX plugin) factory table, in registration order. Empty until `crate::ofx::scan` + `register_plugin_nodes` ran.
```c
int32_t quercus_effect_factory_dynamic_sub_category_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_effect_factory_dynamic_type_id_at` — The runtime-registered (OpenFX plugin) factory table, in registration order. Empty until `crate::ofx::scan` + `register_plugin_nodes` ran.
```c
int32_t quercus_effect_factory_dynamic_type_id_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_effect_factory_flags_at` — The built-in factory table (registration order, C++ parity).
```c
uint64_t quercus_effect_factory_flags_at(int32_t index);
```
### `quercus_effect_factory_name_at` — The built-in factory table (registration order, C++ parity).
```c
int32_t quercus_effect_factory_name_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_effect_factory_type_id_at` — The built-in factory table (registration order, C++ parity).
```c
int32_t quercus_effect_factory_type_id_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_effect_input_id` — Returns the input that carries a node's effect chain, if it has one.
```c
int32_t quercus_effect_input_id(const void* project, uint64_t node, uint8_t* buf, size_t len);
```
### `quercus_effect_is_enabled` — Whether effect node `effect` is currently enabled.
```c
bool quercus_effect_is_enabled(const void* project, uint64_t effect);
```
### `quercus_effect_plugin_instance_handle` — Returns an OpenFX node's instance registry id without taking ownership.
```c
uint64_t quercus_effect_plugin_instance_handle(const void* project, uint64_t node);
```
### `quercus_effect_set_enabled` — Enable or disable effect node `effect` as one undoable command.
```c
bool quercus_effect_set_enabled(const void* project, uint64_t effect, bool enabled);
```
<a id="module-timeline"></a>
## Module: `timeline` (14 functions)

### `quercus_timeline_add_track` — Adds an empty track to a track list. One undoable edit step.
```c
uint64_t quercus_timeline_add_track(const void* project, uint64_t track_list);
```
### `quercus_timeline_block_move` — Moves a block to track `track_index` of `timeline` at `in_` (the source slot becomes a gap).
```c
bool quercus_timeline_block_move(const void* project, uint64_t timeline, int32_t track_index, uint64_t block, Rational in_);
```
### `quercus_timeline_block_place` — Places a block onto track `track_index` of `timeline` at `in_` (moving across tracks / inserting at a point).
```c
bool quercus_timeline_block_place(const void* project, uint64_t timeline, int32_t track_index, uint64_t block, Rational in_);
```
### `quercus_timeline_block_resize` — Resizes a block by moving its out point while keeping the in point anchored. One undoable edit step.
```c
bool quercus_timeline_block_resize(const void* project, uint64_t block, Rational new_length);
```
### `quercus_timeline_block_resize_media_in` — Resizes a block keeping the out point (the media in point shifts).
```c
bool quercus_timeline_block_resize_media_in(const void* project, uint64_t block, Rational new_length);
```
### `quercus_timeline_block_set_enabled` — Enables or disables a block without removing it. One undoable edit step.
```c
bool quercus_timeline_block_set_enabled(const void* project, uint64_t block, bool enabled);
```
### `quercus_timeline_block_set_media_in` — Sets a clip's media in point. One undoable edit step.
```c
bool quercus_timeline_block_set_media_in(const void* project, uint64_t block, Rational media_in);
```
### `quercus_timeline_block_split` — Splits a block at `point` (seconds). The second half becomes a new block (re-query the track for it).
```c
bool quercus_timeline_block_split(const void* project, uint64_t block, Rational point);
```
### `quercus_timeline_block_trim` — Trims a block to `new_length` with explicit trim semantics. One undoable edit step.
```c
bool quercus_timeline_block_trim(const void* project, uint64_t track, uint64_t block, Rational new_length, int32_t mode);
```
### `quercus_timeline_remove_track` — Removes a track and every block it owns. One undoable edit step.
```c
bool quercus_timeline_remove_track(const void* project, uint64_t track);
```
### `quercus_timeline_ripple_remove_area` — Ripple-removes `[in_, out)` from every track of a timeline (the sequence node), closing the gap.
```c
bool quercus_timeline_ripple_remove_area(const void* project, uint64_t timeline, Rational in_, Rational out);
```
### `quercus_timeline_split_preserving_links` — Splits every block at `point` as ONE undoable command (originally linked pairs keep their rear halves linked).
```c
bool quercus_timeline_split_preserving_links(const void* project, const uint64_t* blocks, size_t count, Rational point);
```
### `quercus_timeline_track_split_at` — Splits every block of a track at `point` into two blocks. One undoable edit step.
```c
bool quercus_timeline_track_split_at(const void* project, uint64_t track, Rational point);
```
### `quercus_timeline_transition_set_offsets` — Sets a transition block's in/out offsets and keeps it pinned to its seam.
```c
bool quercus_timeline_transition_set_offsets(const void* project, uint64_t block, Rational in_offset, Rational out_offset);
```
<a id="module-undo"></a>
## Module: `undo` (14 functions)

### `quercus_undo_add_observer` — Registers a change observer: called after any undoable command lands (push, undo, redo, jump, clear) — the "project modified" signal.
```c
void quercus_undo_add_observer(UndoObserverFn cb, void* userdata);
```
### `quercus_undo_can_redo` — Whether the global undo stack can redo.
```c
bool quercus_undo_can_redo(void);
```
### `quercus_undo_can_undo` — Whether the global undo stack can undo.
```c
bool quercus_undo_can_undo(void);
```
### `quercus_undo_clear` — Clears the undo stack (leaving the fresh "new project" row).
```c
bool quercus_undo_clear(void);
```
### `quercus_undo_command_done` — Whether history row `row` is currently done; None when out of range.
```c
int32_t quercus_undo_command_done(int64_t row);
```
### `quercus_undo_command_name` — The display name of history row `row`; None when out of range.
```c
int32_t quercus_undo_command_name(int64_t row, uint8_t* buf, size_t buf_len);
```
### `quercus_undo_count` — Number of commands on the undo stack.
```c
int64_t quercus_undo_count(void);
```
### `quercus_undo_group_abort` — Aborts the current undo group, rolling it back. False when none is open.
```c
bool quercus_undo_group_abort(void);
```
### `quercus_undo_group_begin` — Opens a global undo group: every undoable edit until `undo_group_end` (or `undo_group_abort`) is merged into one history row named `name`.
```c
bool quercus_undo_group_begin(const uint8_t* name, size_t name_len);
```
### `quercus_undo_group_end` — Closes the current undo group. False when none is open.
```c
bool quercus_undo_group_end(void);
```
### `quercus_undo_index` — The current position on the undo stack.
```c
int64_t quercus_undo_index(void);
```
### `quercus_undo_jump` — Jumps to history position `index` (undoing/redoing as needed).
```c
bool quercus_undo_jump(int64_t index);
```
### `quercus_undo_redo` — Redoes one step. False when nothing to redo.
```c
bool quercus_undo_redo(void);
```
### `quercus_undo_undo` — Undoes one step. False when the stack is empty.
```c
bool quercus_undo_undo(void);
```
<a id="module-node"></a>
## Module: `node` (13 functions)

### `quercus_node_color` — Returns the node's color override: a palette index in `0..=7` chosen when a clip is created and kept for the node's lifetime, or `-1` when no override is set / the token is stale.
```c
int32_t quercus_node_color(const void* project, uint64_t id);
```
### `quercus_node_connect` — Connects `from`'s output to `to`'s `input_id` at `element` (-1 = the scalar input). These edges are document structure (persisted in the project file).
```c
bool quercus_node_connect(const void* project, uint64_t from, uint64_t to, const uint8_t* input_id, size_t input_id_len, int32_t element);
```
### `quercus_node_disconnect` — Removes the connection feeding `to`'s input `input_id` at element index `element` (`-1` = the scalar input; `0..` selects an array element).
```c
bool quercus_node_disconnect(const void* project, uint64_t to, const uint8_t* input_id, size_t input_id_len, int32_t element);
```
### `quercus_node_id_invalid` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
uint64_t quercus_node_id_invalid(void);
```
### `quercus_node_id_is_valid` — Whether this token names a real node slot.
```c
bool quercus_node_id_is_valid(uint64_t id);
```
### `quercus_node_kind` — Returns the node's coarse `NodeKind` (folder, footage, sequence, track, clip, gap, transition, adjustment or track list).
```c
int32_t quercus_node_kind(const void* project, uint64_t id);
```
### `quercus_node_label` — Returns the node's user-visible label (set on import/creation or via `Project::node_set_label`).
```c
int32_t quercus_node_label(const void* project, uint64_t id, uint8_t* buf, size_t buf_len);
```
### `quercus_node_remove` — Deletes `id` from the project graph entirely, along with the edges that referenced it. Undoable.
```c
bool quercus_node_remove(const void* project, uint64_t id);
```
### `quercus_node_set_label` — Sets the node's user label (a direct write, not an undoable edit).
```c
bool quercus_node_set_label(const void* project, uint64_t id, const uint8_t* label, size_t label_len);
```
### `quercus_node_set_standard_value` — Writes the static value of `id`'s input `input_id` as a single undoable parameter edit.
```c
bool quercus_node_set_standard_value(const void* project, uint64_t id, const uint8_t* input_id, size_t input_id_len, int32_t element, NodeValue value);
```
### `quercus_node_standard_value` — Reads the static (non-animated) value of `id`'s input `input_id` at element index `element` (`-1` = the scalar input).
```c
bool quercus_node_standard_value(const void* project, uint64_t id, const uint8_t* input_id, size_t input_id_len, int32_t element, NodeValue* out);
```
### `quercus_node_type_id` — Returns the node's full factory type id string (for example `org.olivevideoeditor.Olive.video`), or `None` when the token names no live node.
```c
int32_t quercus_node_type_id(const void* project, uint64_t id, uint8_t* buf, size_t buf_len);
```
### `quercus_node_value_at_time` — The input's value at `time` (keyframe-interpolated when animated).
```c
bool quercus_node_value_at_time(const void* project, uint64_t id, const uint8_t* input_id, size_t input_id_len, int32_t element, Rational time, NodeValue* out);
```
<a id="module-codec"></a>
## Module: `codec` (91 functions)

### `quercus_codec_codec_count` — The number of real codecs (the engine's `Count` sentinel).
```c
int32_t quercus_codec_codec_count(void);
```
### `quercus_codec_codec_get_name` — The human-readable display name (e.g. `"H.264"`).
```c
int32_t quercus_codec_codec_get_name(int32_t codec, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_codec_is_lossless` — Whether the codec is lossless (PCM/FLAC).
```c
bool quercus_codec_codec_is_lossless(int32_t codec);
```
### `quercus_codec_codec_is_still_image` — Whether the codec produces still images (OpenEXR/PNG/TIFF).
```c
bool quercus_codec_codec_is_still_image(int32_t codec);
```
### `quercus_codec_codec_is_valid` — The discriminant for a raw ABI value; `None` outside `0..=18`.
```c
bool quercus_codec_codec_is_valid(int32_t codec);
```
### `quercus_codec_decoder_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_decoder_add_ref(const void* session);
```
### `quercus_codec_decoder_close` — Closes the stream (safe when already closed; also happens on drop).
```c
bool quercus_codec_decoder_close(void* session);
```
### `quercus_codec_decoder_ids` — The ids of all registered decoders (e.g. "ffmpeg", "oiio").
```c
void* quercus_codec_decoder_ids(void);
```
### `quercus_codec_decoder_image_sequence_digit_count` — The trailing-digit count marking `filename` as an image sequence (0 when it is not one).
```c
int32_t quercus_codec_decoder_image_sequence_digit_count(const uint8_t* filename, size_t len);
```
### `quercus_codec_decoder_image_sequence_index` — The sequence index encoded in `filename` (0 when there is none).
```c
int64_t quercus_codec_decoder_image_sequence_index(const uint8_t* filename, size_t len);
```
### `quercus_codec_decoder_open` — Opens a decode session for `filename`'s `stream_index`.
```c
void* quercus_codec_decoder_open(const uint8_t* decoder_id, size_t decoder_id_len, const uint8_t* filename, size_t filename_len, int32_t stream_index);
```
### `quercus_codec_decoder_probe` — Probes `filename` and returns its stream inventory. `None` for the decoder id tries every registered decoder in order; otherwise only the named one. `None` when nothing can read the file.
```c
void* quercus_codec_decoder_probe(const uint8_t* decoder_id, size_t decoder_id_len, const uint8_t* filename, size_t filename_len, const void* cancel);
```
### `quercus_codec_decoder_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_decoder_release(const void* session);
```
### `quercus_codec_decoder_retrieve_audio` — Decodes the interleaved f32 audio covering `range` into `dest`.
```c
int32_t quercus_codec_decoder_retrieve_audio(void* session, float* dest, size_t dest_len, Rational in_, Rational out, int32_t sample_rate, uint64_t channel_layout);
```
### `quercus_codec_decoder_retrieve_video_frame` — Retrieves one video frame into CPU memory.
```c
void* quercus_codec_decoder_retrieve_video_frame(void* session, const void* params);
```
### `quercus_codec_decoder_transform_image_sequence_filename` — Substitutes the frame number into an image-sequence filename (`frame_0001.png` + 42 → `frame_0042.png`).
```c
int32_t quercus_codec_decoder_transform_image_sequence_filename(const uint8_t* filename, size_t filename_len, int64_t number, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_encoder_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_encoder_add_ref(const void* session);
```
### `quercus_codec_encoder_close` — Writes the trailer and closes the output (idempotent; also happens on drop).
```c
bool quercus_codec_encoder_close(void* session);
```
### `quercus_codec_encoder_create` — Creates a configured encoder session; `None` when no encoder supports the requested format or configuration fails.
```c
void* quercus_codec_encoder_create(const QuercusCodecEncodingParams* params);
```
### `quercus_codec_encoder_filename_contains_digit_placeholder` — Whether `filename` contains an image-sequence digit placeholder (`[####]`).
```c
bool quercus_codec_encoder_filename_contains_digit_placeholder(const uint8_t* filename, size_t len);
```
### `quercus_codec_encoder_filename_remove_digit_placeholder` — `filename` with the image-sequence digit placeholder removed.
```c
int32_t quercus_codec_encoder_filename_remove_digit_placeholder(const uint8_t* filename, size_t filename_len, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_encoder_flush` — Flushes the encoders.
```c
bool quercus_codec_encoder_flush(void* session);
```
### `quercus_codec_encoder_image_sequence_placeholder_digit_count` — The digit count of the sequence placeholder in `filename` (0 when none).
```c
int32_t quercus_codec_encoder_image_sequence_placeholder_digit_count(const uint8_t* filename, size_t len);
```
### `quercus_codec_encoder_open` — Opens the output file and writes the headers.
```c
bool quercus_codec_encoder_open(void* session);
```
### `quercus_codec_encoder_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_encoder_release(const void* session);
```
### `quercus_codec_encoder_write_audio` — Encodes `frame_count` frames of interleaved f32 audio.
```c
bool quercus_codec_encoder_write_audio(void* session, const float* samples, size_t sample_count, int32_t frame_count);
```
### `quercus_codec_encoder_write_subtitle` — Encodes one subtitle entry (`in_seconds`/`out_seconds` in seconds). Currently the ffmpeg encoder does not implement subtitle encoding, so this returns `Err` until it does.
```c
bool quercus_codec_encoder_write_subtitle(void* session, const uint8_t* text, size_t text_len, double in_seconds, double out_seconds);
```
### `quercus_codec_encoder_write_video` — Encodes one video frame; the caller keeps its `Frame` reference.
```c
bool quercus_codec_encoder_write_video(void* session, const void* frame);
```
### `quercus_codec_encoding_params_default` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
QuercusCodecEncodingParams quercus_codec_encoding_params_default(void);
```
### `quercus_codec_footage_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_footage_add_ref(const void* fd);
```
### `quercus_codec_footage_audio_stream_count` — The `audio_stream_count` operation.
```c
int32_t quercus_codec_footage_audio_stream_count(const void* fd);
```
### `quercus_codec_footage_decoder_name` — The id of the decoder that produced this description (e.g. "ffmpeg").
```c
int32_t quercus_codec_footage_decoder_name(const void* fd, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_footage_duration` — The total duration, when known.
```c
bool quercus_codec_footage_duration(const void* fd, TimeRange* out);
```
### `quercus_codec_footage_get_audio_stream` — The `ordinal`-th audio stream's params.
```c
bool quercus_codec_footage_get_audio_stream(const void* fd, int32_t ordinal, AudioStreamParams* out);
```
### `quercus_codec_footage_get_subtitle_stream` — The `ordinal`-th subtitle stream's params (per-type ordinal).
```c
bool quercus_codec_footage_get_subtitle_stream(const void* fd, int32_t ordinal, SubtitleParams* out);
```
### `quercus_codec_footage_get_video_stream` — The `ordinal`-th video stream's params (per-type ordinal, NOT the probe-order index).
```c
bool quercus_codec_footage_get_video_stream(const void* fd, int32_t ordinal, VideoParams* out);
```
### `quercus_codec_footage_has_source_start_time` — The source start time, when the media carries one.
```c
bool quercus_codec_footage_has_source_start_time(const void* fd);
```
### `quercus_codec_footage_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_footage_release(const void* fd);
```
### `quercus_codec_footage_source_start_time` — The source start time, when the media carries one.
```c
Rational quercus_codec_footage_source_start_time(const void* fd);
```
### `quercus_codec_footage_stream_type` — The kind of the `index`-th stream in probe order.
```c
int32_t quercus_codec_footage_stream_type(const void* fd, int32_t index);
```
### `quercus_codec_footage_subtitle_stream_count` — The `subtitle_stream_count` operation.
```c
int32_t quercus_codec_footage_subtitle_stream_count(const void* fd);
```
### `quercus_codec_footage_total_stream_count` — Total stream count (video + audio + subtitle).
```c
int32_t quercus_codec_footage_total_stream_count(const void* fd);
```
### `quercus_codec_footage_video_stream_count` — The `video_stream_count` operation.
```c
int32_t quercus_codec_footage_video_stream_count(const void* fd);
```
### `quercus_codec_format_count` — The number of real formats (the engine's `Count` sentinel).
```c
int32_t quercus_codec_format_count(void);
```
### `quercus_codec_format_get_audio_codecs` — The audio codecs this format can carry (see `Format::video_codecs`).
```c
void* quercus_codec_format_get_audio_codecs(int32_t format);
```
### `quercus_codec_format_get_extension` — The file extension this format normally uses (e.g. `"mkv"`).
```c
int32_t quercus_codec_format_get_extension(int32_t format, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_format_get_name` — The human-readable display name (e.g. `"Matroska Video"`).
```c
int32_t quercus_codec_format_get_name(int32_t format, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_format_get_video_codecs` — The video codecs this format can carry, as raw discriminants in an engine vector; convert with `Codec::from_i32`.
```c
void* quercus_codec_format_get_video_codecs(int32_t format);
```
### `quercus_codec_format_is_valid` — The discriminant for a raw ABI value; `None` outside `0..=14`.
```c
bool quercus_codec_format_is_valid(int32_t format);
```
### `quercus_codec_frame_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_frame_add_ref(const void* this_);
```
### `quercus_codec_frame_allocate` — Allocates the pixel buffer per the current params.
```c
bool quercus_codec_frame_allocate(void* this_);
```
### `quercus_codec_frame_allocated_size` — The allocated buffer size in bytes (0 when unallocated).
```c
size_t quercus_codec_frame_allocated_size(const void* this_);
```
### `quercus_codec_frame_channel_count` — The number of audio channels in the allocated frame.
```c
int32_t quercus_codec_frame_channel_count(const void* this_);
```
### `quercus_codec_frame_data` — The pixel buffer; `None` until allocated. See the type docs for the validity window.
```c
uint8_t* quercus_codec_frame_data(void* this_);
```
### `quercus_codec_frame_format` — The allocated pixel format.
```c
int32_t quercus_codec_frame_format(const void* this_);
```
### `quercus_codec_frame_get_params` — A copy of the frame's params.
```c
bool quercus_codec_frame_get_params(const void* this_, VideoParams* out);
```
### `quercus_codec_frame_height` — The frame height in pixels.
```c
int32_t quercus_codec_frame_height(const void* this_);
```
### `quercus_codec_frame_is_allocated` — Whether the pixel buffer has been allocated.
```c
bool quercus_codec_frame_is_allocated(const void* this_);
```
### `quercus_codec_frame_linesize_bytes` — The row stride in bytes.
```c
int32_t quercus_codec_frame_linesize_bytes(const void* this_);
```
### `quercus_codec_frame_linesize_pixels` — The row stride in pixels (the allocated linesize divided by the pixel size).
```c
int32_t quercus_codec_frame_linesize_pixels(const void* this_);
```
### `quercus_codec_frame_new` — An empty frame with default (invalid) params.
```c
void* quercus_codec_frame_new(void);
```
### `quercus_codec_frame_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_frame_release(const void* this_);
```
### `quercus_codec_frame_set_params` — Replaces the frame's params (recomputes line sizes, does NOT reallocate the buffer).
```c
void quercus_codec_frame_set_params(void* this_, VideoParams params);
```
### `quercus_codec_frame_set_timestamp` — Sets the frame timestamp.
```c
void quercus_codec_frame_set_timestamp(void* this_, Rational ts);
```
### `quercus_codec_frame_timestamp` — The frame timestamp (rational seconds).
```c
Rational quercus_codec_frame_timestamp(const void* this_);
```
### `quercus_codec_frame_width` — The frame width in pixels.
```c
int32_t quercus_codec_frame_width(const void* this_);
```
### `quercus_codec_frame_with_params` — A frame carrying a copy of `params`, with line sizes computed.
```c
void* quercus_codec_frame_with_params(VideoParams params);
```
### `quercus_codec_mediaio_register` — Registers `provider` as the process-wide media byte source.
```c
int32_t quercus_codec_mediaio_register(const QuercusMediaIoVTable* vt, void* userdata);
```
### `quercus_codec_mediaio_unregister` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_codec_mediaio_unregister(void);
```
### `quercus_codec_proxy_filename_has_audio` — Whether the proxy filename encodes an audio stream.
```c
bool quercus_codec_proxy_filename_has_audio(const uint8_t* proxy_filename, size_t len);
```
### `quercus_codec_proxy_get_filename` — Derives the proxy filename for a source stream.
```c
int32_t quercus_codec_proxy_get_filename(const uint8_t* cache_path, size_t cache_path_len, const uint8_t* source_filename, size_t source_filename_len, int32_t stream_index, ProxyParams params, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_proxy_get_or_start` — Returns the proxy for a source stream, starting generation through the registered task callback when it is missing.
```c
QuercusCodecProxyResult quercus_codec_proxy_get_or_start(const uint8_t* cache_path, size_t cache_path_len, const uint8_t* source_filename, size_t source_filename_len, int32_t stream_index, ProxyParams params);
```
### `quercus_codec_proxy_get_state` — The on-disk proxy state for `proxy_filename`. Null/invalid input reads as `Missing` on the engine side; from Rust this always succeeds.
```c
int32_t quercus_codec_proxy_get_state(const uint8_t* proxy_filename, size_t len);
```
### `quercus_codec_proxy_get_working_filename` — The working (in-progress) filename for a proxy; `None` on engine error.
```c
int32_t quercus_codec_proxy_get_working_filename(const uint8_t* proxy_filename, size_t len, uint8_t* buf, size_t buf_len);
```
### `quercus_codec_proxy_params_default` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
ProxyParams quercus_codec_proxy_params_default(void);
```
### `quercus_codec_proxy_params_from_config` — The proxy parameters from the user's configuration (defaults when unset).
```c
ProxyParams quercus_codec_proxy_params_from_config(void);
```
### `quercus_codec_retrieve_params_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_retrieve_params_add_ref(const void* p);
```
### `quercus_codec_retrieve_params_clear_target_size` — The target output size; decode scales in one step. `None` keeps the native size.
```c
void quercus_codec_retrieve_params_clear_target_size(void* p);
```
### `quercus_codec_retrieve_params_new` — Creates a new value.
```c
void* quercus_codec_retrieve_params_new(void);
```
### `quercus_codec_retrieve_params_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_codec_retrieve_params_release(const void* p);
```
### `quercus_codec_retrieve_params_set_force_range` — Forces a color-range override for the retrieved frame.
```c
void quercus_codec_retrieve_params_set_force_range(void* p, int32_t range);
```
### `quercus_codec_retrieve_params_set_image_sequence` — Marks the source as an image sequence and bakes in the frame number.
```c
void quercus_codec_retrieve_params_set_image_sequence(void* p, int32_t digits, int64_t number);
```
### `quercus_codec_retrieve_params_set_length` — Sets the footage length used for early-seek semantics.
```c
void quercus_codec_retrieve_params_set_length(void* p, TimeRange length);
```
### `quercus_codec_retrieve_params_set_mode` — Selects the render mode the decoder should use.
```c
void quercus_codec_retrieve_params_set_mode(void* p, int32_t mode);
```
### `quercus_codec_retrieve_params_set_premultiplied_alpha` — Declares whether the retrieved frame's alpha is premultiplied.
```c
void quercus_codec_retrieve_params_set_premultiplied_alpha(void* p, bool premultiplied);
```
### `quercus_codec_retrieve_params_set_stream` — Selects the source file and stream to decode.
```c
bool quercus_codec_retrieve_params_set_stream(void* p, const uint8_t* filename, size_t filename_len, int32_t stream_index);
```
### `quercus_codec_retrieve_params_set_target_size` — The target output size; decode scales in one step. `None` keeps the native size.
```c
void quercus_codec_retrieve_params_set_target_size(void* p, uint32_t width, uint32_t height);
```
### `quercus_codec_retrieve_params_set_time` — Sets the timestamp to retrieve.
```c
void quercus_codec_retrieve_params_set_time(void* p, Rational time);
```
### `quercus_codec_task_set_submit_cb` — Registers (or replaces, or with `None` clears) the process-wide task submit callback. The closure receives a borrowed `TaskRequest` valid only for the call duration and returns 0 to accept or a negative code to reject.
```c
void quercus_codec_task_set_submit_cb(TaskSubmitFn cb, void* userdata);
```
### `quercus_codec_task_submit` — Submits one task request. `Ok(true)` when the task was accepted, `Ok(false)` when no callback is registered, `Err` on invalid input (filename containing NUL) or engine error.
```c
int32_t quercus_codec_task_submit(const QuercusCodecTaskRequest* req);
```
### `quercus_codec_task_submit_is_registered` — Whether a task submit callback is currently registered.
```c
bool quercus_codec_task_submit_is_registered(void);
```
<a id="module-audio"></a>
## Module: `audio` (31 functions)

### `quercus_audio_buffer_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_audio_buffer_add_ref(const void* audio);
```
### `quercus_audio_buffer_copy_samples` — Copies the interleaved F32 samples into an owned vector.
```c
size_t quercus_audio_buffer_copy_samples(const void* audio, float* dst, size_t capacity);
```
### `quercus_audio_buffer_info` — Returns timing and output-layout metadata.
```c
bool quercus_audio_buffer_info(const void* audio, AudioBlockInfo* out);
```
### `quercus_audio_buffer_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_audio_buffer_release(const void* audio);
```
### `quercus_audio_estimate_envelope_offset` — Correlates two envelopes, optionally excluding invalid windows.
```c
OffsetEstimate quercus_audio_estimate_envelope_offset(const double* reference, size_t reference_len, const double* candidate, size_t candidate_len, const bool* reference_valid, size_t reference_valid_len, const bool* candidate_valid, size_t candidate_valid_len, size_t window_samples, int64_t max_offset_windows);
```
### `quercus_audio_estimate_stretch_offset` — Searches a playback-rate correction and offset over two envelopes.
```c
StretchEstimate quercus_audio_estimate_stretch_offset(const double* reference, size_t reference_len, const double* candidate, size_t candidate_len, const bool* reference_valid, size_t reference_valid_len, const bool* candidate_valid, size_t candidate_valid_len, size_t window_samples, int64_t max_offset_windows, double min_rate, double max_rate, double rate_step);
```
### `quercus_audio_levelmeter_analyze` — Analyzes a planar f32 sample buffer (one slice per channel; all channels must have the same length).
```c
bool quercus_audio_levelmeter_analyze(const float** planar, size_t samples_per_channel, size_t channels, ChannelStats* channel_stats, QuercusLevelStats* out);
```
### `quercus_audio_manager_clear_buffered_output` — Drops the queued-but-unplayed output samples.
```c
bool quercus_audio_manager_clear_buffered_output(void);
```
### `quercus_audio_manager_create_instance` — Initializes the audio manager singleton. Idempotent; call once at startup (without it, the calls below fail or report `None`).
```c
bool quercus_audio_manager_create_instance(void);
```
### `quercus_audio_manager_destroy_instance` — Destroys the audio manager singleton (no-op when absent); a later `create_instance` resurrects it with fresh playback state.
```c
void quercus_audio_manager_destroy_instance(void);
```
### `quercus_audio_manager_device_index_by_name` — The enumeration index of the device named `name`, or `None` when no such device exists. The index is what the `set_*_device` calls take.
```c
int32_t quercus_audio_manager_device_index_by_name(const uint8_t* name, size_t len, bool is_output);
```
### `quercus_audio_manager_drop_output_frames` — Drops up to `frames` stale frames from the device queue, returning how many were actually dropped.
```c
int64_t quercus_audio_manager_drop_output_frames(int64_t frames);
```
### `quercus_audio_manager_input_device_names` — The `input_device_names` operation.
```c
void* quercus_audio_manager_input_device_names(void);
```
### `quercus_audio_manager_output_device_names` — The `output_device_names` operation.
```c
void* quercus_audio_manager_output_device_names(void);
```
### `quercus_audio_manager_output_levels` — Writes per-channel linear peaks into `peaks`, returning the live channel count (0 when nothing is playing). `None` on engine failure.
```c
int32_t quercus_audio_manager_output_levels(float* peaks, size_t capacity);
```
### `quercus_audio_manager_output_queued_frames` — Frames currently queued on the output device.
```c
int64_t quercus_audio_manager_output_queued_frames(void);
```
### `quercus_audio_manager_push_to_output` — Pushes one chunk of interleaved samples to the output device. On failure the engine's error text (when it provides one) comes back in the `Err`.
```c
bool quercus_audio_manager_push_to_output(AudioParams params, const uint8_t* sample, size_t sample_len, uint8_t* error_buf, size_t error_buf_len);
```
### `quercus_audio_manager_reset_output_clock` — Re-anchors the output clock at zero.
```c
bool quercus_audio_manager_reset_output_clock(void);
```
### `quercus_audio_manager_seconds` — Seconds of audio consumed by the output device since the last clock reset; `None` when no output stream has run yet.
```c
bool quercus_audio_manager_seconds(double* seconds);
```
### `quercus_audio_manager_set_input_device` — Selects the input device by enumeration index (-1 = system default); used by recording.
```c
bool quercus_audio_manager_set_input_device(int32_t device);
```
### `quercus_audio_manager_set_output_device` — Selects the output device by enumeration index (-1 = system default). The output stream reopens on the next pushed samples.
```c
bool quercus_audio_manager_set_output_device(int32_t device);
```
### `quercus_audio_manager_stop_output` — Stops the output stream (queued samples are dropped).
```c
bool quercus_audio_manager_stop_output(void);
```
### `quercus_audio_manager_take_output_underrun_frames` — Frames the device had to pad with silence since the last call (the counter resets on read). `None` when the instance is gone.
```c
int64_t quercus_audio_manager_take_output_underrun_frames(void);
```
### `quercus_audio_params_bytes_per_sample_per_channel` — Bytes per sample in one channel for a `SampleFormat`.
```c
int64_t quercus_audio_params_bytes_per_sample_per_channel(int32_t format);
```
### `quercus_audio_params_channel_count` — Channel count of an ffmpeg-style channel-layout mask.
```c
int32_t quercus_audio_params_channel_count(uint64_t channel_layout);
```
### `quercus_audio_params_frames_to_rational` — Converts a frame count to rational seconds at `sample_rate`.
```c
Rational quercus_audio_params_frames_to_rational(int64_t frames, int32_t sample_rate);
```
### `quercus_audio_params_rational_to_samples` — Converts a rational time to a sample-frame count at `sample_rate`.
```c
int64_t quercus_audio_params_rational_to_samples(Rational time, int32_t sample_rate);
```
### `quercus_audio_params_samples_to_bytes` — Byte count of `samples` frames across all channels in this format.
```c
int64_t quercus_audio_params_samples_to_bytes(int32_t sample_rate, uint64_t channel_layout, int32_t format, int64_t samples);
```
### `quercus_audio_place_by_source_time` — Aligns candidate source timecode to the reference at a timeline anchor.
```c
SyncPlacement quercus_audio_place_by_source_time(const SourceClip* reference, const SourceClip* candidate, Rational reference_timeline_in);
```
### `quercus_audio_place_by_waveform_offset` — Converts a measured sample offset into a timeline placement.
```c
SyncPlacement quercus_audio_place_by_waveform_offset(Rational reference_timeline_in, int64_t candidate_offset_samples, int32_t sample_rate);
```
### `quercus_audio_waveform_extract` — Decode an audio stream of `filename` into an owned waveform, blocking the calling thread.
```c
const void* quercus_audio_waveform_extract(const uint8_t* filename, size_t filename_len, int32_t stream_index, int32_t samples_per_point);
```
<a id="module-otio"></a>
## Module: `otio` (56 functions)

### `quercus_otio_clip_enabled` — Whether the clip is enabled (None for a non-clip).
```c
int32_t quercus_otio_clip_enabled(const void* c);
```
### `quercus_otio_clip_media_url` — The clip's media URL (None for a non-clip or a non-external — e.g. missing — media reference).
```c
int32_t quercus_otio_clip_media_url(const void* c, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_clip_new` — Builds a clip composable that references external media.
```c
void* quercus_otio_clip_new(const uint8_t* name, size_t name_len, const uint8_t* url, size_t url_len, const QuercusOtioTimeRange* source_range);
```
### `quercus_otio_clip_set_enabled` — Enables or disables a clip.
```c
bool quercus_otio_clip_set_enabled(void* c, bool enabled);
```
### `quercus_otio_clip_set_media_url` — Points the clip's media reference at `url`. False for a non-clip.
```c
bool quercus_otio_clip_set_media_url(void* c, const uint8_t* url, size_t url_len);
```
### `quercus_otio_clip_set_source_range` — Sets the source range of a clip — the portion of its source media it uses.
```c
bool quercus_otio_clip_set_source_range(void* c, QuercusOtioTimeRange range);
```
### `quercus_otio_composable_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_composable_add_ref(const void* c);
```
### `quercus_otio_composable_kind` — The concrete composable schema behind this handle.
```c
int32_t quercus_otio_composable_kind(const void* c);
```
### `quercus_otio_composable_name` — The name ("" for unknown blocks).
```c
int32_t quercus_otio_composable_name(const void* c, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_composable_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_composable_release(const void* c);
```
### `quercus_otio_composable_source_range` — The source range of clips and gaps (None for other kinds or an unset range).
```c
bool quercus_otio_composable_source_range(const void* c, QuercusOtioTimeRange* out);
```
### `quercus_otio_document_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_document_add_ref(const void* doc);
```
### `quercus_otio_document_collection_timeline_at` — The timeline children of a collection root.
```c
void* quercus_otio_document_collection_timeline_at(const void* doc, int32_t index);
```
### `quercus_otio_document_collection_timeline_count` — The timeline children of a collection root.
```c
int32_t quercus_otio_document_collection_timeline_count(const void* doc);
```
### `quercus_otio_document_from_json` — Parses an OTIO JSON document from the full document text `json`.
```c
void* quercus_otio_document_from_json(const uint8_t* json, size_t json_len);
```
### `quercus_otio_document_from_timeline` — Wraps a live `timeline` in a `Document` whose root is that timeline.
```c
void* quercus_otio_document_from_timeline(const void* timeline);
```
### `quercus_otio_document_kind` — The root schema of the parsed document.
```c
int32_t quercus_otio_document_kind(const void* doc);
```
### `quercus_otio_document_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_document_release(const void* doc);
```
### `quercus_otio_document_timeline` — The root timeline (None when the root is not a timeline).
```c
void* quercus_otio_document_timeline(const void* doc);
```
### `quercus_otio_document_to_json` — Serializes to OTIO JSON.
```c
int32_t quercus_otio_document_to_json(const void* doc, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_gap_new` — Builds a gap composable that occupies time without any media.
```c
void* quercus_otio_gap_new(const uint8_t* name, size_t name_len, const QuercusOtioTimeRange* source_range);
```
### `quercus_otio_timeline_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_timeline_add_ref(const void* t);
```
### `quercus_otio_timeline_append_track` — Appends `track` after the last track of the stack.
```c
bool quercus_otio_timeline_append_track(void* tl, const void* track);
```
### `quercus_otio_timeline_global_start_time` — The timeline's global start time, if one is set.
```c
bool quercus_otio_timeline_global_start_time(const void* tl, RationalTime* out);
```
### `quercus_otio_timeline_name` — The timeline's name as UTF-8 text.
```c
int32_t quercus_otio_timeline_name(const void* tl, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_timeline_new` — Creates a timeline named `name` with no tracks.
```c
void* quercus_otio_timeline_new(const uint8_t* name, size_t name_len);
```
### `quercus_otio_timeline_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_timeline_release(const void* t);
```
### `quercus_otio_timeline_remove_track` — Removes the track at `index`, shifting later tracks down by one.
```c
bool quercus_otio_timeline_remove_track(void* tl, int32_t index);
```
### `quercus_otio_timeline_set_global_start_time` — Sets the timeline's global start time (OTIO `global_start_time`), the time value of the timeline's first frame.
```c
bool quercus_otio_timeline_set_global_start_time(void* tl, RationalTime start);
```
### `quercus_otio_timeline_set_name` — Renames the timeline, replacing the previous name with `name`.
```c
bool quercus_otio_timeline_set_name(void* tl, const uint8_t* name, size_t name_len);
```
### `quercus_otio_timeline_set_track` — Replaces the track at `index` with `track`, publishing edits made to a clone.
```c
bool quercus_otio_timeline_set_track(void* tl, int32_t index, const void* track);
```
### `quercus_otio_timeline_to_json` — Serializes the timeline alone to OTIO JSON.
```c
int32_t quercus_otio_timeline_to_json(const void* tl, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_timeline_track_at` — The track at `index` (a clone; edit and write back with `Timeline::set_track`).
```c
void* quercus_otio_timeline_track_at(const void* tl, int32_t index);
```
### `quercus_otio_timeline_track_count` — Number of tracks.
```c
int32_t quercus_otio_timeline_track_count(const void* tl);
```
### `quercus_otio_timelines_add` — Appends `timeline` to the list, adding one more `<sequence>` on FCPXML export.
```c
bool quercus_otio_timelines_add(void* t, const void* timeline);
```
### `quercus_otio_timelines_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_timelines_add_ref(const void* t);
```
### `quercus_otio_timelines_at` — The timeline at `index` (a clone; edit and write back with `Timelines::set`).
```c
void* quercus_otio_timelines_at(const void* t, int32_t index);
```
### `quercus_otio_timelines_count` — Number of timelines.
```c
int32_t quercus_otio_timelines_count(const void* t);
```
### `quercus_otio_timelines_from_fcpxml` — Parses an FCPXML document from the full document text `xml`.
```c
void* quercus_otio_timelines_from_fcpxml(const uint8_t* xml, size_t xml_len);
```
### `quercus_otio_timelines_new` — A new, empty list.
```c
void* quercus_otio_timelines_new(void);
```
### `quercus_otio_timelines_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_timelines_release(const void* t);
```
### `quercus_otio_timelines_set` — Replaces the timeline at `index` (write-back after editing).
```c
bool quercus_otio_timelines_set(void* t, int32_t index, const void* timeline);
```
### `quercus_otio_timelines_to_fcpxml` — Serializes every timeline in the list to one FCPXML document.
```c
int32_t quercus_otio_timelines_to_fcpxml(const void* t, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_track_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_track_add_ref(const void* t);
```
### `quercus_otio_track_append_child` — Appends `child` (a clip, gap, or transition) after the last composable.
```c
bool quercus_otio_track_append_child(void* t, const void* child);
```
### `quercus_otio_track_child_at` — The child at `index` (a clone; edit and write back with `Track::set_child`).
```c
void* quercus_otio_track_child_at(const void* t, int32_t index);
```
### `quercus_otio_track_child_count` — Number of children (clips/gaps/transitions).
```c
int32_t quercus_otio_track_child_count(const void* t);
```
### `quercus_otio_track_kind` — The track kind ("Video", "Audio", ...).
```c
int32_t quercus_otio_track_kind(const void* t, uint8_t* buf, size_t buf_len);
```
### `quercus_otio_track_new` — A new track of the given kind (`track_kind::VIDEO`, `track_kind::AUDIO`, ...).
```c
void* quercus_otio_track_new(const uint8_t* kind, size_t kind_len);
```
### `quercus_otio_track_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_otio_track_release(const void* t);
```
### `quercus_otio_track_remove_child` — Removes the child at `index`, shifting later children down by one.
```c
bool quercus_otio_track_remove_child(void* t, int32_t index);
```
### `quercus_otio_track_set_child` — Replaces the child at `index` with `child`, publishing edits made to a clone.
```c
bool quercus_otio_track_set_child(void* t, int32_t index, const void* child);
```
### `quercus_otio_transition_new` — Builds a transition composable that blends two adjacent clips.
```c
void* quercus_otio_transition_new(const uint8_t* name, size_t name_len, RationalTime in_offset, RationalTime out_offset);
```
### `quercus_otio_transition_offset` — One side's offset of a transition, in the timeline rate.
```c
bool quercus_otio_transition_offset(const void* c, bool is_in, RationalTime* out);
```
### `quercus_otio_transition_set_offset` — Sets one side's offset of a transition.
```c
bool quercus_otio_transition_set_offset(void* c, bool is_in, RationalTime offset);
```
### `quercus_otio_transition_type` — The transition type ("SMPTE_Dissolve", ...; None for a non-transition).
```c
int32_t quercus_otio_transition_type(const void* c, uint8_t* buf, size_t buf_len);
```
<a id="module-ofx"></a>
## Module: `ofx` (91 functions)

### `quercus_ofx_alive_count` — Live instance count (leak diagnostic).
```c
int32_t quercus_ofx_alive_count(void);
```
### `quercus_ofx_cancel_render` — Cancels inline and worker plugin renders at their next progress boundary.
```c
void quercus_ofx_cancel_render(void);
```
### `quercus_ofx_curveset_add_curve` — Appends an empty curve, returning its index.
```c
int32_t quercus_ofx_curveset_add_curve(const void* set);
```
### `quercus_ofx_curveset_add_identity_curve` — Appends an identity curve over `[lo, hi]` (the parametric default), returning its index.
```c
int32_t quercus_ofx_curveset_add_identity_curve(const void* set, double lo, double hi);
```
### `quercus_ofx_curveset_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_ofx_curveset_add_ref(const void* set);
```
### `quercus_ofx_curveset_clear_curve` — Removes all control points of a curve (it then evaluates as identity).
```c
bool quercus_ofx_curveset_clear_curve(const void* set, int32_t curve);
```
### `quercus_ofx_curveset_curve_count` — Number of curves.
```c
int32_t quercus_ofx_curveset_curve_count(const void* set);
```
### `quercus_ofx_curveset_delete_point` — Removes the control point at `index` from `curve` and recomputes the remaining slopes; later points shift down one index.
```c
bool quercus_ofx_curveset_delete_point(const void* set, int32_t curve, int32_t index);
```
### `quercus_ofx_curveset_evaluate` — Evaluates a curve at `x` (cubic Hermite; out-of-range keys clamp to the endpoint values, an empty curve is identity).
```c
double quercus_ofx_curveset_evaluate(const void* set, int32_t curve, double x);
```
### `quercus_ofx_curveset_from_json` — Parses curve-set JSON (as produced by `Self::to_json` or the C++/node side) into a new refcounted set.
```c
void* quercus_ofx_curveset_from_json(const uint8_t* json, size_t json_len);
```
### `quercus_ofx_curveset_new` — A new, empty curve set.
```c
void* quercus_ofx_curveset_new(void);
```
### `quercus_ofx_curveset_point_at` — Copies the control point at `index` of `curve`, including its explicit Hermite `slope`.
```c
bool quercus_ofx_curveset_point_at(const void* set, int32_t curve, int32_t index, QuercusOfxCurvePoint* out);
```
### `quercus_ofx_curveset_point_count` — Number of control points in `curve`, or `None` when `curve` is out of range.
```c
int32_t quercus_ofx_curveset_point_count(const void* set, int32_t curve);
```
### `quercus_ofx_curveset_push_point` — Inserts `point` into `curve` at its `key` position, or overwrites the point already stored at that exact key, keeping the explicit Hermite `slope` (unlike `Self::upsert_point`, which recomputes slopes).
```c
bool quercus_ofx_curveset_push_point(const void* set, int32_t curve, QuercusOfxCurvePoint point);
```
### `quercus_ofx_curveset_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_ofx_curveset_release(const void* set);
```
### `quercus_ofx_curveset_to_json` — Serializes the whole set — every curve and control point, slopes included — to the engine's canonical curve-set JSON, the payload node values and project files store.
```c
int32_t quercus_ofx_curveset_to_json(const void* set, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_curveset_upsert_point` — Inserts or overwrites the control point at `key` (key order is maintained; slopes are recomputed).
```c
bool quercus_ofx_curveset_upsert_point(const void* set, int32_t curve, double key, double value);
```
### `quercus_ofx_gl_acquire` — Makes the GL context current on the calling thread (globally serialized). None when GL is unavailable.
```c
bool quercus_ofx_gl_acquire(void);
```
### `quercus_ofx_gl_available` — Whether the offscreen GL context can be created (false → fall back to CPU rendering).
```c
bool quercus_ofx_gl_available(void);
```
### `quercus_ofx_gl_bind_fbo` — Binds `fbo` as the framebuffer used by subsequent draws and readbacks; `0` selects the default framebuffer. Pair with `create_fbo` and restore `0` before handing control back to the host.
```c
void quercus_ofx_gl_bind_fbo(int32_t fbo);
```
### `quercus_ofx_gl_clear` — Clears the color buffer.
```c
void quercus_ofx_gl_clear(void);
```
### `quercus_ofx_gl_clear_color` — Sets the color written by `clear` into the currently bound framebuffer, as normalised RGBA components (`0.0..=1.0`, values outside that range are clamped by GL).
```c
void quercus_ofx_gl_clear_color(float r, float g, float b, float a);
```
### `quercus_ofx_gl_create_fbo` — Creates an FBO with `texture` attached, returning its GL name.
```c
int32_t quercus_ofx_gl_create_fbo(int32_t texture, int32_t width, int32_t height);
```
### `quercus_ofx_gl_create_input_texture` — Creates an RGBA32F texture and uploads `pixels` (tight RGBA F32, `width*height*4` floats), returning its GL name.
```c
int32_t quercus_ofx_gl_create_input_texture(int32_t width, int32_t height, const float* pixels);
```
### `quercus_ofx_gl_create_output_texture` — Creates an RGBA32F render-target texture of `width` x `height` pixels and returns its GL name. The texture is not attached to any FBO yet; use `create_fbo` to make it renderable and `delete_texture` to free it.
```c
int32_t quercus_ofx_gl_create_output_texture(int32_t width, int32_t height);
```
### `quercus_ofx_gl_delete_fbo` — Deletes a framebuffer object previously returned by `create_fbo`. The attached texture is not deleted; release it separately with `delete_texture` once no FBO references it.
```c
void quercus_ofx_gl_delete_fbo(int32_t fbo);
```
### `quercus_ofx_gl_delete_texture` — Deletes the GL texture `texture`, as returned by `create_output_texture` or `create_input_texture`. The caller must first detach it from any live FBO and must not use the name afterwards.
```c
void quercus_ofx_gl_delete_texture(int32_t texture);
```
### `quercus_ofx_gl_read_pixels_f32` — Reads the current framebuffer back as tight RGBA F32 into `out` (`width*height*4` floats). None on failure.
```c
int32_t quercus_ofx_gl_read_pixels_f32(int32_t width, int32_t height, float* out, size_t out_capacity);
```
### `quercus_ofx_gl_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_ofx_gl_release(void);
```
### `quercus_ofx_gl_set_viewport` — Sets the GL viewport to the `width` x `height` pixel rectangle at the framebuffer's lower-left origin, which is the region later draws and `read_pixels_f32` operate on.
```c
void quercus_ofx_gl_set_viewport(int32_t width, int32_t height);
```
### `quercus_ofx_gl_version` — The version string of the offscreen GL context (the `GL_VERSION` text reported by the GL bridge), for diagnostics and logging.
```c
int32_t quercus_ofx_gl_version(uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_has_progress_handler` — Whether a progress handler is registered.
```c
bool quercus_ofx_has_progress_handler(void);
```
### `quercus_ofx_install_render_executor` — Installs the render executor / plugin-node duplicator (idempotent).
```c
void quercus_ofx_install_render_executor(void);
```
### `quercus_ofx_instance_begin_sequence` — The beginSequenceRender bracket (CPU mode): opens a sequence render over `[min, max]` seconds. Pair with `Instance::end_sequence`.
```c
bool quercus_ofx_instance_begin_sequence(uint64_t id, double min, double max);
```
### `quercus_ofx_instance_clear_persistent_messages` — Clears the instance's persistent messages (the badge/message data the inspector shows), e.g. when the instance is released or the host shuts down.
```c
void quercus_ofx_instance_clear_persistent_messages(uint64_t id);
```
### `quercus_ofx_instance_context` — The context the instance was created in.
```c
int32_t quercus_ofx_instance_context(uint64_t id, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_instance_count` — Number of registered instances (diagnostic).
```c
int32_t quercus_ofx_instance_count(void);
```
### `quercus_ofx_instance_create` — Creates an instance of `plugin_id`. `None` context uses the host's preferred-context fallback chain (filter → general → tracker → paint → rest). None on failure (unknown plugin, no supported context, plugin error).
```c
uint64_t quercus_ofx_instance_create(const uint8_t* plugin_id, size_t plugin_id_len, const uint8_t* context, size_t context_len);
```
### `quercus_ofx_instance_end_sequence` — Closes the `endSequenceRender` bracket opened by `Self::begin_sequence`, reporting the rendered `[min, max]` range so the plugin can flush per-sequence caches and resources.
```c
bool quercus_ofx_instance_end_sequence(uint64_t id, double min, double max);
```
### `quercus_ofx_instance_get_rod` — The instance's region of definition at `time` — the canonical (render-scale 1:1) rectangle the plugin renders into, used to size and position the node's output.
```c
bool quercus_ofx_instance_get_rod(uint64_t id, double time, QuercusOfxRectD* out);
```
### `quercus_ofx_instance_is_cancelled` — Whether the cancel flag is set.
```c
bool quercus_ofx_instance_is_cancelled(uint64_t id);
```
### `quercus_ofx_instance_param_count` — Number of parameters.
```c
int32_t quercus_ofx_instance_param_count(uint64_t id);
```
### `quercus_ofx_instance_persistent_message_count` — Reads an existing instance's persistent-message count without taking ownership of its registry handle.
```c
int32_t quercus_ofx_instance_persistent_message_count(uint64_t id);
```
### `quercus_ofx_instance_push_button` — Triggers a push-button parameter (the inspector's button path).
```c
bool quercus_ofx_instance_push_button(uint64_t id, const uint8_t* param, size_t param_len);
```
### `quercus_ofx_instance_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_ofx_instance_release(uint64_t id);
```
### `quercus_ofx_instance_render_frame` — Renders one frame through the effect (CPU path): `src` is the input frame (tight RGBA F32; `None` for sources/generators), the output is written to `dst` (tight RGBA F32, must hold `dst_w*dst_h*4` floats). False on failure (invalid buffers, cancelled, plugin error). Set parameters first; long renders report progress through the registered handler.
```c
bool quercus_ofx_instance_render_frame(uint64_t id, double time, const float* src, int32_t src_w, int32_t src_h, float* dst, int32_t dst_w, int32_t dst_h);
```
### `quercus_ofx_instance_set_cancelled` — Sets/clears the cancel flag: a running render aborts at the next progress batch boundary.
```c
void quercus_ofx_instance_set_cancelled(uint64_t id, bool cancelled);
```
### `quercus_ofx_interact_create` — Creates the overlay interact (None when the plugin declares none). The interact is destroyed on drop.
```c
uint64_t quercus_ofx_interact_create(uint64_t instance);
```
### `quercus_ofx_interact_destroy` — Explicitly destroys the interact now. Other clones become inert, and their later drops are no-ops.
```c
void quercus_ofx_interact_destroy(uint64_t id);
```
### `quercus_ofx_interact_draw` — The draw action: the plugin paints its overlay into the currently bound framebuffer (acquire the GL bridge first — see `gl`). `viewport` is the viewport size in pixels, `pixel_scale` the canonical→pixel ratio. No background image is passed; composite the overlay over the frame yourself.
```c
int32_t quercus_ofx_interact_draw(uint64_t id, double viewport_width, double viewport_height, double pixel_scale_x, double pixel_scale_y, double time);
```
### `quercus_ofx_interact_gain_focus` — Notifies the plugin that its overlay gained viewer focus at `time`; the pen and key actions are only meaningful while the interact holds focus.
```c
int32_t quercus_ofx_interact_gain_focus(uint64_t id, double time);
```
### `quercus_ofx_interact_idle` — The idle action (throttle to ~10 Hz).
```c
int32_t quercus_ofx_interact_idle(uint64_t id);
```
### `quercus_ofx_interact_key_down` — The key down action (`key_sym` is an ofxKeySyms value — see `key`; `key_string` the character for printable keys).
```c
int32_t quercus_ofx_interact_key_down(uint64_t id, int32_t key_sym, const uint8_t* key, size_t key_len, double time);
```
### `quercus_ofx_interact_key_up` — The key up action: tells the plugin a key was released at `time` (OFX `kOfxInteractActionKeyUp`), completing the pair opened by `Self::key_down`.
```c
int32_t quercus_ofx_interact_key_up(uint64_t id, int32_t key_sym, const uint8_t* key, size_t key_len, double time);
```
### `quercus_ofx_interact_lose_focus` — Notifies the plugin that its overlay lost viewer focus at `time`, so it can dismiss transient UI. Pair with `Self::gain_focus`.
```c
int32_t quercus_ofx_interact_lose_focus(uint64_t id, double time);
```
### `quercus_ofx_interact_pen_down` — The pen down action: tells the plugin the pen was pressed at `position` and `time` (OFX `kOfxInteractActionPenDown`), reporting full pressure `1.0`.
```c
int32_t quercus_ofx_interact_pen_down(uint64_t id, double x, double y, double time);
```
### `quercus_ofx_interact_pen_motion` — The pen motion action (`pen_down` maps to pressure 1.0/0.0).
```c
int32_t quercus_ofx_interact_pen_motion(uint64_t id, double x, double y, bool pen_down, double time);
```
### `quercus_ofx_interact_pen_up` — The pen up action: tells the plugin the pen was lifted at `position` and `time` (OFX `kOfxInteractActionPenUp`), closing the pair opened by `Self::pen_down`.
```c
int32_t quercus_ofx_interact_pen_up(uint64_t id, double x, double y, double time);
```
### `quercus_ofx_interact_take_redraw` — Takes the redraw-requested flag (set by the plugin via interactRedraw).
```c
bool quercus_ofx_interact_take_redraw(uint64_t id);
```
### `quercus_ofx_interact_take_swap` — Takes the swap-buffers-requested flag.
```c
bool quercus_ofx_interact_take_swap(uint64_t id);
```
### `quercus_ofx_param_get_bool` — Reads a boolean parameter.
```c
int32_t quercus_ofx_param_get_bool(uint64_t id, const uint8_t* name, size_t name_len);
```
### `quercus_ofx_param_get_choice` — Reads a choice parameter as its zero-based option index.
```c
int32_t quercus_ofx_param_get_choice(uint64_t id, const uint8_t* name, size_t name_len);
```
### `quercus_ofx_param_get_color` — Reads a color parameter, returning 3 components for RGB and 4 for RGBA (the parameter's arity).
```c
int32_t quercus_ofx_param_get_color(uint64_t id, const uint8_t* name, size_t name_len, double* out, size_t cap);
```
### `quercus_ofx_param_get_curves_json` — Reads a parametric parameter as a curve set, parsed from the canonical curve-set JSON the plugin returns.
```c
int32_t quercus_ofx_param_get_curves_json(uint64_t id, const uint8_t* name, size_t name_len, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_param_get_double` — Reads a double-family parameter (Double/Double2D/Double3D), returning one value per dimension in component order.
```c
int32_t quercus_ofx_param_get_double(uint64_t id, const uint8_t* name, size_t name_len, double* out, size_t cap);
```
### `quercus_ofx_param_get_int` — Reads an integer-family parameter (Integer/Integer2D/Integer3D).
```c
int32_t quercus_ofx_param_get_int(uint64_t id, const uint8_t* name, size_t name_len, int32_t* out, size_t cap);
```
### `quercus_ofx_param_get_string` — Reads a string-family parameter (String/StrChoice) as UTF-8 text.
```c
int32_t quercus_ofx_param_get_string(uint64_t id, const uint8_t* name, size_t name_len, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_param_label_at` — The display label (`kOfxPropLabel`) of the parameter at `index`, as shown in the inspector.
```c
int32_t quercus_ofx_param_label_at(uint64_t id, int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_param_name_at` — The internal name of the parameter at `index`, in definition order; this is the key accepted by the `param_*`/`set_param_*` accessors.
```c
int32_t quercus_ofx_param_name_at(uint64_t id, int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_param_set_bool` — Writes a boolean parameter.
```c
bool quercus_ofx_param_set_bool(uint64_t id, const uint8_t* name, size_t name_len, bool value);
```
### `quercus_ofx_param_set_choice` — Writes a choice parameter by option index (the position in the parameter's option menu, not its string).
```c
bool quercus_ofx_param_set_choice(uint64_t id, const uint8_t* name, size_t name_len, int32_t index);
```
### `quercus_ofx_param_set_color` — Writes a color parameter from `values` (3 components for RGB, 4 for RGBA, matching the parameter's arity).
```c
bool quercus_ofx_param_set_color(uint64_t id, const uint8_t* name, size_t name_len, const double* values, size_t dim);
```
### `quercus_ofx_param_set_curves_json` — Writes a parametric parameter from `curves`, serialising them to the canonical curve-set JSON that the plugin's `set` action reads.
```c
bool quercus_ofx_param_set_curves_json(uint64_t id, const uint8_t* name, size_t name_len, const uint8_t* json, size_t json_len);
```
### `quercus_ofx_param_set_double` — Writes a double-family parameter (Double/Double2D/Double3D) from `values`; the length must equal the parameter's arity (1..=3).
```c
bool quercus_ofx_param_set_double(uint64_t id, const uint8_t* name, size_t name_len, const double* values, size_t dim);
```
### `quercus_ofx_param_set_int` — Writes an integer-family parameter. False on unknown name or a type/arity mismatch.
```c
bool quercus_ofx_param_set_int(uint64_t id, const uint8_t* name, size_t name_len, const int32_t* values, size_t dim);
```
### `quercus_ofx_param_set_string` — Writes a string-family parameter (String/StrChoice), replacing its current text.
```c
bool quercus_ofx_param_set_string(uint64_t id, const uint8_t* name, size_t name_len, const uint8_t* value, size_t value_len);
```
### `quercus_ofx_param_type_at` — The parameter's OFX type string (`kOfxParamType*`) at `index`.
```c
int32_t quercus_ofx_param_type_at(uint64_t id, int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_plugin_context_at` — The metadata of the plugin at `index` in scan order, or `None` when `index` is out of range or one of its metadata fields cannot be read.
```c
int32_t quercus_ofx_plugin_context_at(int32_t index, int32_t context_index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_plugin_context_count_at` — The metadata of the plugin at `index` in scan order, or `None` when `index` is out of range or one of its metadata fields cannot be read.
```c
int32_t quercus_ofx_plugin_context_count_at(int32_t index);
```
### `quercus_ofx_plugin_count` — Number of discovered plugins.
```c
int32_t quercus_ofx_plugin_count(void);
```
### `quercus_ofx_plugin_identifier_at` — The metadata of the plugin at `index` in scan order, or `None` when `index` is out of range or one of its metadata fields cannot be read.
```c
int32_t quercus_ofx_plugin_identifier_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_plugin_label_at` — The metadata of the plugin at `index` in scan order, or `None` when `index` is out of range or one of its metadata fields cannot be read.
```c
int32_t quercus_ofx_plugin_label_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_plugin_path_at` — The metadata of the plugin at `index` in scan order, or `None` when `index` is out of range or one of its metadata fields cannot be read.
```c
int32_t quercus_ofx_plugin_path_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_ofx_plugin_version_at` — The metadata of the plugin at `index` in scan order, or `None` when `index` is out of range or one of its metadata fields cannot be read.
```c
bool quercus_ofx_plugin_version_at(int32_t index, uint32_t* major, uint32_t* minor);
```
### `quercus_ofx_register_plugin_nodes` — Registers every discovered plugin into the node factory as a dynamic node type (idempotent; also installs the render executor and the plugin-node duplicator). Returns the number of newly registered node types.
```c
int32_t quercus_ofx_register_plugin_nodes(void);
```
### `quercus_ofx_scan` — Scans the standard OFX plugin paths. None when the scan failed (plugins are optional; a failure never aborts the caller).
```c
int32_t quercus_ofx_scan(void);
```
### `quercus_ofx_scan_path` — Scans one extra plugin directory on top of the standard OFX search paths, loading any bundles found into the discovered set.
```c
bool quercus_ofx_scan_path(const uint8_t* path, size_t path_len);
```
### `quercus_ofx_set_progress_handler` — Registers the progress handler: `on_progress(label, message, fraction)` fires on every plugin progressUpdate (return false to request cancellation), `on_end()` on progressEnd. Re-registering replaces the previous handler.
```c
void quercus_ofx_set_progress_handler(ProgressFn cb, ProgressEndFn end_cb, void* userdata);
```
### `quercus_ofx_set_project_extent` — Pushes the project extent (canvas size in canonical pixels) that the OFX normalised-coordinate default conversion maps the `[0, 1]` range onto.
```c
void quercus_ofx_set_project_extent(double width, double height);
```
### `quercus_ofx_set_viewer_time` — Updates the active-viewer time snapshot the timeline suite falls back to when no render context is live.
```c
void quercus_ofx_set_viewer_time(double time, double range_min, double range_max);
```
### `quercus_ofx_shutdown` — Shuts the host down (destroys live instances, unloads plugins).
```c
void quercus_ofx_shutdown(void);
```
<a id="module-plugin"></a>
## Module: `plugin` (10 functions)

### `quercus_plugin_add_command` — Contributes a command (menu item). `menu_path` uses `/` separators (e.g. "Tools/Export"). False when the id is already contributed.
```c
bool quercus_plugin_add_command(int32_t index, const uint8_t* command_id, size_t command_id_len, const uint8_t* title, size_t title_len, const uint8_t* menu_path, size_t menu_path_len);
```
### `quercus_plugin_command_count` — This plugin's contributed commands.
```c
int32_t quercus_plugin_command_count(int32_t index);
```
### `quercus_plugin_command_id_at` — This plugin's contributed commands.
```c
int32_t quercus_plugin_command_id_at(int32_t index, int32_t command_index, uint8_t* buf, size_t buf_len);
```
### `quercus_plugin_command_menu_path_at` — This plugin's contributed commands.
```c
int32_t quercus_plugin_command_menu_path_at(int32_t index, int32_t command_index, uint8_t* buf, size_t buf_len);
```
### `quercus_plugin_command_title_at` — This plugin's contributed commands.
```c
int32_t quercus_plugin_command_title_at(int32_t index, int32_t command_index, uint8_t* buf, size_t buf_len);
```
### `quercus_plugin_count` — Number of registered plugins.
```c
int32_t quercus_plugin_count(void);
```
### `quercus_plugin_descriptor` — The plugin's metadata as currently registered.
```c
bool quercus_plugin_descriptor(int32_t index, QuercusPluginDescriptor* out);
```
### `quercus_plugin_invoke_command` — Invokes a command by id (dispatches to the contributing plugin).
```c
bool quercus_plugin_invoke_command(const uint8_t* command_id, size_t command_id_len);
```
### `quercus_plugin_register` — Registers `descriptor` and installs `on_command` as its command callback.
```c
int32_t quercus_plugin_register(const QuercusPluginDescriptor* descriptor, CommandFn on_command, void* userdata);
```
### `quercus_plugin_unregister` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_plugin_unregister(int32_t index);
```
<a id="module-render"></a>
## Module: `render` (32 functions)

### `quercus_render_disk_cache_clear` — Clears the disk cache, deleting every cached file rather than only entries over the configured budget.
```c
bool quercus_render_disk_cache_clear(void);
```
### `quercus_render_disk_cache_max_bytes` — The configured disk-cache byte budget (`0` = unlimited).
```c
int64_t quercus_render_disk_cache_max_bytes(void);
```
### `quercus_render_disk_cache_path` — Returns the disk cache directory currently in use.
```c
int32_t quercus_render_disk_cache_path(uint8_t* buf, size_t buf_len);
```
### `quercus_render_disk_cache_prune` — Prunes the disk cache to the configured budget; returns bytes freed.
```c
int64_t quercus_render_disk_cache_prune(void);
```
### `quercus_render_disk_cache_set_max_bytes` — Sets the disk-cache byte budget (`0` = unlimited) and prunes immediately.
```c
bool quercus_render_disk_cache_set_max_bytes(int64_t max_bytes);
```
### `quercus_render_disk_cache_set_path` — Points the disk cache at `path` (absolute; created when missing). Only caches created afterwards use it.
```c
bool quercus_render_disk_cache_set_path(const uint8_t* path, size_t path_len);
```
### `quercus_render_disk_cache_size` — The disk cache size in bytes.
```c
int64_t quercus_render_disk_cache_size(void);
```
### `quercus_render_frame_f32` — Renders a sequence or video-footage viewer at `time`, returning tight F32 RGBA pixels (`width*height*4` floats). None on failure (unknown viewer, invalid size, render error).
```c
int32_t quercus_render_frame_f32(const void* project, uint64_t viewer, Rational time, int32_t width, int32_t height, float* dst, size_t dst_capacity);
```
### `quercus_render_frame_rgba8_display` — Renders a viewer frame with the project's working → display transform applied on the GPU, returning tight 8-bit RGBA (`width*height*4` bytes).
```c
int32_t quercus_render_frame_rgba8_display(const void* project, uint64_t viewer, Rational time, int32_t width, int32_t height, const uint8_t* gamut, size_t gamut_len, const uint8_t* transfer, size_t transfer_len, uint8_t* dst, size_t dst_capacity);
```
### `quercus_render_gpu_host_installed` — True when the host installed or marked the shared render device.
```c
bool quercus_render_gpu_host_installed(void);
```
### `quercus_render_gpu_init` — Initializes the engine-owned shared GPU context with the requested backend, before the first render. Unlike `gpu_install` this needs no host wgpu device/queue pointers, so pure C hosts can use it too: the engine creates and owns the device exactly like the lazy first-render path, just eagerly and with an explicit backend choice.
```c
bool quercus_render_gpu_init(int32_t backend_kind);
```
### `quercus_render_gpu_install` — Hands the host's wgpu device/queue to the engine so the render thread creates its textures on the presenter's device (zero-copy present).
```c
bool quercus_render_gpu_install(const void* wgpu_device, const void* wgpu_queue, int32_t backend_kind);
```
### `quercus_render_gpu_is_adopted` — True when the engine renders on a host-adopted device (zero-copy present); false when no context exists or the engine created its own.
```c
bool quercus_render_gpu_is_adopted(void);
```
### `quercus_render_gpu_kind` — The engine's active GPU backend, or `None` when no context exists.
```c
int32_t quercus_render_gpu_kind(void);
```
### `quercus_render_gpu_mark_host` — Declares the engine-created shared context as the host's render device (macOS/Windows, where the windowing layer does not hand out its wgpu device); creates the context on first use. False without an adapter.
```c
bool quercus_render_gpu_mark_host(void);
```
### `quercus_render_gpu_set_display_lut` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_render_gpu_set_display_lut(uint32_t edge, const float* lo, const float* hi, const float* data, size_t len);
```
### `quercus_render_gpu_set_display_transform_required` — Declares whether the host applies a display/output transform on the CPU.
```c
void quercus_render_gpu_set_display_transform_required(bool required);
```
### `quercus_render_manager_clear_graph_snapshot` — Drops the manager's current snapshot (project close).
```c
void quercus_render_manager_clear_graph_snapshot(void);
```
### `quercus_render_manager_init` — Initializes the process-wide render manager. The render/export tasks (`crate::task::export`) need a manager — or a worker pool — to render. False when already initialized.
```c
bool quercus_render_manager_init(int32_t backend);
```
### `quercus_render_manager_is_initialized` — Whether the render manager is initialized.
```c
bool quercus_render_manager_is_initialized(void);
```
### `quercus_render_manager_poll` — Pumps the manager's completion dispatcher; call once per UI tick.
```c
void quercus_render_manager_poll(void);
```
### `quercus_render_manager_resync_graph_snapshot` — Re-uploads the snapshot although the undo revision did not change.
```c
bool quercus_render_manager_resync_graph_snapshot(const void* project, uint64_t revision);
```
### `quercus_render_manager_set_graph_snapshot` — Uploads the project snapshot at `revision` to the worker pool.
```c
bool quercus_render_manager_set_graph_snapshot(const void* project, uint64_t revision);
```
### `quercus_render_manager_set_inline_project` — Points the inline backend at the live project (same shared project).
```c
bool quercus_render_manager_set_inline_project(const void* project);
```
### `quercus_render_manager_set_workspace_size` — Sizes the worker pool's per-frame workspace for the current sequence.
```c
void quercus_render_manager_set_workspace_size(int32_t width, int32_t height, uint32_t fps);
```
### `quercus_render_manager_shutdown` — Shuts the render manager down (drains pending work).
```c
void quercus_render_manager_shutdown(void);
```
### `quercus_render_swizzle_bgra8_to_rgba8` — Swizzles BGRA8 pixels to RGBA8 (B/G/R/A -> R/G/B/A; alpha passes through).
```c
bool quercus_render_swizzle_bgra8_to_rgba8(const uint8_t* src, uint8_t* dst, size_t pixel_count);
```
### `quercus_render_textengine_add_font_data` — Adds a font file's bytes to the shared text backend; the next `font_families` call includes its families. Empty data is rejected.
```c
bool quercus_render_textengine_add_font_data(const uint8_t* bytes, size_t len);
```
### `quercus_render_textengine_add_font_dir` — Adds every font file under `path` (recursively) to the shared text backend. False for a missing/non-directory path.
```c
bool quercus_render_textengine_add_font_dir(const uint8_t* path, size_t path_len);
```
### `quercus_render_textengine_font_family_at` — System font families from the same backend used by render workers.
```c
int32_t quercus_render_textengine_font_family_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_render_textengine_font_family_count` — System font families from the same backend used by render workers.
```c
int32_t quercus_render_textengine_font_family_count(void);
```
### `quercus_render_textengine_install` — Installs the text measure/render backends (idempotent; text nodes render empty without them).
```c
void quercus_render_textengine_install(void);
```
<a id="module-storage"></a>
## Module: `storage` (27 functions)

### `quercus_storage_backend_count` — The registered backends, in registration order (the arbitration order within a scheme).
```c
int32_t quercus_storage_backend_count(void);
```
### `quercus_storage_backend_for_uri` — The name of the backend claiming `uri` (None when none does).
```c
int32_t quercus_storage_backend_for_uri(const uint8_t* uri, size_t uri_len, uint8_t* buf, size_t buf_len);
```
### `quercus_storage_backend_name_at` — The registered backends, in registration order (the arbitration order within a scheme).
```c
int32_t quercus_storage_backend_name_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_storage_backend_scheme_at` — The registered backends, in registration order (the arbitration order within a scheme).
```c
int32_t quercus_storage_backend_scheme_at(int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_storage_default_library_uri` — The configured default library URI (`None` when storage is disabled).
```c
int32_t quercus_storage_default_library_uri(uint8_t* buf, size_t buf_len);
```
### `quercus_storage_flush` — Flushes every binding and stops the snapshot worker.
```c
void quercus_storage_flush(void);
```
### `quercus_storage_library_clip_count` — Lists the project rows of the library at `uri`.
```c
int32_t quercus_storage_library_clip_count(const uint8_t* uri, size_t uri_len, int32_t index);
```
### `quercus_storage_library_count` — Lists the project rows of the library at `uri`.
```c
int32_t quercus_storage_library_count(const uint8_t* uri, size_t uri_len);
```
### `quercus_storage_library_create` — Creates a new, empty project row named `name` in this library.
```c
int32_t quercus_storage_library_create(const uint8_t* uri, size_t uri_len, const uint8_t* name, size_t name_len, uint8_t* uuid_buf, size_t uuid_buf_len);
```
### `quercus_storage_library_created_at` — Lists the project rows of the library at `uri`.
```c
int64_t quercus_storage_library_created_at(const uint8_t* uri, size_t uri_len, int32_t index);
```
### `quercus_storage_library_delete` — Deletes the library row `uuid` from the library at `uri`.
```c
int32_t quercus_storage_library_delete(const uint8_t* uri, size_t uri_len, const uint8_t* uuid, size_t uuid_len);
```
### `quercus_storage_library_duplicate` — Copies the library row `uuid` into a new row with a fresh uuid.
```c
int32_t quercus_storage_library_duplicate(const uint8_t* uri, size_t uri_len, const uint8_t* uuid, size_t uuid_len, uint8_t* new_uuid_buf, size_t new_uuid_buf_len);
```
### `quercus_storage_library_duration_ms` — Lists the project rows of the library at `uri`.
```c
int64_t quercus_storage_library_duration_ms(const uint8_t* uri, size_t uri_len, int32_t index);
```
### `quercus_storage_library_export` — Exports the library row `uuid` to the project file at `file_uri`.
```c
int32_t quercus_storage_library_export(const uint8_t* uri, size_t uri_len, const uint8_t* uuid, size_t uuid_len, const uint8_t* file_uri, size_t file_uri_len);
```
### `quercus_storage_library_footage_count` — Lists the project rows of the library at `uri`.
```c
int32_t quercus_storage_library_footage_count(const uint8_t* uri, size_t uri_len, int32_t index);
```
### `quercus_storage_library_import` — Imports the project document at `file_uri` into this library as a new row.
```c
int32_t quercus_storage_library_import(const uint8_t* uri, size_t uri_len, const uint8_t* file_uri, size_t file_uri_len, uint8_t* new_uuid_buf, size_t new_uuid_buf_len);
```
### `quercus_storage_library_modified_at` — Lists the project rows of the library at `uri`.
```c
int64_t quercus_storage_library_modified_at(const uint8_t* uri, size_t uri_len, int32_t index);
```
### `quercus_storage_library_name_at` — Lists the project rows of the library at `uri`.
```c
int32_t quercus_storage_library_name_at(const uint8_t* uri, size_t uri_len, int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_storage_library_rename` — Renames the library row `uuid` at `uri` to `new_name`.
```c
int32_t quercus_storage_library_rename(const uint8_t* uri, size_t uri_len, const uint8_t* uuid, size_t uuid_len, const uint8_t* new_name, size_t new_name_len);
```
### `quercus_storage_library_supported` — Whether the backend claiming `uri` implements the project-library operations (`projects/create/delete/rename/duplicate/import/export`).
```c
bool quercus_storage_library_supported(const uint8_t* uri, size_t uri_len);
```
### `quercus_storage_library_track_count` — Lists the project rows of the library at `uri`.
```c
int32_t quercus_storage_library_track_count(const uint8_t* uri, size_t uri_len, int32_t index);
```
### `quercus_storage_library_uuid_at` — Lists the project rows of the library at `uri`.
```c
int32_t quercus_storage_library_uuid_at(const uint8_t* uri, size_t uri_len, int32_t index, uint8_t* buf, size_t buf_len);
```
### `quercus_storage_open` — Opens the project at `uri` through the claiming backend. Err on a load failure (the code is the storage error).
```c
void* quercus_storage_open(const uint8_t* uri, size_t uri_len, int32_t* version_info);
```
### `quercus_storage_register_transport` — Registers `backend` as a process-wide transport and returns a guard that unregisters it on drop.
```c
int32_t quercus_storage_register_transport(const QuercusStorageTransportVTable* vt, void* userdata);
```
### `quercus_storage_save` — Saves the project to `uri` through the claiming backend.
```c
int32_t quercus_storage_save(const void* project, const uint8_t* uri, size_t uri_len, uint32_t options);
```
### `quercus_storage_unregister_transport` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
bool quercus_storage_unregister_transport(int32_t slot);
```
### `quercus_storage_uri_is_valid` — Whether `uri` parses as a storage URI.
```c
bool quercus_storage_uri_is_valid(const uint8_t* uri, size_t uri_len);
```
<a id="module-task"></a>
## Module: `task` (22 functions)

### `quercus_task_audio_render` — Starts asynchronous audio mixing for a sequence or footage range.
```c
uint64_t quercus_task_audio_render(const void* project, uint64_t viewer, TimeRange range, int32_t sample_rate, uint64_t channel_layout);
```
### `quercus_task_cancel` — Requests cooperative cancellation without joining the worker.
```c
void quercus_task_cancel(uint64_t id);
```
### `quercus_task_error` — The error message ("" when none).
```c
int32_t quercus_task_error(uint64_t id, uint8_t* buf, size_t buf_len);
```
### `quercus_task_export` — Starts an export of the viewer node (a sequence or footage) to `filename` with `params`. None on invalid input.
```c
uint64_t quercus_task_export(const void* project, uint64_t viewer, const uint8_t* filename, size_t filename_len, const EncodingParams* params);
```
### `quercus_task_progress` — Returns the worker's most recent progress in `0.0..=1.0` (`0.0` = nothing done yet, `1.0` = complete).
```c
double quercus_task_progress(uint64_t id);
```
### `quercus_task_project_load_otio` — Starts loading an interchange document (`.otio` / `.fcpxml`) as a new project. None on invalid input. Take the project with `Task::take_project` once the task succeeds.
```c
uint64_t quercus_task_project_load_otio(const uint8_t* filename, size_t filename_len);
```
### `quercus_task_project_save_otio` — Starts a project save to an interchange format (`.otio` / `.fcpxml`, chosen by the filename extension). None on invalid input.
```c
uint64_t quercus_task_project_save_otio(const void* project, const uint8_t* filename, size_t filename_len);
```
### `quercus_task_proxy_max_concurrent` — Maximum number of proxy transcodes that should run concurrently.
```c
int32_t quercus_task_proxy_max_concurrent(void);
```
### `quercus_task_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_task_release(uint64_t id);
```
### `quercus_task_state` — Returns the task's current `TaskState` as published by the worker.
```c
int32_t quercus_task_state(uint64_t id);
```
### `quercus_task_take_audio` — Takes the owned result of a successful audio mix exactly once.
```c
const void* quercus_task_take_audio(uint64_t id);
```
### `quercus_task_take_project` — Takes the produced project of a succeeded load task (None when none / already taken).
```c
void* quercus_task_take_project(uint64_t id);
```
### `quercus_task_take_thumbnail` — Takes the owned result of a successful thumbnail render exactly once.
```c
const void* quercus_task_take_thumbnail(uint64_t id);
```
### `quercus_task_take_video` — Takes the owned result of a successful video render exactly once.
```c
const void* quercus_task_take_video(uint64_t id);
```
### `quercus_task_take_waveform` — Takes the owned result of a successful waveform task. Subsequent calls return `None`, and non-waveform tasks never produce a result here.
```c
const void* quercus_task_take_waveform(uint64_t id);
```
### `quercus_task_thumbnail_render` — Starts an asynchronous F32 RGBA thumbnail render of a sequence or video footage node.
```c
uint64_t quercus_task_thumbnail_render(const void* project, uint64_t viewer, Rational time, int32_t width, int32_t height);
```
### `quercus_task_title` — Returns the task's display title assigned when it was spawned (for example `Rendering Video ...`), suitable for a task/progress list.
```c
int32_t quercus_task_title(uint64_t id, uint8_t* buf, size_t buf_len);
```
### `quercus_task_video_render` — Starts an asynchronous sequence or footage video render into an owned frame.
```c
uint64_t quercus_task_video_render(const void* project, uint64_t viewer, Rational time, int32_t width, int32_t height, int32_t pixel_format);
```
### `quercus_task_video_render_playback` — Starts an asynchronous Playback-priority preview frame render.
```c
uint64_t quercus_task_video_render_playback(const void* project, uint64_t viewer, Rational time, int32_t width, int32_t height, int32_t pixel_format, int64_t frame, int64_t distance, uint64_t generation);
```
### `quercus_task_video_render_track` — Starts an asynchronous render of one multicam-angle video track.
```c
uint64_t quercus_task_video_render_track(const void* project, uint64_t viewer, uint64_t track, Rational time, int32_t width, int32_t height, int32_t pixel_format);
```
### `quercus_task_wait` — Blocks until the task finishes. True on success.
```c
bool quercus_task_wait(uint64_t id);
```
### `quercus_task_waveform_extract` — Starts asynchronous min/max peak extraction from one audio stream of `filename`.
```c
uint64_t quercus_task_waveform_extract(const uint8_t* filename, size_t filename_len, int32_t stream_index, int32_t samples_per_point);
```
<a id="module-thumbnail"></a>
## Module: `thumbnail` (4 functions)

### `quercus_thumbnail_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_thumbnail_add_ref(const void* thumbnail);
```
### `quercus_thumbnail_copy_pixels` — Copies the owned tight F32 RGBA pixels out of the engine handle.
```c
size_t quercus_thumbnail_copy_pixels(const void* thumbnail, float* dst, size_t capacity);
```
### `quercus_thumbnail_info` — Returns dimensions and the exact pixel-vector capacity.
```c
bool quercus_thumbnail_info(const void* thumbnail, ThumbnailInfo* out);
```
### `quercus_thumbnail_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_thumbnail_release(const void* thumbnail);
```
<a id="module-waveform"></a>
## Module: `waveform` (4 functions)

### `quercus_waveform_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_waveform_add_ref(const void* waveform);
```
### `quercus_waveform_info` — Returns stream format and per-channel point count.
```c
bool quercus_waveform_info(const void* waveform, WaveformInfo* out);
```
### `quercus_waveform_point_at` — Copies one point of one channel; `None` for an out-of-range point/channel.
```c
bool quercus_waveform_point_at(const void* waveform, size_t point, int32_t channel, MinMax* out);
```
### `quercus_waveform_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_waveform_release(const void* waveform);
```
<a id="module-worker"></a>
## Module: `worker` (2 functions)

### `quercus_worker_ofx_host` — Runs the single OpenFX host loop. `args` is the full argv (including the program name); the `--ofx-crash-*` flags are only used by crash tests.
```c
int32_t quercus_worker_ofx_host(const uint8_t* args, size_t args_len);
```
### `quercus_worker_run` — Runs the ticket worker loop with the given backend (`opengl`, `vulkan`, `cpu`, `none`, ...). Returns the process exit code.
```c
int32_t quercus_worker_run(const uint8_t* backend, size_t backend_len);
```
<a id="module-generator"></a>
## Module: `generator` (1 functions)

### `quercus_generator_create` — Instantiates the generator node type `type_id` in `project`.
```c
uint64_t quercus_generator_create(const void* project, const uint8_t* type_id, size_t type_id_len);
```
<a id="module-folder"></a>
## Module: `folder` (3 functions)

### `quercus_folder_child_at` — The `index`-th child of a folder (`NodeId::INVALID` when out of range).
```c
uint64_t quercus_folder_child_at(const void* project, uint64_t id, int32_t index);
```
### `quercus_folder_child_count` — Returns the number of direct children of the bin folder `folder`.
```c
int32_t quercus_folder_child_count(const void* project, uint64_t id);
```
### `quercus_folder_create` — Creates a new empty bin folder named `name` under the folder `parent` (pass `Project::root` to add at the top level). Undoable.
```c
uint64_t quercus_folder_create(const void* project, uint64_t parent, const uint8_t* name, size_t name_len);
```
<a id="module-video"></a>
## Module: `video` (5 functions)

### `quercus_video_buffer_add_ref` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_video_buffer_add_ref(const void* frame);
```
### `quercus_video_buffer_copy_bytes` — Copies the source bytes. F32 frames are native-endian RGBA; BGRA8 frames are packed blue/green/red/alpha bytes with `linesize` bytes per row.
```c
size_t quercus_video_buffer_copy_bytes(const void* frame, uint8_t* dst, size_t capacity);
```
### `quercus_video_buffer_info` — Returns geometry, origin and pixel layout.
```c
bool quercus_video_buffer_info(const void* frame, VideoFrameInfo* out);
```
### `quercus_video_buffer_release` — No Rust SDK doc comment is attached to this C ABI entry point.
```c
void quercus_video_buffer_release(const void* frame);
```
### `quercus_video_buffer_take_texture` — Clones the optional zero-copy presentation texture. The returned pointer is an `Arc<wgpu::Texture>` raw pointer; the caller owns it and must wrap it with `Arc::from_raw` (using the same wgpu 29 generation).
```c
const void* quercus_video_buffer_take_texture(void* frame, int32_t* width, int32_t* height);
```
## Documentation coverage

Summaries are harvested from contiguous `///` blocks in the Rust SDK by matching wrapper functions to the `quercus_*` symbols they call. Where no doc comment exists, a neutral fallback marks the entry undocumented; no behavior is inferred from the name. Harvested summaries quote the Rust wrapper, so they may use Rust vocabulary (`Option`, `None`, `Result`); the C prototype on each entry is authoritative for the ABI.

| Item | Documented | Total |
| --- | ---: | ---: |
| Functions (Rust SDK summary) | 677 | 744 |
| Functions (neutral fallback) | 67 | 744 |
| Types and callbacks (Rust SDK summary) | 65 | 85 |
