# OpenFX Windows GPU 零拷贝：手工测试计划

状态：Windows 为**实验性、默认关闭**；本计划只做真机验收，不改变 Linux
默认行为。
目标机器：借来的 Windows 10/11 64 位电脑，**没有 Rust / Visual Studio**。
所有可执行文件由 Linux 侧用 `cargo-xwin` 交叉编译，现场只需要运行
`.exe` 和 PowerShell 脚本。

## 1. 测试目标

分三层，任何一层失败都停止并记录：

1. 机器是否有真实的厂商 GPU 驱动（不是 Microsoft Basic Display）；
2. 驱动是否暴露 WGL/OpenGL 的 `GL_EXT_memory_object_win32` 以及 Vulkan
   `VK_KHR_external_memory_win32`；
3. Quercus 的 Vulkan 纹理 ↔ GL 内存对象路径是否真的能完成
   “同一块显存、无 CPU 回读”的输入/输出闭环。

## 2. 测试包

现场使用 `dist/windows-zero-copy-test-kit/`：

| 文件 | 用途 |
|---|---|
| `quercus-win-gl-probe.exe` | 独立 WGL/GL 扩展探针，不需要 Rust/VS |
| `run-probe.ps1` | 采集系统信息 + 运行探针 + 写 `probe.log` |
| `README.txt` | 现场操作说明 |
| `sha256.txt` | 校验和 |

这个探针**不链接 Quercus**，只验证驱动能力。真正的 Quercus 零拷贝
测试需要第二个预构建包（见第 6 节），不要用探针代替。

## 3. 现场前置检查

以普通用户运行 PowerShell：

```powershell
Get-CimInstance Win32_VideoController |
  Select-Object Name,DriverVersion,Status

Get-PnpDevice -Class Display |
  Select-Object FriendlyName,Status,Problem

Test-Path "$env:WINDIR\System32\vulkan-1.dll"
```

通过条件：

- Display 设备是 NVIDIA / AMD / Intel 厂商适配器；
- **不能**是 `Microsoft Basic Display Adapter`；
- `DriverVersion` 非空；
- 推荐 `vulkan-1.dll` 存在（没有 Vulkan 运行时的机器直接不具备条件）。

如果显示的是 Microsoft Basic Display，先装厂商驱动，不要继续。

## 4. 阶段 1：WGL / GL 扩展探针

在测试包目录运行：

```powershell
powershell -ExecutionPolicy Bypass -File .\run-probe.ps1
```

记录 `probe.log`。

期望：

```text
GL_VERSION=4.x
GL_RENDERER=<厂商 GPU 名称>
GL_VENDOR=<NVIDIA/AMD/Intel>
GL_EXT_memory_object_win32_functions=true
GL_EXT_memory_object_win32_listed=true
```

判定：

- `wglCreateContextAttribsARB failed`：没有真实 GL 3.2+ 驱动，或当前是
  RDP/远程会话，或只有 Microsoft Basic Display；停止。
- `GL_EXT_memory_object_win32_functions=false`：该驱动不支持 Windows
  零拷贝，停止。
- `GL_RENDERER` 不是目标 GPU（例如出现 Remote Display/QXL）：换到物理
  控制台或调整首选 GPU 后重跑。
- 两项都为 true：进入阶段 2。

## 5. 阶段 2：Quercus Vulkan ↔ GL 闭环

需要 Linux 侧预先交叉编译的 `quercus-zerocopy-probe.exe`（同一个测试
包或单独包）。它做：

1. 创建 wgpu Vulkan 设备；
2. 创建 `Rgba32Float`、`OPAQUE_WIN32` exportable 纹理；
3. `vkGetMemoryWin32HandleKHR` 导出 NT handle；
4. WGL 隐藏窗口 + `GL_EXT_memory_object_win32` 导入；
5. GL 写入已知颜色，`glFinish`；
6. Vulkan `download` 读回，验证 GL 写入可见；
7. wgpu 上传已知颜色，GL 回读，验证反方向可见；
8. 输出一行 `PASS`/`FAIL` 和每步结果。

运行：

```powershell
$env:QUERCUS_OFX_GPU_ZEROCOPY = "1"
.\quercus-zerocopy-probe.exe | Tee-Object -FilePath .\zerocopy.log
```

通过条件：

- `vulkan_export=PASS`
- `gl_import=PASS`
- `gl_write_vulkan_read=PASS`
- `vulkan_write_gl_read=PASS`
- 颜色误差 < 1e-4
- 进程退出码 0

## 6. 阶段 3：OFX 插件集成（可选，取决于预构建包）

如果 Linux 侧能提供完整的预构建 OFX 测试包：

```powershell
$env:QUERCUS_OFX_GPU_ZEROCOPY = "1"
$env:QUERCUS_GPU_TESTS = "1"
$env:QUERCUS_REQUIRE_GPU = "1"
cargo test -p quercus-ofx-plugin --test gl_render_test `
  gl_plugin_renders_zero_copy -- --nocapture
```

**现场没有 Rust 时不要执行这条命令。** 应改为运行预构建的
`gl_render_test.exe`（需要把测试插件 DLL、FFmpeg/OCIO 等运行时 DLL
一起打包）。

期望：

- 25 帧循环通过；
- 输出 token 与目标 token 相同；
- `glReadPixels` 计数不增加；
- upload/download 计数不增加；
- 输出颜色 0.1 / 0.2 / 0.3 / 1.0；
- 下游 wgpu blit 采样颜色正确。

如果没有这个包，阶段 2 通过即可先证明“驱动 + 核心互操作”可用；
OFX 插件集成留到下一次有 Rust 构建环境时做。

## 7. 阶段 4：稳定性与回退

在阶段 2/3 通过后：

1. 连续运行 100 次（探针可用 `-loops 100` 参数，若有）；
2. 测试 1920×1080 和 640×480 两种尺寸；
3. 关闭零拷贝再跑一次：
   ```powershell
   Remove-Item Env:\QUERCUS_OFX_GPU_ZEROCOPY
   .\quercus-zerocopy-probe.exe
   ```
   期望它报告“disabled/fallback”，不崩；
4. 检查 Windows 事件查看器：
   - 应用程序日志中的崩溃；
   - 系统日志中的 `Display` TDR（事件 ID 4101）。

通过条件：无崩溃、无 TDR、无黑帧、无持续句柄增长。

## 8. 需要带回的材料

- `probe.log`
- `zerocopy.log`（如果做了阶段 2）
- 系统信息：
  ```powershell
  Get-CimInstance Win32_VideoController | Format-List * > video.txt
  dxdiag /t dxdiag.txt
  ```
- `GL_RENDERER` 截图或日志
- 失败时的 Windows 事件查看器截图
- 测试包 `sha256.txt` 校验结果

## 9. 失败处理

| 现象 | 结论/动作 |
|---|---|
| Microsoft Basic Display | 驱动未装；停止，先装厂商驱动 |
| WGL core 创建失败 | 无 GL 3.2+ / RDP 会话 / 软件 GL；停止 |
| `memory_object=false` | 驱动不支持；Windows 零拷贝不可用 |
| `vulkan_export=FAIL` | 不是 Vulkan 后端或没有 `VK_KHR_external_memory_win32` |
| `gl_import=FAIL` | 句柄/格式/驱动问题；收集日志 |
| 黑帧/花屏 | 布局或同步问题；收集驱动版本和尺寸 |
| 进程崩溃/TDR | 立即停止；记录事件日志，不再继续跑 |

## 10. 现场注意

- **不要**在测试机上安装 Rust/VS/CUDA/Vulkan SDK；
- 尽量在**物理控制台**运行，不要在 RDP 里跑 WGL 探针；
- 不要做 VFIO/GPU 直通，这台机器是独立 Windows；
- 测试前确认机器没有重要数据，测试包只读运行，不修改系统；
- 任何 `memory_object=false` 都直接停止，不要继续尝试“强制开启”。

## 11. 时间安排

- 系统/驱动检查：5 分钟
- 阶段 1 探针：2 分钟
- 阶段 2 核心闭环：10 分钟（如果包已准备好）
- 阶段 3 OFX：15 分钟（如果包已准备好）
- 稳定性/日志：15 分钟

总计约 45 分钟；如果阶段 1 失败，5 分钟内即可结束。
