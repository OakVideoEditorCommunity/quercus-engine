# 引擎补全规格：音频后期 + 字幕

面向 `~/Projects/quercus-engine`（published: `quercus-task`/`quercus-node` 0.5.2、
`quercus-engine` 0.5.3）。只列能力和实现方案，不重复正在修的导出 bug。

---

# 一、音频后期

## 1. 现状（证据）

- 混音入口：`quercus-render/src/eval.rs:2614` `mix_audio_montage()`。
  它只做 `buf[i] * clip.gain`（`:2657`），**从不读取 `clip.effects`**。
- `MontageClip`（`quercus-render/src/ticket.rs:67`）字段：
  `filename / stream_index / in_time / out_time / media_in / gain / effects`；
  **没有 pan、fade、envelope**。
- 导出音频 montage：`quercus-task/src/render.rs:437` `audio_montage()`，
  固定 `gain: 1.0`、`effects: Vec::new()`，注释明确写
  “Audio clips carry no effect stack”（`:585-588`）。
- 预览音频 montage：`quercus-engine/src/preview_audio.rs:168`，同样
  `gain: 1.0`、`effects: Vec::new()`（`:226-229`、`:271-274`）。
- 音频效果节点：`quercus-node/src/nodes/volume.rs:27,31`（`samples_in` + `volume_in`）、
  `pan.rs:27,31`（`samples_in` + `panning_in`）。节点本身能处理 `SampleBuffer`，
  但**没有任何地方把它们接到片段/轨道上**：Clip block 只有 `tex_in`
  （`quercus-node/src/block.rs:169`），且混音器绕过节点图直接解码。
- 导出/预览的音频布局来自序列 `audio_params`（`quercus-task/src/nodeops.rs:393`）。
- Wire 协议：`quercus-render/src/ipc.rs:381` `WireMontageEffect`、
  `:398` `WireMontageClip`，全部 `#[serde(default)]`，**加字段是向后兼容的**。

## 2. 缺口

| # | 缺口 | 影响 |
|---|---|---|
| A1 | Volume/Pan 节点未接入混音 | 音量/声像调了等于没调 |
| A2 | `MontageClip` 没有 pan/fade/envelope | 无法做淡入淡出、ducking、自动化 |
| A3 | 没有“片段音频效果链”的数据模型/解析 | 无法按片段挂 EQ/压缩等 |
| A4 | 混音器不评估关键帧 | 音量自动化在导出中失效 |
| A5 | 没有 EQ/压缩/限制/门限/ducking 节点 | 音乐/旁白无法做后期 |
| A6 | 没有 solo，mute 在导出侧也未处理（bug 清单） | 多轨混音不可控 |
| A7 | 没有响度计量/归一化 | 交付音量不可控 |
| A8 | 没有总线/子混音/路由 | 复杂工程无法组织 |
| A9 | 音频效果不进入 Wire/worker | 后台渲染与主进程不一致 |

## 3. 推荐实现（分阶段）

### Phase 1（P0）：把 Volume/Pan/淡入淡出/自动化接进混音

**3.1 数据模型（render/ticket）**

```rust
pub struct MontageAudioEffect {
    pub type_id: String,          // "org.olivevideoeditor.Olive.volume" / ".pan" / ".eq" ...
    pub enabled: bool,
    pub params: Vec<(String, NodeValue)>,          // 静态值
    pub keyframes: Vec<(String, Vec<(Rational, NodeValue)>)>, // 可选：参数名 -> 关键帧
}

pub struct MontageClip {
    // ... 现有字段 ...
    pub gain: f32,                 // 保留，兼容
    pub pan: f32,                  // -1..1，新增
    pub fade_in: Rational,         // 新增
    pub fade_out: Rational,        // 新增
    pub audio_effects: Vec<MontageAudioEffect>, // 新增
}
```

Wire 对应 `WireMontageClip`/`WireMontageEffect` 加同名字段（`#[serde(default)]`），
worker 反序列化保持旧包兼容。

**3.2 片段音频效果链的解析**

两种路线，建议先 A 后 B：

- **A. 混音器内联效果（短期，改动小）**
  - 在 Clip block 增加 `samples_in`（`ValueType::Samples`），把 footage 的
    音频源接到它；音频效果节点（Volume/Pan/EQ…）按 `samples_in` 顺序串联。
  - `audio_montage()` 遍历片段时解析这条链，产出 `MontageAudioEffect`。
  - 混音器直接应用内建 kernel（volume/pan/EQ/compressor），不需要节点图求值。
- **B. 节点图求值（长期，支持 OFX 音频）**
  - 渲染侧构造“音频子图”，用 `NodeBehavior::value()` 对 `SampleBuffer` 求值，
    支持任意音频节点/OFX；成本高，Phase 2 再做。

**3.3 混音器**

`mix_audio_montage()`：
1. 解码 → 按 `pan` 做等功率声像（-3 dB 中心）；
2. 应用 `gain` + `fade_in/out` + 关键帧包络（按 chunk 或样本插值）；
3. 按顺序应用 `audio_effects`（Volume→Pan→EQ→Compressor…）；
4. 未知 type_id 透传并记录 warning（不要静默失败）。

**3.4 命令 / SDK**

`EditCommand` 新增（并进 ABI/dispatch）：

```text
AddAudioEffect { clip, type_id, index }
RemoveAudioEffect { clip, effect }
SetAudioEffectEnabled { effect, enabled }
SetAudioEffectParam { effect, input_id, element, value }   // 复用 NodeValue
SetClipGain { clip, gain }
SetClipPan { clip, pan }
SetClipFade { clip, fade_in, fade_out }
SetAudioEnvelope { clip, input_id, keyframes }             // 可选
```

**3.5 一致性与验收**

- `preview_audio`、`render_audio`、导出必须走同一套 montage/混音。
- 验收：
  1. Volume 0.5 → 导出峰值/RMS 降 6 dB；
  2. Pan -1/+1 → 左/右声道隔离；
  3. fade_in 1s → 起始 1s 线性/等功率上升；
  4. 音量关键帧 0→1 → 导出包络正确（同时验证关键帧导出修复）；
  5. Volume→Pan 顺序生效；
  6. mute/solo 生效；
  7. 48k/44.1k、mono/stereo 转换；
  8. 音频单独导出 WAV 成功（配合 audio-only 修复）。

### Phase 2（P1）：音频处理节点

- EQ（3 段或参数均衡）、Compressor、Limiter、Noise Gate；
- Ducking（sidechain：音乐轨引用对白轨包络）；
- 响度计量（peak/RMS/LUFS）与归一化；
- 自动化曲线（线性/保持/贝塞尔），和视频同一套关键帧语义；
- 轨道/总线级效果与 solo/mute 规则。

### Phase 3（P2）

- 子混音/总线路由、环绕声、音频 OFX。

## 4. 代码触点

- `quercus-render/src/ticket.rs`：`MontageClip`、新 `MontageAudioEffect`。
- `quercus-render/src/eval.rs`：`mix_audio_montage()` 应用 pan/fade/effects。
- `quercus-render/src/ipc.rs`：`WireMontageClip`/`WireMontageEffect` 加字段。
- `quercus-task/src/render.rs`：`audio_montage()` 解析片段链；`export.rs` 音频编码。
- `quercus-engine/src/preview_audio.rs`：同一解析逻辑。
- `quercus-node/src/block.rs`：Clip 增加 `samples_in`（路线 A）。
- `quercus-node/src/nodes/volume.rs`、`pan.rs`、新增 eq/compressor/limiter。
- `quercus-engine/src/editor_commands/`、`editor_dispatch/{command,abi,execute}.rs`。
- `quercus-engine-rs/src/edit.rs`、`node.rs`（SDK 暴露）。
- `.ove` 序列化：片段音频链、gain/pan/fade 持久化。

---

# 二、字幕

## 1. 现状（证据）

- `SubtitleParams`（`quercus-engine-rs/src/types.rs:399`）只有
  `stream_index/enabled/entry_count`，是**流元数据**，不是字幕内容。
- 序列有字幕轨（`sequence.rs:88` `subtitle_param_in`；`TrackType::Subtitle`），
  但**没有字幕块/行为/渲染**：`quercus-node/src/nodes/` 下没有任何 subtitle 节点。
- 文字/生成器只能放视频轨：`editor_commands/generators.rs:146-190`
  `place_text_clip`/`place_generator_clip` 固定 `TrackType::Video`。
- Codec 层有 SRT：`quercus-codec/src/exportformat.rs:205`（SRT 只支持 SRT 字幕），
  `encoder.rs:80` `write_subtitle(text, in_seconds, out_seconds)`。
- 导出侧有字幕钩子但**没有调用者**：
  `quercus-task/src/render.rs:141` `fn encode_subtitle(&mut self, task, text)`;
  `export.rs:479` 实现里写死 `write_subtitle(text, 0.0, 0.0)`；全仓库没有
  任何地方调用 `encode_subtitle`。`subtitles_enabled` 只是透传。
- OTIO/FCPXML：`quercus-otio/src/fcpxml.rs:1547` 只有一个创建字幕轨的测试，
  没有真正的字幕导入/导出语义。

## 2. 缺口

| # | 缺口 | 影响 |
|---|---|---|
| S1 | 没有字幕块数据模型（文本/时间/样式） | 无法在时间线放字幕 |
| S2 | 没有创建/编辑/导入/导出命令 | 只能手工做 Text 片段 |
| S3 | 没有烧录渲染（字幕合成到画面） | 参考片那种字幕做不了 |
| S4 | SRT 只有编码器，没有解析器 | 无法导入 SRT |
| S5 | 没有样式（字体/大小/颜色/描边/位置/对齐） | 字幕不可控 |
| S6 | 预览不显示字幕 | 无法检查 |
| S7 | 字幕不参与 `.ove` 序列化 | 保存丢失 |
| S8 | 没有 OTIO/FCPXML 字幕映射 | 交换格式丢字幕 |
| S9 | 导出没有“烧录 / 独立字幕流 / sidecar”模式 | 交付方式单一 |
| S10 | 没有换行/安全区/重叠策略 | 多行/拥挤场景不可控 |

## 3. 推荐实现

### Phase 1（P0）：字幕块 + SRT + 烧录

**3.1 数据模型**

```rust
pub struct SubtitleStyle {
    pub font_family: String,
    pub font_size: f32,
    pub color: [f64; 4],
    pub outline_color: [f64; 4],
    pub outline_width: f32,
    pub align: i32,        // left/center/right
    pub margin_h: f32,
    pub margin_v: f32,
    pub line_spacing: f32,
}

pub struct SubtitleBlockBehavior {
    // core: in_/out()/enabled（复用 BlockCore）
    pub text: String,
    pub style: SubtitleStyle,
}
```

- 放到 `TrackType::Subtitle` 的 track list；用现有 `TrackPlaceBlockCommand` 放置。
- 序列化进 `.ove`（`serializer.rs` + `sequence.rs` 的 custom 段）。
- undo：`AddSubtitle`/`SetSubtitle*` 都是 undoable command。

**3.2 命令 / SDK**

```text
AddSubtitle { track_index, in_ts, out_ts, text, style? }
SetSubtitleText { subtitle, text }
SetSubtitleStyle { subtitle, style }
SetSubtitleRange { subtitle, in_ts, out_ts }
RemoveSubtitle { subtitle }
ImportSubtitles { path }        // SRT -> 多个 block（一条 undo 组）
ExportSubtitles { path }        // block -> SRT
```

`ImportSubtitles` 建议一个 undo group（`undo_group_begin/end`），避免几百条字幕
产生几百个撤销点。

**3.3 SRT**

- 在 `quercus-codec` 增加 SRT parser（`HH:MM:SS,mmm`、多行、UTF-8/UTF-16 BOM），
  输出 `(in, out, text)`；serializer 用现有 `write_subtitle`。
- 导入时按帧率把秒换成 `in_ts/out_ts`；导出时反向。
- 重叠/空档策略：按 cue 直接落块；重叠允许但渲染时按轨道顺序叠。

**3.4 渲染（烧录）**

- `VideoTicketParams` 增加 `subtitle_spans: Vec<SubtitleSpan>`：
  `{ in_time, out_time, text, style }`（wire 同步，`#[serde(default)]`）。
- 渲染器在合成完视频层后，用现有 Text 渲染路径把字幕画到画面
  （复用 `textengine` 字体/描边/发光）。
- `preview_frame` 同样应用字幕，便于模型检查。
- 开关：导出参数 `subtitles_enabled` + 新模式：
  - `burn_in`：渲染进帧；
  - `sidecar`：写 `.srt`；
  - `container`：写 MKV/MP4 字幕流（`write_subtitle`）。

**3.5 导出钩子**

- 把 `RenderTaskBehavior::encode_subtitle(&mut self, task, text)` 改成
  `encode_subtitle(&mut self, task, text, in_time, out_time)`；
- 在渲染循环里按字幕 span 调用；sidecar 模式写外部文件，container 模式写编码器。
- 删除现在写死 `0.0/0.0` 的占位实现。

### Phase 2（P1）：样式与交换

- 背景框/阴影、安全区、自动换行、字体回退；
- 双语字幕、ASS 导入（可选）；
- OTIO/FCPXML 字幕轨映射（导入/导出）；
- MKV 字幕流与 MP4 `mov_text`（若 FFmpeg 支持）；
- 字幕轨的 solo/mute/可见性。

### Phase 3（P2）

- 卡拉 OK/逐字高亮、字幕模板、AI 字幕时间轴对齐。

## 4. 代码触点

- `quercus-node/src/nodes/subtitle.rs`（新）、`block.rs`、`factory.rs`、`serializer.rs`。
- `quercus-node/src/sequence.rs`：字幕轨 list 已有，补字幕块计数/序列化。
- `quercus-engine/src/editor_commands/`（新 `subtitle.rs`）、`editor_dispatch/{command,abi,execute}.rs`。
- `quercus-engine/src/preview_video/montage.rs`：收集 `SubtitleSpan`。
- `quercus-render/src/ticket.rs`（`SubtitleSpan`）、`eval.rs`（合成）、`ipc.rs`（wire）。
- `quercus-task/src/render.rs`（`encode_subtitle` 时序）、`export.rs`、`precache.rs`。
- `quercus-codec`：SRT parser/serializer；`encoder.rs` 已有 `write_subtitle`。
- `quercus-otio/src`：字幕轨映射。
- `quercus-engine-rs/src/edit.rs`、`types.rs`：SDK 类型/命令。

---

# 三、跨切面注意

- **ABI/版本**：新命令进 `QuercusEditOp`、C header、dispatch；保持旧 opcode 不变。
- **Wire 兼容**：`#[serde(default)]` 加字段；worker/protocol 版本号按现有机制递增。
- **撤销**：批量导入字幕用一个 undo group；音频效果链插入/删除可 undo。
- **序列化**：`.ove` 新节点/字段；load 旧工程要能缺省。
- **一致性**：预览、`render_audio`、导出必须共用同一 montage 构建逻辑。
- **性能**：音频效果按 chunk（如 256/1024 帧）处理，避免逐样本虚调用；
  字幕只在可见帧合成。
- **字体**：字幕渲染复用 `textengine_add_font_dir` 注入的字体（当前系统 341 个）。

# 四、验收清单

## 音频

1. Volume 0.5 → 导出 -6 dB；
2. Pan 左右隔离；
3. fade_in/out 包络正确；
4. 音量关键帧 0→1 在导出中生效；
5. 效果顺序 Volume→Pan；
6. mute/solo 生效；
7. 预览/`render_audio`/导出一致；
8. 44.1k↔48k、mono↔stereo；
9. 音频单独导出 WAV/M4A；
10. 未知音频效果不崩溃、有 warning。

## 字幕

1. 导入 SRT（中文、多行）→ 块时间正确；
2. 导出 SRT → 时间/文本 round-trip；
3. 烧录：cue 帧有字、间隙帧无字；
4. 预览显示字幕；
5. 保存/重开保留字幕；
6. 字体/字号/颜色/描边/位置生效；
7. 换行/安全区；
8. MKV 字幕流 + sidecar SRT；
9. 重叠 cue 策略；
10. OTIO/FCPXML round-trip。

# 五、优先级

- **P0**：音频（Volume/Pan 接入、gain/pan/fade/自动化、mute/solo）+
  字幕（字幕块、SRT 导入导出、烧录/预览）。
- **P1**：EQ/压缩/限制/ducking/响度；字幕样式、sidecar/容器、OTIO。
- **P2**：总线/路由、环绕、卡拉 OK、AI 时间轴。
