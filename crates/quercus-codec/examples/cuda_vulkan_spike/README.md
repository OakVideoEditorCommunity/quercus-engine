# NVIDIA Linux CUDA → Vulkan GPU 导出 spike（go/no-go）

- 任务：task-9。目标：评估/原型 NVDEC(CUDA) 帧在 Linux 上不经 CPU 回读进入 wgpu/Vulkan 的可行路径；允许 GPU 侧拷贝。
- 仓库：`/home/sunyu/Projects/quercus-engine`（workspace 0.5.2）。
- 本文件与 `cuda_vulkan_probe.c` / `run.sh` 是**新增的 spike 资产**；按任务约束，**未修改** `gpuinterop.rs`、`external.rs` 等生产代码（原因见 §8）。
- 项目 FFmpeg：**8.1.2**（`tooling/ffmpeg/build-ffmpeg.sh:50`，源码树 `.cache/ffmpeg-src`）。

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

## 0. 结论摘要（先看这个）

| 路线 | 判定 | 关键条件 |
|---|---|---|
| **A. 直接导出 FFmpeg 现有 CUDA 帧为 DMA-BUF，再导入 wgpu 的 Vulkan 设备** | **条件 GO**（首选，零额外拷贝） | 需同时成立：① `CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED(124)` 为真；② 导出区间满足 host page 对齐（FFmpeg 的帧池大小不是 page 倍数，需要"页内对齐子区间"或自建池）；③ 目标驱动接受 `VK_EXT_external_memory_dma_buf` + `DRM_FORMAT_MOD_LINEAR` 的线性图像导入。三点都由 `probe` 在 GPU 上一次性判定 |
| **B. 自建可导出的 CUDA VMM 帧池，喂给 FFmpeg** | **GO（A 失败时的备选）** | 用 `cuMemCreate(handleTypes=POSIX_FD|…)` + `cuMemMap` + `cuMemSetAccess` 分配；FFmpeg 允许用户预置 `AVHWFramesContext.pool`（`hwcontext.h:169-181`，`hwcontext.c:373-374`），于是 NVDEC→帧的既有 `cuMemcpy2DAsync` 直接落进可导出内存；再 `cuMemExportToShareableHandle`/范围导出 → Vulkan `OPAQUE_FD` 或 `DMA_BUF` 导入。成本高于 A，但不依赖 `cuMemAlloc` 可导出 |
| **C. Vulkan Video 解码（`-hwaccel vulkan`）** | **当前构建 NO-GO**；重编 FFmpeg 后为 **条件 GO（4:2:0 8/10bit，前提是驱动暴露该 profile）** | 项目构建 `CONFIG_VULKAN=0`（`.cache/ffmpeg-src/config.h:658`、`824`），`libavcodec.a` 里 `h264_vulkan`/`hevc_vulkan` 字符串为 0，`libavutil.a` 无 `hwcontext_vulkan`。FFmpeg 8.1 中 Vulkan 解码是 **hwaccel**（`configure:3392-3393`、`3410-3411`），不是名为 `h264_vulkan` 的 decoder（见 §4.1 命名纠正）。需 `--enable-vulkan` 重编；4:2:2 是否可行由驱动能力查询决定 |
| **D. Vulkan Video 编码（`h264_vulkan`/`hevc_vulkan`）做 10-bit 4:2:2 导出** | **NO-GO** | FFmpeg 8.1 的 H.264 profile 映射函数**没有 High10 / High422** 分支（`vulkan_video.c:260-269`），编码器已知 profile 表止于 High10 且经该映射→`INVALID`（`vulkan_encode_h264.c:701-704,719`），4:2:2 根本无法表达；HEVC 侧有 RExt profile（`vulkan_encode_h265.c:616-631`），但 GeForce/RTX 5070 Ti 的 NVENC 是否支持 4:2:2 需 probe 的 HEVC RExt 编码能力查询确认（预期不支持，属推断） |

**一句话**：数据不离 GPU 的可行主线是 **A（cuMemAlloc 帧 → DMA-BUF → Vulkan 线性多平面图像）**，B 作为保底；Vulkan Video 只能等 FFmpeg 重建，而且 4:2:2 在 Vulkan 视频路径上大概率不可用。

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

- **Vulkan 视频标准头本身允许** H.264 High 4:2:2：`STD_VIDEO_H264_PROFILE_IDC_HIGH_10=110`、`HIGH_422=122`（`/usr/include/vk_video/vulkan_video_codec_h264std.h:44-51`）；HEVC：`MAIN_10=2`、`FORMAT_RANGE_EXTENSIONS=4`（`vulkan_video_codec_h265std.h:59-65`）。
- **解码**：FFmpeg 8.1 把 `avctx->profile`、`chromaSubsampling`、`lumaBitDepth`/`chromaBitDepth` 原样交给 `vkGetPhysicalDeviceVideoCapabilitiesKHR`（`vulkan_decode.c:771-857`；profile 不匹配时可用 `AV_HWACCEL_FLAG_ALLOW_PROFILE_MISMATCH` 回退）。所以 4:2:2 10-bit 解码**在 FFmpeg 侧可表达**，成败取决于驱动是否报告支持。NVIDIA 的 Vulkan 驱动是否暴露 High 4:2:2 profile 必须实测（probe 的 H.264 decode High4:2:2 10bit 段）。
- **编码（H.264）**：`ff_vk_h264_profile_to_vk` 仅映射 Baseline/Main/High/High444Predictive，**没有 High10、没有 High422**（`vulkan_video.c:260-269`）。编码器虽把 `AV_PROFILE_H264_HIGH_10` 放进候选表（`vulkan_encode_h264.c:701-704`），但经上述映射得到 `INVALID`，`vkGetPhysicalDeviceVideoCapabilitiesKHR` 必然失败（`:733-745`）→ **H.264 Vulkan 10-bit 编码在 8.1 不可用，4:2:2 无论如何不可用**。
- **编码（HEVC）**：候选 profile 含 Main/Main10/RExt；`NV12` 只试 Main、`P010` 只试 Main10、其他格式才试 RExt（`vulkan_encode_h265.c:616-631`）→ 4:2:2 输入（P210/NV16）理论上会试 RExt；RExt 映射存在（`vulkan_video.c` 的 `ff_vk_h265_profile_to_vk`）。但 GeForce NVENC 的 4:2:2 支持历来仅限专业卡（推断），且 FFmpeg 的 Vulkan 编码器输入必须是 `AV_PIX_FMT_VULKAN` 帧（`CODEC_PIXFMTS(AV_PIX_FMT_VULKAN)`），需要先上传/转换到 Vulkan 帧。→ **实际可用性由 probe 的 HEVC RExt encode 能力查询决定，预期 NO-GO。**
- **引擎侧另外的成本**：即使 Vulkan Video 解码可用，wgpu 目前没有 video queue；FFmpeg 只能用自己的 VkDevice 解码，然后把帧导回 wgpu。FFmpeg 的 Vulkan hwcontext 提供 `vulkan_map_to_drm`（`hwcontext_vulkan.c:4131-4165`，Vulkan→DRM_PRIME），正好能接现有 dma-buf 导入；但"CUDA 设备派生 Vulkan 设备"确实存在（`hwcontext_vulkan.c:2242-2262`），只是帧映射方向是 Vulkan→CUDA（`export_mem_to_cuda` `:3748`），没有 CUDA→Vulkan 的 `av_hwframe_map`（`vulkan_map_from` 只处理 DRM/VAAPI，`:4276-4295`）。也就是说：**任何"CUDA 解码、wgpu 合成"的组合都绕不开 dma-buf；Vulkan Video 只是换了解码器，不消除导出步骤。**

### 4.4 引擎当前的 4:2:2 状态

好消息：4:2:2 10-bit 的平面格式和渲染 pass 已存在：
- `PlanarFormat::Yuv422P10`（`crates/quercus-core/src/texture.rs:570-579`，descriptor 为 `Cs422`/`U16`，`:591-593`）；
- 描述符驱动的平面矩阵 YUV→RGB pass（`crates/quercus-core/src/backend.rs:339`、`:1809` `run_planar_yuv_to_rgb`、`:1038` `upload_planar_matrix_planes`）。

> 注意：`quercus-core` 的 `texture.rs`/`backend.rs` 在本审计期间正被另一任务并行修改（新增 `ColorModel`/`Subsampling::Cs422` 等通用像素矩阵抽象，见工作树未提交 diff），上述行号为撰写时工作树；以符号名为准。`gpuinterop.rs`/`external.rs` 未被并行修改。

缺口：Linux 导入只识别 NV12/P010 的 fourcc（`crates/quercus-codec/src/gpuinterop.rs:355-360`），且 chroma 高度写死减半（`:374` `let (cw, ch) = (lw.div_ceil(2), lh.div_ceil(2));`）。要支持 4:2:2 需按 `AVPixelFormat` 决定 chroma 高度并用 `PlanarFormat::Yuv422P10`。

---

## 5. 引擎侧最小改动设计（**未实施**，等 probe 放行）

### 5.1 路线 A：`gpuinterop.rs` 的 CUDA 分支（约 150–220 行）

```rust
// 伪代码，真实实现需在 Linux 上 cfg-gate；不新增 crate 依赖：
// dlopen("libcuda.so.1") + dlsym 以下符号（与 FFmpeg 的 cudl 同款做法）
unsafe extern "C" {
    fn cuCtxPushCurrent(ctx: *mut c_void) -> i32;
    fn cuCtxPopCurrent(ctx: *mut *mut c_void) -> i32;
    fn cuMemGetHandleForAddressRange(handle: *mut c_int, dptr: u64, size: usize,
                                     handle_type: i32, flags: u64) -> i32;
}
// 1) 从 frame->hw_frames_ctx->device_ctx->hwctx 取 AVCUDADeviceContext.cuda_ctx
// 2) 页对齐区间 + dup(fd)
// 3) ctx.import_dmabuf_multplane(...)  // external.rs 新增（见 5.2）
```
- 读取 `AVFrame.data[0]`/`linesize[0..2]` 作为 plane layout；`AVCUDADeviceContext` 公开字段足以定位 CUcontext（`hwcontext_cuda.h:42-46`）。
- 失败分类：`CUDA_ERROR_NOT_SUPPORTED`/`INVALID_VALUE` → `ImportOutcome::Unsupported`，不喂熔断；其它 → `Failed`（保持现有语义 `gpuinterop.rs:203-232`）。
- 配置键：新增 `GpuDecodeImportCuda`，env `QUERCUS_GPU_IMPORT_CUDA=0` 关闭，风格对齐 `gpuinterop.rs:77-104`。

### 5.2 路线 A：`external.rs` 的多平面 DMA-BUF 导入（约 120–180 行）

新增 `import_dmabuf_multplane(width, height, vk_format, planes: &[DmaBufPlane], keep_alive) -> Result<u64>`：
- 语义与现有 `import_dmabuf_plane` 相同（`external.rs:292-458`），区别是 `VkImageDrmFormatModifierExplicitCreateInfoEXT.drmFormatModifierPlaneCount = 2` + 两个 `VkSubresourceLayout`（offset=plane1.offset，rowPitch=plane.pitch）；
- 一次 `vkAllocateMemory`（`DMA_BUF_EXT` import）绑定整幅图，然后按 plane 各注册一次纹理 token（`ImportedTextureDesc.aspect` 选 `PLANE_0/PLANE_1`，与 Windows 的 `import_d3d11_shared_texture` 同一套 `register_imported_texture` 机制：`external.rs:449-…`、`gpuinterop.rs:433-505`）；
- 若驱动拒绝显式 LINEAR plane layout（`VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`），退化为"每平面 R8/RG8 导入"（需要 `vkBindImageMemory` offset 对齐，probe 验证）。

### 5.3 路线 B 的额外工作（若 A 的 attr 124 为假）

- 用 `cuMemGetAllocationGranularity`/`cuMemCreate`/`cuMemMap`/`cuMemSetAccess` 建池，`handleTypes` 取 `CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR`（或 dma-buf 能力允许时）；
- 在 `av_hwframe_ctx_init` 之前设置 `AVHWFramesContext.pool`（`hwcontext.h:169-181`）和 `initial_pool_size`（`hwcontext.c:377-381` 会预分配）；
- 导出用 `cuMemExportToShareableHandle` 或 `cuMemGetHandleForAddressRange`；导入同样走 DMA_BUF，或 `OPAQUE_FD`（同一驱动）。

成本：A ≈ M（2 个文件、~300 行 + probe 验证）；B 额外 M（自建帧池 + 生命周期）。

---

## 6. 验证命令（在**有 GPU 设备节点**的会话里执行）

```sh
# 1) 现状事实（本机已跑过，见 §1）
grep -E '^#define (CONFIG_VULKAN|CONFIG_CUDA|CONFIG_CUVID|CONFIG_NVDEC|CONFIG_NVENC|CONFIG_VAAPI)' \
     .cache/ffmpeg-src/config.h
strings .cache/ffmpeg/lib/libavcodec.a | grep -c h264_vulkan     # 现在应为 0

# 2) 能力探针（不需 CUDA toolkit、不走 cargo）
sh crates/quercus-codec/examples/cuda_vulkan_spike/run.sh

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

`probe` 判读：
- `CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED = 1` + 某个 `cuMemGetHandleForAddressRange(...) -> CUDA_SUCCESS, fd=N` → 路线 A 的导出侧成立；
- `dma_buf import extension = YES` + 导入测试 `VK_SUCCESS`/`plane0 -> buffer readback -> VK_SUCCESS` → 路线 A 全通（GO）；
- 只有 `OPAQUE_FD` 成功而 `DMA_BUF` 失败 → 走路线 B 的 fd 形态；
- Vulkan Video 段：`H.264 decode High4:2:2 10bit -> VK_SUCCESS` 才说明驱动支持 4:2:2 解码；`encode … -> VK_SUCCESS` 才说明对应编码 profile 可用。

---

## 7. 探针输出样例（本会话，无设备节点，作为降级验证）

```
[vulkan] no physical devices (count=0, VK_ERROR_INITIALIZATION_FAILED)
[cuda] cuInit -> CUDA_ERROR_NO_DEVICE (no CUDA-capable device is detected)
== summary ==
  vulkan device            : none
  cuda device              : none
  dma_buf export fd        : NO
```
编译（本机通过，无警告）：
```
cc -O2 -Wall -Wextra -o /tmp/p cuda_vulkan_probe.c -ldl -lvulkan
```

---

## 8. 为什么没有直接改生产代码

1. 三条 GO 条件都只能在真实 GPU 上判定（attribute 124、page 对齐语义、驱动是否接受 LINEAR dma-buf / 多平面显式 layout）。本会话没有 `/dev/nvidia*`、`/dev/dri`，`cuInit` 直接 `NO_DEVICE`；在未验证的情况下改 `gpuinterop.rs`/`external.rs` 等于交付一段无人能跑的代码，违反任务"只在安全原型被证明后改生产代码"的约束。
2. 现有 M5 导入路径对 VAAPI/D3D11/VideoToolbox 是**已验收**的；CUDA 分支一旦判断错误会把"每帧回落"的语义搞坏（`HW_IMPORT_UNSUPPORTED` 与 `Failed` 的分流、连续失败熔断）。先用 probe 把硬件事实钉死，再按 §5 落地是风险最低的顺序。
3. 交付物 placement：`crates/quercus-codec/examples/cuda_vulkan_spike/` 不是 cargo example（没有 `main.rs`/`.rs`），`cargo build/test` 完全不会扫描到，不会影响任何现有构建。

---

## 9. 风险与未决项（必须 GPU 验证）

| # | 未决项 | 影响 | 如何判定 |
|---|---|---|---|
| 1 | `CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED` 在 RTX 5070 Ti + 615.78.08 上是否为真 | 决定路线 A 是否存在 | probe CUDA 段 |
| 2 | `cuMemAlloc` 区间"内部页对齐子区间"能否导出（非整段） | 决定是否需要自建池（B） | probe 的 3 个导出用例 |
| 3 | NVIDIA Vulkan 是否接受 `VK_EXT_external_memory_dma_buf` + `DRM_FORMAT_MOD_LINEAR` 的 R8/RG8/R16/RG16 及多平面 P010 导入 | 决定 A 的 Vulkan 侧形状 | probe import caps + import 测试 |
| 4 | 驱动是否支持 H.264 High 4:2:2（解码 caps）与 HEVC RExt 4:2:2（解码/编码 caps） | 决定 4:2:2 是否可能 | probe Vulkan Video 段 |
| 5 | FFmpeg 重编 `--enable-vulkan` 后是否与静态链接/其他滤镜冲突 | 决定路线 C 成本 | 重编后 `cargo build` + 现有测试 |
| 6 | 多平面 LINEAR 导入在 wgpu-hal `TextureMemory::External` 下采样是否正确（YUV→RGB pass） | 决定画质/正确性 | 导入后读数 + 现有 GPU 测试（`QUERCUS_REQUIRE_GPU=1`） |

## 10. 证据索引（本报告引用的关键位置）

- 引擎：`crates/quercus-codec/src/gpuinterop.rs:259-275,355-374,384-419`；`crates/quercus-core/src/backend/external.rs:17-30,56-67,292-458,449-…`；`crates/quercus-core/src/texture.rs:570-579,591-593`；`crates/quercus-core/src/backend.rs:339,1038,1809`；`crates/quercus-codec/src/hwdecode.rs:196-215,424-440`。
- FFmpeg 8.1.2（`.cache/ffmpeg-src`）：`libavutil/hwcontext_cuda.c:96-131,160-216,240-286`；`.cache/ffmpeg/include/libavutil/hwcontext_cuda.h:34,42-50`；`libavutil/hwcontext.h:169-181`；`libavutil/hwcontext.c:373-374`；`libavcodec/cuviddec.c:139-231,343-367,606-639`；`configure:3392-3393,3410-3411,3595,3621`；`libavcodec/vulkan_video.c:260-269`；`libavcodec/vulkan_encode_h264.c:701-704,719-749`；`libavcodec/vulkan_encode_h265.c:616-631`；`libavcodec/vulkan_decode.c:771-857`；`libavutil/hwcontext_vulkan.c:2242-2262,3748,4131-4165,4276-4295`；`config.h:645,647,652-655,658,824`。
- 构建：`tooling/ffmpeg/build-ffmpeg.sh:50,92-131,146-149,158-163`。
- 外部文档：CUDA Driver API（`cuMemGetHandleForAddressRange`、`CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED=124` 等）；Vulkan `VK_EXT_external_memory_dma_buf` / `VK_EXT_image_drm_format_modifier` 规范；Vulkan-Headers `vk_video` 标准头（ProfileIdc 枚举）。
