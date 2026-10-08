//! optiburn 命令行入口：`build-image`、`burn`、`append`、`probe`。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use optiburn_engine::{BurnEngine, BurnJob, GrowJob, XorrisoEngine, grow};
use optiburn_mastering::{DiscProfile, ImageSpec, build_image};
use optiburn_mmc::{DiscStatus, MmcDevice};

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
        /// 刻录引擎
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
        } => append_command(&src, &device, &volume_id, speed),
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

fn burn_command(
    image: &Path,
    device: &str,
    engine: &str,
    speed: Option<u32>,
    close_disc: bool,
) -> Result<(), String> {
    if engine != XorrisoEngine.name() {
        return Err(format!("引擎 {engine} 尚未实现"));
    }
    ensure_burnable(device, false)?;
    let job = BurnJob {
        image: image.to_path_buf(),
        device: device.to_string(),
        speed,
        // 默认多区段：盘不封口，之后还能 append。只有显式 --close-disc 才封盘。
        multi: !close_disc,
    };

    let mut progress = |fraction: f32| eprint!("\r{:>5.1}%", fraction * 100.0);
    let result = XorrisoEngine.burn(&job, &mut progress);
    eprintln!();
    result.map_err(|e| e.to_string())
}

fn append_command(
    src: &Path,
    device: &str,
    volume_id: &str,
    speed: Option<u32>,
) -> Result<(), String> {
    ensure_burnable(device, true)?;
    let job = GrowJob {
        src: src.to_path_buf(),
        device: device.to_string(),
        speed,
        volume_id: volume_id.to_string(),
    };

    println!("追加 {} 到 {}。", src.display(), device);
    let mut progress = |fraction: f32| eprint!("\r{:>5.1}%", fraction * 100.0);
    let result = grow(&job, &mut progress);
    eprintln!();
    result.map_err(|e| e.to_string())
}

/// 刻录前把盘片状态查清楚：等就绪、按状态放行或拒绝。
///
/// 镜像路径（`accept_appendable = false`）不接受可追加盘：单区段镜像不带前面
/// 区段的目录树，写下去会把旧文件遮住；追加必须走 `append` 的增长模式。
fn ensure_burnable(device: &str, accept_appendable: bool) -> Result<(), String> {
    let transport =
        optiburn_transport::open(device).map_err(|e| format!("打开 {device} 失败：{e}"))?;
    let mut mmc = MmcDevice::new(transport);
    wait_until_ready(&mut mmc)?;
    let info = mmc
        .read_disc_information()
        .map_err(|e| format!("读取盘片信息失败：{e}"))?;
    // 查完立刻释放句柄：刻录引擎随后要以独占方式打开设备。
    drop(mmc);

    match info.status {
        DiscStatus::Empty => Ok(()),
        DiscStatus::Appendable if accept_appendable => {
            println!("盘上已有 {} 个区段，将追加新区段。", info.sessions);
            Ok(())
        }
        DiscStatus::Appendable => Err(
            "盘上已有数据区段：以镜像方式追加会把已有文件遮住。请改用 optiburn append <目录>。"
                .to_string(),
        ),
        DiscStatus::Finalized => Err("盘已封口，无法再写入，请更换盘片。".to_string()),
        DiscStatus::Other(bits) => Err(format!("盘片状态未知（状态位 {bits}），拒绝写入。")),
    }
}

/// 等介质就绪：盘片上电与识别要几秒，这期间 TEST UNIT READY 报错。
///
/// 20 秒内每 500 ms 重试一次；超时才把决定权交还给人。
fn wait_until_ready(mmc: &mut MmcDevice) -> Result<(), String> {
    const DEADLINE: Duration = Duration::from_secs(20);
    const INTERVAL: Duration = Duration::from_millis(500);
    let start = Instant::now();
    loop {
        if mmc.test_unit_ready().is_ok() {
            return Ok(());
        }
        if start.elapsed() >= DEADLINE {
            return Err("盘未就绪：请确认已放入可写盘片且仓门已关闭。".to_string());
        }
        sleep(INTERVAL);
    }
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
        DiscStatus::Other(bits) => format!("其它（{bits}）"),
    }
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
        println!("{text} | {identity} | {disc}");
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
}
