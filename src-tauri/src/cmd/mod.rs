//! Tauri 命令层：把核心 crate 的能力暴露给前端，文案按系统语言输出中英文。

use std::path::{Path, PathBuf};

use optiburn_engine::{
    BurnEngine, BurnError, BurnFailure, BurnJob, CancelToken, GrowJob, NativeEngine, NativeGap,
    XorrisoEngine, grow, grow_size, last_session_is_iso, read_volume_id,
};
use optiburn_mastering::{DiscProfile, ImageSpec, MasteringError, build_image};
use optiburn_mmc::{
    DiscInformation, DiscStatus, MmcDevice, MmcError, WriteBlock, approve_write, wait_until_ready,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::i18n::{Lang, lang, pick};
use crate::job::{JobKind, JobState, RunningJob};

pub(crate) mod copy;
pub(crate) mod verify;

/// 一台光驱及其盘片状态，字段与前端 `types.ts` 的 `DeviceInfo` 一一对应。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    path: String,
    identity: Option<String>,
    /// "empty" | "appendable" | "finalized" | "other"，读取失败时为 null。
    status: Option<String>,
    /// status 为 "other" 时的原始状态位（随机可写介质）。
    status_bits: Option<u8>,
    sessions: Option<u8>,
    /// 盘片总容量（字节），读不到时为 null（ADR-0019）。
    capacity_bytes: Option<u64>,
    /// 可用容量（字节），由剩余块数或未格式化介质的最大容量得出，读不到时为 null。
    free_bytes: Option<u64>,
    error: Option<String>,
}

/// 镜像制作结果，对应 `ImageInfo`。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfoDto {
    sectors: u64,
    bytes: u64,
    filesystems: Vec<String>,
}

/// 进度事件载荷（事件名 `job-progress`）。
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
struct JobProgress {
    kind: JobKind,
    fraction: f32,
}

/// 完成事件载荷（事件名 `job-done`），outcome 取 "done" | "cancelled" | "failed"。
/// gate 仅在门禁类失败时给出，供前端弹对应的引导对话框。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct JobDone {
    kind: JobKind,
    outcome: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    gate: Option<&'static str>,
}

/// 刻录/追加前门禁与刻录本体可能失败的来源，文案映射集中在 [`job_error_text`]。
enum JobError {
    Open {
        device: String,
        source: optiburn_transport::TransportError,
    },
    /// 光盘被系统挂载，写盘或抽取都拿不到独占访问。
    Mounted {
        device: String,
        point: PathBuf,
    },
    /// 追加路径上末区段不是 ISO 9660（例如 UDF 盘），续写会遮住原有内容。
    NoIsoSession,
    /// 待写入量超过盘上可用容量，写前容量门禁拒绝（ADR-0019）。
    Capacity {
        needed: u64,
        free: u64,
    },
    /// 待刻录列表或暂存阶段的输入问题，文案已由下层给出。
    Input(String),
    /// 用户中止了可取消的只读任务（复制、校验）。
    Cancelled {
        zh: &'static str,
        en: &'static str,
    },
    /// 等待介质就绪或读取盘片信息失败。
    Disc(MmcError),
    /// 写前门禁拒绝。
    Gate(WriteBlock),
    /// 盘上旧区段的形状不支持原生增长（无 Joliet、启动记录、多 extent 文件等）。
    GrowUnsupported(String),
    /// 追加内容与盘上已有内容冲突（同名文件对目录、命名空间内重名）。
    GrowConflict(String),
    /// 盘上最后一区段是 UDF，但用了本工具读不了的结构（VAT、元数据分区等）。
    UnsupportedUdf(String),
    Burn(BurnError),
    Mastering(MasteringError),
}

/// 盘片状态到前端字段（status, status_bits）的映射。
fn status_fields(status: DiscStatus) -> (String, Option<u8>) {
    match status {
        DiscStatus::Empty => ("empty".into(), None),
        DiscStatus::Appendable => ("appendable".into(), None),
        DiscStatus::Finalized => ("finalized".into(), None),
        DiscStatus::Other(bits) => ("other".into(), Some(bits)),
    }
}

/// 前端介质字符串到 [`DiscProfile`] 的映射。
fn profile_from_str(lang: Lang, profile: &str) -> Result<DiscProfile, String> {
    match profile {
        "cd" => Ok(DiscProfile::Cd),
        "dvd" => Ok(DiscProfile::Dvd),
        "bd" => Ok(DiscProfile::Bd),
        other => Err(match lang {
            Lang::Zh => format!("未知的介质类型：{other}"),
            Lang::En => format!("Unknown media profile: {other}"),
        }),
    }
}

/// GUI 没有“当前目录”语义：默认输出落在源目录的父目录下，同名加 .iso；
/// 源为根目录（没有父目录）时退回 optiburn.iso。
fn default_output(src: &Path) -> PathBuf {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "optiburn".to_string());
    let iso = format!("{name}.iso");
    match src.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => parent.join(iso),
        None => PathBuf::from(iso),
    }
}

/// 用户填写的输出路径没有扩展名时补 `.iso`：手工输入与保存对话框（Linux 端不自动
/// 补后缀）都可能给出无后缀文件名，落盘后系统无法识别镜像格式。已有扩展名（含
/// `.img` 等别名与大小写变体）尊重输入。
fn ensure_iso_extension(path: PathBuf) -> PathBuf {
    let mut path = path;
    if path.extension().is_none() {
        path.set_extension("iso");
    }
    path
}

/// 引擎错误的用户文案：缺工具是跨动作的通用故障，单独成句并给出安装途径，动作
/// 前缀只留给其它错误（挂载占用、盘内路径之类）。调用方给动作的裸词。
pub(crate) fn engine_error_text(
    lang: Lang,
    action_zh: &str,
    action_en: &str,
    error: &BurnError,
) -> String {
    match error {
        BurnError::MissingTool(tool) => missing_tool_text(lang, tool),
        // 盘上的 UDF 结构读不了：这不是“某个动作失败”，文案已自足。
        BurnError::UnsupportedUdf(detail) => match lang {
            Lang::Zh => format!("这张盘的 UDF 结构本工具暂不支持读取：{detail}"),
            Lang::En => {
                format!("This disc uses a UDF structure that this tool cannot read yet: {detail}")
            }
        },
        other => match lang {
            Lang::Zh => format!("{action_zh}失败：{other}"),
            Lang::En => format!("{action_en} failed: {other}"),
        },
    }
}

/// 缺外部工具的文案：中文与 CLI 共用引擎里的一份（口径不漂移），英文镜像在这里。
/// Windows 上不给安装命令：MSYS2 的构建不含光驱访问，装了也读不了盘（见引擎注释）。
pub(crate) fn missing_tool_text(lang: Lang, tool: &str) -> String {
    match lang {
        Lang::Zh => BurnError::missing_tool_user_text(tool),
        Lang::En => format!(
            "{tool} is missing, and reading discs and burning both depend on it. On Linux use your distribution's package manager (Debian/Ubuntu: sudo apt install {tool}). On Windows the MSYS2 build has no drive access, so installing it will not enable reading or burning."
        ),
    }
}

/// 原生引擎能力缺口的两语文案，与 CLI 的同名函数同口径（见原生引擎的 ADR-0017）。
/// 建议里点明平台差异：xorriso 引擎只在 Linux 可用，Windows 上只能换盘或等引擎补齐。
fn native_gap_text(lang: Lang, gap: NativeGap) -> String {
    match gap {
        NativeGap::WriteSpeed => pick(
            lang,
            "原生引擎暂不支持指定倍速：去掉 --speed 再试。",
            "The native engine does not support setting the write speed yet: drop --speed and try again.",
        )
        .to_string(),
        NativeGap::EmptyImage => pick(
            lang,
            "镜像为空文件，没有可刻录的内容。",
            "The image is an empty file, so there is nothing to burn.",
        )
        .to_string(),
        NativeGap::ImageTooLarge => pick(
            lang,
            "镜像超出介质容量，请换更大的盘。",
            "The image does not fit on the disc. Use a larger one.",
        )
        .to_string(),
        NativeGap::ImageBeyondAddressRange => pick(
            lang,
            "镜像超出 2048 字节块的地址上限。",
            "The image exceeds the addressable 2048-byte blocks.",
        )
        .to_string(),
        NativeGap::UnsupportedProfile(code) => match lang {
            Lang::Zh => format!(
                "这种介质（Profile {code:#06x}）暂不支持原生引擎，Linux 上可改用 xorriso 引擎。"
            ),
            Lang::En => format!(
                "The native engine does not support this media yet (profile {code:#06x}). On Linux, use the xorriso engine instead."
            ),
        },
        NativeGap::FinalizedDisc => pick(
            lang,
            "盘已封口，无法再写入，请更换盘片。",
            "The disc is finalized and cannot be written again. Use another disc.",
        )
        .to_string(),
        NativeGap::GrowthOnRewritable => pick(
            lang,
            "可覆写介质（DVD-RAM、BD-RE 这类）上的追加暂不支持原生引擎：这类盘可以直接用「刻录」整体重写，或换可追加的盘片。",
            "Growing a rewritable disc (DVD-RAM, BD-RE) is not supported by the native engine yet. Burn the whole disc again instead, or use an appendable disc.",
        )
        .to_string(),
        NativeGap::EmptyGrowSource => pick(
            lang,
            "追加的目录是空的，没有可写入的内容。",
            "The directory to append is empty, so there is nothing to write.",
        )
        .to_string(),
    }
}

/// 任务失败的文案，按界面语言输出中英文。与 CLI 同源的说法按 GUI 调整（“追加页”而非命令行）。
fn job_error_text(lang: Lang, error: &JobError) -> String {
    match error {
        JobError::Open { device, source } => match lang {
            Lang::Zh => format!("打开 {device} 失败：{source}"),
            Lang::En => format!("Failed to open {device}: {source}"),
        },
        JobError::Mounted { device, point } => match lang {
            Lang::Zh => format!(
                "光盘已被系统挂载在 {}：在文件管理器里卸载该光盘，或运行 udisksctl unmount -b {device} 后再试。",
                point.display()
            ),
            Lang::En => format!(
                "The disc is mounted at {}: unmount it in the file manager (or run udisksctl unmount -b {device}) and try again.",
                point.display()
            ),
        },
        JobError::NoIsoSession => pick(
            lang,
            "盘上最后的区段不是 ISO 9660（例如 Windows 写入的 UDF 盘）：追加 ISO 区段后，按最后一区段挂载的系统将只看到新内容。本工具暂不支持续写这类盘，请换用空白盘重刻。",
            "The last session on this disc is not ISO 9660 (e.g. a UDF disc written by Windows): appending an ISO session would leave systems that mount the last session seeing only the new content. This tool cannot append to such discs yet — use a blank disc instead.",
        )
        .into(),
        JobError::Capacity { needed, free } => {
            let needed = copy::human_bytes(lang, *needed);
            let available = copy::human_bytes(lang, *free);
            match lang {
                Lang::Zh => format!(
                    "这张盘放不下本次写入：待写入约 {needed}，盘上可用容量约 {available}。请减少待写内容或更换盘片。"
                ),
                Lang::En => format!(
                    "The disc cannot hold this write: about {needed} to write, {available} free on the disc. Remove some files or use another disc."
                ),
            }
        }
        JobError::Input(message) => message.clone(),
        JobError::Disc(MmcError::NotReady) => pick(
            lang,
            "盘未就绪：请确认已放入可写盘片且仓门已关闭。",
            "Disc not ready: make sure a writable disc is inserted and the tray is closed.",
        )
        .into(),
        JobError::Disc(other) => match lang {
            Lang::Zh => format!("读取盘片信息失败：{other}"),
            Lang::En => format!("Failed to read disc information: {other}"),
        },
        JobError::Gate(WriteBlock::NeedGrowMode) => pick(
            lang,
            "盘上已有数据区段：以镜像方式追加会把已有文件遮住。请改用追加页。",
            "The disc already has data sessions: writing an image over it would shadow existing files. Use the append page instead.",
        )
        .into(),
        JobError::Gate(WriteBlock::Finalized) => pick(
            lang,
            "盘已封口，无法再写入，请更换盘片。",
            "The disc is finalized and cannot be written again. Use another disc.",
        )
        .into(),
        JobError::GrowUnsupported(detail) => match lang {
            Lang::Zh => {
                format!("这张盘暂时没法用原生引擎追加：{detail}。Linux 上可以改用 xorriso 引擎。")
            }
            Lang::En => format!(
                "This disc cannot be grown by the native engine: {detail}. On Linux the xorriso engine can be used instead."
            ),
        },
        JobError::GrowConflict(detail) => match lang {
            Lang::Zh => format!("追加内容与盘上内容有冲突：{detail}"),
            Lang::En => {
                format!("The appended content conflicts with what is on the disc: {detail}")
            }
        },
        JobError::UnsupportedUdf(detail) => match lang {
            Lang::Zh => format!("这张盘的 UDF 结构本工具暂不支持读取：{detail}"),
            Lang::En => format!(
                "This disc uses a UDF structure that this tool cannot read yet: {detail}"
            ),
        },
        JobError::Burn(BurnError::MissingTool(tool)) => missing_tool_text(lang, tool),
        JobError::Burn(BurnError::NativeGap(gap)) => native_gap_text(lang, *gap),
        JobError::Burn(BurnError::Mmc(MmcError::NotReady)) => pick(
            lang,
            "盘未就绪：请确认已放入可写盘片且仓门已关闭。",
            "Disc not ready: make sure a writable disc is inserted and the tray is closed.",
        )
        .into(),
        JobError::Burn(BurnError::Mmc(other)) => match lang {
            Lang::Zh => format!("设备命令失败：{other}"),
            Lang::En => format!("Device command failed: {other}"),
        },
        JobError::Burn(BurnError::Cancelled) => pick(
            lang,
            "已中止：盘片内容不完整。",
            "Cancelled: the disc content is incomplete.",
        )
        .into(),
        JobError::Cancelled { zh, en } => match lang {
            Lang::Zh => format!("已中止：{zh}没有完成。"),
            Lang::En => format!("Cancelled: {en} did not finish."),
        },
        JobError::Burn(BurnError::Failed(tail)) => {
            let failure = BurnFailure::classify(tail);
            match lang {
                Lang::Zh => failure.user_text(tail),
                Lang::En => match failure {
                    BurnFailure::DriveLost => "The burn was interrupted: the drive lost connection during writing (loose cable, power glitch, or unplugged). This session was not finished; existing content on the disc is unaffected. Reconnect the drive and try again, and check the disc before reusing it.".to_string(),
                    BurnFailure::DeviceBusy => "The device is busy: the drive is in use by another program — most commonly the disc is mounted by the system. Unmount the disc or close the other program, then retry.".to_string(),
                    BurnFailure::Other => format!("Burn failed: {tail}"),
                },
            }
        }
        JobError::Burn(other) => match lang {
            Lang::Zh => format!("刻录失败：{other}"),
            Lang::En => format!("Burn failed: {other}"),
        },
        JobError::Mastering(MasteringError::SourceNotFound(path)) => match lang {
            Lang::Zh => format!("源目录不存在：{}", path.display()),
            Lang::En => format!("Source directory not found: {}", path.display()),
        },
        JobError::Mastering(MasteringError::SourceNotDirectory(path)) => match lang {
            Lang::Zh => format!("源路径不是目录：{}", path.display()),
            Lang::En => format!("Source path is not a directory: {}", path.display()),
        },
        JobError::Mastering(other) => format!("{other}"),
    }
}

/// 列出本机光驱与盘片状态，编排照 CLI 的 probe：逐台 open、INQUIRY、读盘片信息。
#[tauri::command]
pub fn probe_devices() -> Vec<DeviceInfo> {
    optiburn_transport::list_optical_devices()
        .into_iter()
        .map(probe_one)
        .collect()
}

/// 查一台光驱：任何一步失败都填进 error，不让单台设备拖垮整个列表。
fn probe_one(path: String) -> DeviceInfo {
    let mut info = DeviceInfo {
        path,
        identity: None,
        status: None,
        status_bits: None,
        sessions: None,
        capacity_bytes: None,
        free_bytes: None,
        error: None,
    };
    let transport = match optiburn_transport::open(&info.path) {
        Ok(transport) => transport,
        Err(e) => {
            info.error = Some(match lang() {
                Lang::Zh => format!("打开失败：{e}"),
                Lang::En => format!("Failed to open the device: {e}"),
            });
            return info;
        }
    };
    let mut device = MmcDevice::new(transport);
    match device.inquiry() {
        Ok(inquiry) => {
            info.identity = Some(format!(
                "{} {} {}",
                inquiry.vendor, inquiry.product, inquiry.revision
            ));
        }
        Err(e) => {
            info.error = Some(match lang() {
                Lang::Zh => format!("读取设备信息失败：{e}"),
                Lang::En => format!("Failed to read device information: {e}"),
            });
            return info;
        }
    }
    match device.read_disc_information() {
        Ok(disc) => {
            let (status, bits) = status_fields(disc.status);
            info.status = Some(status);
            info.status_bits = bits;
            info.sessions = Some(disc.sessions);
        }
        Err(e) => {
            info.error = Some(match lang() {
                Lang::Zh => format!("读取盘片信息失败：{e}"),
                Lang::En => format!("Failed to read disc information: {e}"),
            });
        }
    }
    // 容量是概览字段，读不到保持 null 且不改写 error（盘片状态已在上面给出）。
    // 两个口径分头取证（可用容量优先取剩余块数，规则见 read_disc_capacity），
    // 任一读不到就单独留空，界面按能读到的部分显示（ADR-0019）。
    let capacity = device.read_disc_capacity();
    info.capacity_bytes = capacity.total;
    info.free_bytes = capacity.free;
    info
}

/// 占住任务槽：已有任务时拒绝，避免两个任务同时操作同一台光驱（写盘与复制都经过这里）。
fn begin_job(state: &State<'_, JobState>) -> Result<CancelToken, String> {
    let mut slot = state.0.lock().expect("job state mutex");
    if slot.is_some() {
        return Err(pick(
            lang(),
            "已有任务在进行，请等它结束。",
            "A task is already running. Wait for it to finish.",
        )
        .into());
    }
    let cancel = CancelToken::default();
    *slot = Some(RunningJob {
        cancel: cancel.clone(),
        join: None,
    });
    Ok(cancel)
}

/// spawn_blocking 之后补记工作句柄，供 confirm_close 等待退出。
fn set_join(state: &State<'_, JobState>, join: tauri::async_runtime::JoinHandle<()>) {
    if let Some(job) = state.0.lock().expect("job state mutex").as_mut() {
        job.join = Some(join);
    }
}

/// 清空任务槽并广播完成事件。
fn finish_job(
    app: &AppHandle,
    kind: JobKind,
    outcome: &str,
    gate: Option<&'static str>,
    message: String,
) {
    app.state::<JobState>()
        .0
        .lock()
        .expect("job state mutex")
        .take();
    let _ = app.emit(
        "job-done",
        JobDone {
            kind,
            outcome: outcome.to_string(),
            message,
            gate,
        },
    );
}

/// 门禁类失败的 gate 标记：前端据其弹对应的引导对话框。增长模式的三类错误
/// （GrowUnsupported、GrowConflict、UnsupportedUdf）文案已自足，不需要引导弹窗。
fn gate_of(error: &JobError) -> Option<&'static str> {
    match error {
        JobError::Gate(WriteBlock::NeedGrowMode) => Some("append"),
        JobError::Gate(WriteBlock::Finalized) => Some("finalized"),
        JobError::Mounted { .. } => Some("mounted"),
        JobError::NoIsoSession => Some("noIsoSession"),
        JobError::Capacity { .. } => Some("capacity"),
        _ => None,
    }
}

/// 失败收尾：广播 job-done，并把同一份文案返回给调用方（invoke 拒绝分支复用）。
fn fail_job(app: &AppHandle, kind: JobKind, error: JobError) -> String {
    // 面向用户的文案经过归类，原始细节写进应用日志备查。
    if let JobError::Burn(BurnError::Failed(tail)) = &error {
        eprintln!("optiburn: 刻录失败原始输出：{tail}");
    }
    let outcome = if matches!(
        error,
        JobError::Burn(BurnError::Cancelled) | JobError::Cancelled { .. }
    ) {
        "cancelled"
    } else {
        "failed"
    };
    let gate = gate_of(&error);
    let text = job_error_text(lang(), &error);
    finish_job(app, kind, outcome, gate, text.clone());
    text
}

/// 把目录做成镜像。制作无进度回调，前端等本命令的返回值拿结果。
#[tauri::command]
pub async fn start_build_image(
    app: AppHandle,
    state: State<'_, JobState>,
    src: String,
    output: Option<String>,
    profile: String,
    volume_id: String,
) -> Result<ImageInfoDto, String> {
    let profile = profile_from_str(lang(), &profile)?;
    let output = match output.as_deref().filter(|o| !o.is_empty()) {
        Some(path) => ensure_iso_extension(PathBuf::from(path)),
        None => default_output(Path::new(&src)),
    };
    begin_job(&state)?;

    let app = app.clone();
    let spec = ImageSpec {
        profile,
        volume_id,
        joliet: true,
    };
    // 阻塞工作放到工作线程；本命令等它结束，把真实的制作结果带回给前端。
    let built = tauri::async_runtime::spawn_blocking(move || {
        match build_image(Path::new(&src), &output, &spec) {
            Ok(info) => {
                finish_job(
                    &app,
                    JobKind::Build,
                    "done",
                    None,
                    match lang() {
                        Lang::Zh => format!("镜像已生成：{}", output.display()),
                        Lang::En => format!("Image created: {}", output.display()),
                    },
                );
                Ok(ImageInfoDto {
                    sectors: info.sectors,
                    bytes: info.bytes,
                    filesystems: info.filesystems,
                })
            }
            Err(e) => Err(fail_job(&app, JobKind::Build, JobError::Mastering(e))),
        }
    })
    .await
    .expect("build worker did not panic");

    // 文案已在 fail_job 里随 job-done 广播并作为 Err 返回。
    built
}

/// 把镜像写到盘上。立即返回，进度与结果走 `job-progress` / `job-done` 事件。
#[tauri::command]
pub async fn start_burn(
    app: AppHandle,
    state: State<'_, JobState>,
    image: String,
    device: String,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<(), String> {
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        let image = PathBuf::from(image);
        let result = check_write_gates(&device, false).and_then(|()| {
            run_disc_task(DiscTask {
                app: &worker,
                kind: JobKind::Burn,
                append: false,
                path: &image,
                device: &device,
                // 卷标只在追加时有意义。
                volume_id: "",
                speed,
                close_disc,
                cancel: &cancel,
            })
        });
        match result {
            Ok(message) => finish_job(&worker, JobKind::Burn, "done", None, message),
            Err(e) => {
                fail_job(&worker, JobKind::Burn, e);
            }
        }
    });
    set_join(&state, join);
    Ok(())
}

/// 把待刻录文件追加到盘上（增长模式）：文件先收进暂存目录，再按盘根写入。
#[tauri::command]
pub async fn start_append(
    app: AppHandle,
    state: State<'_, JobState>,
    files: Vec<String>,
    device: String,
    volume_id: String,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<(), String> {
    if files.is_empty() {
        return Err(pick(lang(), "待刻录列表是空的。", "The file list is empty.").into());
    }
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        let result = run_append_task(
            &worker, &cancel, files, &device, &volume_id, speed, close_disc,
        );
        match result {
            Ok(message) => finish_job(&worker, JobKind::Append, "done", None, message),
            Err(e) => {
                fail_job(&worker, JobKind::Append, e);
            }
        }
    });
    set_join(&state, join);
    Ok(())
}

/// 追加任务主体：先过写前门禁，再把待刻录文件收进暂存目录，写入完成或失败后都清掉暂存。
fn run_append_task(
    app: &AppHandle,
    cancel: &CancelToken,
    files: Vec<String>,
    device: &str,
    volume_id: &str,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<String, JobError> {
    // 门禁先跑：挂载、封口、末区段格式这些拒绝都发生在把文件拷进暂存之前。
    check_write_gates(device, true)?;
    let stage = unique_temp_dir("optiburn-stage").map_err(JobError::Input)?;
    if let Err(message) = stage_files(lang(), &files, &stage) {
        // 暂存阶段的失败同样要清目录，不留用户文件的副本。
        let _ = std::fs::remove_dir_all(&stage);
        return Err(JobError::Input(message));
    }
    let result = run_disc_task(DiscTask {
        app,
        kind: JobKind::Append,
        append: true,
        path: &stage,
        device,
        volume_id,
        speed,
        close_disc,
        cancel,
    });
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// 把待刻录文件收进暂存目录，盘根就是这些文件的文件名。
///
/// 同名文件直接报错而不覆盖（文件来自不同目录时可能出现），目录与复制失败同样报错。
fn stage_files(lang: Lang, files: &[String], stage_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(stage_dir).map_err(|e| match lang {
        Lang::Zh => format!("创建暂存目录失败：{e}"),
        Lang::En => format!("Failed to create the staging directory: {e}"),
    })?;
    for file in files {
        let source = Path::new(file);
        let name = source.file_name().ok_or_else(|| match lang {
            Lang::Zh => format!("路径没有文件名：{file}"),
            Lang::En => format!("Path has no file name: {file}"),
        })?;
        // 粘贴入口可能混进目录或特殊文件（FIFO 等），提前给出解释，不让
        // fs::copy 抛英文错误，也不让打开 FIFO 这类操作把任务卡死。
        let metadata = std::fs::metadata(source).map_err(|e| match lang {
            Lang::Zh => format!("读取 {} 的信息失败：{e}", source.display()),
            Lang::En => format!("Failed to stat {}: {e}", source.display()),
        })?;
        if metadata.is_dir() {
            return Err(match lang {
                Lang::Zh => format!(
                    "待刻录列表里有目录 {}，本版本只接受文件。请展开目录后逐个选择文件。",
                    name.to_string_lossy()
                ),
                Lang::En => format!(
                    "The list contains a directory, {}: this version accepts files only. Expand it and pick files one by one.",
                    name.to_string_lossy()
                ),
            });
        }
        if !metadata.is_file() {
            return Err(match lang {
                Lang::Zh => format!(
                    "待刻录列表里有非常规文件 {}，本版本只接受普通文件。",
                    name.to_string_lossy()
                ),
                Lang::En => format!(
                    "The list contains a non-regular file, {}: only regular files are accepted.",
                    name.to_string_lossy()
                ),
            });
        }
        let target = stage_dir.join(name);
        if target.exists() {
            return Err(match lang {
                Lang::Zh => format!(
                    "待刻录列表里有同名文件 {}，请改名或分批写入。",
                    name.to_string_lossy()
                ),
                Lang::En => format!(
                    "The list contains two entries named {}: rename one of them or burn in batches.",
                    name.to_string_lossy()
                ),
            });
        }
        std::fs::copy(source, &target).map_err(|e| match lang {
            Lang::Zh => format!("复制 {} 失败：{e}", source.display()),
            Lang::En => format!("Failed to copy {}: {e}", source.display()),
        })?;
    }
    Ok(())
}

/// 在系统临时目录建一个本次调用独占的目录。
///
/// 名字带进程号与纳秒时间戳，且用 `create_dir`（已存在即失败）而不是
/// `create_dir_all`：多用户机器上他人可以预置同名符号链接把拷贝引到别处，换名字
/// 比跟随符号链接稳妥。代价是崩溃残留会留在临时目录等系统回收。
fn unique_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(match lang() {
                    Lang::Zh => format!("创建暂存目录失败：{error}"),
                    Lang::En => format!("Failed to create the staging directory: {error}"),
                });
            }
        }
    }
    Err(pick(
        lang(),
        "创建暂存目录失败：名称连续冲突。",
        "Failed to create the staging directory: names kept colliding.",
    )
    .to_string())
}

/// 读盘上最后一个区段的卷标，供追加页预填。读不到时前端保持默认值。
#[tauri::command]
pub async fn disc_volume_id(device: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read_volume_id(&device)
            .map_err(|e| engine_error_text(lang(), "读取卷标", "Reading the volume label", &e))
    })
    .await
    .expect("volume id worker did not panic")
}

/// 取消当前任务：置位令牌，引擎在下一个进度行停掉子进程。
#[tauri::command]
pub fn cancel_job(state: State<'_, JobState>) -> Result<(), String> {
    let slot = state.0.lock().expect("job state mutex");
    match slot.as_ref() {
        Some(job) => {
            job.cancel.cancel();
            Ok(())
        }
        None => Err(pick(lang(), "当前没有进行中的任务。", "No task is running.").into()),
    }
}

/// 关窗确认后的收尾：置位取消、等子进程停掉，再退出整个应用。
#[tauri::command]
pub fn confirm_close(app: AppHandle, state: State<'_, JobState>) {
    let join = {
        let mut slot = state.0.lock().expect("job state mutex");
        if let Some(job) = slot.as_ref() {
            job.cancel.cancel();
        }
        slot.take().and_then(|job| job.join)
    };
    if let Some(join) = join {
        let _ = tauri::async_runtime::block_on(join);
    }
    app.exit(0);
}

/// 刻录/追加共用的阻塞任务输入。append 决定 path 的含义与写入方式；
/// volume_id 只在追加时有意义。
struct DiscTask<'a> {
    app: &'a AppHandle,
    kind: JobKind,
    append: bool,
    /// burn 时是镜像文件，append 时是源目录。
    path: &'a Path,
    device: &'a str,
    volume_id: &'a str,
    speed: Option<u32>,
    close_disc: bool,
    cancel: &'a CancelToken,
}

/// 刻录/追加的阻塞主体：跑引擎，完成后读一次区段数放进完成消息。
///
/// 写前门禁不在这里：追加要在暂存用户文件之前拿到拒绝结论，由调用方先跑
/// [`check_write_gates`]。容量门禁例外：追加的待写入量要等暂存目录就绪才能
/// 预演，镜像刻录的待写入量就是镜像本身，两者都收在跑引擎之前。
///
/// 原生增长会在引擎内再做一次容量门禁（镜像尺寸要等会话生成计划算完才算得准），
/// 那道拒绝转成与写前门禁同一个 [`JobError::Capacity`]，用户看到的引导弹窗一致。
fn run_disc_task(task: DiscTask) -> Result<String, JobError> {
    let mut progress = progress_emitter(task.app, task.kind);
    let result = if task.append {
        let job = GrowJob {
            src: task.path.to_path_buf(),
            device: task.device.to_string(),
            speed: task.speed,
            volume_id: task.volume_id.to_string(),
            close_disc: task.close_disc,
        };
        let needed = grow_size(&job).map_err(job_error_of_burn)?;
        ensure_fits(task.device, needed)?;
        grow(&job, &mut progress, task.cancel)
    } else {
        // 与 CLI 相同：默认多区段，显式要求才封盘。
        let job = BurnJob {
            image: task.path.to_path_buf(),
            device: task.device.to_string(),
            speed: task.speed,
            multi: !task.close_disc,
        };
        let needed = std::fs::metadata(task.path)
            .map_err(|e| match lang() {
                Lang::Zh => {
                    JobError::Input(format!("读取镜像 {} 大小失败：{e}", task.path.display()))
                }
                Lang::En => JobError::Input(format!(
                    "Failed to stat the image {}: {e}",
                    task.path.display()
                )),
            })?
            .len();
        ensure_fits(task.device, needed)?;
        burn_engine().burn(&job, &mut progress, task.cancel)
    };
    result.map_err(job_error_of_burn)?;
    Ok(sessions_message(task.device))
}

/// 引擎错误到任务错误：引擎内的容量门禁（原生增长）与写前门禁走同一个引导弹窗，
/// 增长模式的三类拒绝各有自足的文案（见 [`job_error_text`]）与写前预演共用一条
/// 映射，其余折成 [`JobError::Burn`]。
fn job_error_of_burn(error: BurnError) -> JobError {
    match error {
        BurnError::NotEnoughSpace { needed, free } => JobError::Capacity { needed, free },
        BurnError::GrowUnsupported(detail) => JobError::GrowUnsupported(detail),
        BurnError::GrowConflict(detail) => JobError::GrowConflict(detail),
        BurnError::UnsupportedUdf(detail) => JobError::UnsupportedUdf(detail),
        other => JobError::Burn(other),
    }
}

/// 写前容量门禁：待写入量加区段开销超过可用容量就拒绝（ADR-0019）。
///
/// 可用容量取 [`MmcDevice::read_disc_capacity`]（顺序介质上是 READ TRACK
/// INFORMATION 的剩余块数）。读不到时放行：门禁是尽力而为的预检，不该成为
/// 新的故障点，真放不下由引擎写入失败兜底。区段开销余量与 CLI 共用
/// [`optiburn_engine::SESSION_OVERHEAD`]。
fn ensure_fits(device: &str, needed_bytes: u64) -> Result<(), JobError> {
    let transport = optiburn_transport::open(device).map_err(|e| JobError::Open {
        device: device.to_string(),
        source: e,
    })?;
    let mut mmc = MmcDevice::new(transport);
    let capacity = mmc.read_disc_capacity();
    // 查完立刻释放句柄：刻录引擎随后要以独占方式打开设备。
    drop(mmc);
    let Some(free) = capacity.free else {
        return Ok(());
    };
    if needed_bytes + optiburn_engine::SESSION_OVERHEAD > free {
        return Err(JobError::Capacity {
            needed: needed_bytes,
            free,
        });
    }
    Ok(())
}

/// 刻录引擎的选择：Windows 上没有可用的 xorriso（MSYS2 的构建没有光驱访问，
/// 见 ADR-0008 补记），刻录只能走原生引擎。Linux 维持 xorriso，原生引擎在那边
/// 还没有真机验证过（ADR-0017）。追加（增长模式）只有 xorriso 一条路。
fn burn_engine() -> Box<dyn BurnEngine> {
    if cfg!(windows) {
        Box::new(NativeEngine)
    } else {
        Box::new(XorrisoEngine)
    }
}

/// 写前门禁：挂载占用、盘片状态与末区段格式，全部通过才允许动设备。
///
/// 追加在把用户文件拷进暂存之前先跑这里，挂载、封口、UDF 盘这些拒绝都发生在
/// 白拷之前。刻录由 start_burn 的任务闭包先跑。
fn check_write_gates(device: &str, append: bool) -> Result<(), JobError> {
    // 挂载占用必挂：libburn 拿不到独占设备时只回英文报错，这里先换成卸载指引。
    if let Some(point) = optiburn_transport::mounted_at(device) {
        return Err(JobError::Mounted {
            device: device.to_string(),
            point,
        });
    }
    let info = open_gate(device)?;
    // 镜像路径不接受可追加盘（单区段镜像会遮住已有区段的文件），增长模式放行。
    approve_write(&info, append).map_err(JobError::Gate)?;
    // 追加路径再过一道：末区段不是 ISO 9660 的盘（例如 UDF 盘）拒绝，见 ADR-0010。
    if append && info.status == DiscStatus::Appendable {
        let iso_readable = last_session_is_iso(device).map_err(|e| match e {
            // 盘上 UDF 结构读不了：与“末区段格式”不是一回事，文案由 job_error_text 给。
            BurnError::UnsupportedUdf(detail) => JobError::UnsupportedUdf(detail),
            other => JobError::Input(engine_error_text(
                lang(),
                "读取末区段格式",
                "Reading the last session format",
                &other,
            )),
        })?;
        if !iso_readable {
            return Err(JobError::NoIsoSession);
        }
    }
    Ok(())
}

/// 打开设备并完成刻录前查询：等就绪、读盘片信息。查询完立刻释放句柄，
/// 刻录引擎随后要以独占方式打开设备。
fn open_gate(device: &str) -> Result<DiscInformation, JobError> {
    let transport = optiburn_transport::open(device).map_err(|e| JobError::Open {
        device: device.to_string(),
        source: e,
    })?;
    let mut mmc = MmcDevice::new(transport);
    wait_until_ready(&mut mmc).map_err(JobError::Disc)?;
    let info = mmc.read_disc_information().map_err(JobError::Disc)?;
    drop(mmc);
    Ok(info)
}

/// 进度上报闭包：引擎每个百分比行发一次事件。
fn progress_emitter<'a>(app: &'a AppHandle, kind: JobKind) -> impl FnMut(f32) + 'a {
    move |fraction: f32| {
        let _ = app.emit("job-progress", JobProgress { kind, fraction });
    }
}

/// 刻录成功后重开设备读区段数；读不到不影响“完成”这个结论。
fn sessions_message(device: &str) -> String {
    let text = optiburn_transport::open(device).ok().and_then(|transport| {
        let mut mmc = MmcDevice::new(transport);
        mmc.read_disc_information().ok().map(|info| match lang() {
            Lang::Zh => format!("盘上现有 {} 个区段。", info.sessions),
            Lang::En => format!("The disc now has {} sessions.", info.sessions),
        })
    });
    text.unwrap_or_else(|| match lang() {
        Lang::Zh => "刻录完成。".into(),
        Lang::En => "Burn finished.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_mapping_covers_three_media() {
        assert_eq!(profile_from_str(Lang::Zh, "cd"), Ok(DiscProfile::Cd));
        assert_eq!(profile_from_str(Lang::Zh, "dvd"), Ok(DiscProfile::Dvd));
        assert_eq!(profile_from_str(Lang::Zh, "bd"), Ok(DiscProfile::Bd));
        assert!(profile_from_str(Lang::Zh, "hddvd").is_err());
    }

    #[test]
    fn default_output_lands_next_to_the_source() {
        assert_eq!(
            default_output(Path::new("/tmp/photos")),
            PathBuf::from("/tmp/photos.iso")
        );
        // 相对路径没有可用的父目录，落回当前目录语义。
        assert_eq!(
            default_output(Path::new("photos")),
            PathBuf::from("photos.iso")
        );
        assert_eq!(
            default_output(Path::new("/")),
            PathBuf::from("optiburn.iso")
        );
    }

    #[test]
    fn extensionless_output_gains_iso_suffix() {
        assert_eq!(
            ensure_iso_extension(PathBuf::from("/tmp/photos")),
            PathBuf::from("/tmp/photos.iso")
        );
        assert_eq!(
            ensure_iso_extension(PathBuf::from(r"C:\isos\photos")),
            PathBuf::from(r"C:\isos\photos.iso")
        );
        // 已有扩展名（别名与大小写变体）不改写。
        assert_eq!(
            ensure_iso_extension(PathBuf::from("/tmp/photos.img")),
            PathBuf::from("/tmp/photos.img")
        );
        assert_eq!(
            ensure_iso_extension(PathBuf::from("/tmp/photos.ISO")),
            PathBuf::from("/tmp/photos.ISO")
        );
    }

    #[test]
    fn native_engine_wiring_is_pinned() {
        // 平台规则：Windows 走原生引擎（那边没有可用的 xorriso），Linux 维持 xorriso。
        let expected = if cfg!(windows) { "native" } else { "xorriso" };
        assert_eq!(burn_engine().name(), expected);
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::NativeGap(NativeGap::FinalizedDisc))
            ),
            "盘已封口，无法再写入，请更换盘片。"
        );
        assert!(
            job_error_text(
                Lang::En,
                &JobError::Burn(BurnError::NativeGap(NativeGap::WriteSpeed))
            )
            .contains("write speed")
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::Mmc(MmcError::NotReady))
            ),
            "盘未就绪：请确认已放入可写盘片且仓门已关闭。"
        );
    }

    #[test]
    fn job_error_text_matches_the_pinned_wording() {
        // 缺工具：刻录路径与读盘路径共用同一份文案（引擎里的中文）。
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::MissingTool("xorriso".into()))
            ),
            BurnError::missing_tool_user_text("xorriso")
        );
        assert!(
            job_error_text(
                Lang::En,
                &JobError::Burn(BurnError::MissingTool("xorriso".into()))
            )
            .contains("sudo apt install xorriso")
        );
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::Burn(BurnError::Cancelled)),
            "已中止：盘片内容不完整。"
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Cancelled {
                    zh: "复制",
                    en: "copy"
                }
            ),
            "已中止：复制没有完成。"
        );
        assert_eq!(
            job_error_text(
                Lang::En,
                &JobError::Cancelled {
                    zh: "复制",
                    en: "copy"
                }
            ),
            "Cancelled: copy did not finish."
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::Failed("fifo busy".into()))
            ),
            "刻录失败：fifo busy"
        );
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::Disc(MmcError::NotReady)),
            "盘未就绪：请确认已放入可写盘片且仓门已关闭。"
        );
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::Gate(WriteBlock::NeedGrowMode)),
            "盘上已有数据区段：以镜像方式追加会把已有文件遮住。请改用追加页。"
        );
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::Gate(WriteBlock::Finalized)),
            "盘已封口，无法再写入，请更换盘片。"
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Mastering(MasteringError::SourceNotFound(PathBuf::from("/x")))
            ),
            "源目录不存在：/x"
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Mastering(MasteringError::SourceNotDirectory(PathBuf::from("/x")))
            ),
            "源路径不是目录：/x"
        );
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::NoIsoSession),
            "盘上最后的区段不是 ISO 9660（例如 Windows 写入的 UDF 盘）：追加 ISO 区段后，按最后一区段挂载的系统将只看到新内容。本工具暂不支持续写这类盘，请换用空白盘重刻。"
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Capacity {
                    needed: 4700372992,
                    free: 4284481536,
                }
            ),
            "这张盘放不下本次写入：待写入约 4.4 GB，盘上可用容量约 4.0 GB。请减少待写内容或更换盘片。"
        );
        assert_eq!(
            job_error_text(
                Lang::En,
                &JobError::Capacity {
                    needed: 4700372992,
                    free: 4284481536,
                }
            ),
            "The disc cannot hold this write: about 4.4 GB to write, 4.0 GB free on the disc. Remove some files or use another disc."
        );
        assert_eq!(
            gate_of(&JobError::Capacity { needed: 1, free: 1 }),
            Some("capacity")
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Input("复制 /x 失败：no such file".to_string())
            ),
            "复制 /x 失败：no such file"
        );
    }

    /// 增长模式的三类错误与两个新缺口：中英文本钉住，且都要与 CLI 的中文同义。
    #[test]
    fn growth_error_texts_are_pinned() {
        let detail = "no Joliet directory tree".to_string();
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::GrowUnsupported(detail.clone())),
            "这张盘暂时没法用原生引擎追加：no Joliet directory tree。Linux 上可以改用 xorriso 引擎。"
        );
        assert_eq!(
            job_error_text(Lang::En, &JobError::GrowUnsupported(detail)),
            "This disc cannot be grown by the native engine: no Joliet directory tree. On Linux the xorriso engine can be used instead."
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::GrowConflict("/a.txt: a new file replaces an existing directory".into())
            ),
            "追加内容与盘上内容有冲突：/a.txt: a new file replaces an existing directory"
        );
        assert_eq!(
            job_error_text(
                Lang::En,
                &JobError::GrowConflict("/a.txt: a new file replaces an existing directory".into())
            ),
            "The appended content conflicts with what is on the disc: /a.txt: a new file replaces an existing directory"
        );
        assert_eq!(
            job_error_text(Lang::Zh, &JobError::UnsupportedUdf("VAT".into())),
            "这张盘的 UDF 结构本工具暂不支持读取：VAT"
        );
        assert_eq!(
            job_error_text(Lang::En, &JobError::UnsupportedUdf("VAT".into())),
            "This disc uses a UDF structure that this tool cannot read yet: VAT"
        );
        // 读盘路径上也要有 UDF 文案，不能落到“动作失败：English”里。
        assert_eq!(
            engine_error_text(
                Lang::Zh,
                "读取卷标",
                "Reading the volume label",
                &BurnError::UnsupportedUdf("VAT".into())
            ),
            "这张盘的 UDF 结构本工具暂不支持读取：VAT"
        );
        // 三类增长错误不需要引导弹窗。
        for error in [
            JobError::GrowUnsupported("x".into()),
            JobError::GrowConflict("x".into()),
            JobError::UnsupportedUdf("x".into()),
        ] {
            assert!(gate_of(&error).is_none(), "unexpected gate");
        }
        // 原生缺口的两条新文案。
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::NativeGap(NativeGap::GrowthOnRewritable))
            ),
            "可覆写介质（DVD-RAM、BD-RE 这类）上的追加暂不支持原生引擎：这类盘可以直接用「刻录」整体重写，或换可追加的盘片。"
        );
        assert_eq!(
            job_error_text(
                Lang::En,
                &JobError::Burn(BurnError::NativeGap(NativeGap::EmptyGrowSource))
            ),
            "The directory to append is empty, so there is nothing to write."
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::NativeGap(NativeGap::WriteSpeed))
            ),
            "原生引擎暂不支持指定倍速：去掉 --speed 再试。"
        );
        // 引擎内的容量拒绝与写前门禁走同一个弹窗。
        assert_eq!(
            gate_of(&JobError::Capacity {
                needed: 100,
                free: 50
            }),
            Some("capacity")
        );
    }

    #[test]
    fn mount_and_classified_failure_texts_are_pinned() {
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Mounted {
                    device: "/dev/sr0".to_string(),
                    point: PathBuf::from("/run/media/u/我的光盘"),
                }
            ),
            "光盘已被系统挂载在 /run/media/u/我的光盘：在文件管理器里卸载该光盘，或运行 udisksctl unmount -b /dev/sr0 后再试。"
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::Failed(
                    "libburn : FATAL : Lost connection to drive\n".to_string()
                ))
            ),
            "刻录中断：光驱在写入过程中失去了连接，常见原因是线缆松动、供电不稳或被意外拔出。本次区段没有写完，旧内容不受影响。请重新插拔光驱后重试。这张盘如果要继续使用，建议先检查再写。"
        );
        assert_eq!(
            job_error_text(
                Lang::Zh,
                &JobError::Burn(BurnError::Failed(
                    "libburn : SORRY : Cannot open busy device '/dev/sr0'\n".to_string()
                ))
            ),
            "设备被占用：光驱正被其它程序使用，常见是系统挂载了这张光盘。先卸载光盘或关闭占用程序再试。"
        );
    }

    #[test]
    fn stage_files_copies_and_rejects_duplicates() {
        let base = std::env::temp_dir().join(format!("optiburn-stage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::create_dir_all(src.join("b")).unwrap();
        std::fs::write(src.join("a/x.txt"), b"one").unwrap();
        std::fs::write(src.join("b/y.txt"), b"two").unwrap();

        let files = vec![
            src.join("a/x.txt").display().to_string(),
            src.join("b/y.txt").display().to_string(),
        ];
        let stage = base.join("stage");
        stage_files(Lang::Zh, &files, &stage).expect("stage two files");
        assert_eq!(std::fs::read(stage.join("x.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(stage.join("y.txt")).unwrap(), b"two");

        // 同名文件必须报错，不能悄悄覆盖先到的那个。
        let clashing = vec![
            src.join("a/x.txt").display().to_string(),
            src.join("a/x.txt").display().to_string(),
        ];
        let err =
            stage_files(Lang::Zh, &clashing, &base.join("stage-clash")).expect_err("must fail");
        assert!(err.contains("同名文件"), "{err}");

        // 不存在的源文件同样报错。
        let missing = vec![src.join("a/none.txt").display().to_string()];
        let err =
            stage_files(Lang::Zh, &missing, &base.join("stage-missing")).expect_err("must fail");
        assert!(err.contains("none.txt"), "{err}");

        // 列表里混进目录时给出中文解释，而不是让 fs::copy 抛英文 EISDIR。
        let directory = vec![src.join("a").display().to_string()];
        let err =
            stage_files(Lang::Zh, &directory, &base.join("stage-dir")).expect_err("must fail");
        assert!(err.contains("目录"), "{err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unique_temp_dir_creates_fresh_dirs() {
        let first = unique_temp_dir("optiburn-test-stage").expect("create first");
        let second = unique_temp_dir("optiburn-test-stage").expect("create second");
        assert_ne!(first, second);
        assert!(first.is_dir() && second.is_dir());
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }

    #[test]
    fn status_fields_split_other_with_raw_bits() {
        assert_eq!(
            status_fields(DiscStatus::Empty),
            ("empty".to_string(), None)
        );
        assert_eq!(
            status_fields(DiscStatus::Appendable),
            ("appendable".to_string(), None)
        );
        assert_eq!(
            status_fields(DiscStatus::Finalized),
            ("finalized".to_string(), None)
        );
        assert_eq!(
            status_fields(DiscStatus::Other(3)),
            ("other".to_string(), Some(3))
        );
    }
}
