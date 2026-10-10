use super::*;
use optiburn_mastering::{DiscProfile, ImageSpec, build_image};

/// 内存块源：给手造的区段布局做测试。
struct MemoryBlocks(Vec<u8>);

impl BlockSource for MemoryBlocks {
    fn read_blocks_at(&mut self, lba: u32, out: &mut [u8]) -> Result<(), BurnError> {
        let start = lba as usize * SECTOR_BYTES;
        let end = start + out.len();
        let Some(data) = self.0.get(start..end) else {
            return Err(BurnError::ReadFailed(format!(
                "测试盘只有 {} 块，却要读第 {lba} 块",
                self.0.len() / SECTOR_BYTES
            )));
        };
        out.copy_from_slice(data);
        Ok(())
    }
}

/// 临时目录守卫：建在系统临时目录，析构时删除。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("optiburn-disc-read-{tag}-{}", std::process::id()));
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

/// 用 mastering 建一张带中文名的真镜像（Joliet 开），返回镜像路径。
fn fixture_image(tmp: &TempDir) -> PathBuf {
    let src = tmp.path().join("src");
    let sub = src.join("子目录");
    std::fs::create_dir_all(&sub).expect("create source tree");
    std::fs::write(src.join("a.md"), b"alpha notes").expect("write a.md");
    std::fs::write(sub.join("中文文件.txt"), "中文内容".as_bytes()).expect("write zh file");
    let bin: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("随机数据.bin"), &bin).expect("write bin");
    let iso = tmp.path().join("out.iso");
    build_image(
        &src,
        &iso,
        &ImageSpec {
            profile: DiscProfile::Dvd,
            volume_id: "ROUNDTRIP".to_string(),
            joliet: true,
        },
    )
    .expect("build image");
    iso
}

#[test]
fn roundtrip_volume_id_list_and_extract() {
    let tmp = TempDir::new("roundtrip");
    let iso = fixture_image(&tmp);
    let iso_text = iso.to_str().expect("utf-8 temp path").to_string();
    let backend = NativeRead;

    assert!(backend.last_session_is_iso(&iso_text).unwrap());
    assert_eq!(backend.read_volume_id(&iso_text).unwrap(), "ROUNDTRIP");

    // 列举：中文名原样、目录 size 0、文件 size 是内容长度。排序后整表比对，
    // 顺序本身不在契约里（前端按路径算层级）。
    let mut listed = backend.list_tree(&iso_text).expect("list");
    listed.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        listed,
        vec![
            DiscEntry {
                path: "/a.md".into(),
                size: 11,
                is_dir: false,
            },
            DiscEntry {
                path: "/子目录".into(),
                size: 0,
                is_dir: true,
            },
            DiscEntry {
                path: "/子目录/中文文件.txt".into(),
                size: "中文内容".len() as u64,
                is_dir: false,
            },
            DiscEntry {
                path: "/随机数据.bin".into(),
                size: 5000,
                is_dir: false,
            },
        ]
    );

    // 整树抽取，逐字节比对。
    let tree_out = tmp.path().join("tree-out");
    backend
        .extract_tree(&iso, &tree_out, &CancelToken::default())
        .expect("extract tree");
    assert_eq!(
        std::fs::read(tree_out.join("a.md")).unwrap(),
        b"alpha notes"
    );
    assert_eq!(
        std::fs::read(tree_out.join("子目录/中文文件.txt")).unwrap(),
        "中文内容".as_bytes()
    );
    let expected_bin: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(
        std::fs::read(tree_out.join("随机数据.bin")).unwrap(),
        expected_bin
    );

    // 按路径抽取：一个文件加一个目录。
    let paths_out = tmp.path().join("paths-out");
    backend
        .extract_paths(
            &iso_text,
            &["/a.md".to_string(), "/子目录".to_string()],
            &paths_out,
            &CancelToken::default(),
        )
        .expect("extract paths");
    assert_eq!(
        std::fs::read(paths_out.join("a.md")).unwrap(),
        b"alpha notes"
    );
    assert_eq!(
        std::fs::read(paths_out.join("子目录/中文文件.txt")).unwrap(),
        "中文内容".as_bytes()
    );
    assert!(!paths_out.join("随机数据.bin").exists(), "没要的不抽");

    // 盘上没有的路径与不安全的路径分开报。
    assert!(matches!(
        backend.extract_paths(
            &iso_text,
            &["/没有这个文件".to_string()],
            &paths_out,
            &CancelToken::default()
        ),
        Err(BurnError::ReadFailed(_))
    ));
    assert!(matches!(
        backend.extract_paths(
            &iso_text,
            &["/../逃逸".to_string()],
            &paths_out,
            &CancelToken::default()
        ),
        Err(BurnError::UnsafePath(_))
    ));
}

#[test]
fn cancelled_extract_tree_stops_before_any_file() {
    let tmp = TempDir::new("cancel");
    let iso = fixture_image(&tmp);
    let token = CancelToken::new();
    token.cancel();

    let out = tmp.path().join("out");
    assert!(matches!(
        NativeRead.extract_tree(&iso, &out, &token),
        Err(BurnError::Cancelled)
    ));
}

#[test]
fn non_iso_sources_report_no_session() {
    let token = CancelToken::default();
    // 垃圾缓冲：描述符区没有 CD001。
    let garbage = MemoryBlocks(vec![0x41u8; 40 * SECTOR_BYTES]);
    assert!(matches!(
        open_session_view(garbage, 0, u64::MAX, &token),
        Err(BurnError::NoIsoSession)
    ));
    // 缓冲短得放不下描述符区：读取越过源末尾，按读失败上报（真实空白盘到不了
    // 这里，READ TOC 先给出“没有区段”）。
    let short = MemoryBlocks(vec![0u8; 10 * SECTOR_BYTES]);
    assert!(matches!(
        open_session_view(short, 0, u64::MAX, &token),
        Err(BurnError::ReadFailed(_))
    ));
    // 临时文件版（走文件源分支）对 last_session_is_iso 报“不是”。
    let tmp = TempDir::new("noniso");
    let junk = tmp.path().join("junk.iso");
    std::fs::write(&junk, vec![0x55u8; SECTOR_BYTES * 3]).expect("write junk");
    assert!(
        !NativeRead
            .last_session_is_iso(junk.to_str().unwrap())
            .unwrap()
    );
}

/// 手造一张“xorriso 增长模式”形状的区段：extent 全是盘级绝对地址、描述符区
/// 在区段起点加 16。PVD 卷标留空、SVD 带 Joliet 转义序列与中文卷标，一并
/// 覆盖卷标回退。目录记录里的名字按 Joliet 的 UTF-16BE 写（单棵目录树，
/// 与 Joliet 命名空间共用）。
#[test]
fn absolute_session_with_joliet_volume_fallback_reads() {
    const BASE: u32 = 100;
    // 逻辑块：16 PVD、17 SVD、18 终止符、19 根目录、20 文件数据。vss 按实测
    // 口径写区段相对大小（xorriso 增长区段也是这么写的），即 21。
    let root_extent = BASE + 19;
    let file_extent = BASE + 20;
    let session_blocks = 21u32;
    let mut disc = vec![0u8; (BASE + session_blocks) as usize * SECTOR_BYTES];

    let mut pvd = [0u8; SECTOR_BYTES];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    write_u32_both(&mut pvd[80..88], u64::from(session_blocks));
    write_u16_both(&mut pvd[120..124], 1);
    write_u16_both(&mut pvd[124..128], 1);
    write_u16_both(&mut pvd[128..132], 2048);
    write_u32_both(&mut pvd[132..140], 0);
    write_root_record(&mut pvd[156..190], root_extent);
    copy_block(&mut disc, BASE + 16, &pvd);

    let mut svd = [0u8; SECTOR_BYTES];
    svd[0] = 2;
    svd[1..6].copy_from_slice(b"CD001");
    svd[6] = 1;
    let name: Vec<u8> = "中文卷标"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    svd[40..40 + name.len()].copy_from_slice(&name);
    // hadris 的 SVD 字段顺序（repr(C) 逐字节排）：头 7、flags 1、系统标识 32、
    // 卷标识 32、保留 8，之后卷空间（80）与转义序列（88）。
    write_u32_both(&mut svd[80..88], u64::from(session_blocks));
    svd[88..91].copy_from_slice(b"%/E");
    write_root_record(&mut svd[156..190], root_extent);
    copy_block(&mut disc, BASE + 17, &svd);

    let mut term = [0u8; SECTOR_BYTES];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    copy_block(&mut disc, BASE + 18, &term);

    let mut root = [0u8; SECTOR_BYTES];
    write_dir_record(&mut root[0..], root_extent, 2048, 0x02, &[0]);
    write_dir_record(&mut root[34..], root_extent, 2048, 0x02, &[1]);
    let file_name: Vec<u8> = "FILE.TXT;1"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    write_dir_record(&mut root[68..], file_extent, 15, 0x00, &file_name);
    copy_block(&mut disc, BASE + 19, &root);

    let payload = b"hello absolute!";
    disc[file_extent as usize * SECTOR_BYTES..file_extent as usize * SECTOR_BYTES + payload.len()]
        .copy_from_slice(payload);

    // 相对候选的读取（BASE + root_extent）越过测试盘末尾，探测要软失败并落回
    // 绝对候选，这条路径也在本用例覆盖。
    let token = CancelToken::default();
    let iso = open_session_view(MemoryBlocks(disc), BASE, u64::MAX, &token)
        .expect("open the absolute session");
    assert_eq!(volume_id(&iso, &token).unwrap(), "中文卷标");

    // 列举与抽取直接走内部函数（list_tree 走设备路径，测试机上没有光驱）。
    let image_root = iso.root_dir();
    let joliet = is_joliet_root(&image_root.entry_type());
    let mut listed = Vec::new();
    walk_list(&iso, image_root.dir_ref(), "", joliet, &mut listed, &token)
        .expect("walk the absolute session");
    assert_eq!(
        listed,
        vec![DiscEntry {
            path: "/FILE.TXT".into(),
            size: payload.len() as u64,
            is_dir: false,
        }]
    );

    let out = TempDir::new("absolute");
    let target = out.path().join("extract");
    extract_dir(&iso, image_root.dir_ref(), joliet, &target, &token)
        .expect("extract the absolute session");
    assert_eq!(std::fs::read(target.join("FILE.TXT")).unwrap(), payload);
}

#[test]
fn absolute_mapping_keeps_descriptor_area_at_the_session_start() {
    let source = SessionSource::new(
        Box::new(MemoryBlocks(Vec::new())),
        100,
        AddressMode::DiscAbsolute { desc_len: 3 },
        0,
        CancelToken::default(),
    );
    // 描述符区（16 起到 16+desc_len）加区段起点，数据块原样。
    assert_eq!(source.disc_lba(16), 116);
    assert_eq!(source.disc_lba(18), 118);
    assert_eq!(source.disc_lba(19), 19);
    // 相对约定一律加起点。
    let relative = SessionSource::new(
        Box::new(MemoryBlocks(Vec::new())),
        100,
        AddressMode::SessionRelative,
        0,
        CancelToken::default(),
    );
    assert_eq!(relative.disc_lba(16), 116);
    assert_eq!(relative.disc_lba(19), 119);
}

#[test]
fn root_self_reference_check_reads_extent_and_flags() {
    let mut sector = [0u8; SECTOR_BYTES];
    sector[0] = 34;
    sector[25] = 0x02;
    sector[2..6].copy_from_slice(&7u32.to_le_bytes());
    assert!(root_record_is_self_referential(&sector, 7));
    assert!(!root_record_is_self_referential(&sector, 8), "块号对不上");
    sector[25] = 0x00;
    assert!(!root_record_is_self_referential(&sector, 7), "不是目录");
    sector[25] = 0x02;
    sector[0] = 33;
    assert!(!root_record_is_self_referential(&sector, 7), "记录过短");
}

/// 把一个 2048 字节的块放进盘缓冲的指定 LBA。
fn copy_block(disc: &mut [u8], lba: u32, block: &[u8; SECTOR_BYTES]) {
    disc[lba as usize * SECTOR_BYTES..(lba as usize + 1) * SECTOR_BYTES].copy_from_slice(block);
}

/// 写一对小端/大端 32 位字段（ISO 描述符的冗余双端表示）。
fn write_u32_both(field: &mut [u8], value: u64) {
    let value = value as u32;
    field[..4].copy_from_slice(&value.to_le_bytes());
    field[4..8].copy_from_slice(&value.to_be_bytes());
}

/// 写一对小端/大端 16 位字段。
fn write_u16_both(field: &mut [u8], value: u64) {
    let value = value as u16;
    field[..2].copy_from_slice(&value.to_le_bytes());
    field[2..4].copy_from_slice(&value.to_be_bytes());
}

/// PVD 的根目录记录（34 字节定长）。
fn write_root_record(record: &mut [u8], extent: u32) {
    write_dir_record(record, extent, SECTOR_BYTES as u32, 0x02, &[0]);
}

/// 一条目录记录：固定头 33 字节加名字，总长补齐到偶数（名字偶数长时补一个
/// 填充字节，与 hadris 解析器对长度的校验一致）。
fn write_dir_record(record: &mut [u8], extent: u32, size: u32, flags: u8, name: &[u8]) {
    record[0] = ((33 + name.len() + 1) & !1) as u8;
    record[1] = 0;
    record[2..6].copy_from_slice(&extent.to_le_bytes());
    record[6..10].copy_from_slice(&extent.to_be_bytes());
    record[10..14].copy_from_slice(&size.to_le_bytes());
    record[14..18].copy_from_slice(&size.to_be_bytes());
    record[18..25].fill(b'0');
    record[25] = flags;
    record[28..30].copy_from_slice(&1u16.to_le_bytes());
    record[30..32].copy_from_slice(&1u16.to_be_bytes());
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
}
#[cfg(test)]
mod hardware_tests {
    //! 真机测试：需要光驱，用环境变量指定设备后手动跑（ADR-0018 的实测记录）。
    //!
    //! - 末区段（自家原生引擎刻的）：`OPTIBURN_DEVICE=D: OPTIBURN_IMAGE=n.iso
    //!   cargo test -p optiburn-engine -- --ignored native_read_last_session_real --nocapture`
    //! - 绝对地址约定（xorriso 增长模式刻的旧区段）：再加 `OPTIBURN_SESSION_BASE`
    //!   指向该区段起点（READ TOC 或 probe 的轨道表里取）。

    use super::*;
    use crate::compare_trees;

    #[test]
    #[ignore = "needs optical drive"]
    fn native_read_last_session_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
        let reference =
            std::env::var("OPTIBURN_IMAGE").expect("set OPTIBURN_IMAGE to the burned .iso");

        assert_eq!(NativeRead.read_volume_id(&device).unwrap(), "NATIVETEST");
        assert!(NativeRead.last_session_is_iso(&device).unwrap());
        let entries = NativeRead.list_tree(&device).unwrap();
        assert!(!entries.is_empty(), "盘上应列得出文件: {entries:?}");

        // 从盘上与镜像文件各抽一棵树，逐文件对拍：盘上区段与镜像内容一致的证据。
        let tmp = std::env::temp_dir().join(format!("optiburn-native-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("disc")).expect("create dir");
        std::fs::create_dir_all(tmp.join("file")).expect("create dir");
        let token = CancelToken::default();
        NativeRead
            .extract_tree(Path::new(&device), &tmp.join("disc"), &token)
            .expect("extract from disc");
        NativeRead
            .extract_tree(Path::new(&reference), &tmp.join("file"), &token)
            .expect("extract from image");
        let differences = compare_trees(&tmp.join("file"), &tmp.join("disc"), "zh");
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(differences, Vec::<String>::new());
    }

    #[test]
    #[ignore = "needs optical drive"]
    fn native_read_absolute_session_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
        let base: u32 = std::env::var("OPTIBURN_SESSION_BASE")
            .expect("set OPTIBURN_SESSION_BASE to an xorriso-written session start LBA")
            .parse()
            .expect("decimal LBA");
        let token = CancelToken::default();
        let transport = optiburn_transport::open(&device).expect("open device");
        let iso = open_session_view(
            DiscBlocks(MmcDevice::new(transport)),
            base,
            u64::MAX,
            &token,
        )
        .expect("open the session");
        let id = volume_id(&iso, &token).expect("volume id");
        assert!(!id.is_empty(), "旧区段应读得出卷标");
        let root = iso.root_dir();
        let joliet = is_joliet_root(&root.entry_type());
        let mut entries = Vec::new();
        walk_list(&iso, root.dir_ref(), "", joliet, &mut entries, &token)
            .expect("walk the session");
        assert!(!entries.is_empty(), "旧区段应列得出文件");
        println!(
            "volume {id:?}, {} entries, first: {:?}",
            entries.len(),
            entries.first()
        );
    }
}
