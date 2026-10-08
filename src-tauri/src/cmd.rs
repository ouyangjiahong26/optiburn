//! Tauri 命令层：把核心 crate 的能力暴露给前端，并统一中文文案。

use std::path::{Path, PathBuf};

use optiburn_engine::{BurnEngine, BurnError, BurnJob, CancelToken, GrowJob, XorrisoEngine, grow};
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
    /// 等待介质就绪或读取盘片信息失败。
    Disc(MmcError),
    /// 写前门禁拒绝。
    Gate(WriteBlock),
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
        JobError::Burn(BurnError::Failed(tail)) => format!("刻录失败：{tail}"),
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

/// 把目录追加到盘上（增长模式），交互与刻录一致。
#[tauri::command]
pub async fn start_append(
    app: AppHandle,
    state: State<'_, JobState>,
    src: String,
    device: String,
    volume_id: String,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<(), String> {
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        let src = PathBuf::from(src);
        let result = run_disc_task(DiscTask {
            app: &worker,
            kind: JobKind::Append,
            append: true,
            path: &src,
            device: &device,
            volume_id: &volume_id,
            speed,
            close_disc,
            cancel: &cancel,
        });
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
    let info = open_gate(task.device)?;
    // 镜像路径不接受可追加盘（单区段镜像会遮住已有区段的文件），增长模式放行。
    approve_write(&info, task.append).map_err(JobError::Gate)?;

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
