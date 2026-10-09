//! 盘上内容命令：浏览末区段、复制到系统剪贴板与从剪贴板读取文件（ADR-0012）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use optiburn_engine::{BurnError, CancelToken, extract_paths, list_tree};
use optiburn_mmc::{DiscStatus, MmcDevice};
use serde::Serialize;
use tauri::{AppHandle, State};

use super::{JobError, begin_job, fail_job, finish_job, job_error_text, set_join, unique_temp_dir};
use crate::i18n::{Lang, lang, pick};
use crate::job::{JobKind, JobState};

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
    tauri::async_runtime::spawn_blocking(move || match list_tree(&device) {
        Ok(entries) => Ok(entries
            .into_iter()
            .map(|entry| DiscEntryDto {
                path: entry.path,
                size: entry.size,
                is_dir: entry.is_dir,
            })
            .collect()),
        // 空盘本来就没有内容，返回空清单。盘上有区段却读不出 ISO 9660（例如 UDF
        // 盘）时按错误上报，不能显示成空盘。
        Err(BurnError::NoIsoSession) if disc_is_empty(&device) => Ok(Vec::new()),
        Err(error) => Err(disc_read_error(&device, error)),
    })
    .await
    .expect("list worker did not panic")
}

/// 盘片上没有任何区段时为真。读不到盘片信息时按“不是空盘”处理。
fn disc_is_empty(device: &str) -> bool {
    optiburn_transport::open(device)
        .ok()
        .map(|transport| {
            MmcDevice::new(transport)
                .read_disc_information()
                .map(|info| info.status == DiscStatus::Empty)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// 一次复制的结果，用于生成完成消息。
struct CopyReport {
    count: usize,
    bytes: u64,
}

/// 每次复制使用一个新的序号子目录，复制成功后清掉更早的暂存。
static COPY_SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// 复制暂存的根目录：整个进程共用一个（唯一名字，首次复制时创建）。
///
/// 失败不缓存：瞬时故障（例如临时目录满）之后下一次复制可以重试。
static COPY_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

fn copy_root() -> Result<PathBuf, String> {
    let mut slot = COPY_ROOT.lock().expect("copy root mutex");
    if slot.is_none() {
        *slot = Some(unique_temp_dir("optiburn-copy")?);
    }
    Ok(slot.as_ref().expect("copy root set").clone())
}

/// 把盘上选中的文件复制出来，并放进系统剪贴板，供文件管理器粘贴。
///
/// 复制会读盘并与写盘争同一台光驱，因此与写盘同占任务槽：可被中止，退出确认会
/// 等它收尾（ADR-0012）。
#[tauri::command]
pub async fn copy_disc_files(
    app: AppHandle,
    state: State<'_, JobState>,
    device: String,
    paths: Vec<String>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err(pick(lang(), "没有选中文件。", "No files are selected.").into());
    }
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        match copy_disc_files_blocking(&device, &paths, &cancel) {
            Ok(report) => finish_job(
                &worker,
                JobKind::Copy,
                "done",
                match lang() {
                    Lang::Zh => format!(
                        "已复制 {} 个文件（{}）到系统剪贴板，去文件管理器里粘贴即可。",
                        report.count,
                        human_bytes(report.bytes)
                    ),
                    Lang::En => format!(
                        "Copied {} files ({}) to the clipboard — paste them in your file manager.",
                        report.count,
                        human_bytes(report.bytes)
                    ),
                },
            ),
            Err(error) => {
                // 失败原因随失败事件广播，同时写一份到 stderr 备查（取消不算失败）。
                if !matches!(error, JobError::Cancelled { .. }) {
                    eprintln!(
                        "optiburn: 复制盘上文件失败：{}",
                        job_error_text(lang(), &error)
                    );
                }
                fail_job(&worker, JobKind::Copy, error);
            }
        }
    });
    set_join(&state, join);
    Ok(())
}

/// 人类可读的字节数，口径与设备页一致（GB、MB、KB、字节）。
fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} 字节")
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
fn copy_disc_files_blocking(
    device: &str,
    paths: &[String],
    cancel: &CancelToken,
) -> Result<CopyReport, JobError> {
    let root = copy_root().map_err(JobError::Input)?;
    let staging = root.join(COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed).to_string());
    std::fs::create_dir_all(&staging).map_err(|e| match lang() {
        Lang::Zh => JobError::Input(format!("创建暂存目录失败：{e}")),
        Lang::En => JobError::Input(format!("Failed to create the staging directory: {e}")),
    })?;
    if let Err(error) = extract_paths(device, paths, &staging, cancel) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(match error {
            BurnError::Cancelled => JobError::Cancelled {
                zh: "复制",
                en: "copy",
            },
            other => JobError::Input(disc_read_error(device, other)),
        });
    }
    let mut files = Vec::new();
    if let Err(e) = collect_files(&staging, &mut files) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(JobError::Input(match lang() {
            Lang::Zh => format!("扫描暂存目录失败：{e}"),
            Lang::En => format!("Failed to scan the staging directory: {e}"),
        }));
    }
    if let Err(e) = crate::clipboard::copy_files(&files) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(JobError::Input(e));
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
    Ok(CopyReport {
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

/// 读取盘片失败的中文文案：盘被挂载占用时给出卸载指引，末区段与路径问题单独说清。
fn disc_read_error(device: &str, error: BurnError) -> String {
    match &error {
        BurnError::NoIsoSession => {
            "盘上末区段不是 ISO 9660（例如 Windows 写入的 UDF 盘），读不出内容。".to_string()
        }
        BurnError::UnsafePath(path) => format!("盘上存在不安全的路径，已拒绝抽取：{path}"),
        _ => match optiburn_transport::mounted_at(device) {
            Some(point) => format!(
                "读取盘片失败，且光盘正被系统挂载在 {}：先在文件管理器里卸载（或运行 udisksctl unmount -b {device}）再试。",
                point.display()
            ),
            None => format!("读取盘片失败：{error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_matches_device_page_wording() {
        assert_eq!(human_bytes(512), "512 字节");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(3 * (1 << 20)), "3.0 MB");
    }
}
