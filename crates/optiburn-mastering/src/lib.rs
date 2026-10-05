//! 镜像层：把一棵目录树做成 Windows 能读的光盘镜像。
//!
//! 介质与文件系统的对应关系（详见 `docs/WINDOWS-COMPAT.md`）：
//!
//! | Profile | ISO 9660 | Joliet | UDF |
//! |---|---|---|---|
//! | [`DiscProfile::Cd`] | Level 2 + 长文件名 | Level 3 | 不用 |
//! | [`DiscProfile::Dvd`] | Level 2 + 长文件名 | Level 3 | 1.02（Bridge） |
//! | [`DiscProfile::Bd`] | Level 2 + 长文件名 | Level 3 | 2.50 |
//!
//! ISO 与 UDF 两个文件系统共用同一份文件数据（UDF Bridge），Windows 优先读 UDF，
//! 老系统读 ISO 9660。上游 hadris-cd 的自带测试持续用两套 reader 回读同一镜像。

use std::path::{Path, PathBuf};

use hadris_cd::{FileTree, JolietLevel, OpticalImageOptions, OpticalImageWriter};
use hadris_udf::UdfRevision;

/// 光盘逻辑扇区大小。
const SECTOR_SIZE: u64 = 2048;

/// 目标介质类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscProfile {
    Cd,
    Dvd,
    Bd,
}

/// 一次镜像制作的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSpec {
    pub profile: DiscProfile,
    /// 卷标，光盘属性里显示的名字。
    pub volume_id: String,
    /// 是否写入 Joliet Level 3（Windows 长文件名与中文名）。
    pub joliet: bool,
}

impl Default for ImageSpec {
    fn default() -> Self {
        Self {
            profile: DiscProfile::Dvd,
            volume_id: "OPTIBURN".to_string(),
            joliet: true,
        }
    }
}

/// 制作结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// 镜像占用的 2048 字节扇区数（由字节数向上取整，写出的镜像本就按扇区对齐）。
    pub sectors: u64,
    /// 镜像字节数。
    pub bytes: u64,
    /// 实际写进镜像的文件系统，按 Windows 的读取优先级排列。
    pub filesystems: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MasteringError {
    #[error("source directory not found: {0}")]
    SourceNotFound(PathBuf),
    #[error("source is not a directory: {0}")]
    SourceNotDirectory(PathBuf),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("image writer failed: {0}")]
    Writer(#[from] hadris_cd::Error),
}

/// 把 `source_dir` 的内容写成一个镜像文件 `output`。
///
/// 目录树由 hadris-cd 的 `FileTree::from_fs` 递归读取（跳过符号链接，按名字排序，
/// 保证同样输入产出字节相同的镜像）。
pub fn build_image(
    source_dir: &Path,
    output: &Path,
    spec: &ImageSpec,
) -> Result<ImageInfo, MasteringError> {
    let metadata = std::fs::metadata(source_dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => MasteringError::SourceNotFound(source_dir.to_path_buf()),
        _ => MasteringError::Io(e),
    })?;
    if !metadata.is_dir() {
        return Err(MasteringError::SourceNotDirectory(source_dir.to_path_buf()));
    }

    let tree = FileTree::from_fs(source_dir)?;
    // 必须以读写方式打开：hadris 写完卷描述符后要回读并就地打补丁，只写句柄会得到 EBADF。
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    OpticalImageWriter::new(file, options_for(spec)).finish(tree)?;

    let bytes = std::fs::metadata(output)?.len();
    Ok(ImageInfo {
        sectors: bytes.div_ceil(SECTOR_SIZE),
        bytes,
        filesystems: filesystems_for(spec),
    })
}

/// 把 profile 与 Joliet 开关翻译成 hadris-cd 的镜像选项。
fn options_for(spec: &ImageSpec) -> OpticalImageOptions {
    let mut options = OpticalImageOptions::default().volume_id(spec.volume_id.clone());
    if spec.joliet {
        options = options.joliet(JolietLevel::Level3);
    } else {
        options.iso.joliet = None;
    }
    match spec.profile {
        DiscProfile::Cd => options.udf.enabled = false,
        DiscProfile::Dvd => {
            options.udf.enabled = true;
            options.udf.revision = UdfRevision::V1_02;
        }
        DiscProfile::Bd => {
            options.udf.enabled = true;
            options.udf.revision = UdfRevision::V2_50;
        }
    }
    options
}

/// 与 [`options_for`] 对应的“实际写进去了什么”，供 CLI 回报。
fn filesystems_for(spec: &ImageSpec) -> Vec<String> {
    let mut filesystems = vec!["ISO 9660".to_string()];
    if spec.joliet {
        filesystems.push("Joliet Level 3".to_string());
    }
    match spec.profile {
        DiscProfile::Cd => {}
        DiscProfile::Dvd => filesystems.push(format!("UDF {}", UdfRevision::V1_02)),
        DiscProfile::Bd => filesystems.push(format!("UDF {}", UdfRevision::V2_50)),
    }
    filesystems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(profile: DiscProfile, joliet: bool) -> ImageSpec {
        ImageSpec {
            profile,
            volume_id: "TEST".to_string(),
            joliet,
        }
    }

    #[test]
    fn profiles_pick_the_documented_filesystems() {
        let cd = options_for(&spec(DiscProfile::Cd, true));
        assert!(!cd.udf.enabled);
        assert_eq!(cd.iso.joliet, Some(JolietLevel::Level3));

        let dvd = options_for(&spec(DiscProfile::Dvd, true));
        assert!(dvd.udf.enabled);
        assert_eq!(dvd.udf.revision, UdfRevision::V1_02);

        let bd = options_for(&spec(DiscProfile::Bd, true));
        assert_eq!(bd.udf.revision, UdfRevision::V2_50);
    }

    #[test]
    fn joliet_can_be_turned_off() {
        assert_eq!(options_for(&spec(DiscProfile::Dvd, false)).iso.joliet, None);
        assert_eq!(
            filesystems_for(&spec(DiscProfile::Dvd, false)),
            vec!["ISO 9660", "UDF 1.02"]
        );
    }

    #[test]
    fn filesystems_report_matches_the_profile() {
        assert_eq!(
            filesystems_for(&spec(DiscProfile::Cd, true)),
            vec!["ISO 9660", "Joliet Level 3"]
        );
        assert_eq!(
            filesystems_for(&spec(DiscProfile::Bd, true)),
            vec!["ISO 9660", "Joliet Level 3", "UDF 2.50"]
        );
    }

    #[test]
    fn missing_source_and_file_source_are_distinguished() {
        let tmp = TempDir::new("errors");
        let out = tmp.path().join("out.iso");

        let missing = tmp.path().join("nope");
        assert!(matches!(
            build_image(&missing, &out, &ImageSpec::default()),
            Err(MasteringError::SourceNotFound(_))
        ));

        let file = tmp.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            build_image(&file, &out, &ImageSpec::default()),
            Err(MasteringError::SourceNotDirectory(_))
        ));
    }

    /// 在镜像里找一段标记，返回字节偏移。
    fn find_marker(image: &[u8], marker: &[u8]) -> Option<usize> {
        image.windows(marker.len()).position(|w| w == marker)
    }

    #[test]
    fn built_image_has_iso_pvd_and_udf_bridge_for_dvd() {
        let tmp = TempDir::new("structure");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("hello.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/note.txt"), b"note").unwrap();
        let out = tmp.path().join("out.iso");

        let info = build_image(&src, &out, &ImageSpec::default()).unwrap();
        assert_eq!(info.bytes % SECTOR_SIZE, 0, "镜像必须按扇区对齐");
        assert_eq!(info.sectors * SECTOR_SIZE, info.bytes);
        assert_eq!(
            info.filesystems,
            vec!["ISO 9660", "Joliet Level 3", "UDF 1.02"]
        );

        let image = std::fs::read(&out).unwrap();
        // ISO 9660 主卷描述符固定在扇区 16，类型字节 1，识别串在第 2–6 字节。
        assert_eq!(image[16 * 2048], 1);
        assert_eq!(&image[16 * 2048 + 1..16 * 2048 + 6], b"CD001");
        // UDF 卷识别序列：BEA01 → NSR02(UDF 1.02) → TEA01。
        let bea = find_marker(&image, b"BEA01").expect("UDF BEA01 缺失");
        let nsr = find_marker(&image, b"NSR02").expect("UDF 1.02 应写 NSR02");
        let tea = find_marker(&image, b"TEA01").expect("UDF TEA01 缺失");
        assert!(bea < nsr && nsr < tea, "UDF 卷识别序列顺序不对");
    }

    #[test]
    fn bd_profile_writes_udf_2_50() {
        let tmp = TempDir::new("bd-structure");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("hello.txt"), b"hello").unwrap();
        let out = tmp.path().join("out.iso");

        let info = build_image(
            &src,
            &out,
            &ImageSpec {
                profile: DiscProfile::Bd,
                volume_id: "BDTEST".to_string(),
                joliet: true,
            },
        )
        .unwrap();
        assert_eq!(
            info.filesystems,
            vec!["ISO 9660", "Joliet Level 3", "UDF 2.50"]
        );

        let image = std::fs::read(&out).unwrap();
        // UDF 2.50 的识别串是 NSR03。
        assert!(
            find_marker(&image, b"NSR03").is_some(),
            "UDF 2.50 应写 NSR03"
        );
    }

    #[test]
    fn cd_profile_leaves_udf_out() {
        let tmp = TempDir::new("cd-no-udf");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("hello.txt"), b"hello").unwrap();
        let out = tmp.path().join("out.iso");

        build_image(
            &src,
            &out,
            &ImageSpec {
                profile: DiscProfile::Cd,
                volume_id: "CDTEST".to_string(),
                joliet: true,
            },
        )
        .unwrap();

        let image = std::fs::read(&out).unwrap();
        assert_eq!(image[16 * 2048], 1);
        assert_eq!(&image[16 * 2048 + 1..16 * 2048 + 6], b"CD001");
        assert_eq!(find_marker(&image, b"BEA01"), None, "CD 不该有 UDF");
        assert!(find_marker(&image, b"CD001").is_some());
    }

    /// 测试用的临时目录：`std::env::temp_dir()` 没有 tempfile 依赖，靠进程号与名字隔离。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("optiburn-mastering-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
