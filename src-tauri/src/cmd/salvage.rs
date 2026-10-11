//! 未关闭轨道的抢救命令：把刻录中断留下的那一次写入里能读的数据抽到本地目录
//! （ADR-0022 补记）。设备页的入口在盘可追加时出现，与写盘同占任务槽（互斥）。

use std::path::Path;

use optiburn_engine::{BurnError, CancelToken, SalvageReport, salvage};
use tauri::{AppHandle, State};

use super::{JobError, JobKind, JobState, begin_job, fail_job, finish_job, set_join};
use crate::i18n::{Lang, lang, pick};

/// 抢救未关闭轨道里的数据到 `dest`（只读盘面，只往本地写）。完成消息里带
/// 完整、半截、没写三类文件的数量与半截 zip 的处置结果。
#[tauri::command]
pub async fn salvage_disc(
    app: AppHandle,
    state: State<'_, JobState>,
    device: String,
    dest: String,
) -> Result<(), String> {
    if dest.trim().is_empty() {
        return Err(pick(
            lang(),
            "没有选择抢救目录。",
            "No salvage folder was chosen.",
        )
        .into());
    }
    let cancel = begin_job(&state)?;
    let worker = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        match salvage_blocking(&device, Path::new(&dest), &cancel) {
            Ok(message) => finish_job(&worker, JobKind::Salvage, "done", None, message),
            Err(error) => {
                if !matches!(error, JobError::Cancelled { .. }) {
                    eprintln!(
                        "optiburn: 抢救未关闭轨道失败：{}",
                        super::job_error_text(lang(), &error)
                    );
                }
                fail_job(&worker, JobKind::Salvage, error);
            }
        }
    });
    set_join(&state, join);
    Ok(())
}

/// 抢救的阻塞部分：失败文案在这里定稿，成功时给出完成消息。
fn salvage_blocking(device: &str, dest: &Path, cancel: &CancelToken) -> Result<String, JobError> {
    let report = salvage(device, dest, cancel).map_err(|error| salvage_error(device, error))?;
    let Some(report) = report else {
        return Ok(pick(
            lang(),
            "盘上没有未关闭轨道（没有中断的写入，或它的目录树读不出来）：没有可抢救的内容。",
            "The disc has no unclosed track (no interrupted write, or its directory tree cannot be read): nothing to salvage.",
        )
        .to_string());
    };
    Ok(summary_text(lang(), &report, dest))
}

/// 完成消息：数量、目标目录，以及半截 zip 的处置结果。
fn summary_text(lang: Lang, report: &SalvageReport, dest: &Path) -> String {
    let mut text = match lang {
        Lang::Zh => format!(
            "已抢救 {} 个文件到 {}：完整 {}，半截 {}，一个字节都没写 {}。",
            report.files.len(),
            dest.display(),
            report.complete(),
            report.truncated(),
            report.missing()
        ),
        Lang::En => format!(
            "Salvaged {} files to {}: {} complete, {} truncated, {} not written at all.",
            report.files.len(),
            dest.display(),
            report.complete(),
            report.truncated(),
            report.missing()
        ),
    };
    for file in &report.files {
        let Some(zip) = &file.zip else {
            continue;
        };
        let name = file.path.trim_start_matches('/');
        text.push(' ');
        let note = if zip.entries == 0 {
            let dropped = zip.dropped_entry.as_deref().unwrap_or(match lang {
                Lang::Zh => "第一条",
                Lang::En => "the first entry",
            });
            match lang {
                Lang::Zh => format!(
                    "「{name}」的中断点落在第一条「{dropped}」里面，没有可保留的完整 zip 条目，按原始字节保留。"
                ),
                Lang::En => format!(
                    "The cut inside \"{name}\" falls inside its first entry \"{dropped}\", so no complete zip entry could be kept and the raw bytes were left in place."
                ),
            }
        } else {
            match lang {
                Lang::Zh => format!(
                    "「{name}」已重建为可解的 zip：{} 个完整条目，{} 没写完没有保留。",
                    zip.entries,
                    zip.dropped_entry.as_deref().unwrap_or("最后一个条目")
                ),
                Lang::En => format!(
                    "\"{name}\" was rebuilt into a readable zip with {} complete entries; the truncated one was dropped.",
                    zip.entries
                ),
            }
        };
        text.push_str(&note);
    }
    text
}

/// 抢救失败的文案：取消原样上报，盘被挂载给出卸载指引，其余保留动作前缀。
fn salvage_error(device: &str, error: BurnError) -> JobError {
    if matches!(error, BurnError::Cancelled) {
        return JobError::Cancelled {
            zh: "已中止抢救。",
            en: "Salvage cancelled.",
        };
    }
    if let Some(point) = optiburn_transport::mounted_at(device) {
        return JobError::Mounted {
            device: device.to_string(),
            point,
        };
    }
    let base = super::engine_error_text(
        lang(),
        "抢救未关闭轨道",
        "salvage the unclosed track",
        &error,
    );
    JobError::Input(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use optiburn_engine::{SalvageState, SalvagedFile, ZipSalvage};
    use std::path::PathBuf;

    fn report_with(files: Vec<SalvagedFile>) -> SalvageReport {
        SalvageReport {
            session_start: 301_170,
            boundary: 310_423,
            files,
        }
    }

    #[test]
    fn summary_names_counts_and_the_zip_outcome() {
        let report = report_with(vec![
            SalvagedFile {
                path: "/a.md".to_string(),
                size: 100,
                state: SalvageState::Complete,
                zip: None,
            },
            SalvagedFile {
                path: "/pack.zip".to_string(),
                size: 47_940_648,
                state: SalvageState::Truncated {
                    readable_bytes: 18_895_872,
                },
                zip: Some(ZipSalvage {
                    entries: 0,
                    stored_bytes: 0,
                    dropped_entry: Some("Atlas Playbook v0.5.0.apbx".to_string()),
                }),
            },
            SalvagedFile {
                path: "/b.bin".to_string(),
                size: 4096,
                state: SalvageState::Missing,
                zip: None,
            },
        ]);
        let text = summary_text(Lang::Zh, &report, &PathBuf::from("C:/out"));
        assert!(text.starts_with("已抢救 3 个文件到 C:/out：完整 1，半截 1，一个字节都没写 1。"));
        assert!(
            text.contains("「pack.zip」的中断点落在第一条「Atlas Playbook v0.5.0.apbx」里面"),
            "{text}"
        );
        // 重建成功的分支另给一份说明。
        let rebuilt = report_with(vec![SalvagedFile {
            path: "/old.zip".to_string(),
            size: 900,
            state: SalvageState::Truncated {
                readable_bytes: 500,
            },
            zip: Some(ZipSalvage {
                entries: 7,
                stored_bytes: 400,
                dropped_entry: Some("tail.bin".to_string()),
            }),
        }]);
        let text = summary_text(Lang::En, &rebuilt, &PathBuf::from("C:/out"));
        assert!(
            text.contains("\"old.zip\" was rebuilt into a readable zip with 7 complete entries"),
            "{text}"
        );
    }
}
