# ADR-0005 · 分发：单 archive + 资产运行时下载

- 状态：已接受（2026-08-21），统一资产清单于 2026-09-04 落地，平台支持口径于 2026-09-06 修订
- 决策层级：难逆（对外承诺 + 资产机制是横切基础设施）

## 背景

"单二进制"是相对 Python 方案的差异化卖点，但模型（PP-DocLayoutV3 ONNX 131 MB）与 CJK 字体不可能进二进制；PDFium 若采用则以动态库形态存在（无预编译静态库，2026-08 查证）。

## 决策

1. 对外承诺档位：**单目录 archive**——GitHub Releases 发 tar.gz/zip，内含二进制（+ 可能的动态库），解压即用、无运行时依赖、无 Python。真单文件是努力方向，不是承诺。
2. 模型/字体等大资产：**运行时按需下载**（sha256 校验，缓存于用户缓存目录），提供预取子命令；**用户自备路径**作为逃生门（离线环境）。不打包进 release archive。
3. 下载源可配镜像（国内网络刚需）。
4. Agent Skill 不放入 release archive；仓库内以 `skills/mimus/` 维护，用户通过 `npx skills add eeee0717/mimus` 安装（ADR-0008）。该命令只安装 skill，不代替二进制与资产安装。
5. 模型与默认字体共享一个公开清单，逐项固定名称、版本、URL、SHA-256 与缓存相对路径。
   `assets list` 只读公开清单，`assets pull` 预取全部默认资产；`translate`/`inspect` 只按实际
   需要解析资产。M3.9 确定的 Noto Serif SC、STIX Two Text、STIX Two Math 路径及
   PP-DocLayoutV3 路径保持向后兼容，旧 Noto Sans SC 缓存不删除也不作为默认清单项。
6. 下载流式写入目标目录内的临时文件，边写边报告字节进度和计算 SHA-256；内容长度、
   大小、哈希及兼容性全部通过后才原子 rename。失败由临时文件生命周期清理，进程不会
   发布半成品。机器模式使用 CLI v2 的 `asset_download_started/progress/finished` additive
   事件，人类模式将两级进度写 stderr。
7. 各平台 archive 的可执行文件与动态库清单固定如下；此外每包共同包含 `LICENSE`、
   `THIRD_PARTY_NOTICES`、`licenses/`、`README.md`、`DEPENDENCIES.txt` 和
   `RUST_DEPENDENCIES.txt`：
   - macOS arm64：`mimus`、`libpdfium.dylib`；
   - macOS x64：`mimus`、`libpdfium.dylib`、`libonnxruntime.1.23.2.dylib`；
   - Linux x64：`mimus`、`libpdfium.so`；
   - Windows x64：`mimus.exe`、`pdfium.dll`、`msvcp140.dll`、`msvcp140_1.dll`、
     `vcruntime140.dll`、`vcruntime140_1.dll`。
8. Windows 的四个 VC runtime DLL 取自 runner 上与编译 toolset 配套的 Visual Studio
   Redist 目录，仅在版本目录和各文件 SHA-256 均与 release matrix 钉值相符时随包。Windows
   loader 会先搜索应用目录，且这四个 DLL 均非 KnownDLL，因此与 `mimus.exe` 相邻的
   app-local 部署满足解压即用合同。`mimus.exe` 还直接导入 DirectML、D3D12 与 DXGI，因此
   Windows preview archive 至少需要 Windows 10 1903 或更高版本，或 Windows 11。
9. 平台状态与 archive 是否发布分开管理：正式支持仅为 **macOS arm64（Apple Silicon）**。
   macOS x64、Linux x64、Windows x64 继续随 release matrix 发布，但标为 **preview/best-effort**。
   三个 preview target 已通过 GitHub-hosted CI 构建、依赖审计和真实模型冒烟；在分别完成
   维护者控制的原生干净机安装、真实文档处理和人工视觉验收前，不作兼容性保证。这里不是泛化的
   “arm64 支持”：没有 Linux arm64 或 Windows arm64 archive。

## 后果

- release 体积与模型版本解耦；离线场景经"预取 + 自备路径"覆盖。
- 统一资产清单与预取机制已由 #39 落地；字体与模型解析不得再维护平行 manifest。
- 首次运行需联网下载约 150 MB 资产，需在 CLI 首跑体验中明示进度。
- Agent Skill 与二进制分开安装；skill 必须声明兼容的 CLI 版本，并针对受支持 release 做前向验证。
- Windows archive 比依赖系统预装 VC++ Redistributable 多四个文件，但安装状态不再影响启动，
  runner 镜像升级也会因版本或哈希漂移而 fail-closed。
- 四平台自动化继续作为 archive 发布门；它证明可构建、依赖封闭和真实模型冒烟，不替代正式支持
  所需的维护者真机与人工视觉验收。preview target 的验收失败阻止其晋级，不扩大或削弱 macOS
  arm64 的正式支持承诺。

字体槽位、兼容性与自备路径合同见 [ADR-0018](0018-output-font-assets.md)；生产 layout
模型身份见 [ADR-0019](0019-production-layout-detector.md)。二者的默认资产身份均由 #39
落地的统一公开清单提供。
