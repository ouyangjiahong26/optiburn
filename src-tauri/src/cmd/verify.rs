//! 回读校验命令：把盘上末区段的内容与源逐项对比（ADR-0010）。

use std::path::Path;

use optiburn_engine::{BurnError, CancelToken, compare_trees, extract_tree};
use serde::Serialize;
use tauri::{AppHandle, State};

use super::{JobError, begin_job, fail_job, finish_job, stage_files, unique_temp_dir};
use crate::i18n::{Lang, lang, pick};
use crate::job::{JobKind, JobState};

/// 校验模式：追加校验对比待刻录文件，刻录校验对比镜像。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VerifyMode {
    Append,
    Burn,
}

/// 前端的模式字符串到 [`VerifyMode`] 的映射。
fn verify_mode_from_str(lang: Lang, mode: &str) -> Result<VerifyMode, String> {
    match mode {
        "append" => Ok(VerifyMode::Append),
        "burn" => Ok(VerifyMode::Burn),
        other => match lang {
            Lang::Zh => Err(format!("未知的校验模式：{other}")),
            Lang::En => Err(format!("Unknown verify mode: {other}")),
        },
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
    let mode = verify_mode_from_str(lang(), &mode)?;
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
                    verify_message(lang(), mode, &differences),
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
    let temp = unique_temp_dir("optiburn-verify").map_err(JobError::Input)?;
    let result = extract_and_compare(device, source, files, mode, &temp, cancel);
    let _ = std::fs::remove_dir_all(&temp);
    result
}

/// 抽取失败的映射：取消照实归为取消，其余按读取失败给当前语言的文案。
fn read_error(lang: Lang, action_zh: &str, action_en: &str, error: BurnError) -> JobError {
    match error {
        BurnError::Cancelled => JobError::Cancelled {
            zh: "校验",
            en: "verification",
        },
        other => match lang {
            Lang::Zh => JobError::Input(format!("{action_zh}失败：{other}")),
            Lang::En => JobError::Input(format!("{action_en} failed: {other}")),
        },
    }
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
    std::fs::create_dir_all(&disc_tree).map_err(|e| match lang() {
        Lang::Zh => JobError::Input(format!("创建校验暂存目录失败：{e}")),
        Lang::En => JobError::Input(format!(
            "Failed to create the verification staging directory: {e}"
        )),
    })?;
    extract_tree(Path::new(device), &disc_tree, cancel)
        .map_err(|e| read_error(lang(), "读取盘上内容", "reading the disc", e))?;
    let reference = match mode {
        VerifyMode::Append => {
            if files.is_empty() {
                return Err(JobError::Input(
                    pick(
                        lang(),
                        "请先选择要刻录的文件。",
                        "Select the files to burn first.",
                    )
                    .to_string(),
                ));
            }
            let staged = temp.join("source");
            stage_files(lang(), files, &staged).map_err(JobError::Input)?;
            staged
        }
        VerifyMode::Burn => {
            let image_tree = temp.join("image");
            std::fs::create_dir_all(&image_tree).map_err(|e| match lang() {
                Lang::Zh => JobError::Input(format!("创建校验暂存目录失败：{e}")),
                Lang::En => JobError::Input(format!(
                    "Failed to create the verification staging directory: {e}"
                )),
            })?;
            extract_tree(source, &image_tree, cancel)
                .map_err(|e| read_error(lang(), "读取镜像", "reading the image", e))?;
            image_tree
        }
    };
    Ok(compare_trees(
        &reference,
        &disc_tree,
        crate::i18n::lang_code(),
    ))
}

/// 校验完成文案（job-done 消息）。
fn verify_message(lang: Lang, mode: VerifyMode, differences: &[String]) -> String {
    if !differences.is_empty() {
        return match lang {
            Lang::Zh => {
                format!(
                    "校验未通过：发现 {} 处差异，明细见页面。",
                    differences.len()
                )
            }
            Lang::En => format!(
                "Verification failed: {} differences found. See the page for details.",
                differences.len()
            ),
        };
    }
    match (mode, lang) {
        (VerifyMode::Append, Lang::Zh) => "校验通过：盘上内容与所选文件逐项一致。".into(),
        (VerifyMode::Append, Lang::En) => {
            "Verification passed: the disc matches the selected files item by item.".into()
        }
        (VerifyMode::Burn, Lang::Zh) => "校验通过：盘上内容与镜像逐项一致。".into(),
        (VerifyMode::Burn, Lang::En) => {
            "Verification passed: the disc matches the image item by item.".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_mode_and_message_wording() {
        assert_eq!(
            verify_mode_from_str(Lang::Zh, "append"),
            Ok(VerifyMode::Append)
        );
        assert_eq!(verify_mode_from_str(Lang::En, "burn"), Ok(VerifyMode::Burn));
        assert_eq!(
            verify_mode_from_str(Lang::En, "copy"),
            Err("Unknown verify mode: copy".to_string())
        );
        assert_eq!(
            verify_message(Lang::Zh, VerifyMode::Append, &[]),
            "校验通过：盘上内容与所选文件逐项一致。"
        );
        assert_eq!(
            verify_message(Lang::En, VerifyMode::Burn, &[]),
            "Verification passed: the disc matches the image item by item."
        );
        assert_eq!(
            verify_message(
                Lang::Zh,
                VerifyMode::Append,
                &["盘上缺少文件：a.txt".to_string()]
            ),
            "校验未通过：发现 1 处差异，明细见页面。"
        );
        assert_eq!(
            verify_message(
                Lang::En,
                VerifyMode::Append,
                &["File missing on disc: a.txt".to_string()]
            ),
            "Verification failed: 1 differences found. See the page for details."
        );
    }
}
