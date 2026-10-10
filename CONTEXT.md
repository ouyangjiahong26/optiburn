# 术语表

本项目的领域词汇。命名（类型名、CLI 选项、测试名、issue 标题）一律用本表的“术语”列，
不要用“禁用同义词”列里的说法。

| 术语 | 定义 | 禁用同义词 |
|---|---|---|
| 刻录（Burn） | 把一份镜像写到可写介质上的动作。对应 `BurnJob`、`BurnEngine::burn`。 | 烧录、写入光盘、录制 |
| 镜像（Image） | 一份按扇区排列的完整光盘映像文件（`.iso`）。对应 `ImageSpec`、`ImageInfo`、`build_image`。 | ISO 文件、光盘文件、映象 |
| 母盘制作（Mastering） | 把目录树组织成镜像的过程：目录结构连同文件数据写成 ISO 9660/Joliet/UDF 元数据齐备的镜像。对应 crate `optiburn-mastering`。 | 打包、构建镜像、制作镜像 |
| 区段（Session） | 一次写入操作在盘上形成的完整 lead-in/数据/lead-out 单位。多区段盘上 Windows 只挂载最后一个区段。 | 会话、分节 |
| 区段地址约定（Session Addressing） | 目录记录里 extent 的解释规则：区段相对（自家原生引擎，独立镜像原样落盘）或盘级绝对（xorriso 增长模式，libisofs 的 ms_block 位移）。读侧按根目录首记录是否自引用探测。UDF 侧另有三种定位（单段、区段相对、盘级绝对，按 AVDP 的 tag_location 区分，ADR-0021）。 | 地址空间、寻址模式 |
| 增长模式（Grow） | 追加刻录的方式：引擎读出盘上已有区段的目录树，把源目录内容并入后作为新区段提交，旧文件保持可见。对应 `GrowJob`、`optiburn_engine::grow`、CLI 的 `append`。Linux 走 xorriso，Windows 走原生嫁接式实现（ADR-0020）。 | 续刻、增量刻录、追加镜像 |
| 嫁接式增长（Grafting） | 原生增长模式的写法：新区段只写目录结构与新文件数据，旧文件的数据块原地引用，既不重读也不重写。对应 `grow.rs` 的会话生成器。 | 合入式追加、增量合并 |
| 增长会话（Growth Session） | 增长模式生成并写下的那一个新区段：ISO 9660 加 Joliet（不带 UDF 描述符），目录记录里的地址是盘级绝对地址。对应 `SessionPlan`。 | 追加区段、新区段镜像 |
| 候选区段（Session Candidate） | 回退时逐个尝试的区段起点：`READ TOC` 的轨道起点加末区段起点，去重后从新到旧。每个区段的首条轨道起点就是该区段起点。对应 `disc_read` 的 `session_candidates`。 | 备用区段、回退起点 |
| 损坏末区段（Damaged Last Session） | 固件把中断刻录的残片登记成末区段，读它的卷描述符区或目录树失败。读盘与会话选择回退到最新的可用候选，追加要求用户确认（ADR-0022）。对应 `IsoSessionState::Damaged`、`SessionFallback`、`GrowJob::allow_damaged_last_session`。 | 坏区段、坏会话、残片区段 |
| 旧区段（Old Session） | 增长模式要嫁接的盘上末区段：引擎读出它的 Joliet 目录树（`OldSession`），新会话引用它里面文件的数据块。起点用 READ TOC Format 1 报的末区段起始地址（损坏末区段时用回退后的候选）。 | 旧会话、上一条区段、原区段 |
| 图形前端（GUI） | optiburn 的图形入口：`src-tauri` 的命令层加 `frontend` 的 React 页面，与 CLI 平级，共用同一批核心 crate。 | 界面、客户端、UI |
| 更新信息（update information） | 嵌在 AppImage 里的更新定位串（`gh-releases-zync|…`），AppImageUpdate 据此找到新版本的 `.zsync` 做增量更新。 | 更新数据、更新字符串 |
| 应用内更新（updater） | `tauri-plugin-updater` 驱动的检查、下载、就地替换通道：更新源是 Release 的 `latest.json`，更新包带 Ed25519 签名。 | 自动更新、自升级 |
| 已中止（Cancelled） | 写盘完成前被用户主动停止的任务结果：子进程已被停止，盘片内容不完整，不算成功也不算失败。 | 中断、打断 |
| 盘片状态（DiscStatus） | `READ DISC INFORMATION` 报出的四种状态：空（Empty）、可追加（Appendable）、已封口（Finalized）、随机可写（Other，状态位 0b11，例如 DVD-RAM、BD-RE）。对应枚举 `DiscStatus`。 | 盘状态、光盘状态、媒体状态 |
| 封口（Finalize） | 写 lead-out 并让盘片状态变为已封口，之后（对 CD-R/DVD-R）不能再追加。 | 关闭、终结、close |
| TAO（Track At Once） | 逐轨写入，轨间留链接块。适合多区段追加。 | 轨道写入 |
| DAO（Disc At Once） | 一次性写完整个盘的写入模式，无轨间间隙，CD 音频母盘常用。 | 整盘写入 |
| Joliet | ISO 9660 的补充卷描述符扩展，提供 Unicode（含中文）长文件名，Windows 原生读取。对应 `JolietLevel`。 | 长文件名扩展 |
| Rock Ridge | ISO 9660 的 POSIX 属性扩展（权限、符号链接、大小写），Linux/Unix 更愿意读它。镜像构建不启用，增长模式的写入会带上（实测，ADR-0010）。 | RR、RRIP |
| UDF Bridge | 同一份镜像里同时存在 ISO 9660 与 UDF 两个文件系统、且共享同一份文件数据区的布局。 | 混合镜像、hybrid ISO、双文件系统 |
| ISO 9660 Level | 主命名空间的名字长度规则：Level 1 是 8.3，Level 2 是 30 字符，Level 3（ISO 9660:1999）允许多区段与更长名字。 | ISO 级别、版本 |
| El Torito | 让光盘可引导的引导记录扩展。v0 未启用。 | 引导记录、boot catalog |
| CDB（Command Descriptor Block） | 下发给设备的命令字节块，长度 6/10/12/16 字节，本仓库上限 16。 | 命令块、命令包 |
| Sense（sense data） | 命令失败时设备返回的详细原因数据，最长 32 字节。本仓库不解释它，原样交给上层。 | 错误码、sense key |
| SG_IO | Linux 上的 SCSI 透传 ioctl（`scsi/sg.h` 的 `sg_io_hdr`），配合 `/dev/sr*` 使用。 | scsi 直通 |
| SPTI | Windows 上的 SCSI 透传接口：`DeviceIoControl` + `IOCTL_SCSI_PASS_THROUGH_DIRECT`。 | 直通 IOCTL、pass-through |
| MMC（MultiMedia Commands） | 光盘驱动器命令集标准，定义了 `INQUIRY`、`READ DISC INFORMATION`、`WRITE(10)` 等命令与响应格式。对应 crate `optiburn-mmc`。 | 光驱命令、SCSI 多媒体命令 |
| 介质 Profile（DiscProfile） | 目标介质类型：CD、DVD、BD。决定写哪几个文件系统。对应枚举 `DiscProfile`。 | 介质类型、盘类型 |
| LBA / MSF | 两种扇区定位方式：LBA 是从 0 开始的线性扇区号，MSF 是分:秒:帧。CD 族的 lead-in/lead-out 地址以 MSF 表示，其它介质用 LBA。 | 扇区号、地址 |
| 引擎（Engine） | 把镜像写到盘上的具体实现。对应 trait `BurnEngine`。现有 `NativeEngine`（原生 MMC 命令，ADR-0017）与 `XorrisoEngine`（子进程）。 | 后端、驱动、刻录器 |
| 传输（Transport） | 把 CDB 交给设备并取回结果的通道。对应 trait `ScsiTransport`。 | 通道、驱动层、SCSI 层 |
| 设备路径（device） | 光驱的寻址字符串：Linux `/dev/sr0`，Windows `E:`（内部规范成 `\\.\E:`）。 | 盘符、设备名 |
| 倍速（speed） | 写入速度相对基准（CD 150 KB/s、DVD 1.35 MB/s、BD 4.5 MB/s 的整数倍）。缺省时交给驱动器自选。 | 速度、速率 |
| 卷标（volume id） | 写在卷描述符里的盘名，Windows 资源管理器显示的就是它。对应 `ImageSpec::volume_id`。 | 标签、盘标、volume label |
| 回读校验（Verify） | 写完盘后把盘上最后一区段的目录树抽回本地，与源（追加的待刻录文件或刻录的镜像）按文件名与内容逐文件对比的动作。对应 `compare_trees`、`JobKind::Verify`、GUI 的“校验盘片”。 | 验证、核对、对拍 |
| UDF 卷识别序列（VRS） | UDF 盘从逻辑块 16 起的识别结构（`BEA01` / `NSR02` 或 `NSR03` / `TEA01`），UDF Bridge 盘的这一区段里还有 ISO 9660 的 `CD001` 描述符。读侧按它判断“这一区段有没有 UDF”。 | 卷识别区、VRS 扇区 |
| 锚点（AVDP） | UDF 的 Anchor Volume Descriptor Pointer：定位主卷描述符序列的入口，固定出现在块 256（区段内相对）与盘的末端位置。它的 tag_location 字段是判断该区段用哪种地址约定的依据（ADR-0021）。 | 锚描述符、AVDP 扇区 |
| 读盘（Disc Reading） | 读出盘上末区段内容的五个能力：卷标、末区段是否 ISO 9660、目录树列举、整树抽取、按路径抽取。接在 `ReadBackend` 接缝上，后端是 xorriso 子进程或原生解析（ISO 9660 见 ADR-0018，UDF 见 ADR-0021）。 | 读碟、光盘读取 |
| 盘片容量（DiscCapacity） | 总容量与可用容量两个口径，可用容量优先取 READ TRACK INFORMATION 的剩余块数，退回 READ FORMAT CAPACITIES 的格式化容量（ADR-0019）。对应 `DiscCapacity`、`read_disc_capacity`。 | 光盘大小、介质容量、容量信息 |
| 可用容量（free） | 盘片容量里还能写入的量，写前容量门禁的比较基准。顺序介质上是剩余块数乘 2048 字节（ADR-0019）。 | 剩余空间、可用空间、自由空间 |
| 格式化容量（FormatCapacity） | READ FORMAT CAPACITIES 报出的两个数值：未格式化介质的最大可格式化容量（描述符类型 1）与已格式化介质的当前格式化容量（类型 2）。对应 `FormatCapacity`、`read_format_capacities`。 | 介质容量、盘片大小 |
| 预演（print size） | 增长模式提交前算出即将写入的新区段大小（字节），与可用容量同口径。Linux 用 `xorriso -print_size`，Windows 用原生会话计划的尺寸，两端都收在 `grow_size`。 | 大小估算、dry-run |
