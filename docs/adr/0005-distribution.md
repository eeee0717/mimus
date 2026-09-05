# ADR-0005 · 分发：单 archive + 资产运行时下载

- 状态：已接受（2026-08-21），统一资产清单于 2026-09-04 落地
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

## 后果

- release 体积与模型版本解耦；离线场景经"预取 + 自备路径"覆盖。
- 统一资产清单与预取机制已由 #39 落地；字体与模型解析不得再维护平行 manifest。
- 首次运行需联网下载约 150 MB 资产，需在 CLI 首跑体验中明示进度。
- Agent Skill 与二进制分开安装；skill 必须声明兼容的 CLI 版本，并针对受支持 release 做前向验证。

字体槽位、兼容性与自备路径合同见 [ADR-0018](0018-output-font-assets.md)；生产 layout
模型身份见 [ADR-0019](0019-production-layout-detector.md)。二者的默认资产身份均由 #39
落地的统一公开清单提供。
