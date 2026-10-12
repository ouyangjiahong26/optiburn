//! 读侧与回读工具：读盘上目录树与卷标、抽取到本地，供设备页浏览与回读校验
//! （ADR-0010）。只读访问一律用 `-drive_access shared` 打开，独占冲突不在这里发生。
//!
//! 两个读侧后端接在 [`ReadBackend`] 接缝上：[`XorrisoRead`]（子进程）与
//! [`NativeRead`]（MMC 加 ISO 9660/UDF 解析，ADR-0018）。Linux 默认 xorriso 优先，
//! 读不到 ISO 9660 时后端回退到原生重试（ADR-0024）。Windows 走原生。分派规则与
//! 刻录引擎同理由：那边哪条路走得通，不是设备语义差异。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{BurnError, CancelToken, IsoSessionState, SalvageReport, TAIL_LINES, XORRISO};

use super::disc_read::NativeRead;
use super::xorriso::run;

/// 读侧后端：读盘能力的实现约定（[`BurnEngine`] 的读侧对偶，ADR-0018）。
pub(crate) trait ReadBackend {
    fn read_volume_id(&self, source: &str) -> Result<String, BurnError>;
    fn iso_session_state(&self, source: &str) -> Result<IsoSessionState, BurnError>;
    fn list_tree(&self, source: &str) -> Result<Vec<DiscEntry>, BurnError>;
    fn extract_tree(
        &self,
        source: &Path,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError>;
    fn extract_paths(
        &self,
        source: &str,
        paths: &[String],
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError>;
    /// 抢救未关闭轨道里的数据，见 [`crate::salvage`]。
    fn salvage(
        &self,
        source: &str,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<Option<SalvageReport>, BurnError>;
}

/// 读侧后端的选择：保守开关 `OPTIBURN_READ_BACKEND` 的三值（ADR-0024）。
///
/// `Auto` 是默认：Linux 先走 xorriso，读不到 ISO 9660 时改用原生读侧重试同一次
/// 操作。Windows 直接走原生。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBackendMode {
    Auto,
    Xorriso,
    Native,
}

impl ReadBackendMode {
    /// 开关值原样回写，用于 `probe` 的诊断行。
    pub fn as_str(&self) -> &'static str {
        match self {
            ReadBackendMode::Auto => "auto",
            ReadBackendMode::Xorriso => "xorriso",
            ReadBackendMode::Native => "native",
        }
    }
}

/// 读侧后端模式：从环境变量 `OPTIBURN_READ_BACKEND` 读（保守开关，ADR-0024 决策 3）。
pub fn read_backend_mode() -> Result<ReadBackendMode, BurnError> {
    read_backend_mode_value(std::env::var("OPTIBURN_READ_BACKEND").ok().as_deref())
}

/// 开关的取值判定：空值与 `auto` 都算默认。抽出来是为了能不碰进程环境测（形状照
/// [`crate::disc_read`] 的 `last_session_only_value`）。
pub(crate) fn read_backend_mode_value(value: Option<&str>) -> Result<ReadBackendMode, BurnError> {
    match value {
        None | Some("") | Some("auto") => Ok(ReadBackendMode::Auto),
        Some("xorriso") => Ok(ReadBackendMode::Xorriso),
        Some("native") => Ok(ReadBackendMode::Native),
        Some(other) => Err(BurnError::ReadFailed(format!(
            "unknown OPTIBURN_READ_BACKEND value {other:?}: expected auto, xorriso, or native"
        ))),
    }
}

/// 模式对应的实际后端名，供 `probe` 报读侧后端现状（ADR-0024 决策 4）。名字与
/// [`selected_read_backend`] 的分派一一对应。
pub fn read_backend_name(mode: ReadBackendMode) -> Result<&'static str, BurnError> {
    match mode {
        // Windows 上唯一能取得的 xorriso 不含光驱访问（ADR-0008 补记），显式要求
        // 用它等于要求一个用不了的后端，直接拒绝并给出退路。
        ReadBackendMode::Xorriso if cfg!(windows) => Err(BurnError::ReadFailed(
            "no usable xorriso build on Windows (ADR-0008); unset OPTIBURN_READ_BACKEND or set auto/native"
                .to_string(),
        )),
        ReadBackendMode::Xorriso => Ok("xorriso"),
        ReadBackendMode::Native => Ok("native"),
        ReadBackendMode::Auto => Ok(if cfg!(windows) {
            "native"
        } else {
            "xorriso+native"
        }),
    }
}

/// 后端回退的触发判定：只有“xorriso 跑了但读不到”的两类错误才换原生重试。
///
/// `MissingTool`、`Cancelled`、`BlankMedium`、`UnsafePath`、`Io` 换一边也一样，
/// 不触发：缺工具要维持安装提示，真空白盘与不安全路径与后端无关（ADR-0024
/// 决策 1、2）。
fn should_fallback(error: &BurnError) -> bool {
    matches!(error, BurnError::NoIsoSession | BurnError::ReadFailed(_))
}

/// 先跑 `primary`，命中 [`should_fallback`] 时改用 `secondary` 重试同一次操作。
///
/// 两个后端都失败时以 `secondary` 的错误为准（ADR-0024 决策 4：两平台同盘同文案，
/// 不并列 xorriso 的输出）。
fn fallback<T>(
    primary: Result<T, BurnError>,
    secondary: impl FnOnce() -> Result<T, BurnError>,
) -> Result<T, BurnError> {
    match primary {
        Err(error) if should_fallback(&error) => secondary(),
        other => other,
    }
}

/// 门禁状态的两个后端归并（ADR-0024 决策 7）。
///
/// xorriso 报 `Unusable` 时问一次原生，只有原生给出 `Damaged`（末区段坏了但更早
/// 还有可用区段，ADR-0022 的候选回退）才采用它的结论。损坏末区段的盘在 xorriso
/// 侧正是“读不到 ISO 9660”的兜底空镜像形态，也就是 `Unusable`，不在 `Err` 上，
/// 只看 `Err` 就永远报不出 `Damaged`。其余形态一律保持 xorriso 的答案：纯 UDF 盘
/// 两侧都是 `Unusable`，语义不变，原生打不开设备时也不把一次分类变成错误。
fn merge_session_state(
    primary: Result<IsoSessionState, BurnError>,
    secondary: impl FnOnce() -> Result<IsoSessionState, BurnError>,
) -> Result<IsoSessionState, BurnError> {
    match primary {
        Ok(IsoSessionState::Unusable) => match secondary() {
            Ok(IsoSessionState::Damaged(fallback)) => Ok(IsoSessionState::Damaged(fallback)),
            _ => Ok(IsoSessionState::Unusable),
        },
        other => fallback(other, secondary),
    }
}

const XORRISO_READ: XorrisoRead = XorrisoRead;
const NATIVE_READ: NativeRead = NativeRead;
const FALLBACK_READ: FallbackRead = FallbackRead;

/// 后端回退的组合后端：Linux `auto` 模式用它（ADR-0024）。
///
/// 六个能力一律先问 xorriso，命中触发信号时用原生读侧重试。后端回退的事实不进
/// 任何面向用户输出：真机排查用保守开关做 A/B 对跑。
struct FallbackRead;

impl ReadBackend for FallbackRead {
    fn read_volume_id(&self, source: &str) -> Result<String, BurnError> {
        fallback(XORRISO_READ.read_volume_id(source), || {
            NATIVE_READ.read_volume_id(source)
        })
    }

    fn iso_session_state(&self, source: &str) -> Result<IsoSessionState, BurnError> {
        merge_session_state(XORRISO_READ.iso_session_state(source), || {
            NATIVE_READ.iso_session_state(source)
        })
    }

    fn list_tree(&self, source: &str) -> Result<Vec<DiscEntry>, BurnError> {
        fallback(XORRISO_READ.list_tree(source), || {
            NATIVE_READ.list_tree(source)
        })
    }

    fn extract_tree(
        &self,
        source: &Path,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        fallback(XORRISO_READ.extract_tree(source, dest, cancel), || {
            NATIVE_READ.extract_tree(source, dest, cancel)
        })
    }

    fn extract_paths(
        &self,
        source: &str,
        paths: &[String],
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        fallback(
            XORRISO_READ.extract_paths(source, paths, dest, cancel),
            || NATIVE_READ.extract_paths(source, paths, dest, cancel),
        )
    }

    fn salvage(
        &self,
        source: &str,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<Option<SalvageReport>, BurnError> {
        // xorriso 侧恒 `ReadFailed`，自然落到原生，Linux 抢救由此打通（ADR-0024
        // 决策 6）。
        fallback(XORRISO_READ.salvage(source, dest, cancel), || {
            NATIVE_READ.salvage(source, dest, cancel)
        })
    }
}

/// 按读侧后端模式选后端（ADR-0024 决策 3）。
fn selected_read_backend() -> Result<&'static dyn ReadBackend, BurnError> {
    let name = read_backend_name(read_backend_mode()?)?;
    let backend: &'static dyn ReadBackend = match name {
        "xorriso" => &XORRISO_READ,
        "native" => &NATIVE_READ,
        _ => &FALLBACK_READ,
    };
    Ok(backend)
}

/// xorriso 读侧后端：包住子进程调用，行为与本模块改造前一致。
struct XorrisoRead;

impl ReadBackend for XorrisoRead {
    fn read_volume_id(&self, device: &str) -> Result<String, BurnError> {
        let (stdout, stderr) = run_output(XORRISO, &pvd_info_args(device))?;
        // 兜底空镜像（读不到 ISO 9660）与真空白盘分开：后者是 0 区段，后端回退
        // 到原生也一样（ADR-0024 决策 2），归 `BlankMedium`。
        if is_blank_image_fallback(&stderr) {
            return Err(BurnError::NoIsoSession);
        }
        if reports_blank_medium(&stderr) {
            return Err(BurnError::BlankMedium);
        }
        parse_volume_id(&stdout)
            .ok_or_else(|| BurnError::ReadFailed("xorriso did not report a Volume Id".to_string()))
    }

    /// xorriso 侧只有“能不能按 ISO 9660 读”这一个判断，没有候选回退：读不到就是
    /// 读不到（Linux 的候选回退由 [`FallbackRead`] 落到原生侧实现，ADR-0024）。
    ///
    /// 抢救未关闭轨道：xorriso 只认 TOC 里登记过的会话，看不见未关闭轨道（中断
    /// 写入）里的内容，这条能力只有原生读侧有（ADR-0022 补记）。
    fn salvage(
        &self,
        _source: &str,
        _dest: &Path,
        _cancel: &CancelToken,
    ) -> Result<Option<SalvageReport>, BurnError> {
        // xorriso 只认 TOC 里登记过的会话。未关闭轨道（中断写入）的内容它看不见。
        // `ReadFailed` 是后端回退的触发信号，抢救因此落到原生侧。
        Err(BurnError::ReadFailed(
            "xorriso cannot read an unclosed session (this needs the native read path)".to_string(),
        ))
    }

    fn iso_session_state(&self, device: &str) -> Result<IsoSessionState, BurnError> {
        let (_, stderr) = run_output(XORRISO, &pvd_info_args(device))?;
        if is_blank_image_fallback(&stderr) {
            Ok(IsoSessionState::Unusable)
        } else {
            Ok(IsoSessionState::Usable)
        }
    }

    fn list_tree(&self, device: &str) -> Result<Vec<DiscEntry>, BurnError> {
        let (stdout, stderr) = run_output(XORRISO, &list_args(device))?;
        // 盘上没有 ISO 9660 时 xorriso 会兜底造一个空镜像并照常成功，stdout 只列根目录。
        // 这里按 stderr 识破兜底：真空白盘由调用方区分，有内容却读不出 ISO 的盘不能
        // 被显示成空盘（实测，见 ADR-0010）。
        if is_blank_image_fallback(&stderr) {
            return Err(BurnError::NoIsoSession);
        }
        Ok(parse_lsdl(&stdout))
    }

    fn extract_tree(
        &self,
        source: &Path,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        let stderr = run(XORRISO, &extract_args(source, dest), &mut |_| {}, cancel)
            .map_err(read_side_error)?;
        // 兜底空镜像时 xorriso 照常成功，但抽出来的是空目录：判成读不到，交给
        // 后端回退重抽，否则调用方会把“纯 UDF 盘”当成空盘（ADR-0024 决策 1）。
        if is_blank_image_fallback(&stderr) {
            return Err(BurnError::NoIsoSession);
        }
        Ok(())
    }

    fn extract_paths(
        &self,
        device: &str,
        paths: &[String],
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        // 路径来自盘片本身，逐条校验并落成本地相对路径后再交给 xorriso。
        let mut targets = Vec::with_capacity(paths.len());
        for path in paths {
            let target = dest.join(safe_relative_path(path)?);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            targets.push((path.clone(), target));
        }
        let stderr = run(
            XORRISO,
            &extract_paths_args(device, &targets),
            &mut |_| {},
            cancel,
        )
        .map_err(read_side_error)?;
        if is_blank_image_fallback(&stderr) {
            return Err(BurnError::NoIsoSession);
        }
        Ok(())
    }
}

/// 从盘上最后一个区段读卷标（PVD 的 `Volume Id`），用于 GUI 追加页预填
/// （ADR-0010）。
///
/// xorriso 路径的解析依赖英文消息，统一加 `LC_ALL=C`，本机实测该 locale 下
/// 非 ASCII 卷标原样输出。
pub fn read_volume_id(device: &str) -> Result<String, BurnError> {
    selected_read_backend()?.read_volume_id(device)
}

/// 抢救未关闭轨道（中断写入）里的数据：列出它的目录树，按“已经写下去的数据边界”
/// 给每个文件定性（完整、只写了一部分、一个字节都没写），把完整与半截的文件抽到
/// `dest`。
///
/// 这条路径超出 libburn 的能力：xorriso 只认 TOC 里登记过的会话，看不见未关闭轨道
/// 里的内容（ADR-0022 补记）。普通读路径与追加门禁不受影响，仍以最后一个已关闭
/// 会话为准。
///
/// 盘上没有未关闭轨道（镜像文件、空盘、驱动器不报开放轨道）时返回 `Ok(None)`；
/// 目录树读不出来按读盘错误上抛，不并进 `None`。xorriso 路径没有这个能力，恒返回
/// 错误；Linux 默认（`auto`）随后端回退落到原生读侧，抢救因此可用（ADR-0024）。
pub fn salvage(
    source: &str,
    dest: &Path,
    cancel: &CancelToken,
) -> Result<Option<SalvageReport>, BurnError> {
    selected_read_backend()?.salvage(source, dest, cancel)
}

/// 盘上 ISO 9660 会话的可用状态，追加前门禁用它分流（ADR-0022）。
///
/// [`IsoSessionState::Unusable`] 挡的是“末区段不是 ISO 9660 且没有更早的可用会话”
/// 的盘，例如 Windows 写入的纯 UDF 盘：这类盘续写 ISO 区段后，按最后一区段挂载的
/// 系统（Windows）只会看到新内容，原有文件被遮住（实测，见 ADR-0010）。
///
/// [`IsoSessionState::Damaged`] 是末区段损坏（固件把残片登记成区段，或目录结构
/// 不完整）但更早区段仍可读的盘：调用方必须把被跳过区段里的文件会从可见视图消失
/// 这件事告知用户，得到确认后把 [`GrowJob::allow_damaged_last_session`] 置位再提交
/// （ADR-0022）。Linux 默认（`auto`）在 xorriso 侧报错时回退到原生读侧，损坏末
/// 区段因此也走候选回退报出 `Damaged`（ADR-0024）。
pub fn iso_session_state(source: &str) -> Result<IsoSessionState, BurnError> {
    selected_read_backend()?.iso_session_state(source)
}

/// 把镜像或设备的目录树抽取到本地目录，供回读校验与复制使用（ADR-0010）。
///
/// xorriso 路径的 `-osirrox on` 打开抽取权限，`-extract / <目标>` 把整树落到目标
/// 目录。xorriso 会尽力还原时间戳，但抽出的文件 ctime 必然是新值，所以校验只在
/// 本地按名字与内容对比，不比较属性。
pub fn extract_tree(source: &Path, dest: &Path, cancel: &CancelToken) -> Result<(), BurnError> {
    selected_read_backend()?.extract_tree(source, dest, cancel)
}

/// 列出盘上最后一区段的目录树，用于 GUI 的盘片浏览。
pub fn list_tree(device: &str) -> Result<Vec<DiscEntry>, BurnError> {
    selected_read_backend()?.list_tree(device)
}

/// 判断 xorriso 是否因为盘上没有 ISO 9660 而兜底造了一个空镜像。
///
/// 这种情况下 `-pvd_info` 报的是空镜像的默认卷标 `ISOIMAGE`，是假数据。实测 UDF
/// 盘（Windows 写入的多区段 DVD-R）走的就是这条路径，此时必须拒绝，不能把假卷标
/// 预填进追加页。
fn is_blank_image_fallback(stderr: &str) -> bool {
    stderr.contains("Creating blank image") || stderr.contains("No ISO 9660 image")
}

/// xorriso 是否报空白介质（0 区段）：它不读 ISO 头，stderr 出 `Media status : is
/// blank`，stdout 报合成的默认卷标 `ISOIMAGE`（实测 1.5.6）。
///
/// 与 [`is_blank_image_fallback`] 分开：末区段门禁要用那个函数放行空盘（首刻），
/// 读卷标则把空盘归 [`BurnError::BlankMedium`]，后端回退不碰它（ADR-0024 决策 2）。
fn reports_blank_medium(stderr: &str) -> bool {
    stderr.contains("Media status : is blank")
}

/// 组装读卷标的参数表。
///
/// 带 `-drive_access shared`：盘被挂载时独占打开会失败，shared 仍能读到 PVD，
/// 卷标预填与末区段检查因此在挂载期间也可用。
fn pvd_info_args(device: &str) -> Vec<OsString> {
    vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
        OsString::from("-indev"),
        OsString::from(device),
        OsString::from("-pvd_info"),
    ]
}

/// 从 `-pvd_info` 的 stdout 里取卷标，空值或没有该行返回 `None`。
///
/// 只认大写 `Volume Id` 一行。stderr 上还有一行带引号的小写 `Volume id : '...'`，
/// 不参与解析，避免两处口径打架。
fn parse_volume_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("Volume Id") else {
            continue;
        };
        let value = rest.split_once(':')?.1.trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// 组装抽取参数表。路径按 `OsStr` 原样传递，不做有损转换。
///
/// 带 `-drive_access shared`：盘被桌面环境自动挂载时独占打开会失败（实测
/// 2026-10-12，udisks2 挂载的 CD-R 上 xorriso 报 `Cannot open busy device`），
/// shared 才能抽。与 [`extract_paths_args`] 同一口径。
fn extract_args(source: &Path, dest: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
        OsString::from("-indev"),
        source.as_os_str().to_os_string(),
        OsString::from("-osirrox"),
        OsString::from("on"),
        OsString::from("-extract"),
        OsString::from("/"),
        dest.as_os_str().to_os_string(),
    ]
}

/// 盘上最后一区段的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscEntry {
    /// 盘上路径，以 `/` 开头，例如 `/子目录/中文文件.txt`。
    pub path: String,
    /// 文件大小，目录为 0。
    pub size: u64,
    pub is_dir: bool,
}

/// 组装列清单的参数表。
fn list_args(device: &str) -> Vec<OsString> {
    vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
        OsString::from("-indev"),
        OsString::from(device),
        OsString::from("-find"),
        OsString::from("/"),
        OsString::from("-exec"),
        OsString::from("lsdl"),
        OsString::from("--"),
    ]
}

/// 解析 `lsdl` 的输出：`-rw-r--r-- 1 1000 1000 7797 Oct 9 10:25 '/路径'`。
///
/// 大小取按空白切分的第 5 个字段。只列普通文件、目录与符号链接（`d`、`-`、`l`
/// 前缀），FIFO 与设备这类非常规条目不进清单。路径按 shell 引号拼接解出：名字里
/// 的单引号会写成 `'"'"'`，名字里的换行原样出现（条目因此可能跨物理行），符号链接行
/// 后面还跟着 ` -> '目标'`，只取链接自身的路径。根目录 `/` 自身不进列表（实测
/// xorriso 1.5.6）。
fn parse_lsdl(stdout: &str) -> Vec<DiscEntry> {
    let mut entries = Vec::new();
    let mut cursor = 0;
    while cursor < stdout.len() {
        let line_end = stdout[cursor..]
            .find('\n')
            .map_or(stdout.len(), |offset| cursor + offset);
        let line = &stdout[cursor..line_end];
        let next = (line_end + 1).min(stdout.len());
        if (line.starts_with('d') || line.starts_with('-') || line.starts_with('l'))
            && let Some(quote) = line.find('\'')
            && let Some((path, after)) = parse_quoted_name(stdout, cursor + quote)
        {
            if !path.is_empty() && path != "/" {
                let size = line
                    .split_whitespace()
                    .nth(4)
                    .and_then(|field| field.parse::<u64>().ok())
                    .unwrap_or(0);
                entries.push(DiscEntry {
                    path,
                    size,
                    is_dir: line.starts_with('d'),
                });
            }
            cursor = after;
            continue;
        }
        cursor = next;
    }
    entries
}

/// 从 `text[start..]` 解析 shell 拼接形式的引号名字，返回名字与结束后的字节位置。
///
/// 拼接段相邻无分隔，例如名字 `quote'file.txt` 输出为 `'/quote'"'"'file.txt'`。
fn parse_quoted_name(text: &str, start: usize) -> Option<(String, usize)> {
    if start > text.len() {
        return None;
    }
    let mut name = String::new();
    let mut index = start;
    while let Some(quote) = text[index..].chars().next() {
        if quote != '\'' && quote != '"' {
            break;
        }
        let rest = &text[index + 1..];
        let end = rest.find(quote)?;
        name.push_str(&rest[..end]);
        index += end + 2;
    }
    Some((name, index))
}

/// 从盘上按 ISO 路径抽取若干文件或目录到本地目录，一次后端调用完成。
///
/// 每个 ISO 路径落到 `dest` 下同名的相对位置，父目录预先建好。只读打开
/// （xorriso 路径带 `-drive_access shared`），供 GUI 把盘上文件复制到本地。
pub fn extract_paths(
    device: &str,
    paths: &[String],
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    selected_read_backend()?.extract_paths(device, paths, dest, cancel)
}

/// 读侧提取失败：写盘语义的 [`BurnError::Failed`] 归一成 [`BurnError::ReadFailed`]，
/// 文案不再带 burn failed 前缀。取消与工具缺失原样保留。
fn read_side_error(error: BurnError) -> BurnError {
    match error {
        BurnError::Failed(tail) => BurnError::ReadFailed(tail),
        other => other,
    }
}

/// 校验盘内路径并转成本地相对路径：只接受常规段，拒绝 `..`、盘符形态段与 Windows
/// 的分隔符。
///
/// 盘片内容不能当可信输入：Windows 上 `C:` 这类带盘符前缀的段会让 `Path` 的拼接
/// 替换掉已累积的路径，把文件抽到暂存目录之外（实测 xorriso 能写出这样的镜像）。
/// 每段都要查，冒号本身不拒绝，只有“单个 ASCII 字母加冒号”这种盘符形态段不放行，
/// `12:30.txt` 这类普通名字照常通过。原生读侧（`disc_read`）落盘文件名也过这层。
pub(crate) fn safe_relative_path(path: &str) -> Result<PathBuf, BurnError> {
    let mut relative = PathBuf::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if is_drive_prefix(segment) {
            return Err(BurnError::UnsafePath(path.to_string()));
        }
        if segment == ".." || segment.contains('\\') {
            return Err(BurnError::UnsafePath(path.to_string()));
        }
        relative.push(segment);
    }
    if relative.as_os_str().is_empty() {
        return Err(BurnError::UnsafePath(path.to_string()));
    }
    Ok(relative)
}

/// 段是否是 Windows 盘符前缀形态（单个 ASCII 字母加冒号）。
fn is_drive_prefix(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// 组装多路径抽取的参数表：一次加载盘片，按序执行每条 `-extract`。
fn extract_paths_args(device: &str, targets: &[(String, PathBuf)]) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
        OsString::from("-indev"),
        OsString::from(device),
        OsString::from("-osirrox"),
        OsString::from("on"),
    ];
    for (path, target) in targets {
        args.push(OsString::from("-extract"));
        args.push(OsString::from(path));
        args.push(target.as_os_str().to_os_string());
    }
    args
}

/// 跑一个只读查询并收集 stdout 与 stderr。
///
/// 文案解析依赖英文关键行，固定 `LC_ALL=C`。实测（2026-10-12，xorriso 1.5.6）该
/// locale 下中文卷标原样输出，但 Joliet 的非 ASCII 名会被换成替代名（libisofs 报
/// `Cannot convert from charset UTF-16BE to ANSI_X3.4-1968`，`/中文追加.txt` 变
/// `/__e______._t_x...`），原生读侧按 Joliet 读得出原名。取舍见 ADR-0024 补记。
/// 失败时取 stderr 尾部做摘要。xorriso 增长模式的尺寸预演
/// （[`crate::xorriso::grow_size`]）也复用它。
pub(crate) fn run_output(program: &str, args: &[OsString]) -> Result<(String, String), BurnError> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BurnError::MissingTool(program.to_string()),
            _ => BurnError::Io(e),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BurnError::ReadFailed(tail_of(&stderr)));
    }
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// 取文本最后 [`TAIL_LINES`] 个非空行，作为失败摘要。`xorriso::run` 的失败路径
/// 复用它。
pub(crate) fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    // 后端回退的夹具集成测试复用 UDF 读侧的夹具（ADR-0024），与 xorriso 一样只在
    // unix 上跑。
    #[cfg(unix)]
    use crate::disc_read::udf::tests::{
        TempDir, bin_content, expected_tree, write_udf_only_image, zh_content,
    };
    #[cfg(unix)]
    use optiburn_mastering::{DiscProfile, ImageSpec, build_image};

    /// 期望参数表：与实现同样用 `OsString`，避免为断言再把路径窄化回 `String`。
    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn pvd_info_args_and_volume_id_parse() {
        assert_eq!(
            pvd_info_args("/dev/sr0"),
            os(&["-drive_access", "shared", "-indev", "/dev/sr0", "-pvd_info"])
        );
        let stdout = "\
Drive current: -indev '/dev/sr0'
PVD address  : 16s
Volume Id    : 我的光盘
Volume Set Id: 
Preparer Id  : XORRISO
";
        assert_eq!(parse_volume_id(stdout), Some("我的光盘".to_string()));
        assert_eq!(parse_volume_id("Volume Id    :    \n"), None);
        assert_eq!(parse_volume_id("Volume Set Id: x\n"), None);
        assert_eq!(parse_volume_id(""), None);
    }

    #[test]
    fn missing_binary_on_the_read_side_is_a_missing_tool_error() {
        let err = run_output("optiburn-nonexistent-program", &[])
            .expect_err("spawning a nonexistent program must fail");
        // 载荷只有工具名，安装指引由调用方按系统给出。
        assert!(
            matches!(&err, BurnError::MissingTool(name) if name == "optiburn-nonexistent-program"),
            "{err:?}"
        );
    }

    #[test]
    fn extract_args_carry_osirrox_and_root() {
        assert_eq!(
            extract_args(Path::new("/tmp/a.iso"), Path::new("/tmp/out")),
            os(&[
                "-drive_access",
                "shared",
                "-indev",
                "/tmp/a.iso",
                "-osirrox",
                "on",
                "-extract",
                "/",
                "/tmp/out"
            ])
        );
    }

    #[test]
    fn parse_lsdl_reads_the_real_output_shape() {
        // 取自实测的 lsdl 输出，含中文路径、目录与根条目。
        let stdout = "\
drwxrwxr-x    1 1000     1000            0 Oct  9 10:39 '/'
-rw-rw-r--    1 1000     1000         7797 Oct  9 10:25 '/ARCHITECTURE.md'
drwxrwxr-x    1 1000     1000            0 Oct  9 10:25 '/子目录'
-rw-rw-r--    1 1000     1000           76 Oct  9 10:25 '/子目录/中文文件.txt'
-rw-rw-r--    1 1000     1000       262144 Oct  9 10:25 '/随机数据.bin'
";
        assert_eq!(
            parse_lsdl(stdout),
            vec![
                DiscEntry {
                    path: "/ARCHITECTURE.md".to_string(),
                    size: 7797,
                    is_dir: false,
                },
                DiscEntry {
                    path: "/子目录".to_string(),
                    size: 0,
                    is_dir: true,
                },
                DiscEntry {
                    path: "/子目录/中文文件.txt".to_string(),
                    size: 76,
                    is_dir: false,
                },
                DiscEntry {
                    path: "/随机数据.bin".to_string(),
                    size: 262144,
                    is_dir: false,
                },
            ]
        );
        assert!(parse_lsdl("xorriso : NOTE : Loading ISO image tree\n").is_empty());
    }

    #[test]
    fn parse_lsdl_takes_only_the_link_path_for_symlinks() {
        // 实测于 xorriso 1.5.6：符号链接的目标也带引号，路径只能取第一对引号。
        let stdout = "lrwxrwxrwx    1 1000     1000            0 Oct  9 13:26 '/link' -> 'sub'\n";
        assert_eq!(
            parse_lsdl(stdout),
            vec![DiscEntry {
                path: "/link".to_string(),
                size: 0,
                is_dir: false,
            }]
        );
    }

    #[test]
    fn parse_lsdl_unescapes_quotes_and_keeps_multiline_names() {
        // 实测：名字里的单引号按 shell 约定写成 '"'"' 拼接。
        let quoted =
            "-rw-rw-r--    1 1000     1000            6 Oct  9 13:47 '/quote'\"'\"'file.txt'\n";
        assert_eq!(
            parse_lsdl(quoted),
            vec![DiscEntry {
                path: "/quote'file.txt".to_string(),
                size: 6,
                is_dir: false,
            }]
        );
        // 名字里含真换行时条目跨物理行，不能被丢掉，也不能把下一行当新条目。
        let multiline = "-rw-rw-r--    1 1000     1000            3 Oct  9 13:47 '/a\nb.txt'\n-rw-rw-r--    1 1000     1000            1 Oct  9 13:47 '/z.txt'\n";
        assert_eq!(
            parse_lsdl(multiline),
            vec![
                DiscEntry {
                    path: "/a\nb.txt".to_string(),
                    size: 3,
                    is_dir: false,
                },
                DiscEntry {
                    path: "/z.txt".to_string(),
                    size: 1,
                    is_dir: false,
                },
            ]
        );
    }

    #[test]
    fn safe_relative_path_rejects_escapes() {
        assert_eq!(
            safe_relative_path("/子目录/中文文件.txt").unwrap(),
            PathBuf::from("子目录/中文文件.txt")
        );
        // 盘符形态段、`..` 与反斜杠分隔符都拒绝。每段都查，Windows 上 push 带前缀
        // 的段会替换已累积路径。盘内路径来自盘片，不能信。
        for bad in [
            "/C:/escaped",
            "/报告/C:/x",
            "/../etc/passwd",
            "/a\\b",
            "/",
            "",
        ] {
            assert!(
                matches!(safe_relative_path(bad), Err(BurnError::UnsafePath(_))),
                "{bad}"
            );
        }
        // 非首段的冒号是 Linux 上的合法文件名，放行。
        assert_eq!(
            safe_relative_path("/12:30.txt").unwrap(),
            PathBuf::from("12:30.txt")
        );
        assert_eq!(
            safe_relative_path("/报告/12:30.txt").unwrap(),
            PathBuf::from("报告/12:30.txt")
        );
    }

    #[test]
    fn list_and_extract_paths_args_tables() {
        assert_eq!(
            list_args("/dev/sr0"),
            os(&[
                "-drive_access",
                "shared",
                "-indev",
                "/dev/sr0",
                "-find",
                "/",
                "-exec",
                "lsdl",
                "--"
            ])
        );
        let targets = vec![
            (
                "/子目录/中文文件.txt".to_string(),
                PathBuf::from("/tmp/out/子目录/中文文件.txt"),
            ),
            ("/a.bin".to_string(), PathBuf::from("/tmp/out/a.bin")),
        ];
        assert_eq!(
            extract_paths_args("/dev/sr0", &targets),
            os(&[
                "-drive_access",
                "shared",
                "-indev",
                "/dev/sr0",
                "-osirrox",
                "on",
                "-extract",
                "/子目录/中文文件.txt",
                "/tmp/out/子目录/中文文件.txt",
                "-extract",
                "/a.bin",
                "/tmp/out/a.bin"
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_output_collects_both_streams_and_tails_errors() {
        let (stdout, stderr) = run_output(
            "sh",
            &os(&["-c", "printf 'Volume Id    : X\\n'; printf 'note\\n' >&2"]),
        )
        .expect("exit 0");
        assert_eq!(stdout, "Volume Id    : X\n");
        assert_eq!(stderr, "note\n");

        let err = run_output("sh", &os(&["-c", "printf 'boom\\n' >&2; exit 3"]))
            .expect_err("script exits 3");
        assert!(
            matches!(&err, BurnError::ReadFailed(msg) if msg.contains("boom")),
            "{err:?}"
        );
    }

    #[test]
    fn read_side_errors_drop_the_burn_wording() {
        assert!(matches!(
            read_side_error(BurnError::Failed("x".to_string())),
            BurnError::ReadFailed(tail) if tail == "x"
        ));
        assert!(matches!(
            read_side_error(BurnError::Cancelled),
            BurnError::Cancelled
        ));
    }

    #[test]
    fn blank_image_fallback_is_detected() {
        // 实测（UDF 多区段 DVD-R）：盘上没有 ISO 9660 时 xorriso 兜底造空镜像，
        // -pvd_info 会报默认卷标 ISOIMAGE。
        let stderr = "libisoburn: WARNING : No ISO 9660 image at LBA 1388208.\n\
                      libisoburn: WARNING : Creating blank image.\n";
        assert!(is_blank_image_fallback(stderr));
        assert!(!is_blank_image_fallback(
            "xorriso : NOTE : Loading ISO image tree from LBA 0\n"
        ));
    }

    #[test]
    fn blank_image_and_blank_medium_signals_are_distinct() {
        // 兜底空镜像：读不到 ISO 9660，后端回退的触发信号。
        let fallback = "libisoburn: WARNING : No ISO 9660 image at LBA 1388208.\n\
                        libisoburn: WARNING : Creating blank image.\n";
        assert!(is_blank_image_fallback(fallback));
        assert!(!reports_blank_medium(fallback));
        // 真空白盘（0 区段）：没有兜底行，stderr 报 is blank，stdout 是合成的
        // ISOIMAGE。门禁不能跟着拒空盘（首刻要放行），读卷标归 BlankMedium，后端
        // 回退不碰它（ADR-0024 决策 2）。
        let blank = "Media current: DVD-R sequential recording\nMedia status : is blank\n";
        assert!(!is_blank_image_fallback(blank));
        assert!(reports_blank_medium(blank));
    }

    #[test]
    fn read_backend_mode_values_and_unknown() {
        assert_eq!(
            read_backend_mode_value(None).unwrap(),
            ReadBackendMode::Auto
        );
        assert_eq!(
            read_backend_mode_value(Some("")).unwrap(),
            ReadBackendMode::Auto
        );
        assert_eq!(
            read_backend_mode_value(Some("auto")).unwrap(),
            ReadBackendMode::Auto
        );
        assert_eq!(
            read_backend_mode_value(Some("xorriso")).unwrap(),
            ReadBackendMode::Xorriso
        );
        assert_eq!(
            read_backend_mode_value(Some("native")).unwrap(),
            ReadBackendMode::Native
        );
        // 未知值要报错并列出合法取值，不能静默落回默认。
        let err = read_backend_mode_value(Some("nope")).expect_err("unknown value must fail");
        let BurnError::ReadFailed(message) = &err else {
            panic!("{err:?}");
        };
        assert!(message.contains("auto, xorriso, or native"), "{message}");
    }

    #[test]
    fn fallback_triggers_only_on_missing_iso_signals() {
        assert!(should_fallback(&BurnError::NoIsoSession));
        assert!(should_fallback(&BurnError::ReadFailed("boom".to_string())));
        // 这些换一边也一样或与后端无关，不能借后端回退掩盖。
        for error in [
            BurnError::MissingTool("xorriso".to_string()),
            BurnError::Cancelled,
            BurnError::BlankMedium,
            BurnError::UnsafePath("/C:".to_string()),
            BurnError::Io(std::io::Error::other("io")),
        ] {
            assert!(!should_fallback(&error), "{error:?}");
        }
    }

    #[test]
    fn read_backend_wiring_is_pinned() {
        // Linux 的 auto 是 xorriso 优先加后端回退，不是整体切原生。Windows 是原生。
        let auto = read_backend_name(read_backend_mode_value(None).unwrap()).unwrap();
        assert_eq!(
            auto,
            if cfg!(windows) {
                "native"
            } else {
                "xorriso+native"
            }
        );
        assert_eq!(
            read_backend_name(ReadBackendMode::Native).unwrap(),
            "native"
        );
        // Windows 上没有能访问光驱的 xorriso，显式要求它是错误（ADR-0024 决策 3）。
        assert_eq!(
            read_backend_name(ReadBackendMode::Xorriso).is_err(),
            cfg!(windows)
        );
        assert_eq!(ReadBackendMode::Auto.as_str(), "auto");
        assert_eq!(ReadBackendMode::Xorriso.as_str(), "xorriso");
        assert_eq!(ReadBackendMode::Native.as_str(), "native");
    }

    #[test]
    fn session_state_asks_native_only_for_a_damaged_answer() {
        let damaged = crate::SessionFallback {
            skipped: 1,
            ordinal: 3,
            candidates: 4,
            session_start: 293970,
        };
        // xorriso 报 Unusable（损坏末区段的兜底空镜像形态或纯 UDF 盘）时问原生：
        // 只有原生报出更早的可用区段才采用，损坏末区段因此在 Linux 上报 Damaged。
        assert_eq!(
            merge_session_state(Ok(IsoSessionState::Unusable), || Ok(
                IsoSessionState::Damaged(damaged)
            ))
            .unwrap(),
            IsoSessionState::Damaged(damaged)
        );
        // 纯 UDF 盘两侧都是 Unusable，门禁口径不变。
        assert_eq!(
            merge_session_state(Ok(IsoSessionState::Unusable), || Ok(
                IsoSessionState::Unusable
            ))
            .unwrap(),
            IsoSessionState::Unusable
        );
        // 原生打不开设备（没权限、非 Linux 设备串）时保持 xorriso 的分类，不把
        // 一次分类变成错误。
        assert_eq!(
            merge_session_state(Ok(IsoSessionState::Unusable), || Err(
                BurnError::ReadFailed("no permission".to_string())
            ))
            .unwrap(),
            IsoSessionState::Unusable
        );
        // xorriso 能读时压根不碰原生。
        assert_eq!(
            merge_session_state(Ok(IsoSessionState::Usable), || panic!("must not be called"))
                .unwrap(),
            IsoSessionState::Usable
        );
        // 读侧报错仍走常规后端回退。
        assert_eq!(
            merge_session_state(Err(BurnError::NoIsoSession), || Ok(IsoSessionState::Usable))
                .unwrap(),
            IsoSessionState::Usable
        );
    }

    #[cfg(unix)]
    #[test]
    fn xorriso_side_reports_no_session_on_udf_only() {
        let tmp = TempDir::new("readback-xorriso-udf");
        let image = write_udf_only_image(&tmp);
        let source = image.to_str().expect("utf-8 temp path");
        let cancel = CancelToken::default();

        // 卷标与列举：xorriso 兜底空镜像后照常成功，按 stderr 信号识破。
        assert!(matches!(
            XorrisoRead.read_volume_id(source),
            Err(BurnError::NoIsoSession)
        ));
        assert!(matches!(
            XorrisoRead.list_tree(source),
            Err(BurnError::NoIsoSession)
        ));

        // 整树抽取同样成功但只抽出一个空目录，按信号判成读不到。
        let tree_out = tmp.path().join("xorriso-tree-out");
        assert!(matches!(
            XorrisoRead.extract_tree(&image, &tree_out, &cancel),
            Err(BurnError::NoIsoSession)
        ));
        assert_eq!(count_files(&tree_out), 0, "兜底空镜像抽不出文件");

        // 按路径抽取：xorriso 找不到文件直接以退出码 5 失败（实测 1.5.6），报
        // ReadFailed，同样是后端回退的触发信号。
        let paths_out = tmp.path().join("xorriso-paths-out");
        let err = XorrisoRead
            .extract_paths(source, &["/自述.txt".to_string()], &paths_out, &cancel)
            .expect_err("a path that does not exist in the blank image must fail");
        assert!(matches!(err, BurnError::ReadFailed(_)), "{err:?}");
        assert!(should_fallback(&err), "读侧报错要能触发后端回退");
    }

    #[cfg(unix)]
    #[test]
    fn fallback_reads_udf_only_image() {
        let tmp = TempDir::new("readback-fallback-udf");
        let image = write_udf_only_image(&tmp);
        let source = image.to_str().expect("utf-8 temp path");
        let cancel = CancelToken::default();

        // 卷标：xorriso 读不到，后端回退到原生读出真卷标。
        assert_eq!(FallbackRead.read_volume_id(source).unwrap(), "UDFONLY");

        // 列举：路径集合等于 UDF 夹具的期望树。
        let mut listed = FallbackRead.list_tree(source).expect("list the UDF disc");
        listed.sort_by(|a, b| a.path.cmp(&b.path));
        let expected: Vec<String> = expected_tree().keys().cloned().collect();
        assert_eq!(
            listed.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            expected
        );

        // 整树抽取：中文名与跨扇区二进制逐字节一致。
        let tree_out = tmp.path().join("fallback-tree-out");
        FallbackRead
            .extract_tree(&image, &tree_out, &cancel)
            .expect("extract the tree");
        assert_eq!(
            std::fs::read(tree_out.join("自述.txt")).unwrap(),
            zh_content()
        );
        assert_eq!(
            std::fs::read(tree_out.join("资料/照片/清单.bin")).unwrap(),
            bin_content()
        );

        // 按路径抽取：只抽要的那一个。
        let paths_out = tmp.path().join("fallback-paths-out");
        FallbackRead
            .extract_paths(source, &["/资料/说明.md".to_string()], &paths_out, &cancel)
            .expect("extract by path");
        assert_eq!(
            std::fs::read(paths_out.join("资料/说明.md")).unwrap(),
            b"top level notes"
        );

        // 镜像文件没有未关闭轨道，抢救给 Ok(None)。门禁把纯 UDF 盘归 Unusable。
        assert!(
            FallbackRead
                .salvage(source, &tmp.path().join("salvage"), &cancel)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            FallbackRead.iso_session_state(source).unwrap(),
            IsoSessionState::Unusable
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fallback_leaves_legit_empty_iso_alone() {
        // 合法空目录的 ISO 盘：抽取成功但零文件、读卷标得真值，不触发后端回退。
        let tmp = TempDir::new("readback-empty-iso");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).expect("create empty source dir");
        let image = tmp.path().join("empty.iso");
        build_image(
            &src,
            &image,
            &ImageSpec {
                profile: DiscProfile::Dvd,
                volume_id: "EMPTY".to_string(),
                joliet: true,
            },
        )
        .expect("build the empty image");
        let source = image.to_str().expect("utf-8 temp path");

        assert_eq!(FallbackRead.read_volume_id(source).unwrap(), "EMPTY");
        let tree_out = tmp.path().join("empty-tree-out");
        FallbackRead
            .extract_tree(&image, &tree_out, &CancelToken::default())
            .expect("extract the empty tree");
        assert_eq!(count_files(&tree_out), 0);
    }

    /// 递归数目标目录里的文件数（目录不算）。
    #[cfg(unix)]
    fn count_files(root: &Path) -> usize {
        fn walk(dir: &Path, count: &mut usize) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, count);
                } else {
                    *count += 1;
                }
            }
        }
        let mut count = 0;
        walk(root, &mut count);
        count
    }

    #[test]
    #[ignore = "needs optical drive"]
    fn read_volume_id_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. /dev/sr0");
        let id = read_volume_id(&device).expect("read volume id failed");
        assert!(!id.is_empty(), "volume id is empty");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "needs optical drive"]
    fn readback_compare_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. /dev/sr0");
        let source =
            std::env::var("OPTIBURN_COMPARE_DIR").expect("set OPTIBURN_COMPARE_DIR to a dir");
        let temp = std::env::temp_dir().join(format!("optiburn-readback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("create temp dir");
        extract_tree(Path::new(&device), &temp, &CancelToken::default()).expect("extract failed");
        let differences = crate::compare_trees(Path::new(&source), &temp, "zh");
        let _ = std::fs::remove_dir_all(&temp);
        assert_eq!(differences, Vec::<String>::new(), "{differences:?}");
    }
}
