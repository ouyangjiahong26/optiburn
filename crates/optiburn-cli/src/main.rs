//! optiburn 命令行入口：`build-image`、`burn`、`append`、`probe`。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use optiburn_engine::{
    BurnEngine, BurnError, BurnFailure, BurnJob, CancelToken, GrowJob, NativeEngine, XorrisoEngine,
    grow, grow_print_size, last_session_is_iso,
};
use optiburn_mastering::{DiscProfile, ImageSpec, build_image};
use optiburn_mmc::{DiscStatus, MmcDevice, MmcError, WriteBlock};

#[derive(Parser)]
#[command(
    name = "optiburn",
    version,
    about = "跨平台光盘刻录工具（ISO 9660 / Joliet / UDF Bridge）"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 把目录做成光盘镜像（ISO 9660 + Joliet + UDF Bridge）
    BuildImage {
        /// 源目录
        src: PathBuf,
        /// 输出镜像路径，默认 `<源目录名>.iso`
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 目标介质：决定写哪些文件系统
        #[arg(long, value_enum, default_value_t = ProfileArg::Dvd)]
        profile: ProfileArg,
        /// 卷标
        #[arg(long, default_value = "OPTIBURN")]
        volume_id: String,
    },
    /// 把镜像写到盘上
    Burn {
        /// 待写入的镜像
        image: PathBuf,
        /// 目标设备，例如 /dev/sr0 或 E:
        #[arg(long)]
        device: String,
        /// 刻录引擎：xorriso（子进程，Linux 默认）或 native（原生 MMC，Windows 唯一可用的）
        #[arg(long, default_value = "xorriso")]
        engine: String,
        /// 写入倍速，缺省交给驱动器自选
        #[arg(long)]
        speed: Option<u32>,
        /// 写完封盘。默认不封口，保留继续追加区段的能力
        #[arg(long)]
        close_disc: bool,
    },
    /// 把目录追加到盘上，与已有区段合并（旧文件保持可见）
    Append {
        /// 源目录，其内容成为盘上根目录
        src: PathBuf,
        /// 目标设备，例如 /dev/sr0 或 E:
        #[arg(long)]
        device: String,
        /// 新区段的卷标
        #[arg(long, default_value = "OPTIBURN")]
        volume_id: String,
        /// 写入倍速，缺省交给驱动器自选
        #[arg(long)]
        speed: Option<u32>,
        /// 写完封盘。默认保持可追加
        #[arg(long)]
        close_disc: bool,
    },
    /// 列出光驱与其中的盘片状态
    Probe,
}

#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    Cd,
    Dvd,
    Bd,
}

impl From<ProfileArg> for DiscProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Cd => Self::Cd,
            ProfileArg::Dvd => Self::Dvd,
            ProfileArg::Bd => Self::Bd,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Command::BuildImage {
            src,
            output,
            profile,
            volume_id,
        } => build_image_command(&src, output.as_deref(), profile.into(), &volume_id),
        Command::Burn {
            image,
            device,
            engine,
            speed,
            close_disc,
        } => burn_command(&image, &device, &engine, speed, close_disc),
        Command::Append {
            src,
            device,
            volume_id,
            speed,
            close_disc,
        } => append_command(&src, &device, &volume_id, speed, close_disc),
        Command::Probe => probe_command(),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("错误：{message}");
            ExitCode::FAILURE
        }
    }
}

fn build_image_command(
    src: &Path,
    output: Option<&Path>,
    profile: DiscProfile,
    volume_id: &str,
) -> Result<(), String> {
    let output = match output {
        Some(path) => path.to_path_buf(),
        None => default_output(src),
    };
    let spec = ImageSpec {
        profile,
        volume_id: volume_id.to_string(),
        joliet: true,
    };

    let info = build_image(src, &output, &spec).map_err(|e| e.to_string())?;
    println!("镜像: {}", output.display());
    println!("  扇区: {}", info.sectors);
    println!("  字节: {}", info.bytes);
    println!("  文件系统: {}", info.filesystems.join(", "));
    Ok(())
}

/// 写盘失败的中文文案：归类走引擎里的共用文案，缺工具与原生引擎的缺口走各自的分支。
fn burn_error_text(error: &BurnError) -> String {
    if let BurnError::Failed(tail) = error {
        eprintln!("optiburn: 刻录失败原始输出：{tail}");
    }
    match error {
        BurnError::Failed(tail) => BurnFailure::classify(tail).user_text(tail),
        // 不给用户看引擎里的英文串（“missing tool: …”），安装途径见共用文案。
        BurnError::MissingTool(tool) => BurnError::missing_tool_user_text(tool),
        BurnError::Mmc(MmcError::NotReady) => {
            "盘未就绪：请确认已放入可写盘片且仓门已关闭。".to_string()
        }
        BurnError::Mmc(other) => format!("设备命令失败：{other}"),
        BurnError::NativeGap(gap) => native_gap_text(gap).to_string(),
        other => other.to_string(),
    }
}

/// 原生引擎的能力缺口文案（中文）。GUI 的英文镜像在 src-tauri 的同名函数里。
fn native_gap_text(gap: &optiburn_engine::NativeGap) -> &'static str {
    use optiburn_engine::NativeGap;
    match gap {
        NativeGap::WriteSpeed => {
            "原生引擎暂不支持指定倍速：去掉 --speed，Linux 上也可以改用 --engine xorriso。"
        }
        NativeGap::EmptyImage => "镜像为空文件，没有可刻录的内容。",
        NativeGap::ImageTooLarge => "镜像超出介质容量，请换更大的盘。",
        NativeGap::ImageBeyondAddressRange => "镜像超出 2048 字节块的地址上限。",
        NativeGap::UnsupportedProfile(_) => {
            "这种介质暂不支持原生引擎，Linux 上可改用 --engine xorriso。"
        }
        NativeGap::FinalizedDisc => "盘已封口，无法再写入，请更换盘片。",
    }
}

/// 读侧引擎错误的文案：缺工具走共用文案（安装途径在里面，别让英文串漏给用户），
/// 其余保留动作前缀。
fn read_error_text(action: &str, error: &BurnError) -> String {
    match error {
        BurnError::MissingTool(tool) => BurnError::missing_tool_user_text(tool),
        other => format!("{action}失败：{other}"),
    }
}

fn burn_command(
    image: &Path,
    device: &str,
    engine: &str,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<(), String> {
    let burner: Box<dyn BurnEngine> = match engine {
        "native" => Box::new(NativeEngine),
        "xorriso" => Box::new(XorrisoEngine),
        other => return Err(format!("引擎 {other} 尚未实现")),
    };
    ensure_burnable(device, false)?;
    let image_bytes = std::fs::metadata(image)
        .map_err(|e| format!("读取镜像 {} 大小失败：{e}", image.display()))?
        .len();
    ensure_fits(device, image_bytes)?;
    let job = BurnJob {
        image: image.to_path_buf(),
        device: device.to_string(),
        speed,
        // 默认多区段：盘不封口，之后还能 append。只有显式 --close-disc 才封盘。
        multi: !close_disc,
    };

    let mut progress = |fraction: f32| eprint!("\r{:>5.1}%", fraction * 100.0);
    // CLI 不提供取消入口，传一个永不置位的默认令牌。
    let result = burner.burn(&job, &mut progress, &CancelToken::default());
    eprintln!();
    result.map_err(|e| burn_error_text(&e))
}

fn append_command(
    src: &Path,
    device: &str,
    volume_id: &str,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<(), String> {
    ensure_burnable(device, true)?;
    let job = GrowJob {
        src: src.to_path_buf(),
        device: device.to_string(),
        speed,
        volume_id: volume_id.to_string(),
        close_disc,
    };
    let needed = grow_print_size(&job).map_err(|e| format!("计算追加数据量失败：{e}"))?;
    ensure_fits(device, needed)?;

    println!("追加 {} 到 {}。", src.display(), device);
    let mut progress = |fraction: f32| eprint!("\r{:>5.1}%", fraction * 100.0);
    let result = grow(&job, &mut progress, &CancelToken::default());
    eprintln!();
    result.map_err(|e| burn_error_text(&e))
}

/// 刻录前把盘片状态查清楚：挂载占用与盘片状态都在这里拦，等就绪、按状态放行或拒绝。
///
/// 镜像路径（`accept_appendable = false`）不接受可追加盘：单区段镜像不带前面
/// 区段的目录树，写下去会把旧文件遮住。追加必须走 `append` 的增长模式。
fn ensure_burnable(device: &str, accept_appendable: bool) -> Result<(), String> {
    // 挂载中的盘写不进去（引擎要独占打开设备），先拦下来并给出卸载指引。
    if let Some(point) = optiburn_transport::mounted_at(device) {
        return Err(format!(
            "光盘已被系统挂载在 {}：先卸载（例如 udisksctl unmount -b {device}）再写入。",
            point.display()
        ));
    }
    let transport =
        optiburn_transport::open(device).map_err(|e| format!("打开 {device} 失败：{e}"))?;
    let mut mmc = MmcDevice::new(transport);
    optiburn_mmc::wait_until_ready(&mut mmc).map_err(|e| match e {
        MmcError::NotReady => "盘未就绪：请确认已放入可写盘片且仓门已关闭。".to_string(),
        // 传输层故障等其它错误不该被“未就绪”文案吞掉。
        other => format!("等待盘片就绪失败：{other}"),
    })?;
    let info = mmc
        .read_disc_information()
        .map_err(|e| format!("读取盘片信息失败：{e}"))?;
    // 查完立刻释放句柄：刻录引擎随后要以独占方式打开设备。
    drop(mmc);

    optiburn_mmc::approve_write(&info, accept_appendable).map_err(|e| match e {
        WriteBlock::NeedGrowMode => {
            "盘上已有数据区段：以镜像方式追加会把已有文件遮住。请改用 optiburn append <目录>。"
                .to_string()
        }
        WriteBlock::Finalized => "盘已封口，无法再写入，请更换盘片。".to_string(),
    })?;
    // 追加路径再过一道：末区段不是 ISO 9660 的盘（例如 Windows 的 UDF 盘）不能续写，
    // 追加 ISO 区段会改变盘在按最后一区段挂载的系统里的可见内容（ADR-0010）。
    if accept_appendable && info.status == DiscStatus::Appendable {
        let iso_readable =
            last_session_is_iso(device).map_err(|e| read_error_text("读取末区段格式", &e))?;
        if !iso_readable {
            return Err(
                "盘上最后的区段不是 ISO 9660（例如 Windows 写入的 UDF 盘）：追加 ISO 区段后，按最后一区段挂载的系统将只看到新内容。本工具暂不支持续写这类盘，请换用空白盘重刻。"
                    .to_string(),
            );
        }
    }
    // 门禁放行后，可追加盘只剩追加路径，把区段数报给用户留个底。
    if info.status == DiscStatus::Appendable {
        println!("盘上已有 {} 个区段，将追加新区段。", info.sessions);
    }
    Ok(())
}

/// 写前容量门禁：待写入量加区段开销超过可用容量就拒绝，避免写到一半废一张盘
/// （ADR-0019）。区段开销余量共用 [`optiburn_engine::SESSION_OVERHEAD`]。
///
/// 容量口径来自 READ FORMAT CAPACITIES（总容量减已写入）。读不到口径（典型是
/// CD 介质与不回该命令的驱动器）时打印提示后放行：门禁是尽力而为的预检，不该
/// 成为新的故障点，真放不下由引擎写入失败兜底。
fn ensure_fits(device: &str, needed_bytes: u64) -> Result<(), String> {
    let transport =
        optiburn_transport::open(device).map_err(|e| format!("打开 {device} 失败：{e}"))?;
    let mut mmc = MmcDevice::new(transport);
    let capacity = mmc.read_format_capacities();
    // 查完立刻释放句柄：刻录引擎随后要以独占方式打开设备。
    drop(mmc);
    let capacity = match capacity {
        Ok(capacity) => capacity,
        Err(e) => {
            println!("未能读取盘片容量（{e}），跳过容量检查。");
            return Ok(());
        }
    };
    let Some(free) = capacity
        .total
        .zip(capacity.used)
        .map(|(total, used)| total - used)
    else {
        println!("盘片未报出容量口径（CD 介质常见），跳过容量检查。");
        return Ok(());
    };
    if needed_bytes + optiburn_engine::SESSION_OVERHEAD > free {
        return Err(format!(
            "这张盘放不下本次写入：待写入约 {}，盘上可用容量约 {}。请减少待写内容或更换盘片。",
            bytes_text(needed_bytes),
            bytes_text(free),
        ));
    }
    Ok(())
}

/// 无 `-o` 时把源目录名当作镜像名，输出到当前目录。
fn default_output(src: &Path) -> PathBuf {
    let stem = src
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "optiburn".to_string());
    PathBuf::from(format!("{stem}.iso"))
}

fn disc_status_text(status: DiscStatus) -> String {
    match status {
        DiscStatus::Empty => "空盘".to_string(),
        DiscStatus::Appendable => "可追加".to_string(),
        DiscStatus::Finalized => "已封口".to_string(),
        DiscStatus::Other(bits) => format!("随机可写（{bits}）"),
    }
}

/// 面向人的容量数字：按 GB/MB/KB 取一位小数，数值与单位之间留一个空格，
/// 与 GUI 的展示口径一致。
fn bytes_text(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// 盘片容量段：总容量与可用容量，读不到口径（CD 介质常见）时为空串。
fn capacity_text(capacity: &optiburn_mmc::DiscCapacity) -> String {
    capacity
        .total
        .zip(capacity.used)
        .map(|(total, used)| {
            format!(
                "，总容量 {}，可用 {}",
                bytes_text(total),
                bytes_text(total - used)
            )
        })
        .unwrap_or_default()
}

fn probe_command() -> Result<(), String> {
    let devices = optiburn_transport::list_optical_devices();
    if devices.is_empty() {
        println!("未发现光驱");
        return Ok(());
    }

    for text in devices {
        let transport = match optiburn_transport::open(&text) {
            Ok(transport) => transport,
            Err(e) => {
                println!("{text} | 打开失败：{e}");
                continue;
            }
        };
        let mut device = MmcDevice::new(transport);
        let identity = match device.inquiry() {
            Ok(inquiry) => format!(
                "{} {} {}",
                inquiry.vendor, inquiry.product, inquiry.revision
            ),
            Err(e) => format!("INQUIRY 失败：{e}"),
        };
        let disc = match device.read_disc_information() {
            Ok(information) => format!(
                "{}，{} 个区段",
                disc_status_text(information.status),
                information.sessions
            ),
            Err(e) => format!("读盘片信息失败：{e}"),
        };
        // 容量是概览字段，读不到就整段省略，不与上面的错误口径混在一起。
        let capacity = device
            .read_format_capacities()
            .map(|c| capacity_text(&c))
            .unwrap_or_default();
        println!("{text} | {identity} | {disc}{capacity}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_uses_the_source_directory_name() {
        assert_eq!(default_output(Path::new("docs")), PathBuf::from("docs.iso"));
        assert_eq!(
            default_output(Path::new("/tmp/photo set")),
            PathBuf::from("photo set.iso")
        );
        assert_eq!(
            default_output(Path::new("/")),
            PathBuf::from("optiburn.iso")
        );
    }

    #[test]
    fn unimplemented_engines_are_rejected_by_name() {
        let err = burn_command(Path::new("/tmp/x.iso"), "/dev/sr0", "cdrdao", None, false)
            .expect_err("only xorriso exists in v0");
        assert_eq!(err, "引擎 cdrdao 尚未实现");
    }

    #[test]
    fn missing_tool_failure_is_chinese_and_does_not_leak_the_engine_string() {
        let text = burn_error_text(&BurnError::MissingTool("xorriso".into()));
        assert_eq!(text, BurnError::missing_tool_user_text("xorriso"));
        assert!(!text.contains("missing tool"), "{text}");
    }

    #[test]
    fn append_preflight_maps_missing_tool_to_the_shared_text() {
        let text = read_error_text("读取末区段格式", &BurnError::MissingTool("xorriso".into()));
        assert_eq!(text, BurnError::missing_tool_user_text("xorriso"));
        assert!(!text.contains("missing tool"), "{text}");
        assert_eq!(
            read_error_text("读取末区段格式", &BurnError::ReadFailed("boom".into())),
            "读取末区段格式失败：boom"
        );
    }

    #[test]
    fn native_engine_names_and_gap_texts_are_pinned() {
        assert_eq!(NativeEngine.name(), "native");
        assert_eq!(XorrisoEngine.name(), "xorriso");
        let text = burn_error_text(&BurnError::NativeGap(
            optiburn_engine::NativeGap::WriteSpeed,
        ));
        assert!(text.contains("倍速"), "{text}");
        let text = burn_error_text(&BurnError::Mmc(MmcError::NotReady));
        assert!(text.contains("盘未就绪"), "{text}");
    }

    #[test]
    fn bytes_text_formats_human_units() {
        assert_eq!(bytes_text(512), "0.5 KB");
        assert_eq!(bytes_text(150 * 1024 * 1024), "150.0 MB");
        // DVD+R 4.7 GB 盘的厂商口径（2295104 块 × 2048 字节）。
        assert_eq!(bytes_text(2295104 * 2048), "4.4 GB");
    }

    #[test]
    fn capacity_text_omits_when_the_drive_reports_nothing() {
        use optiburn_mmc::DiscCapacity;
        assert_eq!(
            capacity_text(&DiscCapacity {
                total: Some(2295104 * 2048),
                used: Some(100000 * 2048),
            }),
            format!(
                "，总容量 {}，可用 {}",
                bytes_text(2295104 * 2048),
                bytes_text(2195104 * 2048)
            )
        );
        // 任一口径缺失（CD 形态）整段省略。
        assert_eq!(
            capacity_text(&DiscCapacity {
                total: None,
                used: None,
            }),
            ""
        );
    }
}
