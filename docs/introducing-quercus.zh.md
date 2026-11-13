# Quercus：一个可以嵌进你自己软件的剪辑引擎

Linux 上似乎没有一个专业级的开源视频编辑器。现有的选项也许很多，但没有一个做到专业级。当时我 Fork 了 Olive，创建了 [Oak Video Editor](https://github.com/OakVideoEditorCommunity/oak)，目标是做一个对标达芬奇的剪辑软件。今天，我把其中的视频剪辑引擎单独抽了出来，你可以基于它定制一个自己的剪辑软件。

Quercus 就是它：一个 headless 的非线性视频编辑引擎，一个可以链接的动态库，一个安全的 Rust SDK。项目以 GPL-3.0-or-later 开源。它不提供界面，也不是拿来即用的软件，只负责剪辑这件事本身。Oak 的编辑器、CLI 和渲染 worker 现在都通过它接入。

做剪辑软件，麻烦的从来不是把时间线画出来。画完之后才是正题：帧时间得用有理数，浮点累加会漂；撤销要能回滚任意一步，解码尽量走硬件，导出要能中途取消，插件崩了不该把主进程带走。这些坑在 Oak 里都踩过一遍，现在收进了引擎。

## 引擎里有什么

时间线是一切的中心。工程里有素材箱、文件夹、序列；序列里是视频轨、音频轨、字幕轨；轨上是片段和转场。嵌套序列、多机位、调整层、变速、标记、工作区这些都有。

时间线上的操作是语义级的。分割、波纹修剪、roll、slide、slip、跨轨移动、波纹删除、区域删除、在接缝加转场，这些在 SDK 里对应 74 个 `EditCommand`。执行一条就是一步撤销，返回一个全局 revision。引擎知道你在剪片子，而不是在改一堆坐标。

素材和解码这块，引擎静态链接 FFmpeg 8.1。视频有 H.264、H.265、ProRes、DNxHD、CineForm、AV1、VP9、OpenEXR、PNG/TIFF，音频有 AAC、Opus、Vorbis、FLAC、PCM，字幕能出 SRT。硬件解码按平台走 hwaccel：macOS 用 VideoToolbox，Linux 用 VA-API，Windows 用 D3D11VA，Linux 和 Windows 后面还有 CUDA 备选，起不来就回软件解码。Windows 的 Vulkan 后端直接导入 D3D11 平面，GL 后端可以通过 D3D11 VideoProcessor 和 WGL interop 转成 RGBA8，驱动不支持时才走 CPU staging。软件编解码使用有界的 CPU 多线程，硬件编解码固定单线程。代理有完整的状态机（未生成、生成中、可用、失败），可以按素材生成、删除、启用。

音频上，PortAudio 的设备管理、重采样、格式转换、电平表、波形都在。另外有两个估计函数，做双系统同期声对齐用的：包络偏移和拉伸偏移。机内麦加独立录音机是独立制作里最常见的配置，这块不用自己写互相关。

效果挂在节点图上，有标准参数、关键帧、贝塞尔插值、参数曲线，效果链的插入、移除、移动、启用都能撤销。OpenFX 宿主这块，扫目录、列插件、创建实例、参数自省、interact、GL bridge、进度和取消、headless 渲染，都有。调色走 OCIO，默认工作空间 ACEScg，LUT、显示 ICC、display/view/look 都在。

渲染管理器有四种跑法：调用线程内联、进程内线程 pipeline、进程隔离的 worker 池，以及环境变量决定的默认值。GPU 后端是 wgpu，Metal、Vulkan、GL、DX12 都能用；没有 GPU 就走 CPU 的 F32 RGBA。CPU 的缩放、合成和像素转换会按行分带多线程处理，GPU 渲染仍走单线程提交。预览是一个带传输控制的会话，play、pause、seek、step、rate、tick 都有，帧请求带 generation，seek 之后旧帧的代次对不上就丢掉。

存盘方面，存储后端可以注册，文件后端和工程库后端都在，工程能绑到库上。交换格式支持 OpenTimelineIO 和 FCPXML 的导入导出，跟 Premiere、Resolve 来回倒不用自己写转换。

除了 OFX，引擎还有一套自己的扩展插件协议。插件注册 id、名字、版本、作者，挂几条带菜单路径的命令，宿主枚举出来建菜单，点了回调给插件。功能型插件完全可以在没有界面的情况下跑批处理。

## 为什么是 C ABI

C ABI 这层主要是给 Rust 之外的语言用的。C、C++、Python、Go、C#、Swift、Flutter、Unity 都能直接链这个动态库。要做 CLI 的话，也可以不背静态链接的体积。

Rust SDK 包在这层上面。它用 `#[link(name = "quercus_engine")]` 链动态库，只调 `extern "C"`，不碰引擎的 Rust API。所以 Rust 用户和 C 用户走的是同一条路，SDK 的测试跑到哪儿，ABI 就被踩到哪儿。

几个约定：

- 句柄是不透明的 `void*`，都配 `add_ref` / `release`，传 `NULL` 是 no-op。
- 字符串传 buf/len，返回该有的长度，不用先问长度再分配。
- 错误是返回码加错误缓冲，不会跨 C 边界 unwind。
- 枚举值是 ABI 的一部分，大版本内只加不改。

头文件是生成的。`quercus_engine.h` 从 Rust 那边的 extern 声明出来，CI 跑 `--check`，改了声明忘了重生成，构建直接红。

时间上还有一个容易踩的坑：引擎用 `{int64_t num, den}` 的有理数，`0/0` 表示空。编辑命令里带 `_ts` 的字段是序列时基下的帧号，两种时间不能混用。

## 写一段看看

前置只有一步：FFmpeg 必须用项目里的脚本构建，不能指向系统、Homebrew 或者发行版的 FFmpeg。引擎静态链接的是一份固定的 FFmpeg 8.1，混用会链接失败，或者更糟，跑起来加载了错的库。

```sh
tooling/install-deps.sh
tooling/ffmpeg/build-ffmpeg.sh
cargo add quercus-engine-rs
```

```rust
use quercus_engine_rs::codec::{Codec, EncodingParams, Format};
use quercus_engine_rs::edit::EditCommand;
use quercus_engine_rs::editor::{EditorSession, TrackKind};
use quercus_engine_rs::effect;
use quercus_engine_rs::render::{self, RenderBackend};
use quercus_engine_rs::task;

fn main() -> Result<(), String> {
    // inline 后端不需要 worker，先把流程跑通
    let _ = render::manager_init(RenderBackend::Threads);

    let session = EditorSession::new().map_err(|e| e.to_string())?;
    let seq = session.create_sequence("Demo").map_err(|e| e.to_string())?;

    // in_ts / out_ts 是序列时基下的帧号，不是秒
    let footage = session
        .execute(EditCommand::ImportFootage { path: "clip.mp4".into() })?
        .primary
        .ok_or("import failed")?;

    let clip = session
        .execute(EditCommand::PlaceFootageClip {
            footage,
            kind: TrackKind::Video,
            track_index: 0,
            in_ts: 0,
            out_ts: 50,
            media_in_ts: 0,
        })?
        .primary
        .ok_or("place failed")?;

    // 效果 id 从 factory_entries() 里拿
    let effect_type = effect::factory_entries()
        .into_iter()
        .find(|e| e.flags & effect::NODE_FLAG_VIDEO_EFFECT != 0)
        .map(|e| e.type_id)
        .ok_or("no effect")?;

    let effect = session
        .execute(EditCommand::EffectInsert {
            host: clip,
            index: usize::MAX, // 放到链尾
            type_id: effect_type,
        })?
        .primary
        .ok_or("effect failed")?;

    let mut params = EncodingParams::new();
    params.set_filename("out.mp4").map_err(|e| e.to_string())?;
    params.format = Format::MPEG4Video as i32;
    params.video_enabled = 1;
    params.video_codec = Codec::H264 as i32;
    params.video_width = 1920;
    params.video_height = 1080;
    params.video_time_base_num = 1;
    params.video_time_base_den = 25;
    params.audio_enabled = 1;
    params.audio_codec = Codec::AAC as i32;
    params.audio_sample_rate = 48_000;
    params.audio_channel_layout = 0x3;

    let export = task::export(&session.project(), seq, "out.mp4", &params)
        .ok_or("export failed to start")?;
    if !export.wait() {
        return Err(export.error());
    }

    // 顺手改一下，再撤销
    session.execute(EditCommand::EffectSetEnabled { effect, enabled: false })?;
    quercus_engine_rs::node::undo();
    Ok(())
}
```

C 侧最小的是这样，编译命令在 `examples/c/README.md` 里：

```c
#include <stdio.h>
#include "quercus_engine.h"

int main(void) {
    void *session = quercus_editor_new();
    if (session == NULL) {
        fprintf(stderr, "quercus_editor_new failed\n");
        return 1;
    }
    uint64_t seq = quercus_editor_create_sequence(session, (const uint8_t *)"Demo", 4);
    printf("sequence %llu\n", (unsigned long long)seq);
    quercus_editor_release(session);
    return 0;
}
```

打包的时候把 `libquercus_engine` 放在可执行文件旁边（或者 `lib/`），rpath 指过去。macOS 是 `@loader_path`，Linux 是 `$ORIGIN`，Windows 直接把 DLL 放 exe 旁边。

## 能拿它做什么

最直接的是做一个自己的剪辑软件。Oak 就是现成的例子，引擎该给的东西都给到，剩下的是产品判断。

另一个方向是往已有的产品里加剪辑。监控、庭审、体育、教育、医疗录播，这些产品要的不是一个完整的 NLE，而是把几段素材拼起来、加个字幕、导出去。引擎把它们从解码到导出的整条路都铺好了，界面可以做得非常窄。

再远一点是让程序自己剪。把 74 个 `EditCommand` 做成模型的工具调用 schema，模型读工程快照，吐命令序列，执行完用 `render_frame_f32` 抽几帧回灌给多模态模型检查，不满意就 undo 重来。比让模型直接写 ffmpeg 命令靠谱的地方是：命令是语义的，改的是“把这段往前挪两秒”“在接缝加个转场”，每一步能撤销，也能 diff 出来给人确认。剪辑本来就是一串可回退的决策，恰好和 undo 栈对得上。

还有一类用法是把引擎当工具，不做编辑器。素材入库的时候抽缩略图和波形，做素材库；交付前用 CPU 渲染同一帧，跟基准帧比对，查黑帧、花屏、字幕错位。调色师想把一条 look 套到两百个镜头上，写个小工具扫目录就行，不用开剪辑软件；视频算法团队可以把缩放、色彩转换和效果渲染放进 CI，用生成器造测试图，渲染出来做像素比对。

批量的活也适合它。婚礼快剪、课程、电商短视频，结构是固定的，每天换素材。把模板存成工程，换素材，导出 N 条。没有界面依赖，塞进一个 worker 里慢慢跑就行。

## 现在的状态

0.5 系列，1.0 之前 API 还会变。仓库里 2,782 个测试，`cargo llvm-cov` 的行覆盖是 89.09%，去掉只用于测试的文件是 89.73%。头文件的生成有 CI 把关，macOS、Linux、Windows 三边都在构建。

用之前要知道的几件事：

- 没有界面。轨道怎么画、快捷键怎么设，都是你的事。
- FFmpeg 得自己构建，第一次编译不快。
- 撤销栈是进程级共享的。一个进程里开多个文档，需要你自己协调，或者干脆多进程。
- `NodeId` 跨存盘不保证还是同一个，重新打开后要从快照重新解析。
- 许可证是 GPL-3.0-or-later。分发链接了它的产品，要按 GPLv3 提供源码；内部使用和 SaaS 不触发分发条款，但改过的引擎对外分发仍然要开源。要闭源分发的话，先看许可证，或者来找我聊。

如果你只是想转个码、裁个片头，ffmpeg 就够了，不用上这个。

## 链接

- 仓库：<https://github.com/OakVideoEditorCommunity/quercus-engine>
- SDK：<https://crates.io/crates/quercus-engine-rs>
- 参考实现：<https://github.com/OakVideoEditorCommunity/oak>
- C 头文件：`crates/quercus-engine/include/quercus_engine.h`
- 构建说明：[`docs/build.md`](build.md)
- CLA：<https://github.com/OakVideoEditorCommunity/quercus-engine/blob/main/CLA.md>

用它做了什么，或者哪里别扭，开 issue 或者直接告诉我都行。
