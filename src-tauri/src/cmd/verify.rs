//! 回读校验命令：把盘上末区段的内容与源逐项对比（ADR-0010）。

use std::path::Path;

use optiburn_engine::{BurnError, CancelToken, compare_trees, extract_tree};
use serde::Serialize;
use tauri::{AppHandle, State};

use super::{
    JobError, begin_job, engine_error_text, fail_job, finish_job, stage_files, unique_temp_dir,
};
use crate::i18n::{Lang, lang, pick};
use crate::job::{JobHandle, JobKind, JobState};

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
    /// 盘上读不出的文件路径，与内容差异分开报。
    unreadable: Vec<String>,
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
    let JobHandle { cancel, seq } = begin_job(&state, &device, JobKind::Verify)?;
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        match run_verify(&device, Path::new(&source), &files, mode, &cancel) {
            Ok(outcome) => {
                let passed = outcome.differences.is_empty() && outcome.unreadable.is_empty();
                finish_job(
                    &worker,
                    &device,
                    seq,
                    JobKind::Verify,
                    if passed { "done" } else { "failed" },
                    None,
                    verify_message(lang(), mode, &outcome.differences, &outcome.unreadable),
                );
                Ok(VerifyReportDto {
                    differences: outcome.differences,
                    unreadable: outcome.unreadable,
                })
            }
            Err(e) => Err(fail_job(&worker, &device, seq, JobKind::Verify, e)),
        }
    })
    .await
    .expect("verify worker did not panic")
}

/// 一次校验的结果：内容差异与盘上读不出的文件分开记。
struct VerifyOutcome {
    differences: Vec<String>,
    unreadable: Vec<String>,
}

/// 校验主体：门禁、抽取、对比。临时目录用完即删，失败路径也要删。
fn run_verify(
    device: &str,
    source: &Path,
    files: &[String],
    mode: VerifyMode,
    cancel: &CancelToken,
) -> Result<VerifyOutcome, JobError> {
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

/// 抽取失败的映射：取消照实归为取消，缺工具走共用文案，其余按读取失败给当前
/// 语言的文案。
fn read_error(lang: Lang, action_zh: &str, action_en: &str, error: BurnError) -> JobError {
    match error {
        BurnError::Cancelled => JobError::Cancelled {
            zh: "校验",
            en: "verification",
        },
        other => JobError::Input(engine_error_text(lang, action_zh, action_en, &other)),
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
) -> Result<VerifyOutcome, JobError> {
    let disc_tree = temp.join("disc");
    std::fs::create_dir_all(&disc_tree).map_err(|e| match lang() {
        Lang::Zh => JobError::Input(format!("创建校验暂存目录失败：{e}")),
        Lang::En => JobError::Input(format!(
            "Failed to create the verification staging directory: {e}"
        )),
    })?;
    let disc_report = extract_tree(Path::new(device), &disc_tree, cancel)
        .map_err(|e| read_error(lang(), "读取盘上内容", "Reading the disc", e))?;
    let unreadable = unreadable_paths(&disc_report);
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
            let image_report = extract_tree(source, &image_tree, cancel)
                .map_err(|e| read_error(lang(), "读取镜像", "Reading the image", e))?;
            let image_unreadable = unreadable_paths(&image_report);
            if !image_unreadable.is_empty() {
                return Err(JobError::Input(image_unreadable_message(
                    lang(),
                    &image_unreadable,
                )));
            }
            image_tree
        }
    };
    // 只有参照树里也存在的文件才在本次校验范围内。盘上多出的旧文件读不出不算
    // 本次校验失败，多区段盘上这种情况很常见。
    let unreadable = scoped_unreadable(&reference, unreadable);
    // 读不出的盘上文件没有落盘，留在参照树里会被对比误报成“盘上缺少文件”。
    drop_unreadable(&reference, &unreadable)?;
    Ok(VerifyOutcome {
        differences: compare_trees(&reference, &disc_tree, crate::i18n::lang_code()),
        unreadable,
    })
}

/// 只保留参照树里存在的读不出路径。
fn scoped_unreadable(reference: &Path, unreadable: Vec<String>) -> Vec<String> {
    unreadable
        .into_iter()
        .filter(|path| reference.join(path.trim_start_matches('/')).exists())
        .collect()
}

/// 报告里的读不出文件路径。
fn unreadable_paths(report: &optiburn_engine::ExtractReport) -> Vec<String> {
    report
        .files
        .iter()
        .filter_map(|file| match &file.state {
            optiburn_engine::ExtractState::Unreadable { .. } => Some(file.path.clone()),
            optiburn_engine::ExtractState::Complete => None,
        })
        .collect()
}

/// 把读不出的盘上路径从参照树里删掉。`NotFound` 忽略，其它本地错误上抛。
fn drop_unreadable(reference: &Path, unreadable: &[String]) -> Result<(), JobError> {
    for path in unreadable {
        let relative = path.trim_start_matches('/');
        let target = reference.join(relative);
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(JobError::Input(match lang() {
                    Lang::Zh => format!("从校验参照树移除读不出的文件失败：{error}"),
                    Lang::En => format!(
                        "Failed to drop an unreadable file from the verification reference: {error}"
                    ),
                }));
            }
        }
    }
    Ok(())
}

/// 镜像里有文件读不出时校验无法进行：参照树不完整，继续对比只会给出假差异。
fn image_unreadable_message(lang: Lang, paths: &[String]) -> String {
    let list = match lang {
        Lang::Zh => paths.join("、"),
        Lang::En => paths.join(", "),
    };
    match lang {
        Lang::Zh => format!(
            "镜像里有 {} 个文件读不出，校验无法进行：{list}。",
            paths.len()
        ),
        Lang::En => format!(
            "{} files in the image could not be read, so verification cannot run: {list}.",
            paths.len()
        ),
    }
}

/// 校验完成文案（job-done 消息）：内容差异与读不出的文件分开计数，两类都为空才算通过。
fn verify_message(
    lang: Lang,
    mode: VerifyMode,
    differences: &[String],
    unreadable: &[String],
) -> String {
    if !differences.is_empty() || !unreadable.is_empty() {
        let mut parts = Vec::new();
        if !differences.is_empty() {
            parts.push(match lang {
                Lang::Zh => format!("{} 处差异", differences.len()),
                Lang::En => format!("{} differences", differences.len()),
            });
        }
        if !unreadable.is_empty() {
            parts.push(match lang {
                Lang::Zh => format!("{} 个文件读不出", unreadable.len()),
                Lang::En => format!("{} unreadable files", unreadable.len()),
            });
        }
        let detail = parts.join(match lang {
            Lang::Zh => "，",
            Lang::En => ", ",
        });
        return match lang {
            Lang::Zh => format!("校验未通过：{detail}，明细见页面。"),
            Lang::En => format!("Verification failed: {detail}. See the page for details."),
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
    fn read_error_routes_missing_tool_to_the_shared_text() {
        match read_error(
            Lang::Zh,
            "读取盘上内容",
            "Reading the disc",
            BurnError::MissingTool("xorriso".into()),
        ) {
            JobError::Input(message) => {
                assert_eq!(message, BurnError::missing_tool_user_text("xorriso"))
            }
            _ => panic!("expected JobError::Input"),
        }
        // 其它读取失败保留动作前缀。
        match read_error(
            Lang::Zh,
            "读取盘上内容",
            "Reading the disc",
            BurnError::ReadFailed("boom".into()),
        ) {
            JobError::Input(message) => assert_eq!(message, "读取盘上内容失败：boom"),
            _ => panic!("expected JobError::Input"),
        }
    }

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
            verify_message(Lang::Zh, VerifyMode::Append, &[], &[]),
            "校验通过：盘上内容与所选文件逐项一致。"
        );
        assert_eq!(
            verify_message(Lang::En, VerifyMode::Burn, &[], &[]),
            "Verification passed: the disc matches the image item by item."
        );
        assert_eq!(
            verify_message(
                Lang::Zh,
                VerifyMode::Append,
                &["盘上缺少文件：a.txt".to_string()],
                &[]
            ),
            "校验未通过：1 处差异，明细见页面。"
        );
        assert_eq!(
            verify_message(
                Lang::En,
                VerifyMode::Append,
                &["File missing on disc: a.txt".to_string()],
                &[]
            ),
            "Verification failed: 1 differences. See the page for details."
        );
        assert_eq!(
            verify_message(
                Lang::Zh,
                VerifyMode::Append,
                &[],
                &["/坏文件.txt".to_string(), "/另一个.bin".to_string()]
            ),
            "校验未通过：2 个文件读不出，明细见页面。"
        );
        assert_eq!(
            verify_message(
                Lang::En,
                VerifyMode::Append,
                &["Content mismatch: a.txt".to_string()],
                &["/bad.txt".to_string()]
            ),
            "Verification failed: 1 differences, 1 unreadable files. See the page for details."
        );
    }

    /// 读不出清单只收参照树里存在的路径：盘上多出的旧文件不算本次校验失败。
    #[test]
    fn scoped_unreadable_keeps_only_reference_files() {
        let root =
            std::env::temp_dir().join(format!("optiburn-verify-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the reference tree");
        std::fs::write(root.join("a.txt"), b"alpha").expect("write a.txt");

        let scoped = scoped_unreadable(
            &root,
            vec![
                "/a.txt".to_string(),
                "/旧区段文件.txt".to_string(),
                "/子目录/不存在.txt".to_string(),
            ],
        );
        assert_eq!(scoped, vec!["/a.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 读不出的文件从参照树里移除后不再被对比报成盘上缺少文件。
    #[test]
    fn drop_unreadable_removes_the_reference_file() {
        let root =
            std::env::temp_dir().join(format!("optiburn-verify-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("子目录")).expect("create the reference tree");
        std::fs::write(root.join("a.txt"), b"alpha").expect("write a.txt");
        std::fs::write(root.join("子目录/坏.txt"), b"bad").expect("write the nested file");

        assert!(
            drop_unreadable(&root, &["/a.txt".to_string(), "/子目录/坏.txt".to_string()]).is_ok(),
            "移除读不出的文件不应失败"
        );
        assert!(!root.join("a.txt").exists());
        assert!(!root.join("子目录/坏.txt").exists());
        // 不存在的路径不算错误（参照树与盘上树不一致时也会走到这里）。
        assert!(
            drop_unreadable(&root, &["/没有这个".to_string()]).is_ok(),
            "不存在的参照文件不算错误"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
