//! 刻录引擎：把镜像写到盘上。
//!
//! 两种引擎接在同一个 [`BurnEngine`] 接缝上：
//!
//! - [`NativeEngine`]：自己发 MMC 命令（MODE SELECT / WRITE(10) / SYNCHRONIZE CACHE /
//!   CLOSE TRACK/SESSION），
//!   不依赖外部程序，Windows 上唯一可用的刻录路径（ADR-0017）。
//! - [`XorrisoEngine`]：调用 `xorriso -as cdrecord` 子进程（ADR-0004）。
//!
//! 除写盘外还有读侧：读盘上卷标、列目录树、抽取到本地，以及本地目录树的内容
//! 对比（回读校验，见 ADR-0010）。读侧同样有两个后端（xorriso 子进程与原生
//! MMC 加 ISO 9660 解析，ADR-0018），Linux 默认 xorriso 优先，读不到 ISO 9660 时
//! 后端回退到原生（保守开关 `OPTIBURN_READ_BACKEND`，ADR-0024），Windows 走原生。
//!
//! 增长模式（`append`/追加页）同样分平台：Windows 用原生引擎读旧区段、生成
//! 绝对地址约定的新区段（ADR-0020），Linux 维持 xorriso 的增长模式。Linux 上
//! 可以用保守开关 `OPTIBURN_GROW_BACKEND` 把整条追加准备与写入切到原生引擎，
//! 代价是原生增长在 Linux 还没有真机验证（ADR-0028）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use optiburn_mmc::{DiscStatus, MediaKind, MmcDevice};

mod capacity;
mod disc_read;
mod grow;
mod native;
mod readback;
mod verify;
mod xorriso;

pub use capacity::{CapacityGate, capacity_gate};
pub use native::NativeEngine;
pub use readback::{
    DiscEntry, ReadBackendMode, extract_paths, extract_tree, iso_session_state, list_tree,
    native_iso_session_state, read_backend_mode, read_backend_name, read_volume_id, salvage,
};
pub use verify::compare_trees;
pub use xorriso::{BurnFailure, XorrisoEngine};

/// 增长模式（追加）后端的选择：界面或保守开关给的值，不是最终生效的后端。
///
/// `Auto` 是默认：先按平台定（Linux 与 Windows 都走原生增长，ADR-0027），再按设备
/// 事实回退（见 [`grow_backend_choice`]）。`Native` 与 `Xorriso` 是显式选择，不做
/// 回退，原生引擎的缺口按 [`NativeGap`] 原样报出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowBackendMode {
    Auto,
    Xorriso,
    Native,
}

impl GrowBackendMode {
    /// 取值原样回写，用于 `probe` 的诊断行与界面回传。
    pub fn as_str(&self) -> &'static str {
        match self {
            GrowBackendMode::Auto => "auto",
            GrowBackendMode::Xorriso => "xorriso",
            GrowBackendMode::Native => "native",
        }
    }
}

/// 增长模式后端选择：从环境变量 `OPTIBURN_GROW_BACKEND` 读（保守开关，ADR-0027）。
pub fn grow_backend_mode() -> Result<GrowBackendMode, BurnError> {
    grow_backend_mode_value(std::env::var("OPTIBURN_GROW_BACKEND").ok().as_deref())
}

/// 取值判定：空值与 `auto` 都算默认。公开是因为界面把选择按字符串传给命令层，
/// 抽成纯函数则不碰进程环境就能测（形状照 `readback` 的 `read_backend_mode_value`）。
pub fn grow_backend_mode_value(value: Option<&str>) -> Result<GrowBackendMode, BurnError> {
    match value {
        None | Some("") | Some("auto") => Ok(GrowBackendMode::Auto),
        Some("xorriso") => Ok(GrowBackendMode::Xorriso),
        Some("native") => Ok(GrowBackendMode::Native),
        Some(other) => Err(BurnError::ReadFailed(format!(
            "unknown growth backend value {other:?}: expected auto, xorriso, or native"
        ))),
    }
}

/// 后端选择的解析：显式选择优先，其次环境开关，最后平台默认（ADR-0027）。
pub fn resolve_grow_backend(choice: Option<GrowBackendMode>) -> Result<GrowBackendMode, BurnError> {
    match choice {
        Some(mode) => Ok(mode),
        None => grow_backend_mode(),
    }
}

/// 写前阶段计时的开关名：`OPTIBURN_TIMING`（ADR-0028）。默认关闭，正常输出不加噪声。
pub const TIMING_ENV: &str = "OPTIBURN_TIMING";

/// 阶段计时是否开启：读 `OPTIBURN_TIMING`，没设、空值与 `0` 都算关闭（ADR-0028）。
///
/// 判定收在引擎里，CLI 与 GUI 命令层共用一个口径，避免两边各写一份后漂移。
pub fn stage_timing_enabled() -> bool {
    stage_timing_enabled_value(std::env::var(TIMING_ENV).ok().as_deref())
}

/// 开关取值判定。抽出来是为了能不碰进程环境测。
pub fn stage_timing_enabled_value(value: Option<&str>) -> bool {
    !matches!(value, None | Some("") | Some("0"))
}

/// 这台设备与这次写入适不适合走原生增长。
///
/// 回退判据只看驱动器的静态事实与调用方的请求，不看写入过程中的报错：写失败时盘上
/// 可能已经写了一半，重试只会多留一个损坏末区段（ADR-0027）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowSuitability {
    /// 适合走原生增长。
    Native,
    /// 盘片是随机可写介质（MMC 状态位 0b11，例如 DVD-RAM、BD-RE），原生增长不支持。
    Rewritable,
    /// 驱动器报的 Profile 不在原生支持的名单里。
    UnsupportedProfile,
    /// 调用方指定了倍速，而原生引擎没有实现 SET CD SPEED。
    WriteSpeed,
    /// 设备事实探不到（打不开、命令失败）。分派不该成为新的故障点，按平台默认走。
    Unknown,
}

impl GrowSuitability {
    /// 面向用户的中文说明：只在从原生态回落时用得上，`Native` 与 `Unknown` 返回空串。
    ///
    /// 与 `BurnFailure::user_text` 同一口径，文案收在引擎里，CLI 与 GUI 共用一份。
    pub fn user_text(&self) -> &'static str {
        match self {
            GrowSuitability::Rewritable => {
                "盘片是随机可写介质，原生增长不支持，本次改用 xorriso 增长模式（写前会慢一些）。"
            }
            GrowSuitability::UnsupportedProfile => {
                "驱动器报的介质类型不在原生增长支持范围内，本次改用 xorriso 增长模式。"
            }
            GrowSuitability::WriteSpeed => {
                "指定了倍速，而原生引擎还没有实现设速，本次改用 xorriso 增长模式。"
            }
            GrowSuitability::Native | GrowSuitability::Unknown => "",
        }
    }
}

/// 从设备事实判定适配性。纯函数，离线可测。
pub(crate) fn grow_suitability_of(
    media_kind: Option<MediaKind>,
    random_writable: bool,
    speed_requested: bool,
) -> GrowSuitability {
    if speed_requested {
        return GrowSuitability::WriteSpeed;
    }
    if random_writable {
        return GrowSuitability::Rewritable;
    }
    match media_kind {
        Some(_) => GrowSuitability::Native,
        None => GrowSuitability::UnsupportedProfile,
    }
}

/// 探一次设备事实：当前 Profile 与盘片状态。任何一步失败都归
/// [`GrowSuitability::Unknown`]，由调用方按平台默认处理。
fn probe_grow_suitability(device: &str, speed_requested: bool) -> GrowSuitability {
    let Ok(transport) = optiburn_transport::open(device) else {
        return GrowSuitability::Unknown;
    };
    let mut mmc = MmcDevice::new(transport);
    let Ok(profile) = mmc.get_configuration() else {
        return GrowSuitability::Unknown;
    };
    let Ok(info) = mmc.read_disc_information() else {
        return GrowSuitability::Unknown;
    };
    grow_suitability_of(
        profile.media_kind(),
        matches!(info.status, DiscStatus::Other(_)),
        speed_requested,
    )
}

/// 这次写入实际使用的增长模式后端，以及从原生态回落的原因（没有回落就是 `None`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrowBackendChoice {
    /// 最终生效的后端名：`native` 或 `xorriso`。
    pub backend: &'static str,
    /// 从原生态回落时的判据，供界面与 CLI 说明为什么没用原生。
    pub fallback: Option<GrowSuitability>,
}

/// 把选择、设备事实与是否指定倍速解析成这次写入实际用的后端（ADR-0027）。
///
/// 规则只有这一份，引擎的三条入口（门禁判定、尺寸预演、写入）与两个前端都用它：
/// - 选择 `xorriso`：就是 xorriso，Windows 上仍拒绝（那边没有能访问光驱的 xorriso）。
/// - 选择 `native`：就是原生，缺口按 [`NativeGap`] 报错，不回退。
/// - 选择 `auto`：Linux 先用原生，命中 [`GrowSuitability`] 里非 Native 的判据时
///   回退 xorriso。Windows 直接用原生（那边没有可回退的对象），与改动前一致。
pub fn grow_backend_choice(
    mode: GrowBackendMode,
    device: &str,
    speed_requested: bool,
) -> Result<GrowBackendChoice, BurnError> {
    let selected = grow_backend_name(mode)?;
    if selected == "xorriso" || mode == GrowBackendMode::Native || cfg!(windows) {
        return Ok(GrowBackendChoice {
            backend: selected,
            fallback: None,
        });
    }
    match probe_grow_suitability(device, speed_requested) {
        GrowSuitability::Native | GrowSuitability::Unknown => Ok(GrowBackendChoice {
            backend: "native",
            fallback: None,
        }),
        reason => Ok(GrowBackendChoice {
            backend: "xorriso",
            fallback: Some(reason),
        }),
    }
}

/// 模式对应的名义后端名，供 `probe` 报增长模式现状（ADR-0027）。`auto` 在两边都
/// 是原生。Linux 上实际生效的可能是回退后的 xorriso，那要问 [`grow_backend_choice`]。
///
/// Windows 上唯一能取得的 xorriso 不含光驱访问（ADR-0008 补记），显式要求它等于
/// 要求一个用不了的后端，直接拒绝并给出退路。
pub fn grow_backend_name(mode: GrowBackendMode) -> Result<&'static str, BurnError> {
    match mode {
        GrowBackendMode::Xorriso if cfg!(windows) => Err(BurnError::ReadFailed(
            "no usable xorriso build on Windows (ADR-0008); unset OPTIBURN_GROW_BACKEND or set auto/native"
                .to_string(),
        )),
        GrowBackendMode::Xorriso => Ok("xorriso"),
        GrowBackendMode::Native | GrowBackendMode::Auto => Ok("native"),
    }
}

/// 把目录追加到盘上（增长模式）：读出盘上末区段的目录树，把源目录并进去后作为
/// 新区段提交。
///
/// `mode` 是调用方的选择，最终用哪个后端由 [`grow_backend_choice`] 按设备事实定
/// （ADR-0027）。Linux 默认走原生增长，写前窗口里不再起子进程。
pub fn grow(
    mode: GrowBackendMode,
    job: &GrowJob,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let choice = grow_backend_choice(mode, &job.device, job.speed.is_some())?;
    match choice.backend {
        "native" => native::grow(job, progress, cancel),
        _ => xorriso::grow(job, progress, cancel),
    }
}

/// 追加写前门禁要问的那个问题：将要写盘的引擎能不能读盘上末区段。
///
/// 门禁的结论决定要不要放行写盘，所以判定要交给写盘方自己的读法（ADR-0027）：
/// xorriso 写盘时走 xorriso 那一侧的读侧分派（libisofs 读得到就放行，读不到再问
/// 原生，ADR-0024 决策 7），原生写盘时走原生读侧。这个查询只读盘，不写。
///
/// `speed_requested` 与写入端同一判据：指定了倍速时实际写盘的是 xorriso，门禁也
/// 必须问 xorriso，否则两侧对同一张盘可能给出不同结论。
pub fn append_gate_session_state(
    mode: GrowBackendMode,
    device: &str,
    speed_requested: bool,
) -> Result<IsoSessionState, BurnError> {
    let choice = grow_backend_choice(mode, device, speed_requested)?;
    gate_session_state_with(choice.backend, device)
}

/// 写入端后端名到门禁查询的映射。抽出来是为了能不碰进程环境测（见测试
/// `gate_answers_are_the_same_under_both_writing_backends`）。
pub(crate) fn gate_session_state_with(
    backend: &str,
    device: &str,
) -> Result<IsoSessionState, BurnError> {
    match backend {
        "native" => native_iso_session_state(device),
        _ => iso_session_state(device),
    }
}

/// 尝试修复被中断刻录留下的损坏轨道与区段：按 libburn 的 `burn_disc_close_damaged`
/// 的顺序（CD 与 DVD-R 族先下发写参数页再关区段，+R 族与 BD-R 关最后一条轨道），
/// 等价于 xorriso 的 `-close_damaged as_needed`（`force` 对应 `force`）。驱动器报
/// 损坏才动，`force` 时无条件尝试。命令与介质族不匹配时返回错误。
///
/// 只做修复与状态复核，不写数据。修复成功后 [`grow`] 就可能在这张盘上继续。
pub fn repair(device: &str, force: bool) -> Result<RepairOutcome, BurnError> {
    if cfg!(windows) {
        native::repair(device, force)
    } else {
        xorriso::repair(device, force)
    }
}

/// 追加会话的尺寸预演：返回即将写入的新区段字节数，供调用方的写前容量门禁
/// （ADR-0019）。不写盘，但会读盘片状态与旧区段目录树。分派与 [`grow`] 用
/// [`grow_backend_choice`] 同一份判据，预演与提交因此看到同一个后端。
pub fn grow_size(mode: GrowBackendMode, job: &GrowJob) -> Result<u64, BurnError> {
    let choice = grow_backend_choice(mode, &job.device, job.speed.is_some())?;
    match choice.backend {
        "native" => native::grow_size(job),
        _ => xorriso::grow_size(job),
    }
}

/// 依赖的可执行文件名。
pub(crate) const XORRISO: &str = "xorriso";
/// 子进程失败时保留多少行 stderr 作为摘要。
pub(crate) const TAIL_LINES: usize = 10;

/// 一次镜像刻录任务的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnJob {
    /// 待写入的镜像文件，例如 `.iso`。
    pub image: PathBuf,
    /// 目标设备：Linux 形如 `/dev/sr0`，Windows 形如 `E:`。
    pub device: String,
    /// 写入倍速。`None` 表示交给驱动自选。
    pub speed: Option<u32>,
    /// 是否以多区段方式追加（`-multi`）。
    pub multi: bool,
}

/// 一次目录追加（增长模式）任务的输入。
///
/// 增长模式不经过镜像文件：xorriso 读出盘上已有区段的目录树，把源目录内容并进
/// 去后作为新区段提交，因此旧文件仍可见。hadris 只能从零建整盘镜像，区段合并
/// 只能落在引擎侧（见 ADR-0006）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrowJob {
    /// 源目录，其内容成为盘上根目录。
    pub src: PathBuf,
    /// 目标设备：Linux 形如 `/dev/sr0`，Windows 形如 `E:`。
    pub device: String,
    /// 写入倍速。`None` 表示交给驱动自选。
    pub speed: Option<u32>,
    /// 新区段的卷标。
    pub volume_id: String,
    /// 提交后把盘标记为不可追加（封盘）。
    pub close_disc: bool,
    /// 允许在末区段损坏的盘上追加，回退到更早的可读区段（ADR-0022）。
    ///
    /// 置位前调用方必须把 [`SessionFallback`] 的内容告知用户：被跳过区段里的文件
    /// 不在新会话的目录树里，等于从可见视图消失。没有这个授权时引擎按
    /// [`BurnError::DamagedLastSession`] 拒绝。
    pub allow_damaged_last_session: bool,
}

/// 末区段损坏时回退到更早区段的信息（读取与追加共用，ADR-0022）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionFallback {
    /// 被跳过的候选个数（0 表示末区段本身可用）。
    pub skipped: usize,
    /// 选中区段在候选里的序号，从旧到新数，1 起。
    pub ordinal: usize,
    /// 参与枚举的候选总数。
    pub candidates: usize,
    /// 选中区段的起点（盘级块号）。
    pub session_start: u32,
}

/// 抢救未关闭轨道（中断写入）时的文件状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalvageState {
    /// 数据完整写在盘上。
    Complete,
    /// 只写了一部分，`readable_bytes` 是能读出来的字节数。
    Truncated { readable_bytes: u64 },
    /// 一个字节都没写（中断点在这个文件的数据之前）。
    Missing,
}

/// 抢救结果里的一条文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalvagedFile {
    pub path: String,
    pub size: u64,
    pub state: SalvageState,
    /// 半截的 `.zip` 走进重建流程后的统计，文件有没有真的被重写看 `rebuilt`，
    /// 其他情况为 `None`。
    pub zip: Option<ZipSalvage>,
}

/// 截断 zip 的重建结果：把能读出来的部分里完整的条目抽成一个新的 zip。
///
/// `entries` 为 0 表示链是 zip 的链但一条完整条目都没有（中断点落在第一条里面），
/// 这时文件没有被重写，还是抢救出来的原始字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipSalvage {
    /// 保留下来（数据读全、长度自洽）的条目数。
    pub entries: u32,
    /// 保留的条目解压前的总大小。
    pub stored_bytes: u64,
    /// 中断点落在的那个条目名，没保留下来。
    pub dropped_entry: Option<String>,
    /// 是否真的把文件重写成了可解的 zip。条目数或总长超出普通 zip 的表达范围时
    /// 为假，文件保留原始字节（与 zip64 同一取舍）。
    pub rebuilt: bool,
}

/// 抢救未关闭轨道（中断写入）的结果（ADR-0022 补记）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalvageReport {
    /// 未关闭轨道的起点。
    pub session_start: u32,
    /// 已经写下去的数据边界（不含）。
    pub boundary: u32,
    /// 目录树里能列出的文件及各自的状态。
    pub files: Vec<SalvagedFile>,
}

impl SalvageReport {
    /// 状态为完整的文件数。
    pub fn complete(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.state == SalvageState::Complete)
            .count()
    }

    /// 只写了一部分的文件数。
    pub fn truncated(&self) -> usize {
        self.files
            .iter()
            .filter(|file| matches!(file.state, SalvageState::Truncated { .. }))
            .count()
    }

    /// 一个字节都没写的文件数。
    pub fn missing(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.state == SalvageState::Missing)
            .count()
    }
}

/// 读盘抽取的结果：每个文件一条结论，单个文件读失败不中断整次抽取（ADR-0029）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    /// 处理过的文件及其状态。
    pub files: Vec<ExtractedFile>,
}

/// 抽取报告里的一条文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFile {
    /// 盘上路径，以 `/` 开头。
    pub path: String,
    /// 盘上目录记录里的字节数，读不出的文件也保留。
    pub size: u64,
    pub state: ExtractState,
}

/// 单个文件的抽取状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractState {
    /// 完整抽出，字节数与目录一致。
    Complete,
    /// 一个字节都没抽出，`reason` 是下层错误的文案。
    Unreadable { reason: String },
}

impl ExtractReport {
    /// 状态为完整的文件数。
    pub fn complete(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.state == ExtractState::Complete)
            .count()
    }

    /// 读不出的文件数。
    pub fn unreadable(&self) -> usize {
        self.files
            .iter()
            .filter(|file| matches!(file.state, ExtractState::Unreadable { .. }))
            .count()
    }
}

/// 修复尝试的结果（`optiburn repair`，对齐 xorriso 的 `-close_damaged`，ADR-0022）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairOutcome {
    /// 驱动器是否把下一轨道报成损坏（MMC 的 Damage 位）。
    pub damaged: bool,
    /// 是否真的执行了修复（未报损坏且没强制时不执行）。
    pub attempted: bool,
    /// 修复后是否拿到可用的可写地址。
    pub writable: bool,
    /// 修复后的下一个可写地址。
    pub next_writable_address: Option<u32>,
    /// 修复后的剩余可写字节数（驱动器报剩余块数时）。
    pub free_bytes: Option<u64>,
    /// 修复命令自身的报错（驱动器拒绝修复时），执行成功是 `None`。
    pub error: Option<String>,
}

/// 盘上 ISO 9660 会话的可用状态，追加门禁据此分流（ADR-0022）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoSessionState {
    /// 末区段就是可读的 ISO 9660 会话。
    Usable,
    /// 末区段损坏，回退到更早的会话。追加会跳过被跳过区段里的文件，需要确认。
    Damaged(SessionFallback),
    /// 盘上没有被支持的 ISO 9660 会话（空白盘、纯 UDF 盘、音频盘）。
    Unusable,
}

#[derive(Debug, thiserror::Error)]
pub enum BurnError {
    #[error("missing tool: {0}")]
    MissingTool(String),
    #[error("burn failed: {0}")]
    Failed(String),
    #[error("burn cancelled")]
    Cancelled,
    /// 盘上最后一区段不是 ISO 9660（例如 UDF 盘），相关读取与续写不可用。
    #[error("no ISO 9660 image at the last session")]
    NoIsoSession,
    /// 真空白盘（0 区段）。与“读不到 ISO 9660”分开，后端回退不碰它（ADR-0024）。
    #[error("the medium is blank")]
    BlankMedium,
    /// 读侧子进程失败（列目录、读卷标等），与刻录失败区分开，动作语境由调用方给出。
    #[error("{0}")]
    ReadFailed(String),
    /// 盘内路径不安全（盘符前缀、`..` 或 Windows 分隔符），拒绝抽取。
    #[error("unsafe path in image: {0}")]
    UnsafePath(String),
    /// 原生引擎的 MMC 命令失败（传输层错误或命令级失败），与子进程引擎的错误分开。
    #[error("MMC: {0}")]
    Mmc(#[from] optiburn_mmc::MmcError),
    /// 原生引擎做不到这次请求，原因见 [`NativeGap`]，文案由 CLI 与 GUI 分别给出。
    #[error("native engine gap: {0}")]
    NativeGap(NativeGap),
    /// 盘上剩余空间放不下待写入的新区段（写前容量门禁，ADR-0019）。
    #[error(
        "not enough space for the new session: needs {needed} bytes, {headroom} bytes of session overhead, {free} free"
    )]
    NotEnoughSpace {
        needed: u64,
        headroom: u64,
        free: u64,
    },
    /// 盘上旧区段的形状不支持嫁接式增长（无 Joliet、启动记录、多 extent 文件等）。
    #[error("cannot grow this disc with the native engine: {0}")]
    GrowUnsupported(String),
    /// 末区段损坏，回退到更早区段前需要用户确认（ADR-0022）。
    #[error(
        "the last session is damaged: skipped {} candidates, the readable session starts at LBA {}",
        .fallback.skipped,
        .fallback.session_start
    )]
    DamagedLastSession { fallback: SessionFallback },
    /// 盘上没有可用的可写地址（NWA_V 清零，或 NWA 不落在末区段之后），不能续写。
    /// `damaged` 区分两种形态：驱动器认了损坏轨道（libburn 的 "Damaged, not closed
    /// and not writable"）与单纯的地址不可用（"No Next-Writable-Address"）。
    #[error(
        "no usable next writable address (last session at LBA {last_session_start}, damaged {damaged})"
    )]
    WriteAddressUnknown {
        last_session_start: u32,
        damaged: bool,
    },
    /// 追加的目录与盘上已有内容冲突（同名文件对目录、命名空间内重名）。
    #[error("growth content conflicts with the disc: {0}")]
    GrowConflict(String),
    /// 盘上最后一区段是 UDF，但用了 hadris-udf 读不了的结构（VAT、元数据分区等）。
    #[error("unsupported UDF structure: {0}")]
    UnsupportedUdf(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// 原生引擎的能力缺口：结构与理由分开，界面层才能各给母语文案（ADR-0017）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeGap {
    #[error("setting the write speed (SET CD SPEED) is not implemented yet")]
    WriteSpeed,
    #[error("the image is empty")]
    EmptyImage,
    #[error("the image does not fit on the media")]
    ImageTooLarge,
    #[error("the image exceeds the addressable 2048-byte blocks")]
    ImageBeyondAddressRange,
    #[error("media profile {0:#06x} is not in the supported list")]
    UnsupportedProfile(u16),
    #[error("the disc is finalized")]
    FinalizedDisc,
    #[error("growing a rewritable disc is not supported by the native engine")]
    GrowthOnRewritable,
    #[error("the source directory is empty")]
    EmptyGrowSource,
}

impl BurnError {
    /// 缺外部工具时给用户的中文说明，CLI 与 GUI 共用一份，避免两端各写一份后
    /// 口径漂移。安装指引只在这里和 GUI 的英文镜像里维护，`MissingTool` 本身
    /// 只带工具名，不再把某一种系统的安装命令编进跨平台的错误里。
    ///
    /// Windows 上写 MSYS2 的构建不可用而不是给出安装命令：实测（1.5.8.pl02，
    /// 2026-10-10 于 USB 光驱）该构建不含 MMC 传输层，`-devices` 报
    /// `No MMC transport adapter is present. Running on sg-dummy.c.`，设备参数会
    /// 落进 libburn 的 stdio 伪设备。装了也读不了盘、刻不了录。
    pub fn missing_tool_user_text(tool: &str) -> String {
        format!(
            "缺少 {tool}，读取盘片与刻录都依赖它。Linux 用发行版的包管理器安装（Debian/Ubuntu 是 sudo apt install {tool}）。Windows 上 MSYS2 的构建不含光驱访问，装了也无法读盘与刻录。"
        )
    }
}

/// 协作式取消令牌：取消方置位，引擎在进度循环里检查并停掉子进程。
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位取消。Relaxed 就够：这一位只承载“要不要停”，晚一行输出被看到
    /// 无妨，也不需要与其它内存访问建立先后关系。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// 一种把镜像写到盘上的方式。
pub trait BurnEngine {
    /// 引擎名，用于 CLI 的 `--engine` 取值与日志。
    fn name(&self) -> &'static str;

    /// 执行刻录。`progress` 收到 0.0–1.0 的进度回调。`cancel` 在进度循环里被
    /// 检查，置位后子进程被停掉并返回 `BurnError::Cancelled`，除非子进程已自然
    /// 写完退出。那时盘已写完，迟到取消不算数。
    fn burn(
        &self,
        job: &BurnJob,
        progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<(), BurnError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺工具文案钉住两条事实：Linux 给得出安装命令，Windows 不给（MSYS2 的构建
    /// 不含光驱访问，写成安装指引会把用户引到死路）。
    #[test]
    fn missing_tool_text_names_the_linux_route_only() {
        let text = BurnError::missing_tool_user_text("xorriso");
        assert!(text.contains("xorriso"), "{text}");
        assert!(text.contains("sudo apt install xorriso"), "{text}");
        assert!(text.contains("MSYS2"), "{text}");
        assert!(!text.contains("pacman"), "{text}");
    }

    /// 增长模式开关的取值判定与读侧同形状：空值与 auto 都是默认，未知值报错并
    /// 列出合法取值，不静默落回默认（ADR-0028）。
    #[test]
    fn grow_backend_mode_values_and_unknown() {
        assert_eq!(
            grow_backend_mode_value(None).unwrap(),
            GrowBackendMode::Auto
        );
        assert_eq!(
            grow_backend_mode_value(Some("")).unwrap(),
            GrowBackendMode::Auto
        );
        assert_eq!(
            grow_backend_mode_value(Some("auto")).unwrap(),
            GrowBackendMode::Auto
        );
        assert_eq!(
            grow_backend_mode_value(Some("xorriso")).unwrap(),
            GrowBackendMode::Xorriso
        );
        assert_eq!(
            grow_backend_mode_value(Some("native")).unwrap(),
            GrowBackendMode::Native
        );
        let err = grow_backend_mode_value(Some("cdrecord")).expect_err("unknown value must fail");
        let BurnError::ReadFailed(message) = &err else {
            panic!("{err:?}");
        };
        assert!(message.contains("auto, xorriso, or native"), "{message}");
        assert_eq!(GrowBackendMode::Auto.as_str(), "auto");
        assert_eq!(GrowBackendMode::Xorriso.as_str(), "xorriso");
        assert_eq!(GrowBackendMode::Native.as_str(), "native");
    }

    /// 增长模式的后端分派：`auto` 两边都是原生（ADR-0027），显式值覆盖平台，
    /// Windows 上的显式 xorriso 被拒（那边没有能访问光驱的 xorriso）。
    #[test]
    fn grow_backend_wiring_is_pinned() {
        assert_eq!(grow_backend_name(GrowBackendMode::Auto).unwrap(), "native");
        assert_eq!(
            grow_backend_name(GrowBackendMode::Native).unwrap(),
            "native"
        );
        assert_eq!(
            grow_backend_name(GrowBackendMode::Xorriso).is_err(),
            cfg!(windows)
        );
        // 显式选择不探设备、不回退：设备串可以随便给。
        assert_eq!(
            grow_backend_choice(GrowBackendMode::Native, "/dev/optiburn-missing", false)
                .unwrap()
                .backend,
            "native"
        );
        if !cfg!(windows) {
            assert_eq!(
                grow_backend_name(GrowBackendMode::Xorriso).unwrap(),
                "xorriso"
            );
            assert_eq!(
                grow_backend_choice(GrowBackendMode::Xorriso, "/dev/optiburn-missing", false)
                    .unwrap()
                    .backend,
                "xorriso"
            );
            // 探不到设备事实时按平台默认走原生，不把分派变成新的故障点。
            let choice =
                grow_backend_choice(GrowBackendMode::Auto, "/dev/optiburn-missing", false).unwrap();
            assert_eq!(choice.backend, "native");
            assert_eq!(choice.fallback, None);
        }
    }

    /// 回退判据只看设备事实与倍速请求，不看写入过程中的报错（ADR-0027）：随机可写
    /// 介质与未知 Profile 回退 xorriso，指定倍速回退，其余走原生。
    #[test]
    fn grow_suitability_rules_are_pinned() {
        assert_eq!(
            grow_suitability_of(Some(MediaKind::Cd), false, false),
            GrowSuitability::Native
        );
        assert_eq!(
            grow_suitability_of(Some(MediaKind::PlusOrBdR), false, false),
            GrowSuitability::Native
        );
        assert_eq!(
            grow_suitability_of(None, false, false),
            GrowSuitability::UnsupportedProfile
        );
        assert_eq!(
            grow_suitability_of(Some(MediaKind::DvdMinus), true, false),
            GrowSuitability::Rewritable
        );
        // 倍速优先于介质判据：指定了倍速就一定走 xorriso。
        assert_eq!(
            grow_suitability_of(Some(MediaKind::Cd), false, true),
            GrowSuitability::WriteSpeed
        );
        assert_eq!(
            grow_suitability_of(Some(MediaKind::PlusOrBdR), true, true),
            GrowSuitability::WriteSpeed
        );
    }

    /// 阶段计时默认关闭：没设、空值与 `0` 都不打行，其余值打开（ADR-0028）。判定收在
    /// 引擎一份，CLI 与 GUI 命令层共用，两边不再各写一遍。
    #[test]
    fn stage_timing_is_opt_in() {
        assert!(!stage_timing_enabled_value(None));
        assert!(!stage_timing_enabled_value(Some("")));
        assert!(!stage_timing_enabled_value(Some("0")));
        assert!(stage_timing_enabled_value(Some("1")));
        assert!(stage_timing_enabled_value(Some("yes")));
    }

    /// 后端选择的解析顺序：显式选择优先，其次环境开关，最后平台默认（ADR-0027）。
    #[test]
    fn resolve_grow_backend_prefers_the_explicit_choice() {
        assert_eq!(
            resolve_grow_backend(Some(GrowBackendMode::Xorriso)).unwrap(),
            GrowBackendMode::Xorriso
        );
        assert_eq!(
            resolve_grow_backend(Some(GrowBackendMode::Native)).unwrap(),
            GrowBackendMode::Native
        );
        // 没有显式选择时读环境开关，未设置就是默认档（不碰进程环境的判定在 value 版）。
        assert_eq!(
            grow_backend_mode_value(None).unwrap(),
            GrowBackendMode::Auto
        );
    }
}
