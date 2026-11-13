# 媒体管线可替换性审计（codec / GPU render / audio / OFX / fonts）

- 仓库：`/home/sunyu/Projects/quercus-engine`，workspace 0.5.2（`Cargo.toml:7`）；`quercus-engine` 0.5.3、`quercus-engine-rs` 0.5.7。
- 范围：解码/编码、GPU 渲染后端、渲染作业后端、音频输出、OFX 宿主、文字/字体，以及上述阶段的 C ABI 出口。
- 方法：只读静态阅读 + 既有测试/注释交叉验证；**未**执行 `cargo build/test`（FFmpeg 工具链不在本机，见 `README.md:50-75`）。
- 标注约定：`【推断】` = 由代码结构推出、未见运行验证的结论；其余为直接代码证据。
- ABI 边界事实（后文反复用到）：
  - 安全 SDK `quercus-engine-rs` **只**通过 `extern "C"` 符号使用引擎（`crates/quercus-engine-rs/Cargo.toml:31-36`，`crates/quercus-engine-rs/src/*.rs` 全是 `unsafe extern "C"` 声明块）。
  - `quercus-engine` 只产出 `cdylib`（`crates/quercus-engine/Cargo.toml:22`），**没有 rlib**——任何 Rust 调用方也无法把引擎当 Rust 库链接。
  - 参考消费者 Oak Video Editor 位于独立仓库、经 SDK 消费（`README.md:9-10`, `README.md:16-18`）。
  - 因此：**凡是没出现在 `crates/quercus-engine/src/**` 里的 `#[unsafe(no_mangle)]` 函数，消费者就换不掉。**

---

## 0. 结论速览（5 个必答阶段）

| 阶段 | 今天可替换？ | 最小改动 |
|---|---|---|
| 解码器（decoder） | **否**。只能按 id 在 2 个内置项里挑（`decoder.rs:355-363`），且注册表在渲染主路径被绕过（`eval.rs:1293`）；第二个内置项 OIIO 是 stub（`oiio.rs:73-80`） | 修 `eval.rs:1293` 走 `receive_list_of_all_decoders()`（零 ABI 变更）；再补 `quercus_codec_decoder_register(vtable)`，签名照抄 `quercus_codec_task_set_submit_cb`（`crates/quercus-engine/src/codec/task.rs:61-64`） |
| GPU 渲染后端 | **否**。只能在 `BackendKind` 枚举内选（`backend.rs:61-99`，config/env 选择 `backend.rs:104-112`），接口 `GpuContextLike`（`backend.rs:262`）与设备接管 `install_shared`/`adopt`（`backend.rs:1865`, `backend.rs:554`）都**未导出**；工作区内 `install_shared` 的唯一调用者是集成测试（`crates/quercus-render/tests/footage_import_test.rs:123,239,311`） | 导出 `quercus_render_gpu_install(void* device, void* queue, int32 backend)`（wgpu 句柄不透明传递）+ `quercus_render_gpu_kind()`；代价是 ABI 与 wgpu major（29）绑定 |
| 音频输出 | **部分可以（绕过）**：设备内建 cpal、无 trait（`manager.rs:44-45`, `outputdevice.rs:46`），但调用方可完全不 `push_to_output`，改用 `quercus_task_audio_render`（`include/quercus_engine.h:1268`）拿混音 buffer 自己送出 | 若要"同一路径换 sink"：加 `quercus_audio_manager_set_output_enabled(bool)`（关掉内部 cpal 流，标量成本）或 `quercus_audio_set_output_sink(vtable)` |
| OFX 宿主 | **否**（`Host::global()` 单例 `host.rs:1224-1240`，身份/能力硬编码 `host.rs:1438-1533`）。但**不建议**把宿主做成可替换 | 只暴露配置：`quercus_ofx_set_host_identity(name, version)` + `quercus_ofx_set_gl_advertised(bool)`（修正 `host.rs:1529-1531` 无条件宣告 GL 的问题） |
| 字体/文字 | **否**。Rust 内部已有函数指针钩子 `set_text_backends`（`textbackend.rs:145-158`），但 ABI 只有硬装 cosmic-text 的 `quercus_render_textengine_install`（`crates/quercus-engine/src/render.rs:152-155`）；字体目录不可加、family 列表是冻结快照（`textengine.rs:253-256`） | 导出 `quercus_render_textengine_add_font_dir(ptr,len)` / `add_font_data(bytes)`（在首次 `install` 前写入 cosmic-text `FontSystem`），并把 `font_families` 缓存改为可失效 |

---

## 1. 阶段现状表

现状取值：**硬编码**（具体类型/静态库）/ **内置可选**（enum 或配置项）/ **可注入**（trait / 函数指针）。

| 阶段 | 现状 | 证据（file:line） | 替换诉求强度 | 建议 |
|---|---|---|---|---|
| 解封装 + 解码（FFmpeg） | 硬编码：静态链接 FFmpeg，`ffmpeg::format::input_with_dictionary(&stream.filename(), …)` 按**文件名**打开，无自定义 AVIO | `crates/quercus-codec/Cargo.toml:29`（`features=["static"]`）、`crates/quercus-codec/src/ffmpeg.rs:784`、`ffmpeg.rs:104-113`（进程一次 `OnceLock` 初始化） | 中（厂商 raw 格式 SDK、远程/缓存媒体、合成源） | P1：见 §5 `MediaIo` + decoder factory |
| 解码器选择 | 内置可选：按 id 字符串选，仅 `"oiio"`/`"ffmpeg"` 两项 | `decoder.rs:355-363`（`create_from_id`）、`decoder.rs:414-425`（注册表 = 硬编码 vec）、ABI `crates/quercus-engine/src/codec/decoder.rs:724-748`、SDK `crates/quercus-engine-rs/src/codec.rs:1242` | 中 | 同上 |
| 第二个内置解码器（OIIO） | 硬编码 stub：`probe` 永远 `None`、`open` 永远失败 | `crates/quercus-codec/src/oiio.rs:25-29`（文档）、`oiio.rs:73-80`（probe→None）、`oiio.rs:82-88`（open→Err） | — | 现实：**今天只有 1 个可用解码器**，"内置可选"接近名义 |
| 图渲染取帧解码（eval 主路径） | 硬编码具体类型，**完全绕过注册表** | `crates/quercus-render/src/eval.rs:45`（`use …ffmpeg::FFmpegDecoder`）、`eval.rs:1293`（`Arc::new(FFmpegDecoder::new())`） | 中 | P1：改用 `receive_list_of_all_decoders()`（无 ABI 变更，纯内部修正） |
| 编码 / 导出 | 内置工厂：按 `EncodingParams.format` 映射到 FFmpeg/OIIO 两个实现，测试可注入 | `encoder.rs:34`（trait）、`encoder.rs:166-183`（`create_from_params`）、`encoder.rs:110-118`（`TEST_ENCODERS`/`set_test_encoders`）、消费方 `crates/quercus-task/src/export.rs:355` | 弱–中（外部硬件编码器、农场专用封装属长尾） | P2：编码器 vtable 与解码器合并设计，不与 storage 抢优先级 |
| 波形提取 | 探测走注册表、解码硬编码 FFmpeg | `crates/quercus-audio/src/waveform.rs:699-711`（注册表探测）、`waveform.rs:764`（`FFmpegDecoder::new()`） | 弱 | 随 P1 一并修正 |
| GPU 渲染后端（wgpu） | 内置可选：`BackendKind` {Auto,Metal,Vulkan,Gl,Cpu}，config `GraphicsBackend` + env；**设备句柄**可被接管但仅 Rust 内部 API | `crates/quercus-core/src/backend.rs:61-99`、`backend.rs:104-112`、`backend.rs:118-140`、`backend.rs:456-552`（建 device）、`backend.rs:1845-1885`、`backend.rs:1901-1909` | 强（零拷贝 present、农场指定设备、自带合成器） | P0/P1：见 §5 `GpuDeviceProvider` |
| 纹理/上下文抽象 | 已有 trait `GpuContextLike`（`Texture::Gpu` 里就是 `Arc<dyn …>`）——但仅供内部/测试 | `crates/quercus-core/src/backend.rs:256-290`、`crates/quercus-core/src/texture.rs:181`、`texture.rs:350-365` | 强 | 导出时以不透明句柄 + 少量必选操作面暴露，避免把整个 wgpu 拉进 ABI |
| 硬件解码 / 零拷贝导入 | 内置开关（config + env），候选设备顺序硬编码 | `crates/quercus-codec/src/hwdecode.rs:59`、`hwdecode.rs:131-140`、`hwdecode.rs:143-…`（候选表）、`crates/quercus-codec/src/gpuinterop.rs:77-105` | 弱（内部优化，非消费者诉求） | 保持 |
| 渲染作业后端（进程池/线程/内联） | 内置可选：ABI 传 int（0..3），详细参数不可传 | `crates/quercus-engine/src/render.rs:30-53`、`crates/quercus-render/src/manager.rs:45-70`、`crates/quercus-render/src/procpool.rs:824-846`（`DispatcherConfig`） | 中（农场要控 worker 数、槽位、worker 路径） | P1：导出 dispatcher 配置结构体（POD），`workers=0` 保持自动策略 |
| `JobDispatch` 注入 | 不可注入：`Arc<dyn JobDispatch>` 只在 Rust 内部构造 | `crates/quercus-render/src/worker.rs:127`、`manager.rs:186-215` | 弱–中 | P2（配置足够，别做 trait） |
| 磁盘缓存 | 路径可配置（config `DiskCachePath` / `QUERCUS_CONFIG_DIR`）；**无容量上限、无淘汰**；ABI 只能查路径/大小/清空 | `crates/quercus-core/src/filefunctions.rs:437-451`、`crates/quercus-render/src/manager.rs:494-520`、`crates/quercus-render/src/cache.rs`（无 size cap/evict；`grep` 无命中）、`crates/quercus-engine/include/quercus_engine.h:1217-1219` | 强（农场/长会话/多项目并发） | P0/P1：加 `DiskCacheMaxBytes` 配额与 LRU 淘汰，并把配额/目录做成 ABI 可写 |
| 音频输出设备 | 硬编码具体类型 `PortAudioOutput`（cpal 0.18），无 trait；设备枚举/选择在 ABI 内 | `crates/quercus-audio/Cargo.toml:24`、`crates/quercus-audio/src/manager.rs:44-45`（`OnceLock`）、`manager.rs:166-190`（首次 push 时开流）、`crates/quercus-audio/src/outputdevice.rs:46`、ABI `crates/quercus-engine/src/audio.rs:177-204` | 中（NDI/SDI/ASIO/远程音频） | P1：`set_output_enabled(false)`（最小）或 sink vtable |
| 音频混音/处理 | 引擎内实现，结果作为 buffer 交给调用方（已是"数据出口"） | `include/quercus_engine.h:1268`（`quercus_task_audio_render`）、`include/quercus_engine.h:668-671`（buffer 复制/信息） | 弱 | 不加 seam |
| 文字排版 / 字体 | **可注入（Rust 内）**：`set_text_backends(measure, render)` 函数指针；ABI 只有 `textengine_install`（硬装 cosmic-text），字体库 = `FontSystem::new()` 系统扫描 | `crates/quercus-node/src/nodes/textbackend.rs:132-171`、`crates/quercus-render/src/textengine.rs:120-131`、`textengine.rs:139-148`（`Once`）、`textengine.rs:253-256`（family 快照）、`crates/quercus-engine/src/render.rs:152-184` | 中（品牌字体/标题/多语言） | P1：字体注入 + 缓存失效（见 §0） |
| OFX 宿主 | 硬编码单例；宿主属性（身份=“Quercus Video Editor”、API 1.4、像素格式、上下文、**GL 能力=true**）在首次 `global()` 固定 | `crates/quercus-ofx-plugin/src/host.rs:1224-1240`、`host.rs:1438-1533`（含 `host.rs:1529-1531` 无条件宣告 GL）、`host.rs:265`（泄漏式 `OfxHost` 指针） | 弱（宿主不该替换） | P2：只开放身份/能力配置 |
| OFX 插件发现/加载 | 内置可选：标准路径 + 3 个 env + ABI `scan_path`；`dlopen` 硬编码 | `host.rs:785-830`（`default_plugin_paths`）、`host.rs:836-875`（env + `scan`）、`host.rs:63-107`（dlopen FFI）、ABI `crates/quercus-engine/src/ofx/host.rs:30-49`、`include/quercus_engine.h:1119-1120` | 中（农场白名单/沙箱/禁插件） | P2：加"禁用/白名单 + 递归深度"，不必换 loader |
| OFX 进度/取消 | **可注入**（C 回调 + userdata，已有先例） | `crates/quercus-ofx-plugin/src/progress.rs:39-72`、`crates/quercus-engine/src/ofx/host.rs:420-465` | 弱（已满足） | 保持 |
| OFX GL 桥 | 硬编码单例；仅 macOS 有实现，Linux/Windows 为 stub | `crates/quercus-ofx-plugin/src/gl_bridge.rs:421`、`gl_bridge.rs:815-828`（非 macOS stub）、`render_driver.rs:162-181`（`use_opengl` 决策） | 弱–中（农场无 GL 时要能彻底关掉） | P2：与 §0 的 host GL 宣告开关合并 |
| 色彩管理（OCIO） | 硬编码：`ocio-rs` bundled，config 路径走 `OCIO` env | `crates/quercus-core/Cargo.toml:13`、`crates/quercus-render/Cargo.toml:31`、`crates/quercus-core/src/color.rs:500`, `color.rs:598`、ABI `crates/quercus-engine/src/colorproc.rs:28-140` | 弱（**明确不推荐暴露**） | 不做 seam；只保留 env/config 路径 |
| 代理转码 | 外置 `ffmpeg` 可执行文件（config `FFmpegPath`）+ 参数 config | `crates/quercus-task/src/proxy.rs:452-457`、`proxy.rs:393-399`、`crates/quercus-codec/src/proxymanager.rs:132-141` | 弱（已足够） | 不加 seam |

---

## 2. 环境变量 / 配置键清单（现有"软旋钮"）

配置键全部可经 ABI 写：`quercus_core_config_set`（`include/quercus_engine.h:867-870`）。

### 2.1 环境变量

| 变量 | 位置 | 作用 / 覆盖不到什么 |
|---|---|---|
| `QUERCUS_RENDER_BACKEND` | `backend.rs:105` | 覆盖 `GraphicsBackend`，选 Metal/Vulkan/GL/CPU；**只选枚举项，不能给 device** |
| `QUERCUS_REQUIRE_GPU` | `backend.rs:407` | 测试/CI 策略：无 GPU 时 panic 而不是跳过；不影响生产路径 |
| `QUERCUS_PIPELINE=threads` | `manager.rs:65` | 作业后端 = 线程管线；其余值 = 进程池 |
| `QUERCUS_WORKER_BIN` | `procpool.rs:2410` | worker 可执行文件路径；**不能设 worker 数** |
| `QUERCUS_HWACCEL=0` | `hwdecode.rs:132` | 强制软解 |
| `QUERCUS_GPU_IMPORT=0` | `gpuinterop.rs:91` | 关零拷贝硬件帧导入 |
| `QUERCUS_DEBUG_AUDIO` | `outputdevice.rs:42` | 音频流诊断日志；不改变设备行为 |
| `QUERCUS_CONFIG_DIR` | `filefunctions.rs:110` | 配置/缓存根目录重定向（间接决定磁盘缓存位置） |
| `OCIO` | `color.rs:500`, `color.rs:598` | OCIO 配置路径 |
| `QUERCUS_DISPLAY_ICC`, `QUERCUS_WINDOWS_COLOR` | `displayicc.rs:60`, `displayicc.rs:596` | 显示 ICC / Windows 色彩管理 |
| `OFX_PLUGIN_PATH`, `OLIVE_OFX_PLUGIN_PATH`, `OLIVE_PLUGIN_PATH` | `host.rs:836-850` | 追加插件搜索路径（另有 ABI `scan_path`） |
| `QUERCUS_OFX_TRACE` | `host.rs:239` | fetchSuite 命中/未命中诊断 |
| `QUERCUS_DEBUG_DISPATCH` | `procpool.rs:1683` | 派发诊断 |
| `QUERCUS_PERF` | `eval.rs:1273` | 解码/打开耗时日志 |
| `QUERCUS_GPU_TESTS` / `QUERCUS_WORKER_CRASH_ON_TICKET` / `QUERCUS_KEEP_TESTMEDIA` | `gl_bridge.rs:102`, `procpool.rs:4461`, `realmedia_tests.rs:479` | 仅测试 |

### 2.2 配置键

| 键 | 位置 | 作用 / 缺口 |
|---|---|---|
| `GraphicsBackend` | `backend.rs:104-112` | GPU 后端偏好（枚举） |
| `DisplayBitDepth` | `backend.rs:157` | 上屏位深（需重启） |
| `HardwareDecoding` | `hwdecode.rs:59`, `hwdecode.rs:134-140` | 硬件解码开关（字符串 `"false"` 才关） |
| `GpuDecodeImport`, `GpuDecodeImportVaapi/D3d11/Videotoolbox` | `gpuinterop.rs:77-85` | 零拷贝导入开关（总 + 分平台） |
| `ProxyMaxConcurrent` | `proxy.rs:393-399` | 代理并发 [1,16]；ABI 只读入口 `include/quercus_engine.h`→`crates/quercus-engine/src/task.rs:518` |
| `ProxyWidth/Height/Divider/CRF/IncludeAudio/Preset` | `proxymanager.rs:132-141` | 代理编码参数 |
| `FFmpegPath` | `proxy.rs:452-457` | 代理转码用外部 ffmpeg |
| `DiskCachePath` | `filefunctions.rs:439-446` | 缓存目录覆盖（**只能目录，不能配额**） |
| `DiskCacheBehind`/`DiskCacheAhead`/`DiskCacheSaveInterval` | 默认值在 `configstore.rs:588-591` | **工作区内无消费者**（`grep` 无命中）——预读窗口配置是死配置【推断：M 系列重构后遗留】 |
| `AudioOutputBufferSize` | `crates/quercus-audio/src/config.rs:28` | cpal 缓冲帧数 |
| `AudioOutput`/`AudioInput` | `config.rs:38-50` | 设备名（按名解析到索引） |

**这些旋钮覆盖不了什么**（直接决定 §5 的取舍）：没有解码器默认选择、没有字体目录、没有 worker 数/槽位、没有缓存容量配额、没有宿主身份、没有音频 sink、没有 GPU 设备句柄。

---

## 3. 全局状态阻塞项（换实现时真正的绊脚石）

| # | 单例 / 全局 | 证据 | 为什么阻塞替换 |
|---|---|---|---|
| 1 | FFmpeg 库初始化 | `ffmpeg.rs:104-113` | `OnceLock<Result<…>>`，进程一次且不可重入；静态链接（`quercus-codec/Cargo.toml:29`）意味着"换 codec 实现"= 换链接，而非运行期 |
| 2 | 解码器注册表 | `decoder.rs:367`, `decoder.rs:402-405`, `decoder.rs:414-425` | 进程级 `Mutex<Vec<Arc<dyn Decoder>>>`；文档自述"C ABI has no way to register a decoder"（`decoder.rs:397-401`）；且 `receive_list_of_all_decoders()` 每次新建实例，无法持有调用方状态 |
| 3 | 编码器注册表 | `encoder.rs:110-118`, `encoder.rs:166-172` | 同上（测试注入优先于内置映射） |
| 4 | `FrameManager` | `crates/quercus-codec/src/framemanager.rs:47-52`, `framemanager.rs:120-131` | 进程级帧池 + GC 线程一次启动；无法按会话隔离内存预算【推断：多项目/农场并发时共享池会互相影响】 |
| 5 | 音频管理器 | `manager.rs:41-45`, `manager.rs:112-125` | `OnceLock` 不可重置；`destroy_instance` 只翻转 `DESTROYED` 标志；注释自认"C++ 可 delete/重建，Rust 做不到" |
| 6 | OFX `Host` + 泄漏的 `OfxHost*` | `host.rs:1224-1240`, `host.rs:258-269` | 属性集在首次 `global()` 固化（身份/能力/GL 宣告），`Box::into_raw` 永久泄漏；`quercus_ofx_shutdown`（`crates/quercus-engine/src/ofx/host.rs:170-174`）只能卸载插件，不能重建宿主 |
| 7 | OFX 实例/节点注册表 | `node_factory.rs:68`, `node_factory.rs:156`（project extent）, `crates/quercus-ofx-plugin/src/node.rs:257`（`NODE_REGISTRY`） | 进程级 u64 身份，跨 ABI 传递；无命名空间隔离 |
| 8 | OFX 渲染执行器槽 | `crates/quercus-render/src/eval.rs:213-224`（`set_plugin_executor`）、`node_factory.rs:913-923`（`EXECUTOR_INSTALLED` + `OnceLock`） | 单槽覆盖式，且"只装一次"；装配顺序错就永久是旧实现 |
| 9 | OFX 进程客户端 | `crates/quercus-render/src/ofxhost.rs:523-538` | 全局单例 + `install_client`；未导出 |
| 10 | GL 上下文 | `gl_bridge.rs:421-427`（`GL_CONTEXT`）+ `GL_LOCK` | 进程级一次创建；非 macOS 直接 stub |
| 11 | GPU 共享上下文槽 | `backend.rs:369-388`, `backend.rs:1845-1885` | `decided` 后固定；`install_shared` 仅能替换"尚未用过"的上下文（`backend.rs:1872-1885`）。**先调 `shared()` 的任何一个缩略图任务都会让后续接管失败** |
| 12 | 文字后端钩子 | `textbackend.rs:161-164`（`MEASURE`/`RENDER` 全局 `Mutex`）、`textengine.rs:120-131`, `textengine.rs:139-148`（`install` 只执行一次）、`textengine.rs:253-256`（`FAMILIES` 快照） | 钩子本身可换（好），但 `install()` 的 `Once` 与 family 快照让"装完再换字体"失效 |
| 13 | 配置存储 | `configstore.rs:86`（`OnceLock<ConfigStore>`） | 全局单配置；多租户/农场按作业切配置不可能（只能靠 `QUERCUS_CONFIG_DIR` 启动前指定） |
| 14 | 引擎侧任务/交互/Viewer 时间表 | `crates/quercus-engine/src/task.rs:139`、`crates/quercus-engine/src/ofx/interact.rs:21`、`crates/quercus-engine/src/ofx/host.rs:303-304` | 均为进程级 map；`VIEWER_PROVIDER` 只装一次（`host.rs:321-326`） |
| 15 | 解码服务（管线后端） | `crates/quercus-render/src/pipeline.rs:641` | 全局槽；未导出 |

**初始化顺序陷阱（值得在设计 seam 时固化成契约）**：`GpuContext::shared()` 先到先得（#11）、`textengine::install()` 与 `font_families` 快照（#12）、OFX `Host::global()` 固化能力（#6）、FFmpeg 一次初始化（#1）。

---

## 4. 已存在 Rust trait、但未导出到 ABI 的清单

| trait / 钩子 | 定义 | ABI 现状 |
|---|---|---|
| `Decoder` | `crates/quercus-codec/src/decoder.rs:262` | 只能按 id 选内置（`engine/src/codec/decoder.rs:724-748`）；无注册入口 |
| `Encoder` | `crates/quercus-codec/src/encoder.rs:34` | 只能经 `EncodingParams.format` 选内置（`include/quercus_engine.h:737`） |
| `GpuContextLike` | `crates/quercus-core/src/backend.rs:262` | 未导出；`Texture` 内部已用 `Arc<dyn GpuContextLike>`（`texture.rs:181`） |
| `StorageBackend` | `crates/quercus-storage/src/backend.rs:67` | **参考 seam**：注册只用 Rust `Registry::register`（`registry.rs:56-65`）；ABI 只有枚举 `backend_count/name_at/scheme_at/for_uri`（`engine/src/storage.rs:153-230`，`include/quercus_engine.h:1244-1247`）。`backend.rs:18-19`/`registry.rs:22-23` 提到的 `ffi.rs` **在 `crates/quercus-storage/src/` 下不存在**（`lib.rs:31-38` 模块表无此模块）→ C vtable 注册从未实现【推断：文档先行、实现未落地】 |
| `JobDispatch` | `crates/quercus-render/src/worker.rs:127` | 未导出；后端只有 3 个内置分支（`manager.rs:45-59`） |
| `AutoCacheEvents` | `crates/quercus-render/src/autocacher.rs:38-43` | 未导出（`set_events` 无 ABI 调用点） |
| `TextMeasureBackend` / `TextRenderBackend` | `textbackend.rs:132-137` | 未导出；ABI 侧只能 `textengine_install` |
| `UiProgressReporter` / `ReporterFactory` | `progress.rs:39`, `progress.rs:49-51` | **已导出**（唯一做对的例子）：`engine/src/ofx/host.rs:420-465` 把 C 回调 + userdata 桥进 factory |
| 插件节点 duplicator | `crates/quercus-node/src/nodes/plugin.rs:120-140` | 未导出（由 `install_render_executor` 内部装好） |

对照结论：**仓库里已有的"可替换"先例是有的（进度回调、storage 注册表、文字钩子、task submit 回调），但它们全都停在进程边界内侧**；唯一穿过 C ABI 的是 OFX 进度回调（`engine/src/ofx/host.rs:420-465`）和 `quercus_codec_task_set_submit_cb`（`engine/src/codec/task.rs:61-64`）。这两个就是新增 seam 的模板。

---

## 5. 值得新增的 seam 草图（优先级）

设计前提：C ABI 只能传 POD/不透明句柄 + `extern "C"` 回调（`(cb, userdata)` 模式），不能传 Rust trait 对象；注册一次、进程级、`None` 清除（模板见 `engine/src/codec/task.rs:56-90`）。

### P0 — `MediaIo` / 媒体 IO 与 decoder 注册（一个 seam 解决 80% 诉求）

为什么先做这个：ffmpeg 打开的输入是**纯文件名**（`ffmpeg.rs:784`），因此"媒体在别处"（代理缓存、对象存储、厂商 raw、合成源）在今天的 ABI 里只能靠调用方先把文件落到本地磁盘。这是唯一同时服务"自带媒体缓存/代理"和"渲染农场"的 seam。

```c
/* 媒体源：按 id 打开，不假设本地文件系统 */
typedef struct {
  void*  (*open)(const char* filename_utf8, void* userdata);      /* 返回句柄或 NULL */
  int64_t(*read)(void* h, void* buf, int64_t cap, void* userdata);/* >0 字节, 0 EOF, <0 错 */
  int64_t(*seek)(void* h, int64_t off, int whence, void* userdata);
  int64_t(*size)(void* h, void* userdata);
  void   (*close)(void* h, void* userdata);
} QuercusMediaIoVtable;
bool quercus_codec_mediaio_install(const QuercusMediaIoVtable* vt, void* userdata); /* NULL 恢复默认 */
```
- 实现落点：`ffmpeg.rs:784` 改为在存在 vtable 时构造自定义 `AVIOContext`（`avio_alloc_context`）；`CodecStream::exists()`（`decoder.rs:230-232`）改为查询 vtable。
- 线程契约：`read/seek` 会在解码线程与 N 个渲染 worker 进程内被并发调用——vtable 必须线程安全，或由引擎按 `(filename, stream)` 串行化（推荐后者并在头文件写明）。
- 成本：中（M）。风险：自定义 AVIO 与 FFmpeg 静态链接无冲突，但 seek 语义要严格（`AVSEEK_SIZE`）。

配套（同一优先级、可分开落地）：**decoder 工厂 vtable**（当 MediaIo 不够，例如厂商 SDK 解码 + 自有时码）
```c
typedef struct { /* id, probe, open, retrieve_video, retrieve_audio, close */ } QuercusDecoderVtable;
bool quercus_codec_decoder_register(const char* id, const QuercusDecoderVtable* vt, void* userdata);
```
- 先决条件：先把 `eval.rs:1293` 改成走 `receive_list_of_all_decoders()`，否则注册了也用不上（**这是零 ABI 变更的 P0 修正**）。

### P1 — `GpuDeviceProvider`（零拷贝 present 的唯一通路）

现状：`install_shared`/`adopt`/`mark_host_context`（`backend.rs:1865`, `554`, `1901`）是 Rust-only，而 `quercus-engine` 只有 cdylib（`Cargo.toml:22`）→ **M2 的"app 交出自己的 gpui device"路径对任何消费者都不可达**【推断：该注释写于单库时代，拆分后无 ABI 出口】。

```c
/* 句柄是不透明指针：由调用方用同一 wgpu 大版本创建，引擎只借用 */
bool quercus_render_gpu_install(void* wgpu_device, void* wgpu_queue, int32_t backend_kind);
bool quercus_render_gpu_mark_host(void);   /* 无 install 时：把引擎自建上下文声明为宿主设备 */
int32_t quercus_render_gpu_kind(void);     /* BackendKind */
```
- 线程契约：必须在第一次渲染前调用（`backend.rs:1872-1885` 的 `decided/used` 语义）；install 后设备由调用方保活，引擎不得 drop。
- 成本：小（S）到中（M）：函数本身薄，代价是 **ABI 与 wgpu major 绑定**（当前 29，`quercus-render/Cargo.toml:21`）。替代方案：干脆只导出 `mark_host` + 让引擎用 `adopt` 逻辑包装宿主传入的 `Arc<Device>`——但只要跨 ABI 传 wgpu 指针，版本锁就存在。**边界诚实**：如果不愿锁 wgpu 版本，就接受"零拷贝 present 只在 in-tree Rust 可用"，并删掉误导性注释。

### P1 — 字体 provider（代价最小、消费者最常提）

```c
bool quercus_render_textengine_add_font_dir(const uint8_t* path, size_t len);
bool quercus_render_textengine_add_font_data(const uint8_t* bytes, size_t len); /* TTF/OTF 内存注入 */
```
- 落点：`textengine.rs:120-131` 的 `FontSystem` 构造前后调用 `db_mut().load_font_data/load_fonts_dir`（cosmic-text 0.19 已具备），并让 `font_families()`（`textengine.rs:253`）可失效。
- 线程契约：必须在 `install()`（`textengine.rs:141`，`Once`）之前完成——或直接把 `Once` 改成"首次使用前可配置"。之后只读。
- 成本：小（S）。若消费者要替换整个排版引擎（不是字体），再考虑把 `set_text_backends`（`textbackend.rs:145-158`）导出为 C vtable（成本 M，且要镜像 `TextLayoutRequest`）。

### P1 — 音频输出开关 / sink

```c
void quercus_audio_manager_set_output_enabled(bool enabled); /* false: 不打开 cpal 流 */
```
- 消费者路线：关掉内部输出 → 用 `quercus_task_audio_render`（`include/quercus_engine.h:1268`）取混音 buffer → 自己送 NDI/SDI/ASIO/远端。这已经能覆盖绝大多数"换输出"的诉求，且不引入新线程契约。
- 成本：小（S）。真正的 sink vtable（`push(params, samples)` + 时钟/欠载回调）成本 M，除非有明确客户，否则推到 P2。

### P1 — 磁盘缓存配额 + 目录 ABI

```c
int64_t quercus_render_disk_cache_max_bytes(void);
bool    quercus_render_disk_cache_set_max_bytes(int64_t bytes); /* <0 = 不限（今天的行为） */
bool    quercus_render_disk_cache_set_path(const uint8_t* path, size_t len);
```
- 今天没有上限、没有淘汰（`cache.rs` 无 evict；`DiskCacheBehind/Ahead` 未被消费，见 §2.2），农场长跑会把盘写满【推断：这是运维事故级风险，不是洁癖】。
- 成本：中（M，要加 LRU/引用保护）。

### P2 — 渲染作业后端配置（不做 trait）

```c
typedef struct { int64_t workers; int32_t slots_per_worker; int32_t width, height; uint64_t handshake_timeout_ms; } QuercusDispatcherConfig;
bool quercus_render_manager_init_ex(const QuercusDispatcherConfig* cfg);
```
- 映射到 `DispatcherConfig`（`procpool.rs:824-846`）；`workers=0` 保持现有自动策略（`procpool.rs:485-…`）。不要导出 `JobDispatch` trait——调用方实现一个调度器没有产品价值。

### P2 — OFX 宿主身份/能力（不做宿主 vtable）

```c
bool quercus_ofx_set_host_identity(const uint8_t* name, size_t len, const uint8_t* version_label, size_t vlen);
bool quercus_ofx_set_gl_advertised(bool enabled);
bool quercus_ofx_set_plugin_paths_disabled(bool disabled);
```
- 理由：商业插件按宿主名/版本分支，渲染农场无 GL 却看到 `OfxImageEffectPropOpenGLRenderSupported="true"`（`host.rs:1529-1531`，而 `gl_bridge.rs:815-828` 在非 macOS 恒 false）。这是**配置**问题，不是**实现替换**问题。
- 成本：小（S）。

### 明确不做的 seam

- **FFmpeg codec 集合**：静态链接 + 项目专用构建（`quercus-codec/Cargo.toml:15-29`, `README.md:50-75`）。暴露"换 codec 库"等于让消费者承担 FFmpeg 版本/许可/静态链接矩阵，ROI 为负。消费者真正要的是**输入输出**（P0/P2），不是内部实现。
- **OCIO 色彩管理**：唯一合理的"替换"是换 config 文件（`OCIO` env，`color.rs:500-598`），已具备。OCIO 本身的正确性/性能不是可插拔项。
- **OFX 宿主整体替换**：OFX 是标准，换宿主 = 换掉整个 quercus-ofx-plugin（`host.rs` 2431 行 + 各 suite）；替代品只能是另一家的宿主，产品上无意义。要开放的是它读到的**属性**与**插件集合**。
- **音频混音/处理**：调用方要的是样本（`task_audio_render`），不是替换 DSP。

---

## 6. 产品判断（写给"参考消费者"）

1. Oak 是**独立仓库 + SDK**，所以任何不能从 C ABI 调用的能力，对 Oak 等于不存在。这直接决定了 GPU device 注入、decoder 注册、字体注入三件事的形态：**都必须是 C 函数**，内部 Rust trait 只是实现手段。
2. 渲染农场最痛的三件事，恰好都在本审计范围内且今天都没有 seam：**媒体不在本地**（P0 MediaIo）、**缓存无配额**（P1）、**不能控制 worker/设备**（P1）。这三件比"能换解码器"更紧急。
3. "自带媒体缓存/代理"这类消费者**不会**要求换 FFmpeg；它会要求"让我自己决定从哪读字节"。所以 P0 选 MediaIo 而不是 decoder vtable 是产品判断，不只是成本判断。
4. 字体是**低成本高感知**的 seam：品牌标题、多语言、渲染农场统一字体目录，都是真实需求，且改动集中在 `textengine.rs` 一个文件。
5. OFX 宿主应当**保持单一实现**，只把它对外宣称的能力参数化。反过来，如果 Oak 要做"插件沙箱/白名单"，正确做法是配置 + 进程隔离（已有 `quercus-worker --ofx-host`，`ofxhost.rs:20-40`），不是宿主 vtable。

---

## 7. 诚实性说明与副产物发现

- 所有文件路径相对仓库根；行号为审计时刻的物理行号。
- 未运行任何构建/测试；`cargo` 相关结论均来自 `Cargo.toml`/`Cargo.lock`（`cpal 0.18`、`ffmpeg-next 9`、`wgpu 29`、`ocio-sys`、`cosmic-text 0.19`/`swash` 均在锁文件中，`Cargo.lock:693,717,1053,2388,3690,4290`）。
- 文档漂移（低危但影响判断）：
  - `crates/quercus-engine/src/codec/decoder.rs:4-6` 写"Decode sessions (open/retrieve/conform) are deliberately not exported yet"，但同文件 `:724`/`:767`/`:789` 已导出 open/retrieve_video_frame/retrieve_audio。**头部注释过期**——容易让审计者误判解码 ABI 不存在。
  - `crates/quercus-storage/src/backend.rs:18-19` 与 `registry.rs:22-23` 引用 `ffi.rs`（C vtable 注册），该文件不存在。
  - `crates/quercus-codec/src/decoder.rs:407-412` 声称 OIIO 是"the real OIIODecoder"，而 `oiio.rs:25-29` 明说是 stub（probe 恒 `None`）。两处注释互相矛盾，**以 `oiio.rs` 为准**。
- 无法验证的项：
  - `install_shared` 的实际生产调用者：工作区内**只有** `crates/quercus-render/tests/footage_import_test.rs:123,239,311` 三处测试调用，无 ABI 出口 → 只能推断为"拆分前 in-tree UI 的遗留接口"。
  - `DiskCacheBehind/Ahead` 是否在别的仓库（Oak）被读取 → 本仓库无消费者。
  - 音频 cpal 在无头机器上的实际行为（`ensure_open` 失败被吞，样本继续入队 → 静音）——仅代码推断，未运行验证。
