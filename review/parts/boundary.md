# 边界审计：公开 SDK / C ABI 的可注入缝隙

审计对象：`/home/sunyu/Projects/quercus-engine`（workspace 版本 0.5.2）。只读审计，未执行 `cargo build/test`（FFmpeg 工具链缺失，且该操作被任务禁止）。下文中 `H` 指 `crates/quercus-engine/include/quercus_engine.h`。

版本注记：`Cargo.toml:7` 声明 workspace 0.5.2，但 `crates/quercus-engine/Cargo.toml:4` 是 0.5.3、`crates/quercus-engine-rs/Cargo.toml:4` 是 0.5.7，内部依赖用 `=0.5.2` 精确钉死（`crates/quercus-engine/Cargo.toml:35` 起）。三处版本不一致是发布漂移，不是 ABI 事实，但会影响 SDK 的 `quercus-engine = "=0.5.3"` 钉版（`crates/quercus-engine-rs/Cargo.toml:35`）。

---

## 0. 结论摘要（最重要的 7 条）

1. **没有引擎级 init/finalize**。`H` 中 `quercus_engine_init` / `_finalize` / `_shutdown` 出现次数均为 0；只有按子系统的生命周期（`quercus_render_manager_init/_shutdown` `H:1222/1229`、`quercus_ofx_shutdown` `H:1124`、`quercus_core_config_load/save`、`quercus_storage_flush`）。其余状态全部靠 `OnceLock` 首次使用惰性初始化，且**不可重置**。
2. **真正"可注入 + 可注销 + 有 userdata"的缝隙只有 2 个**：`quercus_plugin_*`（共 10 个函数，首个 `quercus_plugin_register` 在 `H:1189`）与 `quercus_editor_edit_subscribe`（`H:928`）；后者是唯一同时带 `drop_callback` 的（带析构的 userdata）。
3. **panic containment 只覆盖 edit subscription**。engine `crates/quercus-engine/src/editor/notify.rs:92` 与 SDK `crates/quercus-engine-rs/src/editor/notify.rs:31` 有 `catch_unwind`；plugin / undo observer / OFX progress 三条回调路径都是裸调用（`crates/quercus-engine/src/plugin.rs:252`、`undo.rs:85`、`ofx/host.rs:366`；SDK `plugin.rs:166`、`node.rs:1296`、`ofx.rs:478`）。这与 SDK README 的承诺"the SDK serializes Rust callbacks and catches panics"（`crates/quercus-engine-rs/README.md:110`）不符。
4. **undo observer 是只增不减的**：无注销、无 userdata 析构（`crates/quercus-engine/src/undo.rs:69-77` 明文写 "there is no removal"），SDK 侧同样 no removal 并故意泄漏 `Box`（`crates/quercus-engine-rs/src/node.rs:1276-1304`）。且派发时**持有 `OBSERVERS` 锁调用回调**（`undo.rs:82-87`），回调内再触发 undo 即死锁。
5. **storage backend 的 C vtable 缝隙"文档承诺、代码不存在"**：`crates/quercus-storage/src/registry.rs:22` 与 `backend.rs:18` 都指向 `ffi.rs` 里的 "C vtable pair"，但 `crates/quercus-storage/src/` 下没有 `ffi.rs`；公开 ABI 只有只读枚举（`quercus_storage_backend_count` `H:1244`），`grep storage_register H` = 0。
6. **codec 的 Decoder/Encoder 不可插，源码自认**：`crates/quercus-codec/src/decoder.rs:398` 原文 "Test/extension support (the C ABI has no way to register a decoder...)"。
7. **header 是 SDK 的投影，不是引擎导出的投影**：`tooling/gen_c_header.py:25` 从 `crates/quercus-engine-rs/src` 提取声明。后果：引擎里 2 个 `#[unsafe(no_mangle)]` 导出（`quercus_undo_push_owned`、`quercus_audio_params_rational_from_double`）不在 `H` 中。任何新缝隙都必须先加到 SDK extern block，否则 C 端不可见。

---

## 1. 公开面规模与生成链

| 项 | 数值 | 证据 |
|---|---|---|
| header 函数 | 684 | `H:14` 注释 "48 POD structs, 13 enums, 684 functions"；机械统计同为 684 |
| POD struct / enum | 48 / 13 | `H:14` |
| 引擎 `#[unsafe(no_mangle)]` 导出 | 637 | `grep -c '#\[unsafe(no_mangle)\]' crates/quercus-engine/src` |
| 引擎直接书写的 `extern "C" fn quercus_*` 名（去重） | 594 | 正则提取；差额来自宏生成（如 `handle.rs:59 export_handle!`、`codec/frame.rs:67 frame_getter!`） |
| SDK `#[link(name="quercus_engine")]` 站点 | 30 | `grep -rn 'link(name = "quercus_engine"' crates/quercus-engine-rs/src` |
| SDK 是否用引擎 Rust API | 否（仅文档提及） | `crates/quercus-engine-rs/README.md:7-11`；`src/lib.rs:1-6`；唯一 `quercus_engine::` 出现在注释 `src/edit/abi.rs:3` |
| header 生成器输入 | `crates/quercus-engine-rs/src` | `tooling/gen_c_header.py:25`；输出 `H`：`:26`；入口/`--check`：`:355`、`:450` |
| 回调 typedef 生成 | 支持 `typedef void (*Name)(...)` | `gen_c_header.py:268`、`:282`；产物 `H:658-666`（`TaskSubmitFn`/`UndoObserverFn`/`ProgressFn`/`ProgressEndFn`/`CommandFn`） |
| 内联回调签名 | 直接展开函数指针 | `H:928`（edit subscribe 的 callback + drop_callback） |

**ABI 权威性判定（推断，依据上表）**：公开 ABI ≡ SDK extern block。写新缝隙的顺序必须是：引擎 `#[unsafe(no_mangle)]` → SDK `unsafe extern "C"` 声明 + safe 包装 → `python3 tooling/gen_c_header.py --write`（`crates/quercus-engine-rs/README.md:141-146` 亦如此声明）。只加引擎导出不会进入 `H`。

---

## 2. 公开 API 分类清单（能力 / 可替换性 / 证据）

"可替换性"四档：**注入**（调用方提供实现）、**回调**（调用方提供事件处理）、**配置**（调用方只选参数/枚举/键值）、**无**。

| 能力 | 公开 API（前缀与代表符号） | 可替换性 | 证据 |
|---|---|---|---|
| 生命周期/初始化 | 无全局入口；`quercus_render_manager_init/_is_initialized/_shutdown`、`quercus_ofx_shutdown`、`quercus_core_config_load/save` | 配置（渲染后端枚举）；无全局 init | `grep engine_init H`=0；`crates/quercus-engine/src/render.rs:47,68`；`ofx/host.rs:170` |
| 配置/状态 | `quercus_core_config_*`（11 个）、`quercus_core_color_*`、`quercus_core_displayicc_*` | 配置（键值 + load/save） | `corestate.rs:33-194`；`crates/quercus-core/src/configstore.rs:85-96` |
| 项目模型 | `quercus_project_*`(26)、`quercus_node_*`(13)、`quercus_block_*`(20)、`quercus_track_*`(17)、`quercus_sequence_*`(10)、`quercus_folder_*`(3) | 无（纯数据操作）；唯一全局槽 = active project | `H:1208`；槽定义 `crates/quercus-engine/src/node.rs:87`，setter `node.rs:105` |
| 编辑 | `quercus_editor_*`(67)、`quercus_effect_*`(18)、`quercus_timeline_*`(14) | 回调（edit subscription）；命令面**无**（封闭 opcode） | `crates/quercus-engine/src/editor_dispatch/abi.rs:16-19,366`；enum 不可扩展，SDK 必须镜像 `src/edit/abi.rs:1-5` |
| 编解码/探测 | `quercus_codec_*`(89)、`quercus_footage_*`(13)、`quercus_video_*`(4)、`quercus_generator_*`(1) | 配置（`Codec`/`Format` 枚举）；解码器/编码器**无** | `crates/quercus-codec/src/decoder.rs:398`；`H:726`（只读 id 列表） |
| 渲染/预览 | `quercus_render_*`(17)、`quercus_thumbnail_*`(4)、`quercus_waveform_*`(4) | 配置（4 个后端枚举）+ 安装内建 textengine；调度实现**无** | `render.rs:47-57`；`crates/quercus-render/src/lib.rs:8-9,32`；`render.rs:10-12` 明文"intentionally not exported" |
| 音频 | `quercus_audio_*`(31) | 配置（设备索引） | `crates/quercus-engine-rs/src/audio.rs:192-207`；cpal 硬连 `crates/quercus-audio/src/manager.rs:441,479` |
| 任务/导出 | `quercus_task_*`(22)、`quercus_worker_*`(2) | 轮询（`state/progress/error`）；另有一条回调注入通道（codec task submit） | `crates/quercus-engine-rs/src/task.rs:208-267`；`crates/quercus-engine/src/codec/task.rs:61` |
| 存储 | `quercus_storage_*`(24) | 配置（`Storage/Backend` 键值）；注册**无** | `crates/quercus-storage/src/writethrough.rs:47-66`；`grep storage_register H`=0 |
| OFX | `quercus_ofx_*`(91) | 回调（progress handler）+ 文件系统扫描 `quercus_ofx_scan_path`（唯一真正的第三方代码加载路径） | `H:1120`；`ofx/host.rs:41`；`crates/quercus-engine-rs/src/ofx.rs:24-25` |
| 插件 | `quercus_plugin_*`(10) | **注入**（descriptor + 单回调 + userdata），可注销 | `crates/quercus-engine/src/plugin.rs:67-111,232-253` |
| OTIO | `quercus_otio_*`(56) | 无（格式固定） | `crates/quercus-engine/src/otio.rs`（46 个 extern） |
| 撤销 | `quercus_undo_*`(14) | 回调（observer，只增不减） | `undo.rs:69-107` |
| 诊断 | 仅错误字符串；无 error handler 注入 | 无 | `crates/quercus-engine/src/editor.rs:471`；`crates/quercus-engine-rs/README.md:118-120`；`grep config_set_error_handler H`=0 |

分类计数合计 684，与 `H:14` 一致。

---

## 3. 既有扩展点清单（生命周期 / 线程 / 可注销 / SDK 封装）

| 扩展点 | 可注入内容 | 注册时机 | 可注销 | 线程与锁契约 | SDK 是否封装 | 证据 |
|---|---|---|---|---|---|---|
| plugin registry | 描述符 + 单个 `CommandFn(command_id, userdata)`；**不是 vtable，一个插件只有一个回调** | 任意时刻（`Mutex<Vec<_>>`，无 OnceLock） | 是，按 index（`H` 声明 `quercus_plugin_unregister`） | 回调在**释放注册表锁之后**调用，允许回调内再进注册表 | 是，`Plugin` 的 `Drop` 先 unregister 再释放闭包 | `plugin.rs:58,84-98,104-111,232-253`；SDK `plugin.rs:155-180,234-239` |
| OFX progress handler | `ProgressFn(label,message,fraction,userdata)->bool` + `ProgressEndFn` | 任意时刻，可重复替换 | 是，传 null 清空 | 全局 `Mutex` 存三元组；从渲染线程回调；**无 catch_unwind**；另装 `procpool` 与 reporter factory | 是，`set/clear/has_progress_handler`，闭包用 `usize` 存槽 | `ofx/host.rs:351-380,392-465`；SDK `ofx.rs:435-531` |
| edit subscriptions | `(callback(userdata,u32,i64), userdata, drop_callback)` | 需要活着的 session（`*mut EditorSessionHandle`，null 返回 0） | 是，按 u64 id | 快照后**在 session 锁外**调用；两侧都 `catch_unwind` | 是，`EditSubscription` RAII | `editor/notify.rs:37-95`（engine）、SDK `editor/notify.rs:29-99` |
| undo observer | `UndoObserverFn(userdata)`，无 event、无析构 | 任意时刻，进程级、只注册一次 trampoline | **否**（`undo.rs:75` "there is no removal"） | 派发时**持有 `OBSERVERS` 锁**调用回调；无 catch_unwind → 重入/panic 均危险 | 部分：`add_undo_observer` 存在，但同样不可注销且故意泄漏 | `undo.rs:77-107`；SDK `node.rs:1276-1304` |
| config store | 键值（string/bool/int64/double）+ `load/save` | 任意时刻；`instance()` 首次访问装入编译期默认值 | 否（可覆盖，无删除） | 内部 `Mutex<HashMap>`；`load()` 先 `set_defaults()` | 是，`state.rs` | `corestate.rs:33-194`；`configstore.rs:85-96,102-108` |
| audio 设备选择 | 仅枚举索引（-1 = 系统默认） | 任意时刻 | 否（切回 -1） | cpal 全局；`cpal::default_host()` 硬连 | 是 | SDK `audio.rs:192-207`；`manager.rs:441,479` |
| render backend 选择 | 4 个枚举值之一 | 只能在首次 `manager_init` 时；已初始化再调用返回 false | 否（只能 shutdown 后重来） | 进程单例；`RenderManager::global()` | 是，`RenderBackend` enum | `render.rs:42-57`；测试断言 `render.rs:428` |
| textengine 安装 | 仅内建实现（无参 `install()`） | 任意时刻，幂等 | 否 | 字体族列表进 `OnceLock<Vec<String>>`，首次读取即冻结 | 是 | `render.rs:153-155,163-165,168-184` |
| codec task submit | `TaskSubmitFn(req,userdata)->i32`（Conform/Proxy） | 任意时刻，替换/清空 | 是（传 None） | 全局 `Mutex<SubmitCb>`；每次注册 `Box::leak` 一个 adapter | 是，且文档明示"re-registering leaks the previous closure" | engine `codec/task.rs:56-98`；`crates/quercus-codec/src/task.rs:65-81`；SDK `codec.rs:681-687` |
| OFX 插件发现 | 目录/文件路径（`scan_path`）→ 加载 `.ofx` 二进制 | 任意时刻 | 否（`ofx_shutdown` 才清） | host 单例 + alive 计数 | 是 | `H:1120`；`ofx/host.rs:41,170`；SDK `ofx.rs:24-25` |

补充事实：`quercus_plugin_*` 与 `quercus_undo_add_observer` 都在 `H` 中，属于"公开缝隙"；而 OFX 的 render executor / plugin-node 注册（`quercus_ofx_install_render_executor`、`quercus_ofx_register_plugin_nodes`）只是**安装内建行为**，不接收调用方实现（`ofx/host.rs:280-290`）。

---

## 4. init / lifecycle：惰性单例与"太晚注册"的失败模式

**事实清单（全部 `OnceLock`/进程级静态）**

| 单例 | 首次初始化动作 | 太晚注册/太晚配置的后果 | 证据 |
|---|---|---|---|
| `ConfigStore::INSTANCE` | `set_defaults()` 装入编译期默认值 | `load()` 可用但会先重置；`set_error_handler` 无法从 ABI 触达 | `crates/quercus-core/src/configstore.rs:85-96` |
| `Factory::global()` | 惰性执行 `nodes::register_all()`，静态节点表冻结 | 之后只能 `register_dynamic`（内部 API），静态表不可增 | `crates/quercus-node/src/factory.rs:82-88` |
| `Registry::global()`（storage） | 预注册 builtin（ove-xml/otio） | 无注册入口 → 无"太晚"问题，但也无法扩展 | `crates/quercus-storage/src/registry.rs:37-51` |
| `Host::global()`（OFX） | 构建宿主属性集 + 空插件缓存（`OnceLock`） | 先 scan 后 set_progress_handler 仍生效（handler 是 `Mutex`，不是 OnceLock） | `ofx/host.rs:303-306,392-394`；`crates/quercus-ofx-plugin/src/host.rs:1223` |
| textengine `FAMILIES` | 首次 `font_families()` 缓存字体族列表 | 之后再装后端，**已缓存的字体列表不会刷新** | `render.rs:163-165,175-184` |
| `RenderManager` | `manager_init` 显式；已初始化返回 false | 想换后端必须 shutdown（会 drain 在途任务） | `render.rs:42-70` |
| undo 全局栈 | `OnceLock<Mutex<UndoStack>>` | observer 可随时加；但进程级、永不清理 | `crates/quercus-undo/src/global.rs:43-44,63-75` |
| `ACTIVE_PROJECT` | `RwLock<Option<Arc<Mutex<Project>>>>`，进程级 | 多 session 场景下"active"是 host 自己维护的约定 | `crates/quercus-engine/src/node.rs:87,105` |

**判定（推断，标注依据）**：不存在"引擎 init 后一次性登记所有组件"的模型，因此**注册顺序不是由 API 强制的，而是由使用顺序隐式决定的**。对可安全晚注册的缝隙（plugin、progress handler、task submit、undo observer、edit subscribe）没有顺序约束；对"只能配置一次"的缝隙（render backend、textengine 字体缓存、storage builtin 表、node 静态表）晚配置表现为**静默降级**（返回 bool/false 或缓存旧值），而不是报错。→ 建议在文档中明确 "configure-before-first-use" 清单。

---

## 5. Gap list：内部有 trait/注册表，公开 ABI 无入口

每条都用 header grep 反证过（`grep -c <符号前缀> H` = 0）。

| # | 内部缝隙 | 内部证据 | header 反证 | 说明 |
|---|---|---|---|---|
| G1 | `StorageBackend` + `Registry::register/unregister` | `crates/quercus-storage/src/backend.rs:67`；`registry.rs:55,65` | `storage_register`=0、`storage_backend_register`=0；只有 `H:1244` 起的只读枚举 | **文档承诺的 `ffi.rs` C vtable 从未实现**：`registry.rs:22`、`backend.rs:18` 都写 "the C vtable pair in `ffi.rs`"，而 `crates/quercus-storage/src/` 无 `ffi.rs` |
| G2 | `Decoder` trait + 测试注册表 | `crates/quercus-codec/src/decoder.rs:262,402,414` | `decoder_register`=0、`codec_decoder_create`=0；只有 `H:726` 的只读 id 列表 | `decoder.rs:398` 自认 "the C ABI has no way to register a decoder" |
| G3 | `Encoder` trait | `crates/quercus-codec/src/encoder.rs:34` | `encoder_register`=0 | `quercus_codec_encoder_create` 走封闭 `Codec` 枚举 |
| G4 | `JobDispatch`（渲染调度缝隙，`Arc<dyn JobDispatch>` 注入 manager） | `crates/quercus-render/src/worker.rs:127`；`manager.rs:76-81`；`lib.rs:32` | `render_backend`=0（只有 int 枚举） | `render.rs:10-12` 明文："process pool / scheduler / pipeline are host-side infrastructure and intentionally not exported" |
| G5 | `NodeBehavior` + `Factory::register_dynamic` + `DynamicNodeMeta.create`（Rust 闭包构造器） | `crates/quercus-node/src/node.rs:466`；`factory.rs:59-72,117` | `node_type_register`=0、`register_node`=0；只有只读 `quercus_effect_factory_dynamic_*` | 动态节点只能由 OFX 扫描间接注册（`ofx/host.rs:280-282`） |
| G6 | 文本后端钩子 `set_text_backends` | `crates/quercus-node/src/nodes/textbackend.rs:132-136,145-157` | `textbackend`=0 | 类型是 **Rust `fn(&TextLayoutRequest)` 而非 `extern "C"`**（`:132`、`:136`）→ 跨 ABI 还需 trampoline；槽位是 `Mutex`（可反复替换），与"只能装内建"的公开面矛盾 |
| G7 | `UiProgressReporter` trait | `crates/quercus-ofx-plugin/src/progress.rs:39` | 无独立符号；仅 `quercus_ofx_set_progress_handler` 桥接 update/end 语义 | 部分缝隙：trait 本身不可实现，但可观察行为可表达（`ofx/host.rs:351-380`） |
| G8 | `ConfigStore::set_error_handler` | `crates/quercus-core/src/configstore.rs:506-517` | `config_set_error_handler`=0 | 诊断通道在 ABI 上完全不可达；SDK 只能拿到错误字符串（`README.md:118-120`） |
| G9 | `Command` trait + `quercus_undo_push_owned` | `crates/quercus-undo/src/undocommand.rs:37`；engine `undo.rs:33-47` | 该导出**不在 `H`**（SDK 未声明） | 参数是 `*mut c_void`，实际必须是 `Box<UndoCommand>`（Rust trait object）→ C/SDK 调用方无法构造，属"存在但不可用"的缝隙 |
| G10 | `TaskBehavior` / `TaskBox` / `RenderTaskBehavior` | `crates/quercus-task/src/task.rs:71`；`manager.rs:57`；`render.rs:124` | `task_observer`=0、`task_callback`=0 | 任务种类固定（export/preview/thumbnail/waveform/otio），只能轮询 |
| G11 | `RenderHooks`（图渲染/求值钩子） | `crates/quercus-node/src/traverser.rs:50`；`crates/quercus-render/src/lib.rs:12` | 无对应符号 | 仅由 OFX executor 内部安装 |
| G12 | `GpuContextLike` | `crates/quercus-core/src/backend.rs:262` | 无 | `corestate.rs:7-9` 明确说明 `GpuContext` 故意不导出（wgpu 对象 C 端无法构造） |

---

## 6. ABI 设计约束（本报告后续建议必须遵守）

1. **不得跨 dylib 传 Rust ABI**。`Arc<dyn Trait>`、`&str`、泛型、闭包都不能出现在导出签名中；SDK 现状即证据：所有 API 都是 `(ptr,len)`、`repr(C)` 结构、`u64` 句柄（`crates/quercus-engine-rs/README.md:7-11`、`lib.rs:1-6`）。
2. **注入必须是 (vtable 或 fn ptr, userdata) 对**。现状只有单回调 + userdata（plugin / progress / observer），没有真正的多方法 vtable；唯一 C 形状的"自带函数指针的结构"是 `StorageProjectHandle`（`crates/quercus-engine-rs/src/editor.rs:14-23`，字段 `ctx/addref/release/abi_version`，`H:561-563`）。
3. **结构缺少 size/version 字段**。全部 48 个 POD struct 里只有 `StorageProjectHandle` 有 `abi_version`（`H:563`）；所有注册入口都是裸 `(fn, userdata)`，**无法在不破坏 ABI 的前提下给回调增参**。新增 vtable 必须带 `struct_size` + `abi_version` 且由调用方填写。
4. **panic 不得穿过 C 边界**。Rust 1.85 / edition 2024 下 `extern "C"` 函数逃逸 panic 会 abort 进程（推断，依据 `rust-version = "1.85"` `Cargo.toml:11` 与 `panic = "unwind"` `Cargo.toml:14`）。因此每个回调调用点都必须 `catch_unwind(AssertUnwindSafe(...))`；现存范式见 `crates/quercus-engine/src/editor/notify.rs:92`。
5. **回调调用不得持锁**。正确范式：快照 + 释放锁（`notify.rs:86-95`、`plugin.rs:239-252`）；反例：`undo.rs:82-87`。
6. **所有权/引用计数规则**：`add_ref/release` 成对（`handle.rs:59 export_handle!`）；task 结果只能 `take` 一次（`README.md:91-92`）；userdata 的生命周期必须由 `drop_callback` 或"注册即永久"两者之一明确声明——现状两种并存且未标注（对比 `notify.rs:27-33` 有析构 vs `undo.rs:73-77` 无析构）。
7. **符号版本化缺席**：`H` 中没有任何 `quercus_engine_version`/soname 版本符号（`grep version H` 仅命中 `quercus_ofx_gl_version`、`quercus_ofx_plugin_version_at`，都是 OFX 插件元数据）。演化只能靠"新增符号 + 不改旧签名"。
8. **docs.rs 构建约束**：`quercus-engine` 用 `docs-only` 空 crate（`crates/quercus-engine/Cargo.toml:17-18,29`），SDK 用 `--no-default-features`（`crates/quercus-engine-rs/Cargo.toml:15-19`）。新增缝隙的实现必须放在 `cfg(not(feature = "docs-only"))` 门控之下（引擎 `src/lib.rs:17-72` 每个 mod 都这样门控），且 SDK 在关闭默认 feature 时仍须能编译出文档。
9. **生成器约束**：新回调需要先有 `type X = unsafe extern "C" fn(...)`（生成器 `gen_c_header.py:268-282` 只从类型别名生成 typedef），且 SDK 侧必须真正 `extern` 声明，否则 `H` 不会出现该符号。

---

## 7. 发布策略构成的阻塞（Rust 调用方视角）

- README 明确：除 `quercus-engine` 与 `quercus-engine-rs` 外全部是内部实现细节，"Do not depend on them directly; their APIs are not stable"（`README.md:26-29`）。
- 但内部 crate 仍然发布（`README.md:79-80`、`crates/quercus-engine-rs/README.md:183-188`），所以"实现 trait"在技术上可行——代价是依赖一个 **pinned 且明确不承诺稳定**的 crate：`quercus-storage = { version = "=0.5.2", ... }`（`crates/quercus-engine/Cargo.toml:35` 起，`quercus-engine-rs/Cargo.toml:35` 用 `=0.5.3` 钉引擎）。
- 结论：**这不是"没有路"，而是"路通向不稳定 ABI"**。一旦第三方实现了 `StorageBackend`/`Decoder`，引擎任何 0.5.x 内部重构都会破坏它，且没有编译期的 ABI 契约（trait 方法签名直接在源码层耦合）。若未来开放，必须先把 trait 降维成 C vtable（`struct_size`/`abi_version` + `extern "C"` 方法 + userdata），再由 `quercus-engine` 适配到内部 trait。

---

## 8. 显式结论：纯 Rust SDK 消费者今天能否实现任何内部 trait？

**不能。** 依据：

1. SDK 只链接 C 符号，不依赖任何内部 crate（`crates/quercus-engine-rs/README.md:7-11`；`Cargo.toml:29-35` 中唯一依赖是 `thiserror` 与可选 `quercus-engine`）。
2. SDK 的公开 API 中没有任何"接收 trait 实现"的入口：没有泛型 trait 参数、没有 `impl Trait` 注入点；所有可注入项都是闭包 → C 函数指针（`plugin.rs:155-180`、`ofx.rs:493-513`、`codec.rs:681-687`、`notify.rs:79-99`、`node.rs:1290-1304`）。
3. 公开 ABI 里没有任何以 trait 命名的符号。内部 crate 共 17 个 `pub trait`（不计 `quercus-engine` 自身的 `NativeMirror`/`ArcHandle`/`IntoFfiVec` 三个 FFI 辅助 trait）：其中只有 `UiProgressReporter`（G7）的行为被部分桥接；其余 16 个在 `H` 中均无注册/构造入口——第 5 节逐条 grep 反证了最相关的 11 个（`StorageBackend`、`Decoder`、`Encoder`、`JobDispatch`、`NodeBehavior`、`Command`、`TaskBehavior`、`TaskBox`、`RenderTaskBehavior`、`RenderHooks`、`GpuContextLike`），余下 `XmlRead`、`XmlWrite`、`BlockRange`、`TrackRange`、`AutoCacheEvents` 属内部序列化/区间/缓存抽象，同样无公开入口。
4. 唯一"实现内部 trait"的可行路径是直接 `cargo add quercus-storage/quercus-codec` 并接受不稳定 API——这违反 `README.md:26-29` 的公开承诺，不属于"SDK 消费者"路径。

---

## 9. 不确定项 / 未覆盖

- 第 2 节的分类计数是**机械符号前缀统计**，用于说明规模，不代表语义边界（例如 `quercus_core_*` 同时含 config、color、displayicc、cancelatom）。
- 未逐条阅读 91 个 `quercus_ofx_*` 与 89 个 `quercus_codec_*` 的实现；关于"它们内部是否还有隐藏的注册入口"的否定结论仅基于 header 符号 grep（`*_register`、`*_create` 等前缀），已逐项列出，属可复核证据。
- "panic 逃逸 `extern "C"` 会 abort" 是基于 rustc 版本与 edition 的推断（`Cargo.toml:11`、`crates/quercus-engine/Cargo.toml:3`），未通过运行验证（禁止 build/test）。
- `quercus_undo_push_owned` 的业务用途（"migration command"）未深挖；本报告只断言其 ABI 形态不可被 C/SDK 使用。
- 未审计 `worker/` 的进程协议细节（属另一份报告范围）。
