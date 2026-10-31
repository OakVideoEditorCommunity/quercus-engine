# 运行时/环境注入面审计（config / paths / logging / threading / undo / task）

范围：`quercus-core`(configstore/filefunctions/miscutils/debug/backend/color/textengine)、`quercus-undo::global`、`quercus-render`(manager/procpool)、`quercus-engine`(corestate/task/worker/undo)、`quercus-engine-rs`(SDK)。只读审计；未运行 `cargo build/test`（FFmpeg 工具链缺失），所有结论来自源码阅读。标注 `推断` 的条目未做运行验证。

## 0. 结论先行

- **config 无法在一进程内隔离两个独立消费者（判定：不可能）。** 全进程只有一个 `OnceLock<ConfigStore>`（`crates/quercus-core/src/configstore.rs:85-96`），键空间、`load()`、`save()`、错误处理器（`set_error_handler` 用 `AtomicPtr` 存单例字段，`:70-77`）全部共享；持久化目标路径又由进程级环境/OS 推导（`crates/quercus-core/src/filefunctions.rs:107-147`）。两个消费者要不同 config 文件/格式/内存命名空间，只能改环境变量，而环境变量本身也是进程全局，且 `load()` 会 `set_defaults()` 清空整张表（`:103,:536-538`）。
- **undo 也无法隔离，但这是明确的、有文档的故意设计。** `crates/quercus-engine-rs/README.md:78` “The engine's global undo stack is shared by all sessions by design.”；`crates/quercus-undo/src/global.rs:17-24` 与 `crates/quercus-undo/src/lib.rs:33-39` 说明历史栈/组/观察者都是进程状态。对“单文档 app”成立，对“一个进程里两个独立文档/两个 SDK 消费者”则是硬约束，且没有 per-session 逃生通道。
- **没有可供调用方捕获的日志/诊断 sink。** `quercus_core::debug` 有一套基于 `log` facade 的等级日志（`crates/quercus-core/src/debug.rs:20-28`），但它**没有导出到 C ABI/SDK，且内核代码几乎不用它**（全仓无 `quercus_core_log` 符号，`debug.rs` 之外无 `debug::log` 调用）；实际诊断走 `eprintln!`（`crates/quercus-engine/src/worker/mod.rs:83-85`、`crates/quercus-render/src/procpool.rs:1242,1568,1650`）。config 错误处理器在 domain 存在（`configstore.rs:506-530`）但 ABI 未导出（header 只有 config 读写/加载/保存，`crates/quercus-engine/include/quercus_engine.h:861-870`）。
- **唯一的“注入”手段是环境变量**（`QUERCUS_CONFIG_DIR` / `QUERCUS_PIPELINE` / `QUERCUS_RENDER_BACKEND` / `QUERCUS_WORKER_BIN` / `OCIO` …），而 SDK README 完全没有文档化这些变量（README 无 config/env 章节，只有 `FFMPEG_DIR`）。
- 最严重的单例/初始化顺序风险：①先读 config、后 `Config::load()` 会用默认值初始化组件、且 load 反向清掉运行时 `set()` 的值；②第二个消费者 `RenderManager::init` 直接 `Err(State)`；③`GpuContext` 的 `decided` 槽一旦决定就不能再安装宿主设备（`backend.rs:1865-1888`），而 C ABI 故意不导出 `GpuContext`（`crates/quercus-engine/src/corestate.rs:7-9`）；④render worker 子进程**不加载 config.toml**（`worker/mod.rs:417-419`），主/子进程配置可能不一致（推断：影响依赖 config 的 codec/GPU 开关）。

## 1. 组件盘点表

强度：**强** = 库/SDK 形态下调用方几乎必然要求替换；**中** = 有明确场景（多文档、宿主设备、外部任务系统）；**弱** = 可接受现状。

| 组件 | 现状 | 证据(file:line) | 替换诉求强度 | 最小改动 |
|---|---|---|---|---|
| `ConfigStore` 存储与持久化 | 进程单例；固定 `<config_location>/config.toml`；`save()` 整表写 temp+rename；legacy `config.ini` 一次性迁移 | `crates/quercus-core/src/configstore.rs:17-26,85-96,102-105,293-295`；`:20-21` “Per the config-wave ruling this is a process-wide singleton, NOT a refcounted handle” | **强** | 抽出 `trait ConfigSource { load()->Map; save(&Map) }`，由 `EngineContext` 持有；保留 `ConfigStore::global()` 兼容层 |
| config 初始化顺序 | 构造时 `set_defaults()`；`load()` 再次 `set_defaults()`（清空自定义键）再读文件；**没有任何内部代码自动调用 `load()`**，只有 ABI `quercus_core_config_load` | `configstore.rs:82-95,102-105,536-538,540-543`；`crates/quercus-engine/src/corestate.rs:188-189`；SDK 包装 `crates/quercus-engine-rs/src/state.rs:130-160,277-288` | **强** | 显式 `engine_init(config)` 一次完成“位置→加载→组件初始化”，禁止隐式首访 |
| config 错误处理器 | domain 有 `set_error_handler`，无 ABI 导出；无 handler 时 `eprintln!` | `configstore.rs:35-38,506-530`；header：`quercus_engine.h:861-870` 无对应函数 | **强** | 导出 `quercus_core_config_set_error_handler`，或并入统一 log sink |
| 路径/位置提供者 | 全部内部推导：`QUERCUS_CONFIG_DIR` → portable 文件 → macOS `~/Library/Application Support`/`$XDG_CONFIG_HOME`/`HOME` → temp；temp=`temp_dir()/quercus`；自动恢复=`<config>/autorecovery`；缓存=`DiskCachePath` config 或 `<config>/mediacache` | `crates/quercus-core/src/filefunctions.rs:107-147,157-159,192-199,202-208,437-452` | **强** | `trait LocationProvider{config,data,cache,temp,plugins}`；首次使用前注入；ABI 只读 getter 保留 |
| 路径查询 ABI | 只读：`get_configuration_location/application_path/temp_file_path/auto_recovery_root/default_disk_cache_path` | `crates/quercus-engine/src/coreutil.rs:277-283`；`quercus_engine.h:876-887` | 中 | 增加“设置/重定向”入口或改为 init 参数（现只能靠 env） |
| 日志/诊断 sink | `log` facade + 首次使用时装 stderr logger，宿主先装则让位；有 map 级别；**无 ABI**，内部仍用 `eprintln!` | `debug.rs:20-28,150-161,175-206`；`worker/mod.rs:83-85`；`procpool.rs:1242,1568,1650`；全仓无 `quercus_core_log` | **强** | 导出 `quercus_core_log_set_level`/`quercus_core_log_set_sink(cb,userdata)`；把 `eprintln!` 改走 `debug::log` |
| GPU 共享设备槽 | 进程级 `OnceLock<Mutex<SharedSlot>>`；一旦 `decided` 不可替换；宿主可用 `install_shared`，但只在 Rust API，C ABI 明确不导出 | `crates/quercus-core/src/backend.rs:371-386,1845-1860,1865-1888,1901-1914`；`corestate.rs:6-9` “`GpuContext` is deliberately NOT exported: … needs a separate engine-owns-the-device design” | **强** | 设计 `engine_attach_device`（外部 device 所有权）；在此之前至少暴露 `install_shared`-等价 ABI |
| OCIO 默认配置 / 色彩管线 | 两个进程级 `LazyLock`：`DEFAULT_CONFIG`、`PIPELINE_COLOR`；`$OCIO` 在调用时读取；一个进程只有一套管线色彩 | `crates/quercus-core/src/color.rs:430-447,485-507,597-605` | 中 | 管线色彩移入会话/工程上下文；OCIO 配置句柄按工程持有 |
| 文本后端/字体 | `install()` 用 `Once` 装进程级钩子；注释直言“a second engine instance cannot swap the hooks” | `crates/quercus-render/src/textengine.rs:120-126,141-147,252-270` | 中 | 钩子改为按 engine handle 分派；字体库参数化 |
| Undo 栈/undo group/observers | 进程级 `OnceLock<Mutex<UndoStack>>` + `static GROUP`；observer 无注销 API | `crates/quercus-undo/src/global.rs:43-45,63-66,102-107,165-205,248`；`lib.rs:33-39`；`README.md:78` | **强**（多文档）；单文档=中 | `UndoSession{ Arc<Mutex<UndoStack>> }`，`global::*` 变为默认会话的薄封装 |
| Undo 变更通知的 revision | 编辑通知里的 `revision` 取全局 `global::index()` | `crates/quercus-engine/src/editor/notify.rs:85-95` | 中 | revision 来自 session/stack 句柄（推断：现两个会话 revision 会互相跳变） |
| 活动工程槽 | `ACTIVE_PROJECT` 进程级 `RwLock<Option<Arc<Mutex<Project>>>>`，由宿主 `set_active` | `crates/quercus-engine/src/node.rs:85-87,105-120` | **强** | 活动工程挂到 `EngineContext`/Session |
| RenderManager | 进程级 `Mutex<Option<Arc<RenderManager>>>`；`init` 幂等，二次返回 `Err(State)`；后端由 `QUERCUS_PIPELINE` 选 | `crates/quercus-render/src/manager.rs:38,62-70,127-145`；`render.rs:47-59` | **强** | manager 由 `EngineContext` 创建/持有；工厂参数化（backend、worker 配置） |
| Render worker 子进程 | `Command::new(quercus-worker) --backend auto`，stderr 继承；`DispatcherConfig.worker_bin/workers/slots` 存在但 ABI 只能 `Default::default()` | `procpool.rs:823-840,1936-1966,2406-2424`；`render.rs:53-56` | 中 | C ABI 接受 dispatcher 配置结构（二进制、worker 数、超时） |
| 任务注册表（`static TASKS`） | 进程级 `HashMap<u64, Arc<TaskEntry>>`；每任务 1 线程；`take_*` 一次性转移；`wait` 阻塞调用线程 | `crates/quercus-engine/src/task.rs:139-149,167-205,814-885,892-980` | 中 | 增加进度回调注册 + `poll` 语义；`take_*` 保留；允许注入 submit 队列 |
| `quercus-task::TaskManager` | 单例（`Mutex<Option<ManagerPtr>>`），重复 `init` 返回 `Err(State)`，每任务 1 线程；**未导出**“app-side … intentionally not exported” | `crates/quercus-task/src/manager.rs:70,106-130,160-176`；`crates/quercus-engine/src/task.rs:13-15` | 弱 | 保持内部；若导出改为句柄 |
| Codec 任务提交回调 | 已有真正的注入点：`set_task_submit_cb` 全局替换（ABI `quercus_codec_task_set_submit_cb`） | `crates/quercus-codec/src/task.rs:67-120`；`crates/quercus-engine-rs/src/codec.rs:686-703` | 弱（已满足） | 补文档/生命周期说明 |
| 时钟/超时 | 无 `Clock` 抽象；`Instant::now()` + 固定 deadline（handshake 10s、worker 30s） | `procpool.rs:860`；`worker/mod.rs:850`；`displayicc.rs:776,822` | 弱 | 超时参数注入即可，不必引入时钟 |
| 帧缓存 `FrameManager` | 进程级 `INSTANCE`/`STARTED`，内存预算固定/按 env | `crates/quercus-codec/src/framemanager.rs:49,127` | 中 | 预算随 context 配置 |
| Worker 子进程 runtime | `initialize_runtime` 明确跳过 config：`"config / frame manager / disk manager / project serializer have no Rust backing in the worker binary; skipped"` | `crates/quercus-engine/src/worker/mod.rs:372-430`（引文 `:417-419`） | 中（正确性） | worker 启动参数显式带 config 路径并 `config_load()` |

> 说明：`QuercusCurrent` 的 4 个对象槽（video/audio params、plugin host、plugin cache）是 `OnceLock` 进程单例，注释明确 `CPP-PARITY`（`crates/quercus-core/src/miscutils.rs:200-240`），属“故意保留的 C++ 语义”，不计入高优先注入项。

## 2. 单例 / 初始化顺序风险地图

| # | 触发顺序 | 后果 | 证据 |
|---|---|---|---|
| H1 | 组件先读 config（`ConfigStore::instance()` 首访）→ 调用方后 `Config::load()` | 组件拿到编译期默认值；且 `load()` 内部 `set_defaults()` 会 `clear()` 掉此前运行时的 `set()`，静默丢失配置 | `configstore.rs:85-96,102-105,536-538`；`filefunctions.rs:439`（`DiskCachePath` 调用时读取） |
| H2 | 先 `GpuContext::shared()`（缩略图/任务/渲染任一）→ 再 `install_shared(host_ctx)` | 若已有 device 已建纹理/管线则拒绝替换，进程永久用引擎私有 device；宿主 zero-copy 呈现失效 | `backend.rs:1845-1888` |
| H3 | 先 `RenderManager::init()` （消费者 A）→ 消费者 B 再 init | B 直接 `Err(State)`，B 的 backend/worker 配置无效；`init` 注释 “only the main GUI process does this” | `manager.rs:127-145` |
| H4 | 先 `textengine::install()` | 首个安装者决定进程文本后端，第二个 engine 实例无法替换 | `textengine.rs:141-147` |
| H5 | `color::set_up_default_config*()` 之前发生渲染/导出 | 使用内置 default config（或 `$OCIO` 当次值）；`OCIO` 每次 `config_path()` 重新读，运行中变化不可控 | `color.rs:499-517,597-605` |
| H6 | 会话 A 新建工程/`clear()`（`quercus_undo_clear`）→ 会话 B 的历史 | A 的 clear 清掉 B 的全部 undo 历史；`group_begin` 在任一会话打开组时对另一会话返回 `State` | `global.rs:102-113,248-256`；`quercus_engine.h:1331,1336` |
| H7 | worker 子进程启动 | worker **不加载 config.toml**，所有 `ConfigStore` 读取回落默认值；主进程按 config 选择的 GPU import/hwaccel/DiskCache 与 worker 行为可能分叉（推断） | `worker/mod.rs:417-419`；`gpuinterop.rs:107`、`hwdecode.rs:135`（worker 侧也读 ConfigStore） |
| H8 | 任务 `take_*` 与 `release` 顺序 | `take_*` 只能取一次；`release` 会先 cancel+join（可能长时间阻塞调用线程）；注册表进程级、无会话归属 | `task.rs:859-885,892-980` |
| H9 | 两个消费者都 `quercus_project_set_active` | 后者覆盖前者；插件经 `quercus_project_active` 读到“错误工程” | `node.rs:85-87,105-120` |
| H10 | 宿主先装 `log` logger 再调用引擎 | `debug.rs` 会让位给宿主（唯一较友好的行为），但引擎内部 `eprintln!` 仍然绕过宿主 sink | `debug.rs:150-161`；`procpool.rs:1242` |
| H11 | `ConfigStore::set_error_handler`（domain） | 调用方无法从 ABI 注册；错误统一落到 stderr，无法按实例归因 | `configstore.rs:506-530`；header 无导出 |
| H12 | `TaskManager::init` 两次 | 第二次 `Err(State)`；内部单例未导出，宿主无法用同一 API 建自己的任务系统 | `quercus-task/manager.rs:106-120` |

## 3. 环境变量清单（运行时行为开关）

| 变量 | 读取点 | 作用 | 备注 |
|---|---|---|---|
| `QUERCUS_CONFIG_DIR` | `filefunctions.rs:110` | 覆盖 config 根（并 `create_dir_all`）；cache/autorecovery 随之 | 唯一路径重定向手段；测试大量使用（`configstore.rs:889` 等） |
| `XDG_CONFIG_HOME` / `HOME` | `filefunctions.rs:130,125,132` | 默认 config 根 | 非 macOS 优先 XDG |
| `QUERCUS_RENDER_BACKEND` | `crates/quercus-core/src/backend.rs:105` | 覆盖 `GraphicsBackend` config | 会绕过 configstore |
| `QUERCUS_REQUIRE_GPU` | `backend.rs:407` | 无 adapter 时 panic 而非跳过（CI） | 进程级策略 |
| `QUERCUS_PIPELINE` | `crates/quercus-render/src/manager.rs:64-70` | `threads` → 线程管线，否则 worker 进程池 | 只在 `RenderManager::init()` 读 |
| `QUERCUS_WORKER_BIN` | `procpool.rs:2411`；`ofxhost.rs:574` | 指定 `quercus-worker` 二进制 | `DispatcherConfig.worker_bin` 优先 |
| `QUERCUS_WORKER_FRAME_CACHE_MB` | `worker/framecache.rs:60,84-89` | worker 帧缓存预算 | worker 进程内 |
| `QUERCUS_GPU_IMPORT` (+`_VAAPI/_D3D11/_VIDEOTOOLBOX`) | `crates/quercus-codec/src/gpuinterop.rs:91,100,271,286,301` | 关闭零拷贝导入 | env 优先于 config |
| `QUERCUS_HWACCEL` | `crates/quercus-codec/src/hwdecode.rs:132` | 强制关/开硬解 | 同上 |
| `OCIO` | `color.rs:500,598` | OCIO 配置路径 | 每次 `config_path()` 重读 |
| `QUERCUS_DISPLAY_ICC` | `crates/quercus-core/src/displayicc.rs:60` | 指定显示 ICC | 仅显示路径 |
| `QUERCUS_WINDOWS_COLOR` | `displayicc.rs:596` | Windows ACM 检测覆盖 | 调试 |
| `QUERCUS_DEBUG_AUDIO` | `crates/quercus-audio/src/outputdevice.rs:42` | 音频流诊断 | `OnceLock` 缓存 |
| `QUERCUS_DEBUG_DISPATCH` | `procpool.rs:1897` | 打印派发 batch | 直接 stderr |
| `PATH` / `LOCALAPPDATA` | `crates/quercus-codec/src/proxymanager.rs:317,344` | 查找 ffmpeg、缓存目录 | ffmpeg 路径来自 `configured_path`+PATH（`proxymanager.rs:308-339`） |
| `QUERCUS_WORKER_CRASH_ON_TICKET` / `_MARKER` | `worker/mod.rs:820-832` | 崩溃注入测试钩子 | 仅测试语义 |

（`grep` 中余下 `QUERCUS_*` 命中多为测试代码；上表仅列非测试读取点。）

## 4. 排名建议（“应该可注入”）

1. **强 — ConfigStore + 初始化入口**：引入 `ConfigSource`/`EngineConfig`，由 `engine_init` 一次性传入“位置 + 加载策略 + 错误回调”。理由：这是唯一同时污染持久化、路径、错误上报与所有模块读取顺序的单例（H1/H11；`configstore.rs:20-21` 的 “config-wave ruling” 说明单例是当时裁决，不是外部契约）。
2. **强 — 日志/诊断 sink**：导出等级设置 + sink 回调，并把 `eprintln!` 统一到 `debug::log`。理由：`debug.rs` 已经写好但成为孤岛，调用方今天无法捕获任何引擎诊断（H10）。
3. **强 — 位置/路径提供者**：config/data/cache/temp/plugin 目录一次注入。理由：现在只有 env 覆盖（`filefunctions.rs:107-147`），库宿主无法为不同消费者指定不同根目录；`save()` 甚至会在查询时创建目录（`filefunctions.rs:112,145`）。
4. **强 — Undo 作用域**：`global::*` 改为 `UndoSession` 句柄（保留 `global` 作为默认会话薄封装）。理由：SDK 已是“一个进程多个 `EditorSession`”的形态（README:66-78），但历史、组、观察者全部进程级（H6/H9）。若产品确认永远单文档，可降为中，但需在 README 明示“不可隔离”。
5. **强 — EngineContext + GPU device 所有权**：把 `GpuContext::shared` 的 `decided` 槽改为由 context 持有，提供外部 device 接入 ABI。`corestate.rs:6-9` 已承认这是待设计缺口。
6. **中 — RenderManager / worker 池配置**：允许传入 backend、`worker_bin`、worker 数、超时；当前 ABI 只有 4 个枚举（`render.rs:47-59`）且 `Processes` 用 `Default::default()`。
7. **中 — 任务进度/结果回调**：`static TASKS` 提供 `poll`+回调或外部 submit 队列；保留 `take_*` 一次性语义。理由：宿主已有自己的 task panel（`task.rs:13-15` 明确 app-side TaskManager 不导出），却只能轮询。
8. **中 — 色彩管线/文本后端按实例**：`PIPELINE_COLOR`、`DEFAULT_CONFIG`、文本钩子移到 context（H4/H5）。FFmpeg/wgpu 日志同理：目前完全不可捕获（推断：`ffmpeg` crate 日志或许走 `log`，但 engine 没装/暴露）。
9. **弱 — 时钟/超时**：把 handshake(10s)/worker(30s)/显示 ICC 缓存 TTL 变为参数即可，不需要完整 `Clock` 抽象。
10. **弱 — FrameManager 预算**：跟随 context 配置。

## 5. 多实例隔离判定（明确回答）

- **config：不能隔离。** 单例 `OnceLock`（`configstore.rs:85-96`）+ 单一持久化路径（`configstore.rs:293-295` + `filefunctions.rs:107-147`）+ `load()` 清表语义（`:103,:536-538`）三处叠加，使得“两个独立 engine 消费者、各自 config”在一个进程内无法实现；`Config::global()` 的命名（`crates/quercus-engine-rs/src/state.rs:130-134`）也明示“global”。宿主最多用 key 前缀做约定式命名空间（推断，非框架保证）。
- **undo：不能隔离；这是文档化的故意约束。** SDK README:78、`quercus-undo/src/lib.rs:33-39`、`global.rs:17-24` 三处一致说明“过程级、跨会话共享”。后果：两个“独立文档”共享一条历史；任一会话 `clear()`/`group_begin()` 影响另一会话；编辑通知的 revision 是全局计数（`editor/notify.rs:89`，推断会互相跳变）；观察者无法注销（`global.rs:58-66`），写入穿透会话会收到所有文档的命令。
- **GPU/文本/渲染/任务**：同样是“每进程一份”，第二个消费者的初始配置会被静默忽略或返回错误（H2/H3/H4）。

## 6. 覆盖与限制

- 已读：`configstore.rs`（单例/加载/保存/错误处理/默认值段）、`filefunctions.rs`（位置与缓存段）、`miscutils.rs`（globals 段）、`debug.rs` 全文、`color.rs`（单例/OCIO 段）、`backend.rs`（GPU 槽段）、`textengine.rs`（系统/安装段）、`quercus-undo/src/global.rs` 全文与 `lib.rs` 头部、`quercus-render/manager.rs`（init/backend 段）、`procpool.rs`（DispatcherConfig/spawn/resolve 段）、`quercus-engine/task.rs`、`corestate.rs`、`render.rs`、`coreutil.rs`（路径导出段）、`quercus-engine-rs`（state/render/editor 入口）、`quercus-task/manager.rs` 与 `proxy.rs`（并发段）、`quercus-codec/task.rs`（submit cb）、`quercus-engine/src/worker/mod.rs`（runtime init 段）、`quercus-engine/src/node.rs`（ACTIVE_PROJECT 段）、`quercus-engine/src/undo.rs`。
- 未覆盖：`backend.rs` 其余 4000 行（仅取共享槽与 GPU 计数器）、`color.rs` 其余转换实现、`texengine` 排版实现、`quercus-render/procpool.rs` 的重启/扩缩容实现细节、SDK 全部 task 包装。`crates/` 未做任何修改；`cargo` 未运行，故未验证运行时行为与 build 兼容性。
