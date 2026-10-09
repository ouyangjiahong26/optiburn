//! 读侧与回读工具：读盘上目录树与卷标、抽取到本地，供设备页浏览与回读校验
//! （ADR-0010）。只读访问一律用 `-drive_access shared` 打开，独占冲突不在这里发生。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{BurnError, CancelToken, TAIL_LINES, XORRISO};

use super::xorriso::run;

/// 从 `xorriso -pvd_info` 读盘上最后一个区段的卷标（PVD 的 `Volume Id`），用于
/// GUI 追加页预填（ADR-0010）。
///
/// `-pvd_info` 的关键行出在 stdout：`Volume Id    : <文本>`（不带引号）。解析依赖
/// 英文消息，统一加 `LC_ALL=C`，本机实测该 locale 下非 ASCII 卷标原样输出。
pub fn read_volume_id(device: &str) -> Result<String, BurnError> {
    let (stdout, stderr) = run_output(XORRISO, &pvd_info_args(device))?;
    if has_no_real_volume_id(&stderr) {
        return Err(BurnError::NoIsoSession);
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

/// 读卷标时视为「没有真实卷标」的两种形态：读不出 ISO 9660 的兜底空镜像，或本身
/// 是空白介质（0 区段，xorriso 不读 ISO 头，stderr 报 `Media status : is blank`，
/// stdout 报合成的默认卷标 `ISOIMAGE`，实测 1.5.6）。
///
/// 不并入 [`is_blank_image_fallback`]：末区段门禁要用那个函数放行空盘（首刻），
/// 只有卷标预填需要把空盘一并挡掉。
fn has_no_real_volume_id(stderr: &str) -> bool {
    is_blank_image_fallback(stderr) || stderr.contains("Media status : is blank")
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

/// 组装读卷标的参数表。
///
/// 带 `-drive_access shared`：盘被挂载时独占打开会失败，shared 仍能读到 PVD，
/// 卷标预填与末区段检查因此在挂载期间也可用。
fn pvd_info_args(device: &str) -> Vec<OsString> {
    vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
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

/// 把镜像或设备的目录树抽取到本地目录，供回读校验与复制使用（ADR-0010）。
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
    let (stdout, stderr) = run_output(XORRISO, &list_args(device))?;
    // 盘上没有 ISO 9660 时 xorriso 会兜底造一个空镜像并照常成功，stdout 只列根目录。
    // 这里按 stderr 识破兜底：真空白盘由调用方区分，有内容却读不出 ISO 的盘不能
    // 被显示成空盘（实测，见 ADR-0010）。
    if is_blank_image_fallback(&stderr) {
        return Err(BurnError::NoIsoSession);
    }
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

/// 解析 `lsdl` 的输出：`-rw-r--r-- 1 1000 1000 7797 Oct 9 10:25 '/路径'`。
///
/// 大小取按空白切分的第 5 个字段。路径按 shell 引号拼接解出：名字里的单引号会写成
/// `'"'"'`，名字里的换行原样出现（条目因此可能跨物理行），符号链接行后面还跟着
/// ` -> '目标'`，只取链接自身的路径。根目录 `/` 自身不进列表（实测 xorriso 1.5.6）。
fn parse_lsdl(stdout: &str) -> Vec<DiscEntry> {
    let mut entries = Vec::new();
    let mut cursor = 0;
    while cursor < stdout.len() {
        let line_end = stdout[cursor..]
            .find('\n')
            .map_or(stdout.len(), |offset| cursor + offset);
        let line = &stdout[cursor..line_end];
        let next = (line_end + 1).min(stdout.len());
        if (line.starts_with('d') || line.starts_with('-') || line.starts_with('l'))
            && let Some(quote) = line.find('\'')
            && let Some((path, after)) = parse_quoted_name(stdout, cursor + quote)
        {
            if !path.is_empty() && path != "/" {
                let size = line
                    .split_whitespace()
                    .nth(4)
                    .and_then(|field| field.parse::<u64>().ok())
                    .unwrap_or(0);
                entries.push(DiscEntry {
                    path,
                    size,
                    is_dir: line.starts_with('d'),
                });
            }
            cursor = after;
            continue;
        }
        cursor = next;
    }
    entries
}

/// 从 `text[start..]` 解析 shell 拼接形式的引号名字，返回名字与结束后的字节位置。
///
/// 拼接段相邻无分隔，例如名字 `quote'file.txt` 输出为 `'/quote'"'"'file.txt'`。
fn parse_quoted_name(text: &str, start: usize) -> Option<(String, usize)> {
    if start > text.len() {
        return None;
    }
    let mut name = String::new();
    let mut index = start;
    while let Some(quote) = text[index..].chars().next() {
        if quote != '\'' && quote != '"' {
            break;
        }
        let rest = &text[index + 1..];
        let end = rest.find(quote)?;
        name.push_str(&rest[..end]);
        index += end + 2;
    }
    Some((name, index))
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
    // 路径来自盘片本身，逐条校验并落成本地相对路径后再交给 xorriso。
    let mut targets = Vec::with_capacity(paths.len());
    for path in paths {
        let target = dest.join(safe_relative_path(path)?);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        targets.push((path.clone(), target));
    }
    run(
        XORRISO,
        &extract_paths_args(device, &targets),
        &mut |_| {},
        cancel,
    )
}

/// 校验盘内路径并转成本地相对路径：只接受常规段，拒绝 `..`、盘符前缀与 Windows
/// 的分隔符。
///
/// 盘片内容不能当可信输入：Windows 上 `C:/x` 这类带盘符的路径会让 `Path::join`
/// 直接替换掉暂存目标，把文件抽到暂存目录之外（实测 xorriso 能写出这样的镜像）。
fn safe_relative_path(path: &str) -> Result<PathBuf, BurnError> {
    let mut relative = PathBuf::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains('\\') || segment.contains(':') {
            return Err(BurnError::UnsafePath(path.to_string()));
        }
        relative.push(segment);
    }
    if relative.as_os_str().is_empty() {
        return Err(BurnError::UnsafePath(path.to_string()));
    }
    Ok(relative)
}

/// 组装多路径抽取的参数表：一次加载盘片，按序执行每条 `-extract`。
fn extract_paths_args(device: &str, targets: &[(String, PathBuf)]) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-drive_access"),
        OsString::from("shared"),
        OsString::from("-indev"),
        OsString::from(device),
        OsString::from("-osirrox"),
        OsString::from("on"),
    ];
    for (path, target) in targets {
        args.push(OsString::from("-extract"));
        args.push(OsString::from(path));
        args.push(target.as_os_str().to_os_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 期望参数表：与实现同样用 `OsString`，避免为断言再把路径窄化回 `String`。
    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn pvd_info_args_and_volume_id_parse() {
        assert_eq!(
            pvd_info_args("/dev/sr0"),
            os(&["-drive_access", "shared", "-indev", "/dev/sr0", "-pvd_info"])
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
    fn parse_lsdl_takes_only_the_link_path_for_symlinks() {
        // 实测于 xorriso 1.5.6：符号链接的目标也带引号，路径只能取第一对引号。
        let stdout = "lrwxrwxrwx    1 1000     1000            0 Oct  9 13:26 '/link' -> 'sub'\n";
        assert_eq!(
            parse_lsdl(stdout),
            vec![DiscEntry {
                path: "/link".to_string(),
                size: 0,
                is_dir: false,
            }]
        );
    }

    #[test]
    fn parse_lsdl_unescapes_quotes_and_keeps_multiline_names() {
        // 实测：名字里的单引号按 shell 约定写成 '"'"' 拼接。
        let quoted =
            "-rw-rw-r--    1 1000     1000            6 Oct  9 13:47 '/quote'\"'\"'file.txt'\n";
        assert_eq!(
            parse_lsdl(quoted),
            vec![DiscEntry {
                path: "/quote'file.txt".to_string(),
                size: 6,
                is_dir: false,
            }]
        );
        // 名字里含真换行时条目跨物理行，不能被丢掉，也不能把下一行当新条目。
        let multiline = "-rw-rw-r--    1 1000     1000            3 Oct  9 13:47 '/a\nb.txt'\n-rw-rw-r--    1 1000     1000            1 Oct  9 13:47 '/z.txt'\n";
        assert_eq!(
            parse_lsdl(multiline),
            vec![
                DiscEntry {
                    path: "/a\nb.txt".to_string(),
                    size: 3,
                    is_dir: false,
                },
                DiscEntry {
                    path: "/z.txt".to_string(),
                    size: 1,
                    is_dir: false,
                },
            ]
        );
    }

    #[test]
    fn safe_relative_path_rejects_escapes() {
        assert_eq!(
            safe_relative_path("/子目录/中文文件.txt").unwrap(),
            PathBuf::from("子目录/中文文件.txt")
        );
        // Windows 盘符前缀、`..` 与反斜杠分隔符都拒绝。盘内路径来自盘片，不能信。
        for bad in ["/C:/escaped", "/../etc/passwd", "/a\\b", "/", ""] {
            assert!(
                matches!(safe_relative_path(bad), Err(BurnError::UnsafePath(_))),
                "{bad}"
            );
        }
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
        let targets = vec![
            (
                "/子目录/中文文件.txt".to_string(),
                PathBuf::from("/tmp/out/子目录/中文文件.txt"),
            ),
            ("/a.bin".to_string(), PathBuf::from("/tmp/out/a.bin")),
        ];
        assert_eq!(
            extract_paths_args("/dev/sr0", &targets),
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
    fn blank_media_reports_no_real_volume_id() {
        // 实测空白介质（0 区段）：没有兜底行，stderr 报 is blank，stdout 是合成的
        // ISOIMAGE。门禁不能跟着拒空盘（首刻要放行），所以这层判断只用在读卷标。
        let blank = "Media current: DVD-R sequential recording\nMedia status : is blank\n";
        assert!(!is_blank_image_fallback(blank));
        assert!(has_no_real_volume_id(blank));
        assert!(has_no_real_volume_id(
            "libisoburn: WARNING : Creating blank image.\n"
        ));
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
