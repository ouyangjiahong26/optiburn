# 术语表

本项目的领域词汇。命名（类型名、CLI 选项、测试名、issue 标题）一律用本表的“术语”列，
不要用“禁用同义词”列里的说法。

| 术语 | 定义 | 禁用同义词 |
|---|---|---|
| 刻录（Burn） | 把一份镜像写到可写介质上的动作。对应 `BurnJob`、`BurnEngine::burn`。 | 烧录、写入光盘、录制 |
| 镜像（Image） | 一份按扇区排列的完整光盘映像文件（`.iso`）。对应 `ImageSpec`、`ImageInfo`、`build_image`。 | ISO 文件、光盘文件、映象 |
| 母盘制作（Mastering） | 把目录树组织成镜像的过程：目录结构连同文件数据写成 ISO 9660/Joliet/UDF 元数据齐备的镜像。对应 crate `optiburn-mastering`。 | 打包、构建镜像、制作镜像 |
| 区段（Session） | 一次写入操作在盘上形成的完整 lead-in/数据/lead-out 单位。多区段盘上 Windows 只挂载最后一个区段。 | 会话、分节 |
| 增长模式（Grow） | 追加刻录的方式：引擎读出盘上已有区段的目录树，把源目录内容并入后作为新区段提交，旧文件保持可见。对应 `GrowJob`、`optiburn_engine::grow`、CLI 的 `append`。 | 续刻、增量刻录、追加镜像 |
| 图形前端（GUI） | optiburn 的图形入口：`src-tauri` 的命令层加 `frontend` 的 React 页面，与 CLI 平级，共用同一批核心 crate。 | 界面、客户端、UI |
| 已中止（Cancelled） | 写盘完成前被用户主动停止的任务结果：子进程已被停止，盘片内容不完整，不算成功也不算失败。 | 中断、打断 |
| 盘片状态（DiscStatus） | `READ DISC INFORMATION` 报出的四种状态：空（Empty）、可追加（Appendable）、已封口（Finalized）、随机可写（Other，状态位 0b11，例如 DVD-RAM、BD-RE）。对应枚举 `DiscStatus`。 | 盘状态、光盘状态、媒体状态 |
| 封口（Finalize） | 写 lead-out 并让盘片状态变为已封口，之后（对 CD-R/DVD-R）不能再追加。 | 关闭、终结、close |
| TAO（Track At Once） | 逐轨写入，轨间留链接块。适合多区段追加。 | 轨道写入 |
| DAO（Disc At Once） | 一次性写完整个盘的写入模式，无轨间间隙，CD 音频母盘常用。 | 整盘写入 |
| Joliet | ISO 9660 的补充卷描述符扩展，提供 Unicode（含中文）长文件名，Windows 原生读取。对应 `JolietLevel`。 | 长文件名扩展 |
| Rock Ridge | ISO 9660 的 POSIX 属性扩展（权限、符号链接、大小写），Linux/Unix 更愿意读它。v0 关闭。 | RR、RRIP |
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
| 引擎（Engine） | 把镜像写到盘上的具体实现。对应 trait `BurnEngine`。v0 只有 `XorrisoEngine`。 | 后端、驱动、刻录器 |
| 传输（Transport） | 把 CDB 交给设备并取回结果的通道。对应 trait `ScsiTransport`。 | 通道、驱动层、SCSI 层 |
| 设备路径（device） | 光驱的寻址字符串：Linux `/dev/sr0`，Windows `E:`（内部规范成 `\\.\E:`）。 | 盘符、设备名 |
| 倍速（speed） | 写入速度相对基准（CD 150 KB/s、DVD 1.35 MB/s、BD 4.5 MB/s 的整数倍）。缺省时交给驱动器自选。 | 速度、速率 |
| 卷标（volume id） | 写在卷描述符里的盘名，Windows 资源管理器显示的就是它。对应 `ImageSpec::volume_id`。 | 标签、盘标、volume label |
| 回读校验（Verify） | 写完盘后把盘上最后一区段的目录树抽回本地，与源（追加的源目录或刻录的镜像）按文件名与内容逐文件对比的动作。对应 `compare_trees`、`JobKind::Verify`、GUI 的“校验盘片”。 | 验证、核对、对拍 |
