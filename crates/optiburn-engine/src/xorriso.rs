//! xorriso 子进程引擎（ADR-0004）。
//!
//! 走 `xorriso -as cdrecord` 兼容层。参数组合的依据在 ADR-0004：本机
//! `xorriso -as cdrecord -help` 实查过 `dev=`、`speed=`、`-data`、`-multi` 均受支持。
//! GPL 边界止于进程边界：本仓库不链接 libburn/libisofs。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{BurnEngine, BurnError, BurnJob, CancelToken, GrowJob};

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

    fn burn(
        &self,
        job: &BurnJob,
        progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        run(XORRISO, &cdrecord_args(job), progress, cancel)
    }
}

/// 用 xorriso 增长模式把目录写到盘上（多区段合并）。
///
/// `-dev` 会读出盘上已有区段的目录树，提交时新区段同时携带新旧文件，Windows 等
/// 默认挂载最后一区段的系统仍能看到全部内容。空盘时它直接写第一区段，因此
/// `append` 不必区分首刻与追加。`close_disc` 在提交前加 `-close on`，写完把盘
/// 标记为不可追加，这是默认多区段策略下唯一的封盘出口。xorriso 手册明示 `-close`
/// 对 DVD-RAM、BD-RE 这类可覆写介质不生效，这类盘无需封盘即可继续覆写。
pub fn grow(
    job: &GrowJob,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    run(XORRISO, &grow_args(job), progress, cancel)
}

/// 组装增长模式参数表。
///
/// `-map 源目录 /` 把目录内容映射到盘根，与 `build-image` 的语义一致。不带
/// `-eject`，写完保留盘在仓内供回读校验。增长模式的写进度同样以行内百分比出现在
/// stderr（实测是粗粒度输出），复用同一解析。
fn grow_args(job: &GrowJob) -> Vec<OsString> {
    let mut args = vec![OsString::from("-dev"), OsString::from(&job.device)];
    if let Some(speed) = job.speed {
        args.push(OsString::from("-speed"));
        args.push(OsString::from(format!("{speed}")));
    }
    args.extend([
        OsString::from("-volid"),
        OsString::from(&job.volume_id),
        OsString::from("-joliet"),
        OsString::from("on"),
        OsString::from("-map"),
        // 用 OsStr 而不是 `display()`：后者会把非 UTF-8 字节替换成 U+FFFD，写盘就会找错文件。
        job.src.as_os_str().to_os_string(),
        OsString::from("/"),
    ]);
    if job.close_disc {
        args.extend([OsString::from("-close"), OsString::from("on")]);
    }
    args.push(OsString::from("-commit"));
    args
}

/// 读盘上最后一个区段的卷标（PVD 的 `Volume Id`），用于 GUI 追加页预填（ADR-0010）。
///
/// `-pvd_info` 的关键行出在 stdout：`Volume Id    : <文本>`（不带引号）。解析依赖
/// 英文消息，统一加 `LC_ALL=C`，本机实测该 locale 下非 ASCII 卷标原样输出。
pub fn read_volume_id(device: &str) -> Result<String, BurnError> {
    let (stdout, stderr) = run_output(XORRISO, &pvd_info_args(device))?;
    if is_blank_image_fallback(&stderr) {
        return Err(BurnError::Failed(
            "no ISO 9660 image at the last session".to_string(),
        ));
    }
    parse_volume_id(&stdout)
        .ok_or_else(|| BurnError::Failed("xorriso did not report a Volume Id".to_string()))
}

/// 判断 xorriso 是否因为盘上没有 ISO 9660 而兜底造了一个空镜像。
///
/// 这种情况下 `-pvd_info` 报的是空镜像的默认卷标 `ISOIMAGE`，是假数据。实测 UDF
/// 盘（Windows 写入的多区段 DVD-R）走的就是这条路径，此时必须拒绝，不能把假卷标
/// 预填进追加页。
fn is_blank_image_fallback(stderr: &str) -> bool {
    stderr.contains("Creating blank image") || stderr.contains("No ISO 9660 image")
}

/// 盘上最后一个区段是否能作为 ISO 9660 读出。
///
/// 追加前门禁用它挡住“末区段不是 ISO 9660”的盘，例如 Windows 写入的 UDF 盘。
/// 这类盘续写 ISO 区段后，按最后一区段挂载的系统（Windows）只会看到新内容，
/// 原有文件被遮住（实测，见 ADR-0010）。
pub fn last_session_is_iso(device: &str) -> Result<bool, BurnError> {
    let (_, stderr) = run_output(XORRISO, &pvd_info_args(device))?;
    Ok(!is_blank_image_fallback(&stderr))
}

/// 写盘失败里可归类的成因，供上层组织面向人的文案。
///
/// 判定输入是 xorriso 的 stderr 尾部（[`BurnError::Failed`] 里那段）。条目按
/// 真实遇到的报错逐步补充，认不出的形态归到 [`BurnFailure::Other`]，原始输出
/// 由上层记日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnFailure {
    /// 写入过程中与驱动器的连接中断，常见于拔线、供电不稳。
    DriveLost,
    /// 设备被其它程序占用，常见于系统挂载了光盘。
    DeviceBusy,
    /// 未识别出已知形态。
    Other,
}

impl BurnFailure {
    /// 从 stderr 尾部判定成因。
    pub fn classify(tail: &str) -> Self {
        if tail.contains("Lost connection to drive") || tail.contains("SG_ERR_DID_ERROR") {
            Self::DriveLost
        } else if tail.contains("Cannot open busy device") {
            Self::DeviceBusy
        } else {
            Self::Other
        }
    }
}

/// 组装读卷标的参数表。
fn pvd_info_args(device: &str) -> Vec<OsString> {
    vec![
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

/// 把镜像或设备的目录树抽取到本地目录，供回读校验使用（ADR-0010）。
///
/// `-osirrox on` 打开抽取权限，`-extract / <目标>` 把整树落到目标目录。xorriso 会
/// 尽力还原时间戳，但抽出的文件 ctime 必然是新值，所以校验只在本地按名字与内容
/// 对比，不比较属性。
pub fn extract_tree(source: &Path, dest: &Path, cancel: &CancelToken) -> Result<(), BurnError> {
    run(XORRISO, &extract_args(source, dest), &mut |_| {}, cancel)
}

/// 组装抽取参数表。路径按 `OsStr` 原样传递，不做有损转换。
fn extract_args(source: &Path, dest: &Path) -> Vec<OsString> {
    vec![
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

/// 列出盘上最后一区段的目录树，用于 GUI 的盘片浏览。
///
/// `-find / -exec lsdl` 一次读取拿到全部条目的路径与大小。`-drive_access shared`
/// 是只读打开，本机已挂载的盘也有机会直接读到（挂载点之外仍优先用盘上最后一区段）。
pub fn list_tree(device: &str) -> Result<Vec<DiscEntry>, BurnError> {
    let (stdout, _stderr) = run_output(XORRISO, &list_args(device))?;
    Ok(parse_lsdl(&stdout))
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

/// 解析 `lsdl` 的一行：`-rw-r--r-- 1 1000 1000 7797 Oct 9 10:25 '/路径'`。
///
/// 路径可能在引号里带空格，所以大小取按空白切分的第 5 个字段，路径取首个引号到
/// 末个引号之间的内容。根目录 `/` 自身不进列表。
fn parse_lsdl(stdout: &str) -> Vec<DiscEntry> {
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if !line.starts_with('d') && !line.starts_with('-') && !line.starts_with('l') {
            continue;
        }
        let (Some(first), Some(last)) = (line.find('\''), line.rfind('\'')) else {
            continue;
        };
        if last <= first {
            continue;
        }
        let path = &line[first + 1..last];
        if path == "/" {
            continue;
        }
        let size = line
            .split_whitespace()
            .nth(4)
            .and_then(|field| field.parse::<u64>().ok())
            .unwrap_or(0);
        entries.push(DiscEntry {
            path: path.to_string(),
            size,
            is_dir: line.starts_with('d'),
        });
    }
    entries
}

/// 从盘上按 ISO 路径抽取若干文件或目录到本地目录，一次 xorriso 调用完成。
///
/// 每个 ISO 路径落到 `dest` 下同名的相对位置，父目录预先建好。只读打开
/// （`-drive_access shared`），供 GUI 把盘上文件复制到本地。
pub fn extract_paths(
    device: &str,
    paths: &[String],
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    for path in paths {
        let target = dest.join(path.trim_start_matches('/'));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    run(
        XORRISO,
        &extract_paths_args(device, paths, dest),
        &mut |_| {},
        cancel,
    )
}

/// 组装多路径抽取的参数表：一次加载盘片，按序执行每条 `-extract`。
fn extract_paths_args(device: &str, paths: &[String], dest: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
        OsString::from("-indev"),
        OsString::from(device),
        OsString::from("-osirrox"),
        OsString::from("on"),
    ];
    for path in paths {
        args.push(OsString::from("-extract"));
        args.push(OsString::from(path));
        args.push(dest.join(path.trim_start_matches('/')).into_os_string());
    }
    args
}

/// 跑一个只读查询并收集 stdout 与 stderr。
///
/// 文案解析依赖英文关键行，固定 `LC_ALL=C`。本机实测该 locale 下中文卷标与路径
/// 原样输出。失败时取 stderr 尾部做摘要。
fn run_output(program: &str, args: &[OsString]) -> Result<(String, String), BurnError> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                BurnError::MissingTool(format!("{program} (sudo apt install {program})"))
            }
            _ => BurnError::Io(e),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BurnError::Failed(tail_of(&stderr)));
    }
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// 取文本最后 [`TAIL_LINES`] 个非空行，作为失败摘要。
fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

/// 跑一个写盘子进程，把 stderr 上的百分比转成进度，并在退出码非零时报错。
///
/// 取消是协作式的：令牌只能在读到一行 stderr 的间隙里被检查，置位就杀掉子进程
/// 并返回 [`BurnError::Cancelled`]。子进程已自然写完退出时，迟到的取消不再起
/// 作用——盘已写完，仍按成功返回。
///
/// 单独抽出来是为了能用本地 shell 脚本当替身测试：`xorriso` 的参数由调用方给，
/// 这里只管进程、进度、取消与失败摘要。
fn run(
    program: &str,
    args: &[OsString],
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
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
        if cancel.is_cancelled() {
            // 先杀再收尸，避免留下僵尸进程；退出码反正是 Cancelled，无须再看。
            child.kill()?;
            child.wait()?;
            return Err(BurnError::Cancelled);
        }
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
/// 参数依次是兼容层开关、设备、数据模式、倍速、多区段、镜像路径。
fn cdrecord_args(job: &BurnJob) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-as"),
        OsString::from("cdrecord"),
        OsString::from(format!("dev={}", job.device)),
        OsString::from("-data"),
    ];
    if let Some(speed) = job.speed {
        args.push(OsString::from(format!("speed={speed}")));
    }
    if job.multi {
        args.push(OsString::from("-multi"));
    }
    // 用 OsStr 而不是 `display()`：后者会把非 UTF-8 字节替换成 U+FFFD，写盘就会找错文件。
    args.push(job.image.as_os_str().to_os_string());
    args
}

/// 取出这一行里第一个 `NN%` 并换算成 0.0–1.0。
///
/// xorriso 的 `-as cdrecord` 输出里，百分比既可能来自写入进度（`4.4% done`），也可能
/// 来自缓冲区统计（`(fifo 100%) [buf 97%]`）。后者会先出现，所以 v0 的进度只是粗粒度
/// 提示，成功时统一补发 1.0。精确进度等原生 MMC 引擎（能自己数 LBA）。
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
    // Duration 与 Instant 只有取消测试（unix）在用，Windows 目标上不能出现死导入。
    #[cfg(unix)]
    use std::time::{Duration, Instant};

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

    /// 期望参数表：与实现同样用 `OsString`，避免为断言再把路径窄化回 `String`。
    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn cdrecord_args_cover_all_flag_combinations() {
        assert_eq!(
            cdrecord_args(&job(None, false)),
            os(&[
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "/tmp/optiburn.iso"
            ])
        );
        assert_eq!(
            cdrecord_args(&job(Some(8), false)),
            os(&[
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "speed=8",
                "/tmp/optiburn.iso"
            ])
        );
        assert_eq!(
            cdrecord_args(&job(None, true)),
            os(&[
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "-multi",
                "/tmp/optiburn.iso"
            ])
        );
        assert_eq!(
            cdrecord_args(&job(Some(8), true)),
            os(&[
                "-as",
                "cdrecord",
                "dev=/dev/sr0",
                "-data",
                "speed=8",
                "-multi",
                "/tmp/optiburn.iso"
            ])
        );
    }

    #[test]
    fn grow_args_map_src_to_root_and_commit() {
        let job = GrowJob {
            src: PathBuf::from("/tmp/stage"),
            device: "/dev/sr0".to_string(),
            speed: Some(8),
            volume_id: "OPTIBURN".to_string(),
            close_disc: false,
        };
        assert_eq!(
            grow_args(&job),
            os(&[
                "-dev",
                "/dev/sr0",
                "-speed",
                "8",
                "-volid",
                "OPTIBURN",
                "-joliet",
                "on",
                "-map",
                "/tmp/stage",
                "/",
                "-commit"
            ])
        );
    }

    #[test]
    fn grow_args_omit_speed_when_unset() {
        let job = GrowJob {
            src: PathBuf::from("/tmp/stage"),
            device: "/dev/sr0".to_string(),
            speed: None,
            volume_id: "OPTIBURN".to_string(),
            close_disc: false,
        };
        assert!(!grow_args(&job).contains(&OsString::from("-speed")));
    }

    #[test]
    fn grow_args_close_disc_marks_medium_not_appendable() {
        let job = GrowJob {
            src: PathBuf::from("/tmp/stage"),
            device: "/dev/sr0".to_string(),
            speed: None,
            volume_id: "OPTIBURN".to_string(),
            close_disc: true,
        };
        assert_eq!(
            grow_args(&job),
            os(&[
                "-dev",
                "/dev/sr0",
                "-volid",
                "OPTIBURN",
                "-joliet",
                "on",
                "-map",
                "/tmp/stage",
                "/",
                "-close",
                "on",
                "-commit"
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_image_path_survives_as_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let mut job = job(None, false);
        job.image = PathBuf::from(OsString::from_vec(b"/tmp/\xff\xfe.iso".to_vec()));

        let args = cdrecord_args(&job);
        let last = args.last().expect("image path is the last argument");
        // display() 会把它变成 U+FFFD，OsStr 必须原样保留这几个字节。
        assert_eq!(last.as_encoded_bytes(), b"/tmp/\xff\xfe.iso");
        assert!(
            std::ffi::OsStr::new(last)
                .as_bytes()
                .ends_with(b"\xff\xfe.iso")
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
        let err = run(
            "optiburn-nonexistent-program",
            &[],
            &mut |_| {},
            &CancelToken::default(),
        )
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
            &os(&[
                "-c",
                "printf 'Track 01: 25%% done\\n' >&2; printf '[buf 97%%]\\n' >&2",
            ]),
            &mut |f| seen.push(f),
            &CancelToken::default(),
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
            &os(&[
                "-c",
                "printf 'xorriso : FAILURE : drive is busy\\n' >&2; exit 1",
            ]),
            &mut |_| {},
            &CancelToken::default(),
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
                &CancelToken::default(),
            )
            .expect("burn failed");
        assert!((last - 1.0).abs() < 1e-6);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_stops_the_subprocess() {
        let token = CancelToken::default();
        let mut count = 0u32;
        let start = Instant::now();
        let result = run(
            "sh",
            // 全长约 5 秒的假刻录：每 50 ms 打一行百分比。
            &os(&[
                "-c",
                "for i in $(seq 1 100); do echo \"${i}% done\" >&2; sleep 0.05; done",
            ]),
            &mut |_| {
                count += 1;
                if count == 3 {
                    token.cancel();
                }
            },
            &token,
        );
        let elapsed = start.elapsed();

        assert!(matches!(result, Err(BurnError::Cancelled)), "{result:?}");
        assert!(
            count >= 3,
            "cancel must land after the progress it reacts to"
        );
        // 取消生效必须把 5 秒的脚本砍在两秒以内，证明子进程确实被停掉了。
        assert!(elapsed < Duration::from_secs(2), "elapsed {elapsed:?}");
    }

    #[test]
    fn pvd_info_args_and_volume_id_parse() {
        assert_eq!(
            pvd_info_args("/dev/sr0"),
            os(&["-indev", "/dev/sr0", "-pvd_info"])
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
    fn extract_args_carry_osirrox_and_root() {
        assert_eq!(
            extract_args(Path::new("/tmp/a.iso"), Path::new("/tmp/out")),
            os(&[
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
        let paths = vec!["/子目录/中文文件.txt".to_string(), "/a.bin".to_string()];
        assert_eq!(
            extract_paths_args("/dev/sr0", &paths, Path::new("/tmp/out")),
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
            matches!(&err, BurnError::Failed(msg) if msg.contains("boom")),
            "{err:?}"
        );
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
    fn burn_failure_classification_covers_observed_tails() {
        // 实机样本：写入中拔掉光驱连线后的 libburn 输出。
        let drive_lost = "\
libburn : FAILURE : SCSI command 2Ah yielded host problem: 0x7 SG_ERR_DID_ERROR (Internal error detected in the host adapter)
libburn : FATAL : Lost connection to drive
libburn : SORRY : Drive is already released
";
        assert_eq!(BurnFailure::classify(drive_lost), BurnFailure::DriveLost);
        // 实机样本：盘被系统挂载时尝试独占打开。
        let busy =
            "libburn : SORRY : Cannot open busy device '/dev/sr0' : Device or resource busy\n";
        assert_eq!(BurnFailure::classify(busy), BurnFailure::DeviceBusy);
        assert_eq!(
            BurnFailure::classify("xorriso : aborting : FAILURE"),
            BurnFailure::Other
        );
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
        let differences = crate::compare_trees(Path::new(&source), &temp);
        let _ = std::fs::remove_dir_all(&temp);
        assert_eq!(differences, Vec::<String>::new(), "{differences:?}");
    }
}
