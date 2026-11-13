# 存储可替换性审计：`quercus-storage` → C ABI → SDK

> 审计对象：`/home/sunyu/Projects/quercus-engine`（workspace 0.5.2），只读静态审计。
> 未运行 `cargo build/test`（FFmpeg/OCIO 工具链不在本机），所有结论来自源码、头文件与测试代码的逐行阅读。
> 标注【推断】的条目是静态推理，未运行时验证。

## 0. 结论（TL;DR）

**今天，一个 Oak 型消费者无法接入自己的 Postgres / HTTP / 远程存储后端。** 只有两条“事实上的”替换途径：

1. 用内置的 `quercusdb+sqlite` / `quercusdb+pg`（`Storage/Backend=sqlite|pg` + `Storage/PgUrl`），schema 是引擎自己的；
2. 在进程内以 **Rust** 方式直接依赖内部 crate `quercus-storage` 并调用 `Registry::global().register(...)`——但这违反该 crate 的 `do not depend directly` 契约（`crates/quercus-storage/src/lib.rs:1-5`），且无法影响已经链接进 cdylib 的引擎实例（见 §4.3，【推断】）。

README 声称的 C 侧注册符号 `quercusstorage_backend_register` **在全仓库中只出现在 README 文本里，不存在任何实现或声明**（`crates/quercus-storage/README.md:31`）。这是本次审计最明确的“文档/实现漂移”结论。

替换缝隙被三处硬编码切断：写穿层 `writethrough::backend()` 是 `OnceLock<DatabaseBackend>`（`crates/quercus-storage/src/writethrough.rs:118-121`）、15 个项目库 C 导出直接调用它（`crates/quercus-engine/src/storage.rs:225` 等）、编辑器的 library 打开/创建/绑定同样直连它（`crates/quercus-engine/src/editor.rs:240-243,335,414`）。

## 1. 审计范围与判定口径

- 现状取值：
  - **可替换**：存在被公开 C ABI/SDK 覆盖的注册/注入点，外部实现能真正被调用；
  - **半可替换**：内部存在 trait/registry 缝隙（`Registry::register`、`StorageBackend`），但外部 C/SDK 消费者不可达，或只覆盖部分调用路径；
  - **硬编码**：没有任何缝隙，类型/单例在编译期写死。
- 证据均给 `file:line`。头文件指 `crates/quercus-engine/include/quercus_engine.h`（由 `tooling/gen_c_header.py` 从 SDK 的 `extern "C"` 块生成，见 `tooling/gen_c_header.py:1-20`）。
- 能力分层对应任务要求：(a) 今天可用；(b) trait/registry 存在但公开 ABI/SDK 不可达；(c) 完全写死。

## 2. 能力现状总表

| 能力 | 现状 | 证据（file:line） | 调用者为何想替换 | 备注 |
|---|---|---|---|---|
| `file://` 项目 `open/save`（`.ove` / `.otio` / `.fcpxml`） | 半可替换 (b) | `crates/quercus-engine/src/storage.rs:43,91,130`；`crates/quercus-storage/src/registry.rs:87-94`；`crates/quercus-storage/src/backends/mod.rs:31-36` | 让“打开/保存”落到 Postgres/HTTP/S3，而不是本地文件 | 唯一真正走 registry 的用户路径；但注册入口不在 ABI 上 |
| 新 scheme 后端注册 | 硬编码 (c) | 全仓 `no_mangle` 中无 storage 注册符号；storage C ABI 共 24 个符号（header `1244-1267`），无 register/unregister | 插件式后端、Oak 自有存储服务 | 见 §5 |
| `quercusstorage_backend_register` C vtable | 不存在 (c) | 全仓 grep 仅命中 `crates/quercus-storage/README.md:31` | README 宣称的“foreign backends”接入点 | 文档/实现漂移 P0 |
| 写穿（undo 成功后自动落库） | 硬编码 (c) | `crates/quercus-storage/src/writethrough.rs:118-121`（`OnceLock<DatabaseBackend>`）、`:276-286`（`note_command`）、`:305`（`backend().save_project`） | 每次编辑直接写自有服务/对象存储 | 完全绕过 registry |
| 快照线程（周期快照 + 退出 flush） | 硬编码 (c) | `writethrough.rs:432-446`（线程）、`:451-459`（`Storage/SnapshotIntervalSec`）、`:474`、`:551`、`:504-528`（`flush_all`） | 云端版本/快照策略 | 只认 `DatabaseBackend::snapshot` |
| 项目库 15 个 `quercus_storage_library_*` 能力 | 硬编码 (c) | `crates/quercus-engine/src/storage.rs:225,304,331,358-374,428,452,476,502`；header `1250-1264` | 自建库、多租户、远程 catalog | `list/delete/rename/duplicate/import/export/stats` 都是 `DatabaseBackend` 的固有方法，不在 trait 上 |
| 编辑器 `quercus_editor_open` | 半可替换 (b) | `crates/quercus-engine/src/editor.rs:197` → `storage.rs:80` → `registry.rs:87` | 远程打开项目 | 仅内置 scheme 可达 |
| 编辑器 `quercus_editor_save` | 半可替换 (b) | `editor.rs:374` → `storage.rs:121` → registry | 远程保存项目 | save-as 语义同 open |
| 编辑器 `open_project_from_library` / `create_library` | 硬编码 (c) | `editor.rs:240-243`（直接 `writethrough::backend().load`）、`:335`（`save_project`） | 从自有库打开/新建 | 即便注册了自定义后端也不会被调用 |
| 编辑器 bind / autosave / `storage_bound` / `storage_last_error` | 硬编码 (c) | `editor.rs:414`（`writethrough::bind_project`）、`:442`、`:485`；`writethrough.rs:171-217` | 把编辑会话绑到自有库 | 绑定表只存 `quercusdb+…` URI |
| SDK `ProjectLibrary` | 硬编码 (c) | `crates/quercus-engine-rs/src/storage.rs:304` 硬编码 `backend_for_uri(uri) != Some("quercusdb")` 即拒绝；`:394` 只允许默认库 | 用 SDK 直接接自有库 | 即使引擎支持自定义后端，SDK 也先拒绝 |
| 配置选择后端 | 半可替换 (b) | `writethrough.rs:338-344` 白名单 `{"sqlite","database","pg"}`；`:392-403`（`library_uri`） | 用配置指向自有后端 | 写入其它值静默退化为“不持久化” |
| 版本/格式协商 | 半可替换 (b) | `crates/quercus-storage/src/backend.rs:24-37`（`version_info` 由 backend 判断）；`backends/ove_xml.rs:137-155`；`backends/database/mod.rs:314`（`schema_ver`） | 远程/异构格式的兼容性判定 | 引擎不校验 backend 的判定 |
| 错误上下文跨 ABI | 半可替换 (b) | `crates/quercus-storage/src/error.rs:80-91`（`Failed(String)` 只剩 `-100004`）；`crates/quercus-engine/src/storage.rs:88-90`（resolve 失败不写 `version_info`）、`:93-98`（把负错误码写进 `version_info`） | 远程失败需要 HTTP 状态/可读原因 | SDK 侧 `storage.rs:144-153` 会把 0 当错误码返回 |
| 项目库 import/export 交换 | 半可替换/硬编码 | `backends/database/mod.rs:611-623`（export 固定 `OveXmlBackend`，`:621`）、`:630-634`（import 要求 `file://`）；`editor_commands/media.rs:445-446` 注释却称经 registry 按扩展名分派 | 从/向远程对象存储导入导出 | 注释与实现不符；自定义后端无法当 import/export 端点 |
| 线程/生命周期契约（给外部实现） | 硬编码假设 (c) | `backend.rs:67`（`Send + Sync`）；`writethrough.rs:422`（`quercus-storage-snapshot` 线程） | 远程后端需明确并发/可重入规则 | 现有 trait 没有任何 C 侧契约文档 |

## 3. 调用路径追踪

```
C host / SDK
├─ quercus_storage_open            engine/src/storage.rs:80
│    └─ resolve() :29 → Registry::global().resolve :43  ← 唯一对外 seam
│         └─ StorageBackend::load (backend.rs:81)
│              └─ 内置 ove-xml / otio / quercusdb
├─ quercus_storage_save            storage.rs:121 → resolve() → StorageBackend::save (backend.rs:85)
│
├─ 编辑器
│  ├─ quercus_editor_open          editor.rs:197  → quercus_storage_open（走 registry）
│  ├─ quercus_editor_save          editor.rs:374  → quercus_storage_save（走 registry）
│  ├─ quercus_editor_open_project_from_library  editor.rs:240-243
│  │     └─ writethrough::backend().load          ← 硬编码
│  ├─ quercus_editor_create_library             editor.rs:335
│  │     └─ writethrough::backend().save_project  ← 硬编码
│  └─ quercus_editor_bind_project_to_library    editor.rs:414
│        └─ writethrough::bind_project            ← 硬编码
│
├─ 项目库（15 个导出）
│    storage.rs:225,304,331,358,373,428,452,476,502
│      └─ writethrough::backend()                  ← OnceLock<DatabaseBackend>
│
└─ 写穿 / 自动保存
     quercus_undo::global::add_observer(note_command)   writethrough.rs:152-157
       └─ note_command :276 → write_through :290
            └─ backend().save_project :305             ← 硬编码
     快照线程 :432 → snapshot_dirty :464
            └─ backend().snapshot :474                 ← 硬编码
     退出：quercus_storage_flush (storage.rs:510) → flush_all (writethrough.rs:504)
            └─ flush_one :532 → write_through + backend().snapshot :551
```

可见：**项目文件的 open/save 有 registry 缝隙；项目库 + 写穿 + 快照 + 编辑器绑定全部直连 `DatabaseBackend` 单例，没有缝隙。**

## 4. 三个关键问题的直接回答

### 4.1 “可替换”今天到底指什么

- `Registry::register` / `unregister` 是 `pub`（`registry.rs:55-74`），但仅在 **crate 内部 + 集成测试** 中被调用：生产代码 grep 不到任何 `Registry::register` 调用；唯一调用点是 `crates/quercus-storage/tests/storage_test.rs:576`（以及同文件 `:608,616` 的注销）。`builtins()` 只在 `Registry::global()` 初始化时调用一次（`registry.rs:45`）。
- 因此该 crate 的“可插拔证明”（`tests/storage_test.rs` 的 `backend_pluggability`）是**进程内 Rust 直连内部 crate** 的证明，不是 C/SDK 可用的证明。测试注释本身也写明 “The former `ffi.rs`/`bridge::node.rs` C ABI is gone; save/load now go through `Registry::global().resolve(uri)`”（`tests/storage_test.rs:19-20`）。

### 4.2 注册符号清单

- `quercus-engine` cdylib 中 **没有任何 storage 注册/注销导出**。`crates/quercus-engine/src/storage.rs` 的全部导出为 24 个（header `1244-1267`），逐项核对无 `register` / `unregister` / `set_backend` / `vtable`。
- 全库 `#[no_mangle]` 中带 register 含义的只有其它子系统：`quercus_plugin_register/unregister`（`crates/quercus-engine/src/plugin.rs:68,104`）、`quercus_ofx_register_plugin_nodes`（`crates/quercus-engine/src/ofx/host.rs:280`）、`quercus_codec_task_set_submit_cb`（`crates/quercus-engine/src/codec/task.rs:61`）。它们是**存储注册缺失时最好的实现先例**（见 §7）。
- `quercus-storage` 自身也没有 `ffi.rs`：`src/` 实际文件只有 backend/backends/error/handle/lib/nodeutil/registry/session/uri/writethrough；`backend.rs:17-19` 与 `registry.rs:20-22` 的模块注释仍宣称存在 `ffi.rs`，README 的 Layout 也列出 `ffi.rs` 与 `bridge/node.rs`（`crates/quercus-storage/README.md:72-75`），全部已不存在（`nodeutil.rs:18` 自己写明是 “the former `bridge/node.rs`”）。

### 4.3 Rust 消费者能否绕过 ABI 直接注册

- SDK `quercus-engine-rs` **不依赖** `quercus-storage`（`crates/quercus-engine-rs/Cargo.toml` 仅依赖 `thiserror` + 可选 `quercus-engine`），只声明 C 符号（`crates/quercus-engine-rs/src/storage.rs:18-105`）。故 SDK 用户拿不到 `Registry`。
- 若用户自行 `quercus-storage = "=0.5.2"` 并调用 `register`：`quercus-storage` 同时是 `rlib` 与 `staticlib`（`crates/quercus-storage/Cargo.toml:8-9`），而 `quercus-engine` 以 cdylib 形式把同一个 rlib **静态链接进 `.so`**（`crates/quercus-engine/Cargo.toml:22` 与 `:43`）。消费者进程里会出现**两份** `Registry`（各自一个 `OnceLock`），消费者注册的是自己那份，引擎那份看不到；且 Rust mangled 符号不是 cdylib 导出面，无法跨库调用。【推断：基于 Cargo crate-type 与 Rust cdylib 导出规则的静态推理，未实际链接验证。】

## 5. `quercusstorage_backend_register`：不存在（P0 文档/实现漂移）

- 声称位置：`crates/quercus-storage/README.md:29-33`（“The public C ABI vtable (`quercusstorage_backend_register`) accepts foreign (C-side) backends — the database-swap interface proof”）。
- 实际：全仓对 `quercusstorage_backend_register` / `backend_register` / `register_backend` 的搜索只有 README 这一处；`backend.rs` 中并无 vtable 类型，只有 Rust trait（`backend.rs:66-91`）；头文件中没有 `Backend`/`vtable` 结构（只有 `quercus_storage_backend_count/name_at/scheme_at/for_uri` 四个**只读枚举**符号，header `1244-1247`）。
- 同一 README 还把 database 后端描述为 “declared stub … not registered, `todo!()` bodies”（`README.md:17-21`），而实现是 1654 行的 SeaORM 后端（`backends/database/mod.rs`）且已注册进 `builtins()`（`backends/mod.rs:31-36`，`database/mod.rs:787-816`）。README 整体落后于实现。

## 6. 现有 Rust trait 为什么不能当 C vtable（ABI 细节）

1. **trait 形状本身不是 FFI-safe**：`StorageBackend` 用 `&self`、`fn name(&self) -> &str`、`fn uri_scheme(&self) -> &str`、`&StorageUri`（含两个 `String` 的 Rust struct，`uri.rs:29-36`）、`Result<LoadResult>`（Rust enum，`error.rs:50-76`）、`Arc<dyn StorageBackend>` 存于 registry（`registry.rs:32`）。胖指针 + Rust 枚举布局 + trait object + `Arc` 全部无法跨 `extern "C"` 稳定表达。
2. **`CHandle` 结构本身可跨语言，但语义不可**：`CHandle` 是 `#[repr(C)] {ctx, addref, release, abi_version}`（`crates/quercus-core/src/handle.rs:39-50`），与 C 头里的 `StorageProjectHandle` 完全一致（header `559-564`，并有 `sizeof == 32` 静态断言 `650`），所以**结构形状对 C 可用**；但 `ctx` 必须指向引擎分配的 `RefBox<Arc<Mutex<Project>>>`（`crates/quercus-node/src/handle.rs:46-56,98-109`），而 `nodeutil::project_arc` 用**不做类型校验**的 `get`（`nodeutil.rs:51-55` → `quercus-node/src/handle.rs:136-141`）。外部后端若返回自己构造的 `CHandle`，引擎会按 `RefBox<ProjectArc>` 布局解引用 → 未定义行为【推断：内存安全结论来自上述 unchecked cast 代码】。可用的合法构造入口只有 `quercus_project_into_storage_handle`（`crates/quercus-engine/src/node.rs:53`，它要求先有一个引擎 `ProjectHandle`）。
3. **lifetime/所有权只靠注释**：`save` 的 `CHandle` 是借用（引擎在 backend 返回后立即 release，`storage.rs:134-136`），`load` 返回的是 refcount 1 的拥有句柄（`backend.rs:29-37`，引擎在 `storage.rs:109-110` 转换后 release）。Rust 侧靠 `CHandle: Copy` + 手工 release 约定；C vtable 必须显式写清“const 借用 vs out 参数 + 释放责任”。
4. **C 侧无法从 `CHandle` 取到项目字节**：头文件没有“project → XML bytes”导出；`quercus_project_save` 只接受 `*mut ProjectHandle` 且写**文件路径**（`node.rs:2444-2483`），而 `CHandle → ProjectHandle` 的唯一桥是 `quercus_editor_adopt_storage_project_handle`（`editor.rs:283`）。理论上外部后端可以“adopt → `quercus_editor_project` → 写临时文件 → 读取”，但这依赖文件系统、语义破坏、且完全未文档化【推断：可行但不可接受的逃生通道】。

**结论：现有 `StorageBackend` 只能作为“进程内 Rust 扩展示例”，不能直接作为对外 ABI。跨 C 边界必须走 `repr(C)` vtable + userdata，并优选“字节流”契约而非“句柄”契约。**

## 7. 缺口清单（按优先级）

### P0（不补就无法谈“替换存储”）

- **P0-1 C 注册符号缺失。** 现有 24 个 storage 导出全是使用面，没有注册面（header `1244-1267`）。
  - 最小新增：`int32_t quercus_storage_register_backend(const QuercusStorageBackendVTable*)` + `bool quercus_storage_unregister_backend(int32_t slot)`；实现放在 `crates/quercus-engine/src/storage.rs`，内部调 `Registry::global().register`（`registry.rs:55`）；SDK `extern "C"` 块同步声明（`crates/quercus-engine-rs/src/storage.rs:18`），否则 `tooling/gen_c_header.py` 不会生成头文件。
- **P0-2 写穿/项目库的硬编码单例。** `writethrough::backend()`（`writethrough.rs:118-121`）被 3 处编辑器函数（`editor.rs:241,335`、`editor_commands/media.rs:463`）与 15 个 library 导出直接调用。
  - 最小新增：把 `backend()` 改为 `library_backend(uri) -> Result<LibraryBackend>`，先 `Registry::global().resolve(uri)`，未命中再回退 `DatabaseBackend`；同时把 `storage_enabled()`/`library_uri()` 的白名单（`writethrough.rs:338-344,392-403`）改为“配置 URI 可解析即启用”。
- **P0-3 CHandle 语义不可跨 C。** `load` 返回拥有句柄、`save` 收 borrowed 句柄（`backend.rs:81-90`）。
  - 最小新增：vtable 的 `load` 输出**项目字节**（引擎 XML）、`save` 输入**项目字节**；引擎在 `quercus_storage_open/save` 内做 `serializer_load/save`（`nodeutil.rs:71-78`），`CHandle` 只留在引擎内部。这样外部后端不能伪造项目句柄，格式/版本由引擎掌握。

### P1（决定“好不好用、错得可不可诊断”）

- **P1-1 错误通道有洞。** `quercus_storage_open` 在 `resolve` 失败时返回 null 而 `version_info` 保持 0（`storage.rs:88-90`）；load 失败却把**负**错误码写进 `version_info`（`:93-98`），与“正数 info code”契约冲突；SDK 因此可能返回 `StorageError(0)`（`crates/quercus-engine-rs/src/storage.rs:144-153`）。`Failed(String)` 的上下文在 `code()` 处丢失（`error.rs:80-91`）。
  - 最小新增：`open/save` 增加 `QuercusStorageError* err`（`{i32 code; char message[512];}`），并新增 `quercus_storage_last_error()`。
- **P1-2 SDK 名字硬编码。** `ProjectLibrary::new` 要求 `backend_for_uri == Some("quercusdb")`（`crates/quercus-engine-rs/src/storage.rs:304`），`ProjectLibrary::open` 还要求等于默认库（`:394`）。
  - 最小新增：改为能力查询 `quercus_storage_library_supported(uri)`，或直接信任 registry 解析结果。
- **P1-3 库操作不在 trait 上。** `list_projects/delete_project/rename_project/duplicate_project/import_from_file/export_to_file/project_stats/snapshot/load_at` 都是 `DatabaseBackend` 固有方法（`backends/database/mod.rs:424,450,472,584,611,630,666,693,715`）。
  - 最小新增：`StorageBackend` 增加可选能力（返回 `E_NOT_SUPPORTED` 默认实现）：`library_list`、`library_delete/rename/duplicate`、`library_stats`、`snapshot`；C vtable 用可空函数指针表达。
- **P1-4 export 说一套做一套。** `library_export` 注释称“按扩展名经 registry 分派”（`editor_commands/media.rs:445-446`），实际固定 `OveXmlBackend`（`backends/database/mod.rs:611-623`，`:621`）；`import_from_file` 只接受 `file://`（`:630-634`，`:631`）。
  - 最小新增：export 先 `serializer_save` 成字节，再 `Registry::resolve(file_uri)` 交给后端写；import 去掉 `scheme == "file"` 限制。

### P2（规模化/体验）

- **P2-1 无 async/progress/cancel/auth 回调。** trait 全同步、无回调（README 第 5 条设计，`crates/quercus-storage/README.md:47-48`）；远程后端只能阻塞。最小新增：vtable v2 可选 `progress`/`cancel` 槽位 + `load/save` 的 `int32_t* cancel_flag`。
- **P2-2 仲裁策略不完整。** `resolve` 取第一个 `can_handle`（`registry.rs:87-94`），`register` 只拒绝重名、不拒绝同 scheme（`registry.rs:55-62`）；内置后端永远先注册（`registry.rs:39-50`），外部后端无法覆盖 `file://` 的 `.ove`，但可以抢注新 scheme。最小新增：vtable 增加 `priority`，注册时报告冲突；`unregister` 禁止移除内置。
- **P2-3 文档/测试与实现不一致。** `README.md:17-21,29-33,72-75`；`backend.rs:17-19`；`registry.rs:20-22`；被反复引用的 `docs/zh/plans/riir/M10-quercusstorage.md` 在 `docs/` 下不存在（`docs/` 仅有 `build.md`）。最小新增：修正文档，或把 P0-1 真正实现掉；加一个 `grep` 校验防止 README 再声称不存在的符号。

## 8. C 兼容 vtable 设计草案（sketch，不是最终代码）

设计原则：**后端只负责“把一段字节存到某个 URI / 从某个 URI 取回一段字节”**，项目图的反序列化、版本判定、schema 升级、错误码映射全部留在引擎。这样既避开 §6 的句柄/内存布局问题，又能让引擎强制 schema/version。格式型后端（OTIO 等）继续作为编译期内置，不开放给 C。

### 8.1 数据结构（`repr(C)`，放进 SDK 的 `extern "C"` 块以生成头文件）

```c
#define QUERCUS_STORAGE_VTABLE_VERSION 1

typedef struct QuercusStorageError {
  int32_t code;                 /* QUERCUSSTORAGE_OK / QUERCUSSTORAGE_E_*，0 表示成功 */
  uint8_t message[512];         /* UTF-8，NUL 结尾，best-effort 诊断 */
} QuercusStorageError;

typedef struct QuercusStorageBackendVTable {
  uint32_t struct_size;         /* sizeof(本结构)，用于未来加字段的尺寸协商 */
  uint32_t abi_version;         /* QUERCUS_STORAGE_VTABLE_VERSION */
  const char* name;             /* 静态字符串；进程生命周期内有效；不得与内置重名 */
  const char* scheme;           /* 静态字符串，如 "s3" / "pg+acme" */

  /* 1 = 认领；0 = 不认领；<0 = 错误。uri 只在调用期有效。 */
  int32_t (*can_handle)(void* userdata, const uint8_t* uri, size_t uri_len);

  /* 成功时把项目字节的所有权交给引擎：引擎在读完后回调 free_bytes 恰好一次。 */
  int32_t (*load)(void* userdata,
                  const uint8_t* uri, size_t uri_len,
                  uint8_t** out_bytes, size_t* out_len,
                  int32_t* out_version_info,     /* 可空；TOO_OLD/TOO_NEW/UNKNOWN_VERSION */
                  QuercusStorageError* err);
  void    (*free_bytes)(void* userdata, uint8_t* bytes, size_t len);

  /* 引擎已把项目序列化为字节；options 为 QUERCUSSTORAGE_SAVE_* 位掩码。 */
  int32_t (*save)(void* userdata,
                  const uint8_t* uri, size_t uri_len,
                  const uint8_t* bytes, size_t len, uint32_t options,
                  QuercusStorageError* err);

  /* ---- 可选（NULL = 该后端不支持库能力）；用于替换 15 个 library_* ---- */
  int32_t (*library_list)(void* userdata, const uint8_t* uri, size_t uri_len,
                          QuercusStorageProjectInfo* out, size_t cap, size_t* count,
                          QuercusStorageError* err);
  int32_t (*library_delete)(void* userdata, const uint8_t* uri, size_t uri_len,
                            const uint8_t* uuid, size_t uuid_len, QuercusStorageError* err);
  int32_t (*library_rename)(/* … uuid + new_name … */);
  int32_t (*library_duplicate)(/* … uuid → new_uuid out … */);
  int32_t (*library_stats)(/* … uuid → QuercusStorageProjectStats* … */);
  int32_t (*library_snapshot)(/* … uri + uuid … */);
} QuercusStorageBackendVTable;

int32_t quercus_storage_register_backend(const QuercusStorageBackendVTable* vt);
bool    quercus_storage_unregister_backend(int32_t slot);
```

### 8.2 注册与生命周期规则（必须在头文件注释里写死）

- **注册时机**：`register` 只复制 vtable 结构体（调用后栈上 vtable 可失效），但 `name`/`scheme` 指针必须静态有效；`userdata` 由调用者拥有，必须活到 `unregister` 返回之后。
- **一次性 vs 会话级**：进程级注册（与 `quercus_codec_task_set_submit_cb` 的既有先例一致，`crates/quercus-engine/src/codec/task.rs:56-64`）；重复注册同名返回 `E_STATE`（对齐 `registry.rs:55-62`），SDK 可用“会话级 handle”包装成 drop 时 unregister。
- **线程规则**：`load/save` 可能来自调用者线程，也可能来自 `quercus-storage-snapshot` 后台线程（`writethrough.rs:411-446`），因此 `userdata` 必须线程安全（等价于 `Send + Sync`）；回调内**禁止**再进入 storage 注册/解析 API（避免重入死锁）。
- **内存规则**：跨 DLL 分配的内存不可互 free（尤其 Windows），所以用 `free_bytes` 回调而不是让引擎 `free()`；或约定“引擎提供 allocator 函数指针”。项目字节 UTF-8 XML，引擎负责 schema 校验。
- **错误规则**：返回码统一用现有 `QUERCUSSTORAGE_E_*`（`error.rs:22-44`）；`err.message` 必须 NUL 结尾；引擎把 message 复制进自己的错误上下文（补 P1-1）。

### 8.3 与内置写穿 DB 的关系

- 默认配置（`Storage/Backend` 缺省）保持“无库、不绑定”的现状（`writethrough.rs:338-344`）。
- 若配置到已注册的自定义后端：`bind_project` 的 URI 来自 `library_uri()` 的新分支（读取 `Storage/LibraryUri`，不再拼 `quercusdb+…`），写穿/快照/退出 flush 全部走 registry 解析 → vtable 的 `save`/`library_snapshot`。
- 若自定义后端未实现 `library_*`：`quercus_storage_library_*` 返回 `E_NO_BACKEND`/`E_STATE`，而不是今天这样落到 `DatabaseBackend` 上产生“看似成功”的错库写入。
- 内置 `DatabaseBackend` 仍作为默认注册项保留（`builtins()` 追加），保证零配置行为不变。

### 8.4 SDK 的安全封装（Rust 闭包适配 vtable）

先例：`crates/quercus-engine-rs/src/codec.rs:679-706`（`set_task_submit_cb` 用 `trampoline::<F>` + `Box::into_raw` 把 Rust 闭包放进 userdata，注册为进程级并声明“重复注册会泄漏”）。存储侧照搬：

```rust
pub trait StorageBackend: Send + Sync + 'static {
    fn name(&self) -> &str;                    // SDK 内部转成 CString 存住
    fn scheme(&self) -> &str;
    fn can_handle(&self, uri: &str) -> bool;
    fn load(&self, uri: &str) -> Result<(Vec<u8>, i32), StorageError>;
    fn save(&self, uri: &str, bytes: &[u8], options: u32) -> Result<(), StorageError>;
    // 可选：library_list/delete/... 默认 Unsupported
}

pub struct BackendRegistration { slot: i32 }   // Drop 时 unregister
pub fn register_backend<B: StorageBackend>(b: B) -> Result<BackendRegistration, StorageError>;
```

- `register_backend` 把 `Box<B>` 泄漏成 `userdata`（与 codec 的进程级约定一致），`name/scheme` 用 `Box::leak(CString)`；`trampoline` 里把 `*const u8 + len` 转 `&str`、把 `Vec<u8>` 通过 `free_bytes` 释放；错误从 `QuercusStorageError` 反序列化成 `StorageError`。
- SDK 的 `ProjectLibrary` 改为 `ProjectLibrary::new(uri)` 只做 `uri_is_valid` + `backend_supports_library(uri)`，删掉 `"quercusdb"` 名字检查（`crates/quercus-engine-rs/src/storage.rs:304`）。

## 9. 放开替换的主要风险

1. **仲裁冲突/劫持**：`resolve` 首个 `can_handle` 胜出（`registry.rs:87-94`），且只按注册顺序；内置后端先注册，因此外部后端拿不到 `.ove`/`.otio`，但对内置未认领的 URI 有完全控制权，且可注册与内置同 scheme 的第二实现（`registry.rs:55-62` 只查重名）。建议：注册时检查 scheme 冲突 + 显式 `priority`，并对内置 scheme 做保留（P2-2）。
2. **保存格式漂移**：现状 backend 自己决定 framing/版本（`backend.rs:24-37`，`ove_xml.rs:137-155`）；若 vtable 让 backend 自判版本，不同后端可能把同一 project 写成互不兼容格式。字节化 vtable（§8）把序列化收回引擎，格式型后端只能编译期内置，可消除该风险。
3. **写穿假设被打破**：`DatabaseBackend` 用 `op_lock` 单写者 + 单连接池（`backends/database/mod.rs:323-338,835-851`），写穿/快照/编辑器保存都可能并发调用；外部后端必须自己保证线程安全与原子写（写临时对象再 rename/commit），否则 autosave 会放大竞态。且写穿失败被吞掉（`writethrough.rs:314,491`），只能通过 `last_error` 看到（`:252-265`）——云端后端需要更醒目的失败上报（P1-1）。
4. **autosave 线程生命周期**：`ensure_thread` 在 bind 时启动（`writethrough.rs:215,411-427`），`flush_all` 停止并 join（`:504-528`）。外部后端若阻塞网络，退出会卡在 join；vtable 需要超时/取消语义（P2-1）。会话级注册还必须在 unbind 后拒绝新调用（SDK 的 `BackendRegistration` drop 顺序要明确）。
5. **schema/version 强制**：版本信息目前完全由 backend 判定（`backend.rs:26-28`）；外部后端可对不兼容 payload 返回 `QUERCUSSTORAGE_OK`。字节化后由引擎 `serializer_load` 失败兜底，但仍需定义“库内 schema 版本如何上报/迁移”（`ProjectInfo.schema_ver`，`backends/database/mod.rs:314`，当前是数据库内部字段，未暴露给库后端接口）。
6. **配置误用静默降级**：`Storage/Backend` 写入非白名单值直接 `false`（`writethrough.rs:340-343`），消费者会以为“已启用自定义后端”而实际不落盘。新方案应“URI 可解析即启用，否则显式报错”。

## 10. 验收结论

**“Oak 型消费者能否在今天接入 Postgres/HTTP/远程存储？”——不能。** 具体地：

- 想要**引擎自带 PG**：可以，`Storage/Backend=pg` + `Storage/PgUrl`（`writethrough.rs:58-66,357-376`），但 schema、迁移、journal/snapshot 全是引擎私有（`backends/database/mod.rs`），不是“接入自己的存储”。
- 想要**自己的后端**：需要同时满足 (a) 用 Rust 直接依赖内部 crate 并注册（违反 `lib.rs:1-5`，且很可能因双份静态链接无效，§4.3），**或** (b) 等 P0-1/P0-2/P0-3 落地。C/SDK 消费者两条都走不通。
- 唯一的“替换”仍在原地：换文件格式（`.ove` / `.otio`）和换内置库（sqlite/pg），不是换存储实现。

**最小可行路线（建议）**：先做 P0-1（注册符号，约一个 `storage.rs` 导出 + SDK 声明 + 头文件再生成）+ P0-3（vtable 字节化），即可让外部后端服务 `quercus_storage_open/save` 与 `quercus_editor_open/save`；再做 P0-2 + P1-3，才能覆盖项目库、写穿和自动保存。P0-1 落地前，README 中 `quercusstorage_backend_register` 的说法必须删除或标注为“planned”。

## 11. 未覆盖/未验证

- 未运行 `cargo build/test`，对 database 后端（SQLite/PG）的运行时行为未验证；`database_pg_test.rs` 是否真跑过 PG 未查。
- “消费者直接依赖 `quercus-storage` 会得到两份 `Registry`”是静态推理，未做链接级实验（§4.3 已标【推断】）。
- `docs/zh/plans/riir/M10-quercusstorage.md` 等设计文档在本仓缺失，无法核对 M10 §2.2/§2.3 的原始 vtable 约定；本报告的 vtable 草案是依据现有代码与 `codec/task.rs`、`plugin.rs` 的 C 注册先例设计，不是对 M10 原文的复原。
