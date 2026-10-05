//! xorriso 子进程引擎（ADR-0004）。
//!
//! 走 `xorriso -as cdrecord` 兼容层，参数组合与 DiscCTL 生产环境验证过的形态一致。
//! GPL 边界止于进程边界：本仓库不链接 libburn/libisofs。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use crate::{BurnEngine, BurnError, BurnJob};

/// 依赖的可执行文件名。
const XORRISO: &str = "xorriso";
/// 子进程失败时保留多少行 stderr 作为摘要。
const TAIL_LINES: usize = 10;

/// 用 `xorriso -as cdrecord` 写盘。
#[derive(Debug, Default, Clone, Copy)]
pub struct XorrisoEngine;

impl BurnEngine for XorrisoEngine {
    fn name(&self) -> &'static str {
        "xorriso"
    }

    fn burn(&self, job: &BurnJob, progress: &mut dyn FnMut(f32)) -> Result<(), BurnError> {
        run(XORRISO, &cdrecord_args(job), progress)
    }
}

/// 跑一个写盘子进程，把 stderr 上的百分比转成进度，并在退出码非零时报错。
///
/// 单独抽出来是为了能用本地 shell 脚本当替身测试：`xorriso` 的参数由调用方给，
/// 这里只管进程、进度与失败摘要。
fn run(program: &str, args: &[String], progress: &mut dyn FnMut(f32)) -> Result<(), BurnError> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                BurnError::MissingTool(format!("{program} (sudo apt install {program})"))
            }
            _ => BurnError::Io(e),
        })?;

    let Some(stderr) = child.stderr.take() else {
        return Err(BurnError::Failed(format!(
            "{program}: cannot capture stderr"
        )));
    };
    let mut tail: VecDeque<String> = VecDeque::new();
    for line in BufReader::new(stderr).lines() {
        let line = line?;
        if let Some(fraction) = parse_progress(&line) {
            progress(fraction);
        }
        if tail.len() == TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(BurnError::Failed(
            tail.into_iter().collect::<Vec<_>>().join("\n"),
        ));
    }
    progress(1.0);
    Ok(())
}

/// 组装 `xorriso -as cdrecord` 的参数表。
///
/// 顺序：兼容层开关 → 设备 → 数据模式 → 倍速 → 多区段 → 镜像路径。
fn cdrecord_args(job: &BurnJob) -> Vec<String> {
    let mut args = vec![
        "-as".to_string(),
        "cdrecord".to_string(),
        format!("dev={}", job.device),
        "-data".to_string(),
    ];
    if let Some(speed) = job.speed {
        args.push(format!("speed={speed}"));
    }
    if job.multi {
        args.push("-multi".to_string());
    }
    args.push(job.image.display().to_string());
    args
}

/// 取出这一行里第一个 `NN%` 并换算成 0.0–1.0。
///
/// xorriso 的 `-as cdrecord` 输出里，百分比既可能来自写入进度（`4.4% done`），也可能
/// 来自缓冲区统计（`(fifo 100%) [buf 97%]`）——后者会先出现，所以 v0 的进度只是粗粒度
/// 提示，成功时统一补发 1.0；精确进度等原生 MMC 引擎（能自己数 LBA）。
fn parse_progress(line: &str) -> Option<f32> {
    let percent = line.find('%')?;
    let digits = &line[..percent];
    let start = digits
        .rfind(|c: char| !c.is_ascii_digit() && c != '.')
        .map_or(0, |i| i + c_len(&digits[i..]));
    let value: f32 = digits[start..].parse().ok()?;
    if value.is_finite() && (0.0..=100.0).contains(&value) {
        Some(value / 100.0)
    } else {
        None
    }
}

/// 边界字符的字节长度，用于把 `rfind` 的字节下标推进到下一个字符。
fn c_len(s: &str) -> usize {
    s.chars().next().map_or(0, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn job(speed: Option<u32>, multi: bool) -> BurnJob {
        BurnJob {
            image: PathBuf::from("/tmp/optiburn.iso"),
            device: "/dev/sr0".to_string(),
            speed,
            multi,
        }
    }

    fn close_to(actual: Option<f32>, expected: f32) -> bool {
        actual.is_some_and(|v| (v - expected).abs() < 1e-6)
    }

    #[test]
    fn cdrecord_args_cover_all_flag_combinations() {
        assert_eq!(
            cdrecord_args(&job(None, false)),
            vec![
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "/tmp/optiburn.iso"
            ]
        );
        assert_eq!(
            cdrecord_args(&job(Some(8), false)),
            vec![
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "speed=8",
                "/tmp/optiburn.iso"
            ]
        );
        assert_eq!(
            cdrecord_args(&job(None, true)),
            vec![
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "-multi",
                "/tmp/optiburn.iso"
            ]
        );
        assert_eq!(
            cdrecord_args(&job(Some(8), true)),
            vec![
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "speed=8",
                "-multi",
                "/tmp/optiburn.iso"
            ]
        );
    }

    #[test]
    fn progress_reads_percentage_tokens() {
        assert!(close_to(parse_progress("Track 01: 4.4% done"), 0.044));
        assert!(close_to(parse_progress("  47%  "), 0.47));
        assert!(close_to(parse_progress("Writing: 100%"), 1.0));
    }

    #[test]
    fn progress_ignores_lines_without_a_valid_percentage() {
        assert_eq!(parse_progress("xorriso : NOTE : Recycling -abort_on"), None);
        assert_eq!(parse_progress("%"), None);
        assert_eq!(parse_progress("1000%"), None);
        // 中文前缀不该让切片落在非字符边界上而 panic。
        assert_eq!(parse_progress("写入完成"), None);
        assert!(close_to(parse_progress("写入： 50%"), 0.5));
    }

    #[test]
    fn missing_binary_is_a_missing_tool_error() {
        let err = run("optiburn-nonexistent-program", &[], &mut |_| {})
            .expect_err("spawning a nonexistent program must fail");
        assert!(
            matches!(&err, BurnError::MissingTool(name) if name.starts_with("optiburn-nonexistent-program")),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn progress_streams_and_finishes_at_one() {
        let mut seen = Vec::new();
        run(
            "sh",
            &[
                "-c".to_string(),
                "printf 'Track 01: 25%% done\\n' >&2; printf '[buf 97%%]\\n' >&2".to_string(),
            ],
            &mut |f| seen.push(f),
        )
        .expect("script exits 0");

        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!((seen[0] - 0.25).abs() < 1e-6);
        assert!((seen[1] - 0.97).abs() < 1e-6);
        assert!((seen[2] - 1.0).abs() < 1e-6);
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_reports_stderr_tail() {
        let err = run(
            "sh",
            &[
                "-c".to_string(),
                "printf 'xorriso : FAILURE : drive is busy\\n' >&2; exit 1".to_string(),
            ],
            &mut |_| {},
        )
        .expect_err("script exits 1");
        assert!(
            matches!(&err, BurnError::Failed(msg) if msg.contains("drive is busy")),
            "{err:?}"
        );
    }

    #[test]
    #[ignore = "needs optical drive"]
    fn burn_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. /dev/sr0");
        let image = std::env::var("OPTIBURN_IMAGE").expect("set OPTIBURN_IMAGE to an .iso path");
        let mut last = 0.0f32;
        XorrisoEngine
            .burn(
                &BurnJob {
                    image: PathBuf::from(image),
                    device,
                    speed: None,
                    multi: false,
                },
                &mut |f| last = f,
            )
            .expect("burn failed");
        assert!((last - 1.0).abs() < 1e-6);
    }
}
