//! Tauri 命令层：把核心 crate 的能力暴露给前端，并统一中文文案。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use optiburn_engine::{
    BurnEngine, BurnError, BurnFailure, BurnJob, CancelToken, GrowJob, XorrisoEngine,
    compare_trees, extract_paths, extract_tree, grow, last_session_is_iso, list_tree,
    read_volume_id,
};
use optiburn_mastering::{DiscProfile, ImageSpec, MasteringError, build_image};
use optiburn_mmc::{
    DiscInformation, DiscStatus, MmcDevice, MmcError, WriteBlock, approve_write, wait_until_ready,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::job::{JobKind, JobState, RunningJob};

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
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct JobDone {
    kind: JobKind,
    outcome: String,
    message: String,
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
    /// 待刻录列表或暂存阶段的输入问题，文案已由下层给出。
    Input(String),
    /// 等待介质就绪或读取盘片信息失败。
    Disc(MmcError),
    /// 写前门禁拒绝。
    Gate(WriteBlock),
    Burn(BurnError),
    Mastering(MasteringError),
}

/// 刻录失败文案：先归类成面向人的成因，认不出的形态保留原始输出。
///
/// 原始输出始终会另写进应用日志（见 [`fail_job`]），界面不展示英文长文。
fn burn_failure_text(tail: &str) -> String {
    match BurnFailure::classify(tail) {
        BurnFailure::DriveLost => "刻录中断：光驱在写入过程中失去了连接，常见原因是线缆松动、供电不稳或被意外拔出。本次区段没有写完，旧内容不受影响。请重新插拔光驱后重试。这张盘如果要继续使用，建议先检查再写。".to_string(),
        BurnFailure::DeviceBusy => "设备被占用：光驱正被其它程序使用，常见是系统挂载了这张光盘。先卸载光盘或关闭占用程序再试。".to_string(),
        BurnFailure::Other => format!("刻录失败：{tail}"),
    }
}
fn status_fields(status: DiscStatus) -> (String, Option<u8>) {
    match status {
        DiscStatus::Empty => ("empty".into(), None),
        DiscStatus::Appendable => ("appendable".into(), None),
        DiscStatus::Finalized => ("finalized".into(), None),
        DiscStatus::Other(bits) => ("other".into(), Some(bits)),
    }
}

/// 前端介质字符串到 [`DiscProfile`] 的映射。
fn profile_from_str(profile: &str) -> Result<DiscProfile, String> {
    match profile {
        "cd" => Ok(DiscProfile::Cd),
        "dvd" => Ok(DiscProfile::Dvd),
        "bd" => Ok(DiscProfile::Bd),
        other => Err(format!("未知的介质类型：{other}")),
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

/// 任务失败的中文文案。与 CLI 同源，只按 GUI 的说法调整指引（“追加页”而非命令行）。
fn job_error_text(error: &JobError) -> String {
    match error {
        JobError::Open { device, source } => format!("打开 {device} 失败：{source}"),
        JobError::Mounted { device, point } => format!(
            "光盘已被系统挂载在 {}：在文件管理器里卸载该光盘，或运行 udisksctl unmount -b {device} 后再试。",
            point.display()
        ),
        JobError::NoIsoSession => "盘上最后的区段不是 ISO 9660（例如 Windows 写入的 UDF 盘）：追加 ISO 区段后，按最后一区段挂载的系统将只看到新内容。本工具暂不支持续写这类盘。".into(),
        JobError::Input(message) => message.clone(),
        JobError::Disc(MmcError::NotReady) => "盘未就绪：请确认已放入可写盘片且仓门已关闭。".into(),
        JobError::Disc(other) => format!("读取盘片信息失败：{other}"),
        JobError::Gate(WriteBlock::NeedGrowMode) => {
            "盘上已有数据区段：以镜像方式追加会把已有文件遮住。请改用追加页。".into()
        }
        JobError::Gate(WriteBlock::Finalized) => "盘已封口，无法再写入，请更换盘片。".into(),
        JobError::Burn(BurnError::MissingTool(_)) => {
            "缺少刻录工具 xorriso，请先安装后再刻录。".into()
        }
        JobError::Burn(BurnError::Cancelled) => "已中止：盘片内容不完整。".into(),
        JobError::Burn(BurnError::Failed(tail)) => burn_failure_text(tail),
        JobError::Burn(other) => format!("刻录失败：{other}"),
        JobError::Mastering(MasteringError::SourceNotFound(path)) => {
            format!("源目录不存在：{}", path.display())
        }
        JobError::Mastering(MasteringError::SourceNotDirectory(path)) => {
            format!("源路径不是目录：{}", path.display())
        }
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
        error: None,
    };
    let transport = match optiburn_transport::open(&info.path) {
        Ok(transport) => transport,
        Err(e) => {
            info.error = Some(format!("打开失败：{e}"));
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
            info.error = Some(format!("读取设备信息失败：{e}"));
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
        Err(e) => info.error = Some(format!("读取盘片信息失败：{e}")),
    }
    info
}

/// 占住任务槽：已有任务时拒绝，避免两个任务同时写同一台光驱。
fn begin_job(state: &State<'_, JobState>) -> Result<CancelToken, String> {
    let mut slot = state.0.lock().expect("job state mutex");
    if slot.is_some() {
        return Err("已有任务在进行，请等它结束。".into());
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
fn finish_job(app: &AppHandle, kind: JobKind, outcome: &str, message: String) {
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
        },
    );
}

/// 失败收尾：广播 job-done，并把同一份文案返回给调用方（invoke 拒绝分支复用）。
fn fail_job(app: &AppHandle, kind: JobKind, error: JobError) -> String {
    // 面向用户的文案经过归类，原始细节写进应用日志备查。
    if let JobError::Burn(BurnError::Failed(tail)) = &error {
        eprintln!("optiburn: 刻录失败原始输出：{tail}");
    }
    let outcome = if matches!(error, JobError::Burn(BurnError::Cancelled)) {
        "cancelled"
    } else {
        "failed"
    };
    let text = job_error_text(&error);
    finish_job(app, kind, outcome, text.clone());
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
    let profile = profile_from_str(&profile)?;
    let output = match output.as_deref().filter(|o| !o.is_empty()) {
        Some(path) => PathBuf::from(path),
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
                    format!("镜像已生成：{}", output.display()),
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
        let result = run_disc_task(DiscTask {
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
        });
        match result {
            Ok(message) => finish_job(&worker, JobKind::Burn, "done", message),
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
        return Err("待刻录列表是空的。".into());
    }
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        let result = run_append_task(
            &worker, &cancel, files, &device, &volume_id, speed, close_disc,
        );
        match result {
            Ok(message) => finish_job(&worker, JobKind::Append, "done", message),
            Err(e) => {
                fail_job(&worker, JobKind::Append, e);
            }
        }
    });
    set_join(&state, join);
    Ok(())
}

/// 追加任务主体：待刻录文件收进暂存目录，写入完成或失败后都清掉暂存。
fn run_append_task(
    app: &AppHandle,
    cancel: &CancelToken,
    files: Vec<String>,
    device: &str,
    volume_id: &str,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<String, JobError> {
    let stage = std::env::temp_dir().join(format!("optiburn-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    stage_files(&files, &stage).map_err(JobError::Input)?;
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
/// 同名文件直接报错而不覆盖（文件来自不同目录时可能出现），复制失败同样报错。
fn stage_files(files: &[String], stage_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(stage_dir).map_err(|e| format!("创建暂存目录失败：{e}"))?;
    for file in files {
        let source = Path::new(file);
        let name = source
            .file_name()
            .ok_or_else(|| format!("路径没有文件名：{file}"))?;
        let target = stage_dir.join(name);
        if target.exists() {
            return Err(format!(
                "待刻录列表里有同名文件 {}，请改名或分批写入。",
                name.to_string_lossy()
            ));
        }
        std::fs::copy(source, &target)
            .map_err(|e| format!("复制 {} 失败：{e}", source.display()))?;
    }
    Ok(())
}

/// 读盘上最后一个区段的卷标，供追加页预填。读不到时前端保持默认值。
#[tauri::command]
pub async fn disc_volume_id(device: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read_volume_id(&device).map_err(|e| format!("读取卷标失败：{e}"))
    })
    .await
    .expect("volume id worker did not panic")
}

/// 盘上条目的 DTO，对应前端 `DiscEntry`。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscEntryDto {
    path: String,
    size: u64,
    is_dir: bool,
}

/// 列出盘上最后一区段的内容（只读），供设备页浏览。
#[tauri::command]
pub async fn list_disc(device: String) -> Result<Vec<DiscEntryDto>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        list_tree(&device)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| DiscEntryDto {
                        path: entry.path,
                        size: entry.size,
                        is_dir: entry.is_dir,
                    })
                    .collect()
            })
            .map_err(|e| disc_read_error(&device, e))
    })
    .await
    .expect("list worker did not panic")
}

/// 复制结果，对应前端 `CopyReport`。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyReportDto {
    count: usize,
    bytes: u64,
}

/// 每次复制使用一个新的序号子目录，复制成功后清掉更早的暂存。
static COPY_SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// 把盘上选中的文件复制出来，并放进系统剪贴板，供文件管理器粘贴。
#[tauri::command]
pub async fn copy_disc_files(device: String, paths: Vec<String>) -> Result<CopyReportDto, String> {
    if paths.is_empty() {
        return Err("没有选中文件。".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let result = copy_disc_files_blocking(&device, &paths);
        // 界面可能已经切走、没人展示这次结果，错误同时写进应用日志备查。
        if let Err(error) = &result {
            eprintln!("optiburn: 复制盘上文件失败：{error}");
        }
        result
    })
    .await
    .expect("copy worker did not panic")
}

/// 读取系统剪贴板里的文件，供追加页的粘贴入口使用。
#[tauri::command]
pub async fn paste_files() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(crate::clipboard::read_files)
        .await
        .expect("paste worker did not panic")
}

/// 复制主体：抽取到新序号暂存目录，写清剪贴板后再清掉旧暂存。
///
/// 顺序是有意的：剪贴板指向的文件在切换完成前一直保留，用户中途粘贴不受影响。
/// 失败只清这一次的暂存，不碰仍被旧剪贴板引用的目录。
fn copy_disc_files_blocking(device: &str, paths: &[String]) -> Result<CopyReportDto, String> {
    let root = std::env::temp_dir().join(format!("optiburn-copy-{}", std::process::id()));
    let staging = root.join(COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed).to_string());
    std::fs::create_dir_all(&staging).map_err(|e| format!("创建暂存目录失败：{e}"))?;
    if let Err(e) = extract_paths(device, paths, &staging, &CancelToken::default()) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(disc_read_error(device, e));
    }
    let mut files = Vec::new();
    if let Err(e) = collect_files(&staging, &mut files) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("扫描暂存目录失败：{e}"));
    }
    if let Err(e) = crate::clipboard::copy_files(&files) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    // 剪贴板已指向新目录，旧暂存不再被使用。
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.path() != staging {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let bytes = files
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum();
    Ok(CopyReportDto {
        count: files.len(),
        bytes,
    })
}

/// 递归收集目录下的全部文件。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), out)?;
        } else {
            out.push(entry.path());
        }
    }
    Ok(())
}

/// 读取盘片失败的中文文案：盘被挂载占用时给出卸载指引。
fn disc_read_error(device: &str, error: BurnError) -> String {
    match optiburn_transport::mounted_at(device) {
        Some(point) => format!(
            "读取盘片失败，且光盘正被系统挂载在 {}：先在文件管理器里卸载（或运行 udisksctl unmount -b {device}）再试。",
            point.display()
        ),
        None => format!("读取盘片失败：{error}"),
    }
}

/// 校验模式：追加校验对比源目录，刻录校验对比镜像。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VerifyMode {
    Append,
    Burn,
}

/// 前端的模式字符串到 [`VerifyMode`] 的映射。
fn verify_mode_from_str(mode: &str) -> Result<VerifyMode, String> {
    match mode {
        "append" => Ok(VerifyMode::Append),
        "burn" => Ok(VerifyMode::Burn),
        other => Err(format!("未知的校验模式：{other}")),
    }
}

/// 回读校验结果，对应前端 `VerifyReport`。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReportDto {
    differences: Vec<String>,
}

/// 回读校验（只读）：把盘上最后一区段的树抽到临时目录，再按内容与源对比（ADR-0010）。
/// 追加校验用 `files` 重新暂存后对比，刻录校验用 `source` 指向的镜像。
#[tauri::command]
pub async fn start_verify(
    app: AppHandle,
    state: State<'_, JobState>,
    device: String,
    source: String,
    files: Vec<String>,
    mode: String,
) -> Result<VerifyReportDto, String> {
    let mode = verify_mode_from_str(&mode)?;
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        match run_verify(&device, Path::new(&source), &files, mode, &cancel) {
            Ok(differences) => {
                let outcome = if differences.is_empty() {
                    "done"
                } else {
                    "failed"
                };
                finish_job(
                    &worker,
                    JobKind::Verify,
                    outcome,
                    verify_message(mode, &differences),
                );
                Ok(VerifyReportDto { differences })
            }
            Err(e) => Err(fail_job(&worker, JobKind::Verify, e)),
        }
    })
    .await
    .expect("verify worker did not panic")
}

/// 校验主体：门禁、抽取、对比。临时目录用完即删，失败路径也要删。
fn run_verify(
    device: &str,
    source: &Path,
    files: &[String],
    mode: VerifyMode,
    cancel: &CancelToken,
) -> Result<Vec<String>, JobError> {
    if let Some(point) = optiburn_transport::mounted_at(device) {
        return Err(JobError::Mounted {
            device: device.to_string(),
            point,
        });
    }
    let temp = std::env::temp_dir().join(format!("optiburn-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    let result = extract_and_compare(device, source, files, mode, &temp, cancel);
    let _ = std::fs::remove_dir_all(&temp);
    result
}

/// 抽取与对比：先把盘上树抽到临时目录，再准备参照。追加校验把待刻录文件重新
/// 暂存成目录，刻录校验把镜像树抽出来，最后按内容单向对比。
fn extract_and_compare(
    device: &str,
    source: &Path,
    files: &[String],
    mode: VerifyMode,
    temp: &Path,
    cancel: &CancelToken,
) -> Result<Vec<String>, JobError> {
    let disc_tree = temp.join("disc");
    std::fs::create_dir_all(&disc_tree).map_err(|e| JobError::Burn(BurnError::Io(e)))?;
    extract_tree(Path::new(device), &disc_tree, cancel).map_err(JobError::Burn)?;
    let reference = match mode {
        VerifyMode::Append => {
            if files.is_empty() {
                return Err(JobError::Input("请先选择要刻录的文件。".to_string()));
            }
            let staged = temp.join("source");
            stage_files(files, &staged).map_err(JobError::Input)?;
            staged
        }
        VerifyMode::Burn => {
            let image_tree = temp.join("image");
            std::fs::create_dir_all(&image_tree).map_err(|e| JobError::Burn(BurnError::Io(e)))?;
            extract_tree(source, &image_tree, cancel).map_err(JobError::Burn)?;
            image_tree
        }
    };
    Ok(compare_trees(&reference, &disc_tree))
}

/// 校验完成文案（job-done 消息）。
fn verify_message(mode: VerifyMode, differences: &[String]) -> String {
    if differences.is_empty() {
        match mode {
            VerifyMode::Append => "校验通过：盘上内容与源目录一致。".to_string(),
            VerifyMode::Burn => "校验通过：盘上内容与镜像一致。".to_string(),
        }
    } else {
        format!(
            "校验未通过：发现 {} 处差异，明细见页面。",
            differences.len()
        )
    }
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
        None => Err("当前没有进行中的任务。".into()),
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

/// 刻录/追加的阻塞主体：门禁、引擎、完成后读一次区段数放进完成消息。
fn run_disc_task(task: DiscTask) -> Result<String, JobError> {
    // 挂载占用必挂：libburn 拿不到独占设备时只回英文报错，这里先换成卸载指引。
    if let Some(point) = optiburn_transport::mounted_at(task.device) {
        return Err(JobError::Mounted {
            device: task.device.to_string(),
            point,
        });
    }
    let info = open_gate(task.device)?;
    // 镜像路径不接受可追加盘（单区段镜像会遮住已有区段的文件），增长模式放行。
    approve_write(&info, task.append).map_err(JobError::Gate)?;
    // 追加路径再过一道：末区段不是 ISO 9660 的盘（例如 UDF 盘）拒绝，见 ADR-0010。
    if task.append
        && info.status == DiscStatus::Appendable
        && !last_session_is_iso(task.device).map_err(JobError::Burn)?
    {
        return Err(JobError::NoIsoSession);
    }

    let mut progress = progress_emitter(task.app, task.kind);
    let result = if task.append {
        let job = GrowJob {
            src: task.path.to_path_buf(),
            device: task.device.to_string(),
            speed: task.speed,
            volume_id: task.volume_id.to_string(),
            close_disc: task.close_disc,
        };
        grow(&job, &mut progress, task.cancel)
    } else {
        // 与 CLI 相同：默认多区段，显式要求才封盘。
        let job = BurnJob {
            image: task.path.to_path_buf(),
            device: task.device.to_string(),
            speed: task.speed,
            multi: !task.close_disc,
        };
        XorrisoEngine.burn(&job, &mut progress, task.cancel)
    };
    result.map_err(JobError::Burn)?;
    Ok(sessions_message(task.device))
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
        mmc.read_disc_information()
            .ok()
            .map(|info| format!("盘上现有 {} 个区段。", info.sessions))
    });
    text.unwrap_or_else(|| "刻录完成。".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_mapping_covers_three_media() {
        assert_eq!(profile_from_str("cd"), Ok(DiscProfile::Cd));
        assert_eq!(profile_from_str("dvd"), Ok(DiscProfile::Dvd));
        assert_eq!(profile_from_str("bd"), Ok(DiscProfile::Bd));
        assert!(profile_from_str("hddvd").is_err());
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
    fn job_error_text_matches_the_pinned_wording() {
        assert_eq!(
            job_error_text(&JobError::Burn(BurnError::MissingTool(
                "xorriso (sudo apt install xorriso)".into()
            ))),
            "缺少刻录工具 xorriso，请先安装后再刻录。"
        );
        assert_eq!(
            job_error_text(&JobError::Burn(BurnError::Cancelled)),
            "已中止：盘片内容不完整。"
        );
        assert_eq!(
            job_error_text(&JobError::Burn(BurnError::Failed("fifo busy".into()))),
            "刻录失败：fifo busy"
        );
        assert_eq!(
            job_error_text(&JobError::Disc(MmcError::NotReady)),
            "盘未就绪：请确认已放入可写盘片且仓门已关闭。"
        );
        assert_eq!(
            job_error_text(&JobError::Gate(WriteBlock::NeedGrowMode)),
            "盘上已有数据区段：以镜像方式追加会把已有文件遮住。请改用追加页。"
        );
        assert_eq!(
            job_error_text(&JobError::Gate(WriteBlock::Finalized)),
            "盘已封口，无法再写入，请更换盘片。"
        );
        assert_eq!(
            job_error_text(&JobError::Mastering(MasteringError::SourceNotFound(
                PathBuf::from("/x")
            ))),
            "源目录不存在：/x"
        );
        assert_eq!(
            job_error_text(&JobError::Mastering(MasteringError::SourceNotDirectory(
                PathBuf::from("/x")
            ))),
            "源路径不是目录：/x"
        );
        assert_eq!(
            job_error_text(&JobError::Mounted {
                device: "/dev/sr0".to_string(),
                point: PathBuf::from("/run/media/u/我的光盘"),
            }),
            "光盘已被系统挂载在 /run/media/u/我的光盘：在文件管理器里卸载该光盘，或运行 udisksctl unmount -b /dev/sr0 后再试。"
        );
        assert_eq!(
            job_error_text(&JobError::NoIsoSession),
            "盘上最后的区段不是 ISO 9660（例如 Windows 写入的 UDF 盘）：追加 ISO 区段后，按最后一区段挂载的系统将只看到新内容。本工具暂不支持续写这类盘。"
        );
        assert_eq!(
            job_error_text(&JobError::Input("复制 /x 失败：no such file".to_string())),
            "复制 /x 失败：no such file"
        );
        assert_eq!(
            job_error_text(&JobError::Burn(BurnError::Failed(
                "libburn : FATAL : Lost connection to drive\n".to_string()
            ))),
            "刻录中断：光驱在写入过程中失去了连接，常见原因是线缆松动、供电不稳或被意外拔出。本次区段没有写完，旧内容不受影响。请重新插拔光驱后重试。这张盘如果要继续使用，建议先检查再写。"
        );
        assert_eq!(
            job_error_text(&JobError::Burn(BurnError::Failed(
                "libburn : SORRY : Cannot open busy device '/dev/sr0'\n".to_string()
            ))),
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
        stage_files(&files, &stage).expect("stage two files");
        assert_eq!(std::fs::read(stage.join("x.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(stage.join("y.txt")).unwrap(), b"two");

        // 同名文件必须报错，不能悄悄覆盖先到的那个。
        let clashing = vec![
            src.join("a/x.txt").display().to_string(),
            src.join("a/x.txt").display().to_string(),
        ];
        let err = stage_files(&clashing, &base.join("stage-clash")).expect_err("must fail");
        assert!(err.contains("同名文件"), "{err}");

        // 不存在的源文件同样报错。
        let missing = vec![src.join("a/none.txt").display().to_string()];
        let err = stage_files(&missing, &base.join("stage-missing")).expect_err("must fail");
        assert!(err.contains("复制"), "{err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn verify_mode_and_message_wording() {
        assert_eq!(verify_mode_from_str("append"), Ok(VerifyMode::Append));
        assert_eq!(verify_mode_from_str("burn"), Ok(VerifyMode::Burn));
        assert_eq!(
            verify_mode_from_str("copy"),
            Err("未知的校验模式：copy".to_string())
        );
        assert_eq!(
            verify_message(VerifyMode::Append, &[]),
            "校验通过：盘上内容与源目录一致。"
        );
        assert_eq!(
            verify_message(VerifyMode::Burn, &[]),
            "校验通过：盘上内容与镜像一致。"
        );
        assert_eq!(
            verify_message(VerifyMode::Append, &["盘上缺少文件：a.txt".to_string()]),
            "校验未通过：发现 1 处差异，明细见页面。"
        );
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
