//! optiburn 命令行入口：`build-image`、`burn`、`probe`。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use optiburn_engine::{BurnEngine, BurnJob, XorrisoEngine};
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
        /// 以多区段方式追加
        #[arg(long)]
        multi: bool,
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
            multi,
        } => burn_command(&image, &device, &engine, speed, multi),
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
    multi: bool,
) -> Result<(), String> {
    let job = BurnJob {
        image: image.to_path_buf(),
        device: device.to_string(),
        speed,
        multi,
    };

    match engine {
        name if name == XorrisoEngine.name() => {
            let mut progress = |fraction: f32| eprint!("\r{:>5.1}%", fraction * 100.0);
            let result = XorrisoEngine.burn(&job, &mut progress);
            eprintln!();
            result.map_err(|e| e.to_string())
        }
        other => Err(format!("引擎 {other} 尚未实现")),
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
