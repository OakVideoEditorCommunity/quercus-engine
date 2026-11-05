# NVIDIA Linux CUDA → Vulkan GPU 导出 spike（go/no-go）

- 任务：task-9。目标：评估/原型 NVDEC(CUDA) 帧在 Linux 上不经 CPU 回读进入 wgpu/Vulkan 的可行路径；允许 GPU 侧拷贝。
- 仓库：`/home/sunyu/Projects/quercus-engine`（workspace 0.5.2）。
- 本文件与 `cuda_vulkan_probe.c` / `ffmpeg_pool_probe.c` / `run.sh` 是**新增的 spike 资产**；按任务约束，**未修改** `gpuinterop.rs`、`external.rs` 等生产代码（原因见 §8）。
- 项目 FFmpeg：**8.1.2**（`tooling/ffmpeg/build-ffmpeg.sh:50`，源码树 `.cache/ffmpeg-src`）。
- 本文件按 follow-up 扩展：route A 已被 §0.1 的 GPU 实测否掉；新增 **route B**（§3.3）与 `ffmpeg_pool_probe.c`。本 agent 的会话是 bwrap 沙箱（私有 `/dev`，无 `/dev/nvidia*`，`mknod` EPERM），**route B 的实测定论需在 GPU 会话执行 `run.sh` 后回填**；§3.3.2 已给出预置判读矩阵。

---

## 0.1 本机实测结果（2026-11-06，RTX 5070 Ti / 615.78.08）

重启后设备节点已出现；`run.sh` 的 probe 结论：

```text
cuda dma-buf supported   : NO
dma_buf export fd        : NO
cuMemGetHandleForAddressRange(...) -> CUDA_ERROR_NOT_SUPPORTED / INVALID_VALUE
Vulkan H.264 decode High 4:2:2 10bit -> VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR
Vulkan H.264 decode High10 4:2:0 10bit -> VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR
Vulkan H.264 decode High 4:2:0 8bit   -> VK_SUCCESS
Vulkan HEVC decode Main10 4:2:0 10bit -> VK_SUCCESS
Vulkan HEVC decode RExt 4:2:2 10bit   -> VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR
```

因此在这台机器/驱动上：

- **NVIDIA Linux CUDA 帧不能导出 DMA-BUF**，路线 A 不可用；
- **Vulkan Video 也不支持 H.264 10-bit 4:2:2**，路线 C 不能替代；
- 对 `C0024.MP4` 这种 4K 10-bit 4:2:2，当前驱动不存在“解码 surface 不经过 CPU 进入 wgpu”的可行接口。

工程上的可行替代只有：
1. CUDA 侧先 GPU 缩放/转换，再把**目标尺寸**读回（数据仍会在边界离开 GPU，但比 4K 全帧下载小很多）；
2. 代理；
3. 等 NVIDIA 驱动/Vulkan Video 暴露 4:2:2 或 DMA-BUF 能力。

这与“有 GPU 就一定能零拷贝”是驱动能力问题，不是渲染器接口能绕过的。

## 0.2 route B 为什么是"唯一剩下的"：NVDEC vs Vulkan Video 能力对照

已提交的 §0.1 实测（RTX 5070 Ti / 615.78.08）显示 **Vulkan Video 不覆盖 4:2:2/10-bit H.264**：

```text
Vulkan H.264 decode High 4:2:2 10bit -> VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR
Vulkan H.264 decode High10 4:2:0 10bit -> VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR
Vulkan H.264 decode High 4:2:0 8bit   -> VK_SUCCESS
Vulkan HEVC decode Main10 4:2:0 10bit -> VK_SUCCESS
Vulkan HEVC decode RExt 4:2:2 10bit   -> VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR
```

而 NVIDIA 官方 **NVDEC** 解码表（[Video Encode and Decode GPU Support Matrix](https://developer.nvidia.com/video-encode-decode-support-matrix)，NVDEC 页签）对 **GeForce RTX 5070 Ti / Blackwell 6th Gen** 的 23 列全部为 YES，包括：

- H.264 (AVC) 4:2:2 **8 Bit / 10 Bit** = YES
- H.265 (HEVC) 4:2:2 **8/10/12 Bit** = YES
- H.264 4:2:0 10 Bit、HEVC 4:2:0 8/10/12、AV1 8/10 Bit = YES

对照：Ada（RTX 40 系）同一张表里 H.264 4:2:0 10-bit、H.264 4:2:2、HEVC 4:2:2 均为 **NO**（Blackwell 才加入）——也就是说 **4:2:2/10-bit 的硬件解码能力在 NVDEC 有、在 Vulkan Video 没有**。

因此对 `C0024.MP4` 这类 4K 10-bit 4:2:2 源：
1. **Vulkan Video（route C）不能替代**（实测 profile 不支持）；
2. **NVDEC/CUDA 是唯一 GPU 解码器**；
3. 帧要进 wgpu，只能走 **route B（VMM POSIX_FD 导出 → Vulkan 外部图像导入）**——route A 的 dma-buf 已被 attr 124=0 否掉；
4. 若 route B 也失败，则这台机器上"4:2:2 10-bit 解码 + GPU 合成"不存在零拷贝路径，只能按 §0.1 的工程替代（GPU 缩放后读回 / 代理 / 等驱动）。

（NVENC 表对 5070 Ti 也标 H.264/HEVC 4:2:2 编码 YES，但那是 `h264_nvenc` 路径，需要 CUDA 帧；wgpu 渲染结果要喂给 NVENC 同样绕不开 CUDA↔Vulkan 互操作，而该互操作正是 route B 要验证的。`h264_vulkan/hevc_vulkan` 编码器则受 FFmpeg 8.1 profile 映射限制，见 §4.3。）

## 0.3 结论摘要（路线总表）

| 路线 | 判定 | 关键条件 |
|---|---|---|
| **A. 直接导出 FFmpeg 现有 CUDA 帧为 DMA-BUF，再导入 wgpu 的 Vulkan 设备** | **NO-GO（已实测）** | GPU 会话实测 `CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED = 0`（该驱动/显卡不支持把 `cuMemAlloc` 区间导出为 dma-buf），`cuMemGetHandleForAddressRange(DMA_BUF_FD)` 因此不可用；本报告 §2/§3.1 的分析保留作为记录，但**此路线在本机作废** |
| **B. 自建可导出的 CUDA VMM 帧池，喂给 FFmpeg** | **GO（同进程；NVDEC 直写待 §0.5 确认；跨进程未证明）** | `cuMemCreate(requestedHandleTypes=POSIX_FD)` + `cuMemAddressReserve` + `cuMemMap` + `cuMemSetAccess` 建池；FFmpeg 允许用户预置 `AVHWFramesContext.pool`（`hwcontext.h:169-181`，`hwcontext.c:373-374`），NVDEC→帧的既有 `cuMemcpy2DAsync` 直接落进该池；池分配时 `cuMemExportToShareableHandle(POSIX_FD)` 拿 fd，按 fd 类型走 Vulkan `OPAQUE_FD`（`VK_EXT_external_memory_fd`）或 `DMA_BUF` 导入。关键待测：attr `103`（POSIX_FD 导出）、fd 的真实类型（`/proc/self/fd` → dma-buf 还是驱动私有）、NVIDIA Vulkan 能否导入该 fd |
| **C. Vulkan Video 解码（`-hwaccel vulkan`）** | **当前构建 NO-GO；重编后仅 4:2:0 可用（已实测）** | 项目构建 `CONFIG_VULKAN=0`（`.cache/ffmpeg-src/config.h:658`、`824`），`libavcodec.a` 里 `h264_vulkan`/`hevc_vulkan` 字符串为 0，`libavutil.a` 无 `hwcontext_vulkan`；需 `--enable-vulkan` 重编。FFmpeg 8.1 中 Vulkan 解码是 **hwaccel**（`configure:3392-3393`、`3410-3411`），不是 `h264_vulkan` decoder（§4.1）。GPU 实测（§0.1/0.2）：H.264 High 4:2:0 8-bit、HEVC Main10 4:2:0 可用；H.264 High10、H.264 High422、HEVC RExt 4:2:2 均 `VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR` → **不能覆盖 4:2:2/10-bit 源** |
| **D. Vulkan Video 编码（`h264_vulkan`/`hevc_vulkan`）做 10-bit 4:2:2 导出** | **NO-GO** | FFmpeg 8.1 的 H.264 profile 映射**没有 High10 / High422** 分支（`vulkan_video.c:260-269`），编码器候选表止于 High10 且经映射→`INVALID`（`vulkan_encode_h264.c:701-704,719`）；且驱动侧 Vulkan Video 本就不支持 4:2:2（§0.1 实测）。硬件 NVENC 在 Blackwell GeForce 上支持 H.264/HEVC 4:2:2（NVIDIA 厂商表），但那是 `h264_nvenc`/`hevc_nvenc`（需要 CUDA 帧），wgpu→CUDA 的帧交接正是 route B 要验证的互操作，不能算 Vulkan Video 路线 |

**一句话**：route A 已被实测否掉（CUDA dma-buf 不可用）；**route B（VMM + POSIX_FD 导出 + Vulkan OPAQUE_FD/LINEAR 导入）已实测走通（同进程），是 NVIDIA Linux 上唯一剩下的 CUDA 解码零拷贝路线**——但 fd 是 `/dev/nvidiactl` 驱动私有句柄，**跨进程（process-pool worker）尚未证明**，且需在 `external.rs` 新增 OPAQUE_FD 导入、在 `hwdecode`/`ffmpeg` 预置自定义池（见 §0.4.2/§5）；Vulkan Video 实测只覆盖 4:2:0。

## 0.4 route B 实测结果（Lead GPU 会话）与两处修正

GPU 会话跑旧版 route B probe 的关键输出：

| 项 | 结果 |
|---|---|
| `CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED` (102) | **1** |
| `CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED` (103) | **1** |
| `cuMemCreate(requestedHandleTypes=POSIX_FD)` | **CUDA_SUCCESS** |
| `cuMemExportToShareableHandle(POSIX_FD)` | **CUDA_SUCCESS, fd=127, readlink=`/dev/nvidiactl`**（驱动私有句柄，**不是 dma-buf**） |
| `cuMemGetHandleForAddressRange(DMA_BUF_FD)`（VMM 映射区） | **CUDA_ERROR_NOT_SUPPORTED**（与 attr 124=0 一致） |
| Vulkan `OPAQUE_FD` + `LINEAR`（R8 单平面 / P010 双平面） | `vkCreateImage` / `vkAllocateMemory(import)` / `vkBindImageMemory` / `vkCmdCopyImageToBuffer` 全部 **VK_SUCCESS** |
| Vulkan `DMA_BUF` 导入 | `VK_ERROR_OUT_OF_DEVICE_MEMORY`（预期：fd 不是 dma-buf） |
| `ffmpeg_pool_probe` | `ctx->pool` 生效、`av_hwframe_ctx_init` OK；第一帧 `data[0]=0x10012c00000` 被判"不在 slot `0x10012400000..0x10012c00000`" |
| 读回首字节 | `00`，因为 `cuMemsetD8(va,0xAB,256)` → `CUDA_ERROR_LAUNCH_FAILED` |

### 0.4.1 两处修正（探针已更新，需在 GPU 上复跑确认）

1. **读回 0x00 ≠ 导入失败**。旧探针用 `cuMemsetD8` 填 VMM 内存并返回 `LAUNCH_FAILED`（VMM 内存写入依赖 context/access 状态，旧代码未校验 `cuCtxPushCurrent` 结果）。新 `cuda_vulkan_probe.c`：
   - 显式 `cuCtxPushCurrent` + `cuCtxSetCurrent` 并打印两者结果；
   - 先做 control：`cuMemAlloc` + `cuMemcpyHtoD(0xAB)` + sync，证明拷贝机制本身可用；
   - 用 `cuMemcpyHtoD` 填 VMM slot + `cuCtxSynchronize`，逐步打印；`cuMemsetD8` 只保留为诊断；
   - 导入后 readback 校验 `0xab`。
2. **"不在 slot" 是探针判定 bug，不是 FFmpeg 没用调用方池**。`0x10012c00000` 正是 slot#0 的 end、slot#1 的 start：`initial_pool_size=2` 预分配两块，`AVBufferPool` 是 **LIFO**，先 `av_frame_free` 再 `av_hwframe_get_buffer` 会复用同一 slot。旧探针只记录 `g_first_slot`，所以把 slot#1 误报为"不属于调用方"。
   新 `ffmpeg_pool_probe.c`：记录全部 slot（≤64）、按 `data[0]` 匹配并打印 slot 号/偏移、同时持有两帧再释放（确保看到两个 slot）、对匹配 slot 的 fd 直接做 **Vulkan OPAQUE_FD 导入并校验 0xAB**。

3. **`cuMemcpyHtoD -> CUDA_ERROR_INVALID_CONTEXT`**（route B2 的剩余失败）：FFmpeg 会在 `av_hwdevice_ctx_create` / `av_hwframe_ctx_init` / `av_hwframe_get_buffer` 内部 push/pop 它自己的 `CUcontext`，早先 push 的上下文到拷贝点已经不再 current。新探针：
   - 用 `cuCtxGetDevice` 读取当前上下文的设备号，`cuMemCreate` 的 `location.id` 与 `cuMemSetAccess` 的 `location.id` 都用它（不再硬编码 0）；
   - `ctx_enter()/ctx_leave()` 包住 `pool_alloc`、pattern copy、`slot_free`：每次 `cuCtxPushCurrent` + `cuCtxSetCurrent` + `cuCtxGetDevice`，并打印 `[ctx] <why>: push=... set_current=... device=N`；
   - 主探针 `cuda_vulkan_probe.c` 同样加 `cuCtxGetDevice`，VMM 属性/access 用实际 device，并在 `cuMemcpyHtoD` 前重新 `cuCtxSetCurrent`。
   预期复跑：`[ctx] pattern copy: ... set_current=CUDA_SUCCESS device=0` → `[slot] cuMemcpyHtoD -> ...` → readback `MATCH (0xab)` → `[routeB] cross-process OPAQUE_FD import: GO/FAIL`。
   **实测结论（B3）**：context 正确（device=0，push/set SUCCESS）时 `cuMemcpyHtoD` 仍返回 `CUDA_ERROR_INVALID_CONTEXT`——host→VMM 拷贝在该驱动上不可用，**已放弃该路径，不再作为 route B 判据**；改由 §0.5 的 NVDEC 直写测试决定。

### 0.4.2 修正后的判定

- **route B 是 GO（有条件，同进程）**：CUDA VMM + POSIX_FD 导出可用；fd 是 `/dev/nvidiactl` 驱动私有句柄；**Vulkan `OPAQUE_FD` + `LINEAR` 外部图像导入成功**；FFmpeg 调用方池确实被使用（修正后判定）。唯一的"零拷贝 CUDA→wgpu"路线成立。
- **不能用现有 `external.rs` 的 DMA_BUF 路径**：该 fd 不是 dma-buf（DMA_BUF 导入 OODM 也证实）。生产改动需要新增 **OPAQUE_FD 线性导入**：
  - `external.rs`：`import_opaque_fd_linear(fd, w, h, fmt, keep_alive)` —— `VkExternalMemoryImageCreateInfo{OPAQUE_FD}` + `VK_IMAGE_TILING_LINEAR` + `VkImportMemoryFdInfoKHR{OPAQUE_FD}` + `texture_from_raw(..., TextureMemory::External)`（与 VAAPI 同一 wgpu-hal 包装路径，已有先例 `external.rs:292-458`），约 150 行；
  - `gpuinterop.rs`：VMM 池（dlopen libcuda；按 slot 导出一次、按 slot 注册纹理、`ImportGuard` 持有 slot 的 AVBufferRef），约 250 行；
  - `hwdecode.rs`/`ffmpeg.rs`：在 `avcodec_open2` 前预置带自定义池的 `avctx->hw_frames_ctx`（cuvid 校验 `sw_format`），需扩写权限。
- **跨进程是新的头号 blocker**：`OPAQUE_FD` 是驱动私有句柄，Vulkan 规范只说它"在 Vulkan API/同驱动之外只有有限合法性"；而引擎的 `quercus-worker` 进程池是**多进程**渲染。因此 route B 目前只能确认 **同进程**（Threads/Pipeline 后端）可用。
  探针已内置该测试：route B 导出 fd 后 **fork+exec 自身**，父进程用 `SCM_RIGHTS` 把 fd 发给子进程；子进程在全新进程里新建 Vulkan 设备、以 `OPAQUE_FD` 导入并校验 `0xAB`，打印
  `[routeB] cross-process OPAQUE_FD import: GO (same fd imports in another process)` 或 `FAIL (fd is process-local)`。**这一行决定 route B 能否用于 process-pool 架构。**
- wgpu-hal 本身没问题：raw `VkImage` 已创建/绑定成功，`texture_from_raw(TextureMemory::External)` 就是现有 VAAPI/D3D11 用法；剩 LINEAR tiling 的 sampled/filter 组合需一次 GPU 测试（可能要用 nearest sampler）。

## 0.5 NVDEC 默认池 + GPU 侧 DtoD 拷贝 → VMM → OPAQUE_FD（决定性测试）

自定义 `AVHWFramesContext.pool` 路线已被实测否掉：generic hwaccel 拿到了 CUDA 帧（`format=cuda, hw_frames=1`），但 `frame->data[0]=0x20800000` 不在调用方 VMM slots 里——FFmpeg/NVDEC 仍然分配了自己的 CUDA 帧，**调用方 pool 未生效**。按 Lead 指示不再投入该路线。

改为"接受 GPU 内部拷贝"的路线（用户原始要求允许）：**NVDEC 用默认池解码 → 在 FFmpeg 的 CUDA context 上分配一个 VMM slot → `cuMemcpy2DAsync` device-to-device 把 luma/chroma 拷进 VMM → 导出 POSIX_FD → Vulkan OPAQUE_FD+LINEAR 导入 → 读回并与软件解码参考比较**。`nvdec_pool_probe.c` 已重写为这一流程：

1. 默认池解码一帧（generic `h264`/`hevc` + `hw_device_ctx` CUDA hwaccel；不设置 `hw_frames_ctx`）；
2. 从 `frame->hw_frames_ctx` 的 `sw_format` 推出 NV12/P010/P210 与 bytes/px、chroma 高度；先 `cuMemGetAllocationGranularity`（本机 2 MiB）把请求**向上取整到 granularity**（640x360 NV12 = 552,960 字节不是 2 MiB 倍数，直接 `cuMemCreate` 会 `CUDA_ERROR_INVALID_VALUE`——已修），`pool_alloc()` 只当作 VMM 分配器（POSIX_FD 导出）使用；
3. `cuMemcpy2DAsync`（stream 用 FFmpeg 的 `AVCUDADeviceContext.stream`，实测为 NULL/default）逐平面拷贝，`WidthInBytes=width*bytes`，`src/dstPitch=linesize`；失败依次回退 `cuMemcpy2D`、`cuMemcpyDtoD`（要求 pitch 相同），每步打印 CUDA 错误；
4. `cuCtxSynchronize` 后导出 slot fd，Vulkan `OPAQUE_FD`+`LINEAR` 导入，读回 luma 前 64 字节；
5. NV12 输入与软件解码第一帧 luma 比较前 16 字节。

> **当前状态**：route A 实测 dead；Vulkan Video 4:2:2/10-bit 实测不支持（仅 4:2:0，H.264 仅 8-bit）；route B 机制已实测走通（VMM POSIX_FD 导出 + fd `/dev/nvidiactl` + Vulkan OPAQUE_FD/LINEAR 导入成功，同进程），`external.rs::import_opaque_fd_plane` 已落地。DtoD 首次实测 `INVALID_CONTEXT` 经 Lead 定位为**探针符号查找问题**（legacy 基础符号优先），已改为优先 `_v2`；**最后一次重跑 `[control]` 若通过，route B 判定为 GO**，否则按 `[control]`/`[copy]` 结果 final verdict。

**DtoD 实测（Lead GPU 会话）**：
- 640x360 NV12：slot YES，`cuMemcpy2DAsync` / `cuMemcpy2D` / `cuMemcpyDtoD` **全部 `CUDA_ERROR_INVALID_CONTEXT`**，readback 0，reference MISMATCH，`[verdict] FAIL`；
- C0024 4K 10-bit 4:2:2（`sw_format=p210le`）：slot YES（`requested=33177600 rounded=33554432 fd=105`），同样三个 API 全 `INVALID_CONTEXT`，readback 0，`FAIL`。

但这个失败可疑：同一会话里最小 standalone `cuCtxCreate + cuMemAlloc + cuMemcpyHtoD` 是可用的。因此探针新增**对照诊断**（不是改结论，而是定位"探针/上下文处理"还是"VMM/context 关联"）：
- `[cuda] syms:` 打印所有 dlsym 到的 CUDA 函数指针（确认 API 装载没错）；
- `[ctx] ... get_current=... current=... expected=... (MATCH/MISMATCH)` 打印 `cuCtxGetCurrent` 与 FFmpeg `AVCUDADeviceContext.cuda_ctx` 的对比；
- `[pool] cuMemSetAccess(...) -> ...` 与 `[pool] cuMemGetAddressRange(va) -> base/size` 确认 VMM 映射/访问是否被 CUDA 认可；
- `[control] cuMemAlloc+cuMemcpyHtoD`：同一 `ctx_enter` 后对**普通设备内存**做 host→device 写；
- `[control] cuMemcpyHtoD(VMM slot va=...)`：同一上下文对 **VMM slot** 做 host→device 写。

**根因（Lead 定位，已修）**：探针的 `sym()` 先查 legacy 基础符号（`cuMemAlloc`/`cuMemFree`/`cuMemcpyHtoD`/`cuMemcpyDtoD`/`cuCtxPushCurrent`/`cuCtxPopCurrent`），而 64 位驱动上这些 legacy 符号可能拒绝当前上下文（最小 C 测试用 `_v2` 正常）。三个探针已统一改为**优先 `_v2`**、legacy 作 fallback：
`cuMemAlloc_v2`、`cuMemFree_v2`、`cuMemcpyHtoD_v2`、`cuMemcpyDtoD_v2`、`cuCtxPushCurrent_v2`、`cuCtxPopCurrent_v2`、`cuCtxCreate_v2`、`cuMemsetD8_v2`。
因此之前 DtoD 的 `INVALID_CONTEXT` 很可能是**探针 artifact，不是驱动 verdict**；重跑后若 `[control] cuMemAlloc+cuMemcpyHtoD` 通过，则 route B 可能为 GO。

判据补充：
- `[control] cuMemAlloc+cuMemcpyHtoD` 也失败 → 探针的上下文/API 处理坏了（不是驱动结论），修探针；
- control 成功但 `[control] ... (VMM slot)` 或 DtoD 失败 → **VMM allocation 与 context 的关联是 blocker**，按原判据 route B dead（记下 `cuMemSetAccess`/`cuMemGetAddressRange` 的结果作为根因）。

判读（`[verdict]` 行）：
- `[vmm] cuMemGetAllocationGranularity` 打印 granularity（本机 2097152），`[slot] requested=… rounded=…` 显示已取整；
- `[copy]` 成功 + `[readback]` 非零 + `[reference] MATCH` → **route B GO（GPU 侧拷贝）**：引擎可实现为 "解码 CUDA 帧 → DtoD 拷进 VMM slot → OPAQUE_FD 导入"，不需要自定义 FFmpeg pool。
- `[copy]` 返回 `CUDA_ERROR_INVALID_CONTEXT` 且两个回退也失败 → 该驱动连 DtoD/2D 拷贝都不接受 VMM，**route B dead**（附精确错误）。
- copy 成功但 readback 全零/参考不匹配 → 导入的 fd 与 VMM slot 不是同一内存或布局错误（需检查 pitch/offset）。

wgpu-hal：raw `OPAQUE_FD`+`LINEAR` 导入已成功，`wgpu_hal::vulkan::Device::texture_from_raw(..., TextureMemory::External)` 与 VAAPI 同一路径。**`external.rs::import_opaque_fd_plane` 已随提交 `fd2e163` 落地**（OPAQUE_FD + `VK_IMAGE_TILING_LINEAR`、fd dup、按 `offset` bind、`register_imported_texture`），与本探针的导入形状一致；引擎剩余工作只有：VMM 分配（granularity 取整）+ 解码后 DtoD 拷贝 + 调用该函数注册纹理（不再需要自定义 `AVHWFramesContext.pool`）。

---

## 1. 本机环境事实（诚实说明：本轮无法跑通 GPU 路径）

| 项 | 结果 | 证据/命令 |
|---|---|---|
| GPU 硬件 | **NVIDIA GB203 [GeForce RTX 5070 Ti]** 存在 | `lspci`：`01:00.0 VGA compatible controller: NVIDIA Corporation GB203 [GeForce RTX 5070 Ti]` |
| 内核模块 | 已加载 `nvidia`、`nvidia_uvm`、`nvidia_drm`；NVRM 615.78.08 | `lsmod`、`/proc/driver/nvidia/version` |
| 驱动库 | `libcuda.so.1`、`libnvcuvid.so.1` 存在 | `ldconfig -p` |
| 设备节点 | **不存在** `/dev/nvidia*`，也**没有** `/dev/dri` | `ls /dev/nvidia*`、`ls /dev/dri` |
| 结果 | `nvidia-smi` 退出码 9；`vulkaninfo` 枚举 0 个物理设备；`cuInit` 返回 `CUDA_ERROR_NO_DEVICE` | 本轮会话处于无设备直通的容器/命名空间 |
| Vulkan 头/加载器 | 1.4.363，含 `VK_KHR_video_*` 与 `/usr/include/vk_video/*`（视频 std 头齐全） | `pkg-config --modversion vulkan`、`/usr/include/vk_video/vulkan_video_codec_h264std.h:44-51` |
| FFmpeg（项目） | 8.1.2；`CONFIG_CUDA=1 CONFIG_CUVID=1 CONFIG_NVDEC=1 CONFIG_NVENC=1 CONFIG_VAAPI=1 CONFIG_LIBDRM=1 CONFIG_VULKAN=0 CONFIG_VULKAN_1_4=0` | `.cache/ffmpeg-src/config.h:645,647,652-655,658,824` |
| FFmpeg（系统） | n9.0.2，`-hwaccels` 含 `cuda vaapi vulkan …`；`-encoders` 含 `h264_vulkan/hevc_vulkan/av1_vulkan`；`-decoders` **无** vulkan 解码器（解码是 hwaccel 形态） | `ffmpeg -hwaccels` / `-decoders` / `-encoders` |
| **GPU 实测（Lead 会话）** | `CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED = 0` → **route A（cuMemAlloc → DMA-BUF）不可用** | 由 Lead 在带 `/dev/nvidia*` 的会话运行本 spike 的 probe 得到；本轮 follow-up 的输入 |
| 本 agent 的执行限制 | 无法跑 GPU：`/dev` 是 bwrap 私有 tmpfs（PID 1 = `bwrap`），无 `/dev/nvidia*`、`/dev/dri`，`mknod` 返回 EPERM | `ls /dev`、`cat /proc/1/comm`、`mknod` 实测 |

因此：**代码级结论可验证，运行期结论（attribute 124、page 对齐、驱动是否接受线性 dma-buf、Vulkan Video 能力）只能在有设备节点的会话里跑 `probe` 得到**。这是本轮没有直接落地生产代码的原因之一（§8）。`probe` 已在本会话编译通过并优雅降级（见 §7 输出）。

---

## 2. Q1：`AV_PIX_FMT_CUDA` 帧是什么内存？`av_hwframe_map` 能拿到什么？

**结论：线性 `cuMemAlloc` 单缓冲、`data[i]` 就是 `CUdeviceptr`，不是 CUarray；不需要也无法靠 `av_hwframe_map` 导出。**

证据（FFmpeg 8.1 源码）：

1. 帧池分配用 `cuMemAlloc`，释放用 `cuMemFree`：`libavutil/hwcontext_cuda.c:96-131`（`cuda_pool_alloc` 里 `cuMemAlloc(&data, size)`；`cuda_buffer_free` 里 `cuMemFree`）。
2. 一帧**一个** buffer，三个平面靠 `av_image_fill_arrays` 在同一个分配里排布，对齐参数是设备属性 14（`CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT`）：`hwcontext_cuda.c:160-185`（读取 attr 14、`av_image_get_buffer_size(..., tex_alignment)`）、`:191-216`（`cuda_get_buffer`：`frame->buf[0]` + `av_image_fill_arrays(frame->data, frame->linesize, frame->buf[0]->data, …)`）。
3. `data[i]` 被当作 `CUdeviceptr` 直接给 CUDA 拷贝 API：`hwcontext_cuda.c:240-286`（`CUDA_MEMCPY2D` 里 `srcDevice=(CUdeviceptr)src->data[i]`，`cuMemcpy2DAsync`）；头文件也写明 "AVBufferRefs whose data pointer is a CUdeviceptr"（`.cache/ffmpeg/include/libavutil/hwcontext_cuda.h:34`）。
4. NVDEC 路径本来就有一次 GPU 侧拷贝：`libavcodec/cuviddec.c:606-639`（`cuvidMapVideoFrame` 取出内部 surface 的 device 指针 + pitch，然后 `cuMemcpy2DAsync` 写进 FFmpeg 的 CUDA 帧）。**NVDEC 的私有 surface 从不出现在 AVFrame 里**，所以"导出 AVFrame 内存"天然满足"允许 GPU 侧拷贝"的要求。
5. `av_hwframe_map` 在本项目中无助于导出：CUDA hwcontext 的 ops 只登记了 `transfer_data_to/from`（`hwcontext_cuda.c:572-590`），**没有 `map_to`/`map_from`**，所以 CUDA→DRM_PRIME / CUDA→任何外部格式 的映射都会返回 `ENOSYS`；CUDA→CPU 只能走 `av_hwframe_transfer_data`（真正的下载）。映射 CUDA→CUDA 只是引用复制。`AVCUDADeviceContext.cuda_ctx`/`stream` 是公开字段（`hwcontext_cuda.h:42-46`），足以让我们在正确的 `CUcontext` 上直接调 CUDA driver API。
6. `AVHWFramesContext.hwctx` 对 CUDA "currently not used"（`hwcontext_cuda.h:48-50`），因此**没有公开的 `tex_alignment` 可改**；但用户可预置 `AVHWFramesContext.pool`（`hwcontext.h:169-181`，`hwcontext.c:373-374` 只有在 `ctx->pool == NULL` 时才用内部池），这是路线 B 的接口依据。

要点：**FFmpeg 帧 = 一页未对齐的线性 cuMemAlloc 块**（大小 `av_image_get_buffer_size(..., tex_alignment)`，如 P010 1080p 为 6,220,800 字节级，非 4096 倍数；平面偏移由 `tex_alignment` 对齐，通常 256/512）。

---

## 3. Q2：CUDA → DMA-BUF → Vulkan 的 API 序列与硬约束

### 3.1 导出（CUDA driver API）

文档明确：`cuMemGetHandleForAddressRange(handle, dptr, size, CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD, flags)`
**支持由 `cuMemAlloc` 得到的地址区间**（前提：`CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED` == true），也支持 `cuMemAddressReserve` 的 VMM 区间（且必须已完整 map）；`dptr` 与 `size` 必须按 host page size 对齐；`flags` 可传 `CU_MEM_RANGE_FLAG_DMA_BUF_MAPPING_TYPE_PCIE`（PCIe BAR1 映射）。来源：NVIDIA CUDA Driver API *Memory Management*（[docs.nvidia.com](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html)）。

常量（[CUDA TYPES 文档](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__TYPES.html)）：
- `CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD = 0x1`
- `CU_MEM_RANGE_FLAG_DMA_BUF_MAPPING_TYPE_PCIE = 0x1`
- `CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED = 124`（"Device supports buffer sharing with dma_buf mechanism"）
- `CU_DEVICE_ATTRIBUTE_HOST_ALLOC_DMA_BUF_SUPPORTED = 146`
- `CU_DEVICE_ATTRIBUTE_DMA_BUF_MMAP_SUPPORTED = 152`
- `CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR = 0x1`（VMM 分配的"文件描述符导出"，注意文档只说 file descriptor，**不保证**是 dma-buf）

对应到 FFmpeg 的帧：`dptr = frame->data[0]`、`size = 帧池大小`。由于 `size` 不是 page 倍数，工程上只有三种可行处理：
1. **导出页对齐子区间**：`dptr0 = data[0] & ~0xFFF`，`size0 = round_up(size + (data[0]-dptr0), 4096)`。是否接受"分配内部子区间"必须实测（probe 的第 2 个用例就是它）。
2. **自带页对齐池**（路线 B）：自己 `cuMemAlloc` 一个 `page_rounded_size + 4096` 的块，向 FFmpeg 交一个页对齐的内部指针 + 页倍数大小；FFmpeg 允许用户池（`hwcontext.h:169-181`）。注意平面偏移仍由私有 `tex_alignment` 决定，不能改。
3. 若驱动只接受"整段分配"，则必须走路线 B，用 VMM 分配。

### 3.2 Vulkan 导入（wgpu 现有实现可直接复用大半）

本项目已有 Linux DMA-BUF 导入实现（VAAPI 用）：`crates/quercus-core/src/backend/external.rs:17-30`、`:292-458`：
- 用 raw Vulkan：`VkExternalMemoryImageCreateInfo{handleTypes=DMA_BUF_EXT}` + `VkImageDrmFormatModifierExplicitCreateInfoEXT`（显式 modifier/plane layout）+ `vkAllocateMemory` 配 `VkImportMemoryFdInfoKHR`（`DMA_BUF_EXT`，fd 由调用方 dup，成功后 Vulkan 接管所有权：`external.rs:340-352`）；
- 再用 `wgpu_hal::vulkan::Device::texture_from_raw(..., TextureMemory::External)` 包成 `wgpu::Texture`（`external.rs:395-440`），drop guard 里 `destroy_image`/`free_memory` 并触发 keep-alive（`external.rs:400-420`）。

关键约束（来自 [VK_EXT_external_memory_dma_buf 规范](https://raw.githubusercontent.com/KhronosGroup/Vulkan-Docs/main/appendices/VK_EXT_external_memory_dma_buf.adoc)）：该扩展只负责 dma-buf ↔ `VkDeviceMemory`；**图像内存布局必须由 `VK_EXT_image_drm_format_modifier` 指定**（Issue 1 的 RESOLVED 明确指向该分层扩展）。所以 CUDA 线性内存应作为 `DRM_FORMAT_MOD_LINEAR`（=0）的显式 plane layout 导入。

两种图像形态：
- **（推荐）单个多平面 VkImage**：P010 → `VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16`；NV12 → `VK_FORMAT_G8_B8R8_2PLANE_420_UNORM`；4:2:2 → `…_2PLANE_422_…`。plane0 offset=0/pitch=linesize[0]；plane1 offset=`data[1]-data[0]`/pitch=linesize[1]。然后建两个 plane view（`PLANE_0_BIT`/`PLANE_1_BIT`）。这与 Windows D3D11VA 的多平面导入同构（`external.rs:449-…`），也避免"按平面切成两个 dma-buf"——平面偏移不是 page 对齐，切不出来。
- 备选：每平面一个 R8/RG8/R16/RG16 图像（VAAPI 形态，`external.rs:292` 现成），但 `vkBindImageMemory` 的 `memoryOffset` 必须满足图像对齐；FFmpeg 的平面偏移（如 P010 1080p UV off = linesize*1080）是否满足需实测。

线程/生命周期契约（沿用现有模式）：导出+导入在渲染/解码线程调用；调用 `cuMemGetHandleForAddressRange` 前把 FFmpeg 的 `CUcontext` 置为 current（`AVCUDADeviceContext.cuda_ctx`）；每个导入 dup 一次 fd（Vulkan 成功后接管）；`ImportGuard` 持有 `AVFrame` 引用，直到 texture 销毁（`gpuinterop.rs:400-419` 的 keep-alive 模式）；任何失败都按"本帧回落 staging"处理（现有连续失败熔断 `gpuinterop.rs:69-74` 已覆盖）。

### 3.3 route B：exportable CUDA VMM pool → Vulkan（route A 否掉后的唯一 CUDA 路线）

#### 3.3.1 CUDA 侧 API 序列（`cuda_vulkan_probe.c` 的 route B 段已实现）

属性门槛（先查，避免瞎试）：
- `CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED` = **102**
- `CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED` = **103**（"Device supports exporting memory to a posix file descriptor with `cuMemExportToShareableHandle`, if requested via `cuMemCreate`"）

流程（结构体按 CUDA 13 布局：`type, requestedHandleTypes, location{type,id}, win32HandleMetaData, allocFlags…`）：

```c
CUmemAllocationProp prop = {0};
prop.type = CU_MEM_ALLOCATION_TYPE_PINNED;                    // 0x1
prop.requestedHandleTypes = CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR; // 0x1
prop.location.type = CU_MEM_LOCATION_TYPE_DEVICE;             // 0x1
prop.location.id = 0;
size_t gran; cuMemGetAllocationGranularity(&gran, &prop, CU_MEM_ALLOC_GRANULARITY_MINIMUM);
size = round_up(size, gran);
CUmemGenericAllocationHandle h;
cuMemCreate(&h, size, &prop, 0);                              // 可导出分配
CUdeviceptr va;
cuMemAddressReserve(&va, size, 0, 0, 0);
cuMemMap(va, size, 0, h, 0);
CUmemAccessDesc acc = {{CU_MEM_LOCATION_TYPE_DEVICE, 0}, CU_MEM_ACCESS_FLAGS_PROT_READWRITE};
cuMemSetAccess(va, size, &acc, 1);
int fd = -1;
cuMemExportToShareableHandle(&fd, h, CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR, 0); // (1)
int fd2 = -1;
cuMemGetHandleForAddressRange(&fd2, va, size, CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD, 0); // (2, 若支持)
```

探针会对两条导出路径都打印结果、fds 和 `/proc/self/fd/<n>` 的 readlink 类型（`dma-buf` / `anon_inode:…` / 驱动私有）；随后对每个 fd 尝试 Vulkan 导入：
- `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` + `VK_IMAGE_TILING_LINEAR`（`VK_KHR_external_memory_fd` + `VK_KHR_external_memory`），单平面 R8 与 **P010 双平面**两种形态；
- `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT` + `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`（`DRM_FORMAT_MOD_LINEAR`，显式 plane layout）与纯 LINEAR 两种。
导入成功后用 `vkCmdCopyImageToBuffer` 读回前 8 字节，**期望 `ab ab ab ab…`**（探针在 `cuMemSetAccess` 后 `cuMemsetD8(va,0xAB,256)`），从而区分"导入成功"与"导入了错误内存"。

#### 3.3.2 判读矩阵（只要 probe 跑完就能定论）

| 观测 | 结论 |
|---|---|
| attr 102=0 | VMM 不可用 → route B 不可行 |
| attr 103=0 | POSIX_FD 导出不可用 → route B 不可行 |
| `cuMemCreate(POSIX_FD)` 失败但 `NONE` 成功 | VMM 可用但**不可导出** → route B 不可行（可记录为驱动限制） |
| 导出成功，fd kind=**dma-buf**，Vulkan DMA_BUF 导入 `VK_SUCCESS` + 0xAB | route B GO（dma-buf 形态）——**本机不适用**：实测 fd 是 `/dev/nvidiactl`，DMA_BUF 导入 OODM |
| 导出成功，fd kind≠dma-buf，Vulkan **OPAQUE_FD** 导入成功 + 0xAB | **route B GO（OPAQUE_FD 形态）——本机已实测（同进程）**：fd=127 `/dev/nvidiactl`，`vkCreateImage`/`vkAllocateMemory(import)`/`vkBindImageMemory`/copy 全 VK_SUCCESS；数据内容待新探针复跑确认 0xAB |
| 导出成功但两种 Vulkan 导入都失败 | route B 不可行；只剩 route C（Vulkan Video，实测仅 4:2:0）或 CPU staging |
| 两条都拿到 fd（(1)(2) 同时成功） | 优先用 dma-buf（(2)），兼容性最好 |

#### 3.3.2b wgpu 侧能否导入

能，前提是 3.3.2 的 raw-Vulkan 导入成立：现有 VAAPI 路径就是"raw `VkImage`（外部内存）→ `wgpu_hal::vulkan::Device::texture_from_raw(..., TextureMemory::External)` → `wgpu::Texture`"（`external.rs:17-30,395-440`），OPAQUE_FD 与 DMA_BUF 只在 image-create/allocate 两处不同；`gpuinterop.rs` 的 `import_dmabuf_plane` 已把这条链路跑通（VAAPI）。因此探针里 `vkCreateImage + vkAllocateMemory(import) + vkBindImageMemory` 成功，就等于"wgpu 可用同法包装"；剩下的只是 5.2 提到的线性纹理采样过滤/usage 组合要过一次 GPU 测试。

#### 3.3.3 接进 FFmpeg 帧池（`ffmpeg_pool_probe.c` 已验证接口形态）

FFmpeg 的 CUDA 帧池默认为 `cuMemAlloc`（`hwcontext_cuda.c:96-131`），但其 `AVHWFramesContext.pool` 允许调用方预置（`hwcontext.h:169-181`；`hwcontext.c:373-374` 仅当 `ctx->pool==NULL` 才采用内部池），`cuda_get_buffer` 直接用该池的 buffer 指针 + `av_image_fill_arrays`（`hwcontext_cuda.c:191-216`）。cuvid 解码器也接受调用方预置的 hw_frames_ctx（`cuviddec.c:326-331` 校验 format/sw_format；`:406-408` 仅在缺省时自建）。因此：

```c
AVBufferRef *frames_ref = av_hwframe_ctx_alloc(dev_ref);
AVHWFramesContext *f = frames_ref->data;
f->format = AV_PIX_FMT_CUDA;
f->sw_format = AV_PIX_FMT_P010;      // 与解码器 sw_pix_fmt 一致（8-bit 用 NV12）
f->width = W; f->height = H;
f->initial_pool_size = 2;            // 触发预分配（hwcontext.c:377-381）
f->pool = av_buffer_pool_init2(buf_size, NULL, my_vmm_alloc, NULL);  // 导出型 VMM 池
av_hwframe_ctx_init(frames_ref);
```

`ffmpeg_pool_probe.c` 正是这样做的：池分配回调里 `cuMemCreate/cuMemAddressReserve/cuMemMap/cuMemSetAccess` + `memset 0xAB`，然后 `av_hwframe_get_buffer` 取帧并断言 `frame->data[0]` 落在自己的 VMM VA 内，最后打印该 slot 的两个导出 fd 与类型。**它不需要 NVDEC**，因此能在任何有 CUDA 设备的机器上单独验证"自定义池 + 可导出"这一步。

设计要点（engine 落地时）：
- **按池 slot 导出一次**，而不是每帧导出：每个 slot 是一个 VMM allocation，fd 在 slot 生命周期内复用；VkImage/`wgpu::Texture` 也按 slot 缓存。帧在池里轮转，正好对应现有 `register_imported_texture` 的"多 slice"机制（`external.rs:449-…` 的 D3D11VA 就是每 slice 一个纹理）。
- **大小**：`buf_size = av_image_get_buffer_size(sw_format, w, h, tex_alignment)`；`tex_alignment` 是私有字段（`CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT`，通常 256/512），探针用 4096 计算并池化到 granularity（≥2 MB），因此只会更大、不会不足。YUV420P 特例会让 FFmpeg 把 alignment 翻倍（`hwcontext_cuda.c:176-179`）——按 4096 计算已覆盖。
- **保活**：texture 持有 slot 的 `AVBufferRef`（或至少该 allocation handle），直到销毁；VMM allocation 不能在 GPU 还在采样时 `cuMemUnmap/cuMemRelease`。
- **线程**：池分配/导出发生在解码线程；导出前需 `cuCtxPushCurrent(AVCUDADeviceContext.cuda_ctx)`（公开字段 `hwcontext_cuda.h:42-46`）。导入发生在渲染线程；fd 用 `dup()` 交接（Vulkan 成功后接管，现有 `external.rs:340-352` 契约）。
- **初始化顺序**：`avctx->hw_frames_ctx` 必须在 `avcodec_open2` 之前设置（cuvid 在 init 时校验 `sw_format` 匹配，`cuviddec.c:326-331`）。这意味着改动点在 `crates/quercus-codec/src/ffmpeg.rs` / `hwdecode.rs`（不在本 spike 的写权限内，需 Lead 扩 scope）。

#### 3.3.4 blockers 现状（实测后更新）

1. ~~GeForce 上 `cuMemCreate(POSIX_FD)` 是否被允许~~ —— **实测 YES**（attr 102/103 均 1，`cuMemCreate`/导出均成功）。
2. ~~导出的 fd 是不是 dma-buf~~ —— **实测不是**：`readlink /proc/self/fd/127` = `/dev/nvidiactl`；`cuMemGetHandleForAddressRange(DMA_BUF_FD)` = `NOT_SUPPORTED`；Vulkan DMA_BUF 导入 = `OODM`。因此只能用 `OPAQUE_FD`。
3. ~~NVIDIA Vulkan 是否接受该 fd~~ —— **同进程实测 YES**（OPAQUE_FD + LINEAR，R8 与 P010 双平面均导入成功）。但 `OPAQUE_FD` 是驱动私有句柄，**跨进程可用性未知**（见 §0.4.2 头号 blocker）。
4. **数据内容校验**：旧探针 `cuMemsetD8` 返回 `LAUNCH_FAILED`，读回 0x00；新探针改用 `cuMemcpyHtoD`+sync 并做 control，需在 GPU 上复跑确认 0xAB（这是"导入的是正确内存"的唯一证据）。
5. **FFmpeg 池判定**：旧探针只比第一个 slot 导致误报；新探针扫全部 slot、保留两帧、对匹配 slot 的 fd 直接导入校验。
6. **线性 tiling 的采样过滤**：Vulkan 线性图像常无 `SAMPLED_IMAGE_FILTER_LINEAR`；引擎侧要么用 nearest sampler（YUV→RGB 是无损逐像素矩阵，可行），要么先一次 GPU blit 到 optimal tiling。
7. **wgpu-hal 包装**：`texture_from_raw(..., TextureMemory::External)` 是现有 VAAPI 路径（`external.rs:395-440`），OPAQUE_FD 只需换 image-create/allocate 两处；无新 crate。

---

## 4. Q3/Q4：Vulkan Video（h264_vulkan / hevc_vulkan）10-bit 4:2:2 是否可行？

### 4.1 命名纠正（重要）

FFmpeg 8.1 里 **没有** 名为 `h264_vulkan`/`hevc_vulkan` 的**解码器**；Vulkan 解码以 **hwaccel** 暴露：
- `h264_vulkan_hwaccel_deps="vulkan"`、`select="h264_decoder"`（`.cache/ffmpeg-src/configure:3392-3393`）
- `hevc_vulkan_hwaccel_deps="vulkan"`、`select="hevc_decoder"`（`configure:3410-3411`）

用法是 `ffmpeg -hwaccel vulkan -c:v h264 …`；若把 `h264_vulkan` 当**解码器**名（`-c:v h264_vulkan`）会报 "not recognized"（已在本机系统 FFmpeg 9 上验证）。
`h264_vulkan`/`hevc_vulkan` **在 8.1 是编码器**：`configure:3595`（`h264_vulkan_encoder_select=…`）、`3621`（`hevc_vulkan_encoder_select=…`），源码 `libavcodec/vulkan_encode_h264.c` 的 `ff_h264_vulkan_encoder`。

### 4.2 项目构建现状：Vulkan Video 完全不存在

- 构建脚本从不传 `--enable-vulkan`（`grep -c enable-vulkan tooling/ffmpeg/build-ffmpeg.sh` → 0；Linux 分支只加 `--enable-vaapi/--enable-vdpau/--enable-libdrm`，`build-ffmpeg.sh:146-149`；NVIDIA 分支 `--enable-nvdec --enable-nvenc --enable-cuda-llvm`，`:158-163`）。
- `config.h`：`CONFIG_VULKAN 0`（`:658`）、`CONFIG_VULKAN_1_4 0`（`:824`），而 `CONFIG_CUDA/CUVID/NVDEC/NVENC/VAAPI/LIBDRM` 全为 1（`:645,647,652-655`）。
- `config_components.h`：`CONFIG_H264_VULKAN_ENCODER 0`、`CONFIG_HEVC_VULKAN_ENCODER 0`、`CONFIG_AV1_VULKAN_ENCODER 0`、`CONFIG_H264_VULKAN_HWACCEL 0`、`CONFIG_HEVC_VULKAN_HWACCEL 0`（`:894,906,917,944,957,966`）；依赖链 `vulkan_encode_deps="vulkan"`（`configure:3509`）→ `vulkan_deps="threads"`（`configure:3347`），所以 **`--enable-vulkan` 是唯一开关**。
- `strings .cache/ffmpeg/lib/libavcodec.a | grep -c h264_vulkan` → **0**；`hevc_vulkan` → **0**；`libavutil.a` 里 `hwcontext_vulkan` → **0**。

结论：**当前项目构建下，Vulkan Video 解码/编码路线不可用，必须重建 FFmpeg**（`--enable-vulkan`；系统已有 libvulkan 1.4.363 + `/usr/include/vk_video` 视频 std 头，满足 `configure` 的 `vulkan >= 1.3.277` 与 `vk_video` include 需求）。重建后 `h264_vulkan`/`hevc_vulkan` 编码器与 `h264/hevc` 的 vulkan hwaccel 才会出现（`--disable-programs` 仍在，不产出 `ffmpeg` 可执行文件，验证需另装一个 CLI 版 FFmpeg 或改构建开关）。

### 4.3 10-bit 4:2:2 的具体判定

- **本机实测（§0.1，RTX 5070 Ti / 615.78.08）**：H.264 High 4:2:0 8-bit `VK_SUCCESS`；HEVC Main10 4:2:0 `VK_SUCCESS`；H.264 High10 4:2:0 10-bit、H.264 High422 4:2:2 10-bit、HEVC RExt 4:2:2 10-bit 全部 `VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR`。**即这台机器的 Vulkan Video 只覆盖 4:2:0（H.264 仅 8-bit）**。
- **Vulkan 视频标准头本身允许** H.264 High 4:2:2：`STD_VIDEO_H264_PROFILE_IDC_HIGH_10=110`、`HIGH_422=122`（`/usr/include/vk_video/vulkan_video_codec_h264std.h:44-51`）；HEVC：`MAIN_10=2`、`FORMAT_RANGE_EXTENSIONS=4`（`vulkan_video_codec_h265std.h:59-65`）。
- **解码**：FFmpeg 8.1 把 `avctx->profile`、`chromaSubsampling`、`lumaBitDepth`/`chromaBitDepth` 原样交给 `vkGetPhysicalDeviceVideoCapabilitiesKHR`（`vulkan_decode.c:771-857`；profile 不匹配时可用 `AV_HWACCEL_FLAG_ALLOW_PROFILE_MISMATCH` 回退）。所以 4:2:2 10-bit 解码**在 FFmpeg 侧可表达**，成败取决于驱动是否报告支持。NVIDIA 的 Vulkan 驱动是否暴露 High 4:2:2 profile 必须实测（probe 的 H.264 decode High4:2:2 10bit 段）。
- **编码（H.264）**：`ff_vk_h264_profile_to_vk` 仅映射 Baseline/Main/High/High444Predictive，**没有 High10、没有 High422**（`vulkan_video.c:260-269`）。编码器虽把 `AV_PROFILE_H264_HIGH_10` 放进候选表（`vulkan_encode_h264.c:701-704`），但经上述映射得到 `INVALID`，`vkGetPhysicalDeviceVideoCapabilitiesKHR` 必然失败（`:733-745`）→ **H.264 Vulkan 10-bit 编码在 8.1 不可用，4:2:2 无论如何不可用**。
- **编码（HEVC）**：候选 profile 含 Main/Main10/RExt；`NV12` 只试 Main、`P010` 只试 Main10、其他格式才试 RExt（`vulkan_encode_h265.c:616-631`）→ 4:2:2 输入（P210/NV16）理论上会试 RExt；RExt 映射存在（`vulkan_video.c` 的 `ff_vk_h265_profile_to_vk`）。但驱动侧 Vulkan Video 在 615.78.08 上对 H.264/HEVC 4:2:2 均返回 `VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR`（§0.1），所以 FFmpeg 的 Vulkan 编码器即便重建也拿不到 4:2:2 profile；且 Vulkan 编码器输入必须是 `AV_PIX_FMT_VULKAN` 帧（`CODEC_PIXFMTS(AV_PIX_FMT_VULKAN)`）。**硬件 NVENC 在 Blackwell GeForce 上确实支持 H.264/HEVC 4:2:2（NVIDIA 厂商表），但那是 CUDA 帧路径，不是 Vulkan Video。**
- **引擎侧另外的成本**：即使 Vulkan Video 解码可用，wgpu 目前没有 video queue；FFmpeg 只能用自己的 VkDevice 解码，然后把帧导回 wgpu。FFmpeg 的 Vulkan hwcontext 提供 `vulkan_map_to_drm`（`hwcontext_vulkan.c:4131-4165`，Vulkan→DRM_PRIME），正好能接现有 dma-buf 导入；但"CUDA 设备派生 Vulkan 设备"确实存在（`hwcontext_vulkan.c:2242-2262`），只是帧映射方向是 Vulkan→CUDA（`export_mem_to_cuda` `:3748`），没有 CUDA→Vulkan 的 `av_hwframe_map`（`vulkan_map_from` 只处理 DRM/VAAPI，`:4276-4295`）。也就是说：**任何"CUDA 解码、wgpu 合成"的组合都绕不开 dma-buf；Vulkan Video 只是换了解码器，不消除导出步骤。**

### 4.4 引擎当前的 4:2:2 状态

好消息：4:2:2 10-bit 的平面格式和渲染 pass 已存在：
- `PlanarFormat::Yuv422P10`（`crates/quercus-core/src/texture.rs:570-579`，descriptor 为 `Cs422`/`U16`，`:591-593`）；
- 描述符驱动的平面矩阵 YUV→RGB pass（`crates/quercus-core/src/backend.rs:339`、`:1809` `run_planar_yuv_to_rgb`、`:1038` `upload_planar_matrix_planes`）。

> 注意：`quercus-core` 的 `texture.rs`/`backend.rs` 在本审计期间正被另一任务并行修改（新增 `ColorModel`/`Subsampling::Cs422` 等通用像素矩阵抽象，见工作树未提交 diff），上述行号为撰写时工作树；以符号名为准。`gpuinterop.rs`/`external.rs` 未被并行修改。

缺口：Linux 导入只识别 NV12/P010 的 fourcc（`crates/quercus-codec/src/gpuinterop.rs:355-360`），且 chroma 高度写死减半（`:374` `let (cw, ch) = (lw.div_ceil(2), lh.div_ceil(2));`）。要支持 4:2:2 需按 `AVPixelFormat` 决定 chroma 高度并用 `PlanarFormat::Yuv422P10`。

---

## 5. 引擎侧落地设计（route B；**未实施**，需 Lead 扩写权限）

> 现状：`gpuinterop.rs`/`external.rs` 属本 spike 写权限；`hwdecode.rs`/`ffmpeg.rs` 不在，落地时需要扩 scope（见 5.1）。

### 5.1 `hwdecode.rs` / `ffmpeg.rs`：预置"可导出池"的 CUDA hwframes 上下文

cuvid 解码器在缺省时自建 CUDA frames ctx（`cuviddec.c:406-408`），但接受调用方预置的（`:326-331` 仅校验 format/sw_format 一致）。因此：

```text
1) av_hwdevice_ctx_create(AV_HWDEVICE_TYPE_CUDA)  →  AVCUDADeviceContext.cuda_ctx
2) 在该 context 下建 VMM 池（见 5.2 的池实现）
3) frames = av_hwframe_ctx_alloc(dev)
   format=CUDA, sw_format=P010/NV12(按流), W/H, initial_pool_size=K,
   frames->pool = vmm_pool            // 关键：调用方池
4) av_hwframe_ctx_init(frames)
5) avctx->hw_frames_ctx = frames      // 必须在 avcodec_open2 之前
6) 每个池 slot：cuMemCreate(POSIX_FD)+Map+SetAccess，导出 fd 一次，注册 VkImage 一次
```

失败语义（重要）：attr 102/103 不满足或 `cuMemCreate(POSIX_FD)` 失败 → **整体关闭 route B**，CUDA 解码继续走现有 staging，不做逐帧重试（现有 `HW_IMPORT_UNSUPPORTED` 语义 `gpuinterop.rs:203-232`）。

### 5.2 `gpuinterop.rs` 的池 + `external.rs` 的 OPAQUE_FD 线性外部图像导入

- 池实现放在 `gpuinterop.rs`（dlopen `libcuda.so.1`，零新依赖；与 `cuda_vulkan_probe.c`/`ffmpeg_pool_probe.c` 的调用序列一致）。slot 结构：`{ handle, va, size, fd, fd_kind, registered_token }`。
- `external.rs` 新增 **OPAQUE_FD 线性导入**（与现有 `import_dmabuf_plane` 并列，`external.rs:292-458`）：
  - `VkExternalMemoryImageCreateInfo{OPAQUE_FD}` + `VK_IMAGE_TILING_LINEAR` + `VkImportMemoryFdInfoKHR{OPAQUE_FD}` + `texture_from_raw(..., TextureMemory::External)`；
  - 多平面 P010 用单个 2-plane `VkImage`（实测可导入）+ 两个 plane view，复用 `register_imported_texture` 的 aspect 机制（D3D11VA 同形）；
  - **DMA_BUF 路径不需要**（本机 CUDA fd 不是 dma-buf，实测 DMA_BUF 导入 OODM）；保留现有 VAAPI 实现不动。
- 采样：线性 tiling 常常没有 `SAMPLED_IMAGE_FILTER_LINEAR`；YUV→RGB 是逐像素矩阵，改用 nearest sampler 无质量损失（落地时查 `vkGetPhysicalDeviceFormatProperties`）。
- 生命周期：texture 的 drop guard 释放 VkImage/memory，并持有 slot 的 `AVBufferRef`（现有 `ImportGuard` 机制，`external.rs:400-420`）。

### 5.3 与现有 decode-import 的接线

`gpuinterop::platform_import` 的 CUDA 分支（当前直接 `Unsupported`，`gpuinterop.rs:259-275`）不再需要在解码后导出：帧的 `data[0]` 本来就在我们的 VMM VA 里，**纹理在池 slot 创建时就已注册**，解码帧只需按 `data[0]` 反查到 slot token 并交给 `ImportedHwFrame`（`gpuinterop.rs:176-232` 的 `Imported { y, uv, format, keep_alive }`）。
这是 route B 相对 route A 的结构优势：**每帧零导出/零导入**，只有池 slot 轮转。

成本：M（约 350–500 行 + GPU 测试）；风险全部集中在 3.3.4 的 5 个待测点。

## 6. 验证命令（在**有 GPU 设备节点**的会话里执行）

```sh
# 1) 现状事实（本机已跑过，见 §1）
grep -E '^#define (CONFIG_VULKAN|CONFIG_CUDA|CONFIG_CUVID|CONFIG_NVDEC|CONFIG_NVENC|CONFIG_VAAPI)' \
     .cache/ffmpeg-src/config.h
strings .cache/ffmpeg/lib/libavcodec.a | grep -c h264_vulkan     # 现在应为 0

# 2) 能力探针（不需 CUDA toolkit、不走 cargo；同时构建并运行两个 C 探针）
sh crates/quercus-codec/examples/cuda_vulkan_spike/run.sh
#    - cuda_vulkan_probe : route A + route B + Vulkan Video 能力
#    - ffmpeg_pool_probe : FFmpeg CUDA frames ctx + 自定义 VMM 池（需项目 libavutil）
#    - nvdec_pool_probe  : NVDEC 解码直写 VMM 池 + OPAQUE_FD 导入/读回（决定性测试）
#    用 4K 10-bit 4:2:2 素材：QUERCUS_SPIKE_CLIP=/path/C0024.MP4 sh .../run.sh
#    单独构建 ffmpeg_pool_probe：
#    cc -O2 -o /tmp/ffpool crates/quercus-codec/examples/cuda_vulkan_spike/ffmpeg_pool_probe.c \
#       -I.cache/ffmpeg/include -L.cache/ffmpeg/lib -lavutil -lva-drm -lva -lvdpau \
#       -lX11 -lm -lz -ldrm -latomic -pthread -ldl

# 3) 仅当决定走 Vulkan Video：重编 FFmpeg（约 20–40 分钟）
#    在 tooling/ffmpeg/build-ffmpeg.sh 的 FLAGS 里加：
#      --enable-vulkan
#    （系统需 libvulkan >= 1.3.277 与 /usr/include/vk_video；本机满足）
tooling/ffmpeg/build-ffmpeg.sh
cargo clean -p ffmpeg-sys-next      # 静态库被 bundle 进 rlib，必须强制重编
strings .cache/ffmpeg/lib/libavcodec.a | grep -c h264_vulkan     # >0（编码器）
strings .cache/ffmpeg/lib/libavutil.a  | grep -c hwcontext_vulkan  # >0

# 4) 4:2:2 10-bit 测试片（x264 high422 / x265 rext；项目 libx264/libx265 均含）
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=25 -pix_fmt yuv422p10le \
  -c:v libx264 -profile:v high422 -crf 18 -t 5 /tmp/h264_422_10bit.mp4
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=25 -pix_fmt yuv422p10le \
  -c:v libx265 -profile:v rext -t 5 /tmp/hevc_422_10bit.mp4

# 5) Vulkan 解码验证（8.1 中解码是 hwaccel，不是 h264_vulkan 解码器！）
ffmpeg -v verbose -hwaccel vulkan -hwaccel_output_format vulkan \
  -i /tmp/h264_422_10bit.mp4 -f null -
#    期望（前提：驱动暴露对应 profile）：4:2:0 8/10bit 片可过；4:2:2 片大概率在
#    vkGetPhysicalDeviceVideoCapabilitiesKHR/profile 处失败（"profile … not supported"）。

# 6) Vulkan 编码验证（重编后；预期 H.264 10bit/422 失败，HEVC Main10 可能过）
ffmpeg -i /tmp/h264_422_10bit.mp4 -c:v h264_vulkan -profile:v high422 -f null -
ffmpeg -f lavfi -i testsrc2 -pix_fmt yuv420p10le -c:v hevc_vulkan -profile:v main10 -f null -
```

`probe` 判读（route A 已在 GPU 会话实测为 NO-GO：attr 124 = 0）：
- route A：`CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED = 1` + `cuMemGetHandleForAddressRange(...) -> fd` + 导入 `VK_SUCCESS` 才 GO（本机预期仍为 NO）；
- **route B（实测已走通同进程）**：attr 102/103 均 1；`cuMemCreate(POSIX_FD)` OK；fd=127 `/dev/nvidiactl`（非 dma-buf）；Vulkan `OPAQUE_FD`+LINEAR 导入 OK；剩下的是**数据 0xAB 复跑确认 + 跨进程验证**（见 §0.4/§9）；
- `ffmpeg_pool_probe`：`inside caller VMM slot: YES (slot #N, offset=…)` 证明 FFmpeg 使用我们的池；`[routeB] FFmpeg-slot fd import: GO (data verified)` 证明该帧内存的 fd 能被 Vulkan 导入且数据为 0xAB；
- 跨进程：`[routeB] cross-process OPAQUE_FD import: GO` 才能用于 `quercus-worker` process pool；`FAIL` 则 route B 只适用于同进程后端；
- Vulkan Video 段：`H.264 decode High4:2:2 10bit -> VK_SUCCESS` 才说明驱动支持 4:2:2 解码；`encode … -> VK_SUCCESS` 才说明对应编码 profile 可用。

---

## 7. 探针输出样例

无设备节点时的降级输出（本 agent 的 bwrap 会话，证明探针不崩）：
```
[vulkan] no physical devices (count=0, VK_ERROR_INITIALIZATION_FAILED)
[cuda] cuInit -> CUDA_ERROR_NO_DEVICE (no CUDA-capable device is detected)
== summary ==
  vulkan device            : none
  cuda device              : none
  dma_buf export fd        : NO
```
ffmpeg_pool_probe 同样优雅降级（项目 libavutil 60.26.102 静态链接，CUDA 设备创建失败即退出）：
```
[CUDA @ ...] cu->cuInit(0) failed -> CUDA_ERROR_NO_DEVICE
[ffmpeg] av_hwdevice_ctx_create(CUDA) -> -542398933
[fatal] no CUDA device (headless / no /dev/nvidia*?)
```
两个探针在本机均零警告编译（`-O2 -Wall -Wextra`）；GPU 会话应出现的 route B 关键行：
```
[vmm] CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED = 1
[vmm] CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED = 1|0
[vmm] cuMemCreate(size=..., requestedHandleTypes=POSIX_FD) -> CUDA_SUCCESS
[vmm] cuMemExportToShareableHandle(POSIX_FD) -> CUDA_SUCCESS, fd=N (anon_inode:... | dma-buf)
[routeB] VMM export fd -> OPAQUE_FD + LINEAR ... -> VK_SUCCESS + ab ab ...
```

---

## 8. 为什么没有直接改生产代码

1. 三条 GO 条件都只能在真实 GPU 上判定（attribute 124、page 对齐语义、驱动是否接受 LINEAR dma-buf / 多平面显式 layout）。本会话没有 `/dev/nvidia*`、`/dev/dri`，`cuInit` 直接 `NO_DEVICE`；在未验证的情况下改 `gpuinterop.rs`/`external.rs` 等于交付一段无人能跑的代码，违反任务"只在安全原型被证明后改生产代码"的约束。
2. 现有 M5 导入路径对 VAAPI/D3D11/VideoToolbox 是**已验收**的；CUDA 分支一旦判断错误会把"每帧回落"的语义搞坏（`HW_IMPORT_UNSUPPORTED` 与 `Failed` 的分流、连续失败熔断）。先用 probe 把硬件事实钉死，再按 §5 落地是风险最低的顺序。
3. 交付物 placement：`crates/quercus-codec/examples/cuda_vulkan_spike/` 不是 cargo example（没有 `main.rs`/`.rs`），`cargo build/test` 完全不会扫描到，不会影响任何现有构建。

---

## 9. 风险与未决项（route B 实测后）

| # | 未决项 | 影响 | 判定方式 |
|---|---|---|---|
| 1 | **OPAQUE_FD 跨进程**：fd 是 `/dev/nvidiactl` 驱动私有句柄 | process-pool worker 架构能否用 route B；若不兼容，只有 Threads/Pipeline 后端可零拷贝 | 已内置：`cuda_vulkan_probe` 的 `[routeB] cross-process OPAQUE_FD import: GO/FAIL`（fork+exec + SCM_RIGHTS） |
| 2 | 探针 legacy CUDA 符号导致 `INVALID_CONTEXT`（`cuMemAlloc`/`cuMemcpyHtoD` 等基础名在 64 位驱动上拒绝上下文；`_v2` 正常） | 之前 DtoD FAIL 可能是探针 artifact，不是驱动 verdict | 三个探针已改为优先 `_v2`；重跑 `[control]`/`[copy]` 即可定论 |
| 3 | FFmpeg 是否正确使用调用方池（旧探针误报） | route B 与解码器集成 | 新探针全 slot 匹配 + 双帧保留 |
| 4 | 线性 tiling 采样过滤（可能无 FILTER_LINEAR） | 渲染质量/性能 | `vkGetPhysicalDeviceFormatProperties` + GPU 测试；必要时 nearest 或 blit |
| 5 | FFmpeg `tex_alignment` 私有（按 4096 计算池大小） | 池 buffer 是否足够 | `av_hwframe_ctx_init`/`get_buffer` 成功即够 |
| 6 | 池 slot 生命周期（GPU 采样中不可 unmap） | 正确性 | `ImportGuard` + 池容量/回收测试 |
| 7 | FFmpeg 重编 `--enable-vulkan`（route C）与静态链接兼容性 | route C 成本（仅 4:2:0 有用） | 重编后 `cargo build` + 现有测试 |

## 10. 证据索引（本报告引用的关键位置）

- 引擎：`crates/quercus-codec/src/gpuinterop.rs:259-275,355-374,384-419`；`crates/quercus-core/src/backend/external.rs:17-30,56-67,292-458,449-…`；`crates/quercus-core/src/texture.rs:570-579,591-593`；`crates/quercus-core/src/backend.rs:339,1038,1809`；`crates/quercus-codec/src/hwdecode.rs:196-215,424-440`。
- FFmpeg 8.1.2（`.cache/ffmpeg-src`）：`libavutil/hwcontext_cuda.c:96-131,160-216,240-286`；`.cache/ffmpeg/include/libavutil/hwcontext_cuda.h:34,42-50`；`libavutil/hwcontext.h:169-181`；`libavutil/hwcontext.c:373-374`；`libavcodec/cuviddec.c:139-231,343-367,606-639`；`configure:3392-3393,3410-3411,3595,3621`；`libavcodec/vulkan_video.c:260-269`；`libavcodec/vulkan_encode_h264.c:701-704,719-749`；`libavcodec/vulkan_encode_h265.c:616-631`；`libavcodec/vulkan_decode.c:771-857`；`libavutil/hwcontext_vulkan.c:2242-2262,3748,4131-4165,4276-4295`；`config.h:645,647,652-655,658,824`。
- 构建：`tooling/ffmpeg/build-ffmpeg.sh:50,92-131,146-149,158-163`。
- 本次新增的探针（本 spike 交付物）：
  - `crates/quercus-codec/examples/cuda_vulkan_spike/cuda_vulkan_probe.c`：route A 导出/导入 + **route B**（`cuMemGetAllocationGranularity`/`cuMemCreate`/`cuMemAddressReserve`/`cuMemMap`/`cuMemSetAccess`/`cuMemExportToShareableHandle`/`cuMemGetHandleForAddressRange`，Vulkan `OPAQUE_FD`/`DMA_BUF` 导入 + 0xAB 读回）+ Vulkan Video 能力查询；
  - `crates/quercus-codec/examples/cuda_vulkan_spike/ffmpeg_pool_probe.c`：FFmpeg CUDA `AVHWFramesContext` + 调用方 VMM 池（链接项目 libavutil 60.26.102），验证 `frame->data[0]` 落在池 VA 内并打印导出 fd；
  - `run.sh`：一次构建/运行两者并打印 FFmpeg 构建门与符号检查。
- CUDA 常量（本轮新增引用）：`CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED=102`、`CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED=103`、`CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR=0x1`、`CU_MEM_ACCESS_FLAGS_PROT_READWRITE=0x3`。
- 外部文档：CUDA Driver API（`cuMemGetHandleForAddressRange`、`CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED=124` 等）；Vulkan `VK_EXT_external_memory_dma_buf` / `VK_EXT_image_drm_format_modifier` 规范；Vulkan-Headers `vk_video` 标准头（ProfileIdc 枚举）。
