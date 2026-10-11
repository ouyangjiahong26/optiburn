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
        // 同一进程里并行跑的测试会同时要夹具：只按进程号命名会互相删目录，
        // 再叠一个自增序号。
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "optiburn-disc-read-{tag}-{}-{unique}",
            std::process::id()
        ));
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

    assert!(matches!(
        backend.iso_session_state(&iso_text).unwrap(),
        IsoSessionState::Usable
    ));
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
    // 临时文件版（走文件源分支）对 iso_session_state 报不可用。
    let tmp = TempDir::new("noniso");
    let junk = tmp.path().join("junk.iso");
    std::fs::write(&junk, vec![0x55u8; SECTOR_BYTES * 3]).expect("write junk");
    assert!(matches!(
        NativeRead
            .iso_session_state(junk.to_str().unwrap())
            .unwrap(),
        IsoSessionState::Unusable
    ));
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
/// 假光驱：盘片信息与两级 TOC 按脚本回，READ(10) 从合成盘取数据。区段候选枚举与
/// 回退逻辑靠它测（真机路径只是多一层按路径打开设备）。
struct ScriptedDisc {
    disc: Vec<u8>,
    /// READ TOC Format 1 报的末区段起点。
    last_session_start: u32,
    /// READ TOC Format 0 报的轨道（编号与起点），调用方按需要放导出区。
    tracks: Vec<(u8, u32)>,
}

impl ScriptedDisc {
    fn new(disc: Vec<u8>, last_session_start: u32, tracks: Vec<(u8, u32)>) -> Self {
        Self {
            disc,
            last_session_start,
            tracks,
        }
    }
}

impl optiburn_transport::ScsiTransport for ScriptedDisc {
    fn issue(
        &mut self,
        cdb: &[u8],
        dir: optiburn_transport::Direction,
        data: &mut [u8],
        _timeout: std::time::Duration,
    ) -> Result<optiburn_transport::Completion, optiburn_transport::TransportError> {
        let fail = |cdb: &[u8]| {
            Err(optiburn_transport::TransportError::CommandFailed {
                cdb: cdb.to_vec(),
                scsi_status: 2,
                sense: vec![0x70, 0x00, 0x05, 0x21, 0, 0, 0, 0x0a],
            })
        };
        match cdb[0] {
            0x00 => {}
            // READ DISC INFORMATION：可追加，区段数与末轨号按轨道数凑一个可信值。
            0x51 => {
                if data.len() >= 7 {
                    data[2] = 0b01;
                    data[3] = 1;
                    data[4] = self.tracks.len() as u8;
                    data[5] = self.tracks.len() as u8 + 1;
                    data[6] = self.tracks.len() as u8 + 1;
                }
            }
            0x43 => match cdb[2] & 0x0F {
                // Format 0：轨道列表（LBA 形态）。
                0 => {
                    let length = 2 + self.tracks.len() * 8;
                    if data.len() >= 4 + self.tracks.len() * 8 {
                        data[0] = (length >> 8) as u8;
                        data[1] = length as u8;
                        data[2] = self.tracks.first().map(|t| t.0).unwrap_or(1);
                        data[3] = self.tracks.last().map(|t| t.0).unwrap_or(1);
                        for (index, (track, lba)) in self.tracks.iter().enumerate() {
                            let offset = 4 + index * 8;
                            data[offset + 1] = 0x14;
                            data[offset + 2] = *track;
                            data[offset + 4..offset + 8].copy_from_slice(&lba.to_be_bytes());
                        }
                    }
                }
                // Format 1：区段信息。
                1 => {
                    if data.len() >= 12 {
                        data[1] = 10;
                        data[2] = 1;
                        data[3] = self.tracks.len() as u8;
                        data[8..12].copy_from_slice(&self.last_session_start.to_be_bytes());
                    }
                }
                _ => return fail(cdb),
            },
            0x28 if dir == optiburn_transport::Direction::FromDevice => {
                let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]) as usize;
                let start = lba * SECTOR_BYTES;
                let end = start + data.len();
                match self.disc.get(start..end) {
                    Some(bytes) => data.copy_from_slice(bytes),
                    None => return fail(cdb),
                }
            }
            _ => return fail(cdb),
        }
        Ok(optiburn_transport::Completion {
            scsi_status: 0,
            sense: Vec::new(),
            residual: 0,
        })
    }

    fn device_path(&self) -> &str {
        "D:"
    }
}

/// 造一张合成盘：`[0 到 base 的零] + 会话字节`，会话字节用生成器按 `base` 起点算。
fn disc_with_session(base: u32, total_blocks: u32, session: &[u8]) -> Vec<u8> {
    let mut disc = vec![0u8; total_blocks as usize * SECTOR_BYTES];
    let start = base as usize * SECTOR_BYTES;
    disc[start..start + session.len()].copy_from_slice(session);
    disc
}

/// 用生成器造一个只含给定文件的会话（盘级绝对地址）。
fn session_bytes(base: u32, volume_id: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let tmp = TempDir::new("candidate-session");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source dir");
    for (name, content) in files {
        std::fs::write(src.join(name), content.as_bytes()).expect("write file");
    }
    let plan =
        crate::grow::plan_session(None, &src, volume_id.to_string(), base).expect("plan a session");
    let mut image = Vec::new();
    plan.write_image(&mut image, &CancelToken::default())
        .expect("write the session");
    image
}

fn open_on_device(disc: ScriptedDisc) -> Result<OpenedSession, BurnError> {
    let mmc = MmcDevice::new(Box::new(disc));
    open_readable_session_on_device(mmc, &CancelToken::default())
}

#[test]
fn candidates_come_from_the_track_list_newest_first() {
    let disc = ScriptedDisc::new(
        vec![0u8; 64 * SECTOR_BYTES],
        3000,
        vec![(1, 1000), (2, 2000), (3, 3000), (0xAA, 3200)],
    );
    let mut mmc = MmcDevice::new(Box::new(disc));
    let candidates = session_candidates(&mut mmc).expect("candidates");
    assert_eq!(
        candidates,
        vec![3000, 2000, 1000],
        "新到旧、去重，导出区不参与"
    );
}

#[test]
fn salvage_classification_uses_the_written_boundary() {
    const BOUNDARY: u32 = 1100;
    // 整段在边界之前：完整。
    assert_eq!(
        classify_salvage_state(1000, 2048, BOUNDARY),
        SalvageState::Complete
    );
    // 跨过边界：半截，能读多少算多少。
    assert_eq!(
        classify_salvage_state(1099, 4096, BOUNDARY),
        SalvageState::Truncated {
            readable_bytes: 2048
        }
    );
    // 起点就在边界之后：一个字节都没写。
    assert_eq!(
        classify_salvage_state(1100, 4096, BOUNDARY),
        SalvageState::Missing
    );
    // 零长度文件不需要数据，算完整。
    assert_eq!(
        classify_salvage_state(2000, 0, BOUNDARY),
        SalvageState::Complete
    );
}

#[test]
fn written_boundary_binary_searches_the_first_unreadable_block() {
    const START: u32 = 301_170;
    const WRITTEN: u32 = 9_253;
    let mut reads = 0u32;
    let mut read = |lba: u32, out: &mut [u8]| {
        reads += 1;
        if lba >= START + WRITTEN {
            return Err(BurnError::ReadFailed("未写位置读失败".to_string()));
        }
        out.fill(0xAB);
        Ok(())
    };
    let boundary = written_boundary(&mut read, START, 23_438, &CancelToken::default())
        .expect("扫描要给出边界");
    assert_eq!(boundary, START + WRITTEN);
    // 二分：两万三千多块只读十几次，线性扫要读九千多块。
    assert!(reads <= 20, "读次数 {reads} 应是对数级");
}

#[test]
fn written_boundary_counts_all_zero_blocks_as_written() {
    // 会话开头的系统区就是全零，文件内容也可以是零：全零不算中断点。
    let mut read = |lba: u32, out: &mut [u8]| {
        if lba >= 1700 {
            return Err(BurnError::ReadFailed("未写位置读失败".to_string()));
        }
        out.fill(0);
        Ok(())
    };
    let boundary =
        written_boundary(&mut read, 1000, 5000, &CancelToken::default()).expect("扫描要给出边界");
    assert_eq!(boundary, 1700);
}

#[test]
fn written_boundary_is_the_start_when_the_first_block_fails() {
    let mut read = |_lba: u32, _out: &mut [u8]| Err(BurnError::ReadFailed("读失败".to_string()));
    let boundary =
        written_boundary(&mut read, 500, 100, &CancelToken::default()).expect("扫描要给出边界");
    assert_eq!(boundary, 500);
}

#[test]
fn written_boundary_reaches_the_session_end_when_everything_reads() {
    let mut read = |_lba: u32, out: &mut [u8]| {
        out.fill(7);
        Ok(())
    };
    let boundary =
        written_boundary(&mut read, 500, 100, &CancelToken::default()).expect("扫描要给出边界");
    assert_eq!(boundary, 600);
}

#[test]
fn last_session_only_switch_parses_its_values() {
    assert!(!last_session_only_value(None), "默认关");
    assert!(!last_session_only_value(Some("")));
    assert!(!last_session_only_value(Some("0")));
    assert!(!last_session_only_value(Some("off")));
    assert!(last_session_only_value(Some("1")));
    assert!(last_session_only_value(Some("yes")));
}

#[test]
fn a_damaged_last_session_falls_back_to_the_previous_one() {
    // 盘：[起点 1000 的完好会话][起点 2000 的残片]。残片的描述符区是零，读它报
    // NoIsoSession；候选回退到 1000，并把回退信息报出来。
    let good = session_bytes(1000, "GOODSESSION", &[("keep.txt", "kept")]);
    let mut disc = disc_with_session(1000, 4096, &good);
    // 残片：起点 2000 处放一段不是 ISO 描述符区的字节，目录结构写着它却读不出来。
    let fragment_start = 2000 * SECTOR_BYTES;
    disc[fragment_start..fragment_start + SECTOR_BYTES].fill(0x55);
    let scripted = ScriptedDisc::new(disc, 2000, vec![(1, 1000), (2, 2000), (0xAA, 2100)]);

    let opened = open_on_device(scripted).expect("回退到 1000 的会话");
    let fallback = opened.fallback.expect("末区段损坏要报回退信息");
    assert_eq!(fallback.skipped, 1);
    assert_eq!(fallback.ordinal, 1);
    assert_eq!(fallback.candidates, 2);
    assert_eq!(fallback.session_start, 1000);

    let root = opened.iso.root_dir();
    let mut entries = Vec::new();
    walk_list(
        &opened.iso,
        root.dir_ref(),
        "",
        is_joliet_root(&root.entry_type()),
        &mut entries,
        &CancelToken::default(),
    )
    .expect("walk the recovered session");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "/keep.txt");
}

#[test]
fn a_fragment_with_intact_descriptors_but_a_broken_tree_is_skipped() {
    // 残片的描述符区完整（PVD 与 Joliet 都在），但根目录记录的 extent 指向盘外，
    // 遍历时会读失败：候选验证必须淘汰它，而不是把坏树当成可用会话。
    let good = session_bytes(1000, "GOODSESSION", &[("keep.txt", "kept")]);
    let mut fragment = session_bytes(2000, "FRAGMENT", &[("lost.txt", "lost")]);
    // 把根记录（PVD 偏移 158 起，Joliet SVD 偏移 156 起）的 extent 指到盘外。
    for descriptor in [16usize, 17] {
        let field = descriptor * SECTOR_BYTES + 158;
        fragment[field..field + 4].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
        fragment[field + 4..field + 8].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
    }
    let mut disc = disc_with_session(1000, 4096, &good);
    let fragment_start = 2000 * SECTOR_BYTES;
    disc[fragment_start..fragment_start + fragment.len()].copy_from_slice(&fragment);
    let scripted = ScriptedDisc::new(disc, 2000, vec![(1, 1000), (2, 2000)]);

    let opened = open_on_device(scripted).expect("跳过坏树，回退到 1000");
    assert_eq!(opened.fallback.expect("要报回退").session_start, 1000);
}

#[test]
fn all_candidates_invalid_still_reports_the_newest_error() {
    // 只有一片残片：回退没有去处，报最新那个候选的错误（NoIsoSession），文案与
    // 改动前一致。
    let mut disc = vec![0u8; 4096 * SECTOR_BYTES];
    let fragment_start = 2000 * SECTOR_BYTES;
    disc[fragment_start..fragment_start + SECTOR_BYTES].fill(0x55);
    let scripted = ScriptedDisc::new(disc, 2000, vec![(1, 2000)]);
    let error = match open_on_device(scripted) {
        Err(error) => error,
        Ok(_) => panic!("没有可用候选时不该开出会话"),
    };
    assert!(matches!(error, BurnError::NoIsoSession), "{error:?}");
}

#[test]
fn a_healthy_disc_reports_no_fallback() {
    let good = session_bytes(150093, "HEALTHY", &[("a.txt", "alpha")]);
    let scripted = ScriptedDisc::new(
        disc_with_session(150093, 160000, &good),
        150093,
        vec![(1, 0), (2, 150093)],
    );
    let opened = open_on_device(scripted).expect("末区段可用");
    assert!(opened.fallback.is_none(), "正常盘不该报回退");
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
        assert!(matches!(
            NativeRead.iso_session_state(&device).unwrap(),
            IsoSessionState::Usable
        ));
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

/// 手造一个"存储"（不压缩）的 zip：多个条目加中央目录与 EOCD。
fn build_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for (name, payload) in entries {
        offsets.push(out.len() as u32);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 不压缩
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&zip_recover::crc32(payload).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(payload);
    }
    let directory_offset = out.len() as u32;
    for ((name, payload), offset) in entries.iter().zip(&offsets) {
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&zip_recover::crc32(payload).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    let directory_size = out.len() as u32 - directory_offset;
    let count = entries.len() as u16;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[test]
fn truncated_zip_keeps_only_complete_entries() {
    let zip = build_stored_zip(&[
        ("a.txt", b"aaaa"),
        ("b.txt", b"bbbbbbbb"),
        ("c.txt", b"cccccccccccc"),
    ]);
    // 砍在第三个条目的数据中间：前两条完整，第三条不完整，中央目录全丢。
    let keep = 30 + 5 + 4 + 30 + 5 + 8 + 30 + 5 + 6;
    let truncated = zip[..keep].to_vec();

    let dir = std::env::temp_dir().join("optiburn-zip-recover-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.zip");
    std::fs::write(&path, &truncated).unwrap();

    let salvaged = zip_recover::recover_truncated_zip(&path)
        .unwrap()
        .expect("应重建");
    assert_eq!(salvaged.entries, 2);
    assert_eq!(salvaged.stored_bytes, 12);
    assert_eq!(salvaged.dropped_entry.as_deref(), Some("c.txt"));

    // 重建后的文件应等于"两条条目的原样字节 + 中央目录与 EOCD"。
    let rebuilt = std::fs::read(&path).unwrap();
    let complete = build_stored_zip(&[("a.txt", b"aaaa"), ("b.txt", b"bbbbbbbb")]);
    let eocd = &rebuilt[rebuilt.len() - 22..];
    assert_eq!(&eocd[..4], b"PK\x05\x06", "末尾要有 EOCD");
    assert_eq!(
        u16::from_le_bytes([eocd[10], eocd[11]]),
        2,
        "EOCD 报两条条目"
    );
    assert_eq!(
        &rebuilt[..rebuilt.len() - 22],
        &complete[..complete.len() - 22]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn intact_zip_is_not_rebuilt() {
    let zip = build_stored_zip(&[("a.txt", b"aaaa")]);
    let dir = std::env::temp_dir().join("optiburn-zip-recover-test-3");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("whole.zip");
    std::fs::write(&path, &zip).unwrap();
    assert!(zip_recover::recover_truncated_zip(&path).unwrap().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), zip);
    std::fs::remove_dir_all(&dir).ok();
}

/// 手造一个带数据描述符（通用位 bit 3）的 zip：本地头的长度字段是零，真实长度在
/// 数据后的描述符里。`with_signature` 决定描述符带不带 `PK\x07\x08` 签名。
fn build_descriptor_zip(entries: &[(&str, &[u8])], with_signature: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    let mut records = Vec::new();
    for (name, payload) in entries {
        offsets.push(out.len() as u32);
        let crc = zip_recover::crc32(payload);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0b1000u16.to_le_bytes()); // bit 3：长度写在描述符里
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // CRC 与两个长度都是零
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(payload);
        if with_signature {
            out.extend_from_slice(b"PK\x07\x08");
        }
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        records.push((crc, payload.len() as u32));
    }
    let directory_offset = out.len() as u32;
    for (index, (name, _)) in entries.iter().enumerate() {
        let (crc, size) = records[index];
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0b1000u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&offsets[index].to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    let directory_size = out.len() as u32 - directory_offset;
    let count = entries.len() as u16;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[test]
fn descriptor_style_zip_keeps_only_complete_entries() {
    for with_signature in [true, false] {
        let zip = build_descriptor_zip(
            &[
                ("a.txt", b"aaaa"),
                ("b.txt", b"bbbbbbbb"),
                ("c.txt", b"cccccc"),
            ],
            with_signature,
        );
        // 砍在第三个条目的数据中间（它的本地头 30 + 名字 5 + 数据 6 只留 3 字节）。
        let keep =
            zip.len() - 22 - (3 * (46 + 5)) - (6 + 4 + 4 + if with_signature { 4 } else { 0 }) + 3;
        let cut = zip[..keep].to_vec();
        let dir = std::env::temp_dir().join("optiburn-zip-descriptor-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.zip");
        std::fs::write(&path, &cut).unwrap();

        let salvaged = zip_recover::recover_truncated_zip(&path)
            .unwrap()
            .unwrap_or_else(|| panic!("带签名={with_signature} 时应识别出 zip 链"));
        assert_eq!(salvaged.entries, 2, "带签名={with_signature}");
        assert_eq!(salvaged.stored_bytes, 12, "带签名={with_signature}");
        assert_eq!(salvaged.dropped_entry.as_deref(), Some("c.txt"));
        // 重建后的 zip 里两条条目的长度字段要靠描述符取得（本地头是零）。
        let rebuilt = std::fs::read(&path).unwrap();
        let directory_start = rebuilt
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("重建后要有中央目录");
        let first = &rebuilt[directory_start..];
        assert_eq!(
            u32::from_le_bytes([first[20], first[21], first[22], first[23]]),
            4,
            "第一条的压缩长度应来自描述符"
        );
        assert_eq!(
            u32::from_le_bytes([first[24], first[25], first[26], first[27]]),
            4,
            "第一条的解压长度应来自描述符"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn zip_cut_inside_the_first_entry_reports_it_without_rewriting() {
    let zip = build_stored_zip(&[("big.bin", &[7u8; 4000])]);
    let cut = zip[..100].to_vec();
    let dir = std::env::temp_dir().join("optiburn-zip-first-entry-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.zip");
    std::fs::write(&path, &cut).unwrap();

    let salvaged = zip_recover::recover_truncated_zip(&path)
        .unwrap()
        .expect("应识别出 zip 链");
    assert_eq!(salvaged.entries, 0);
    assert_eq!(salvaged.dropped_entry.as_deref(), Some("big.bin"));
    assert_eq!(std::fs::read(&path).unwrap(), cut, "没有完整条目就不重写");
    std::fs::remove_dir_all(&dir).ok();
}
