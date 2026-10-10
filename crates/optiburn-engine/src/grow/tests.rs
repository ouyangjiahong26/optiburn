//! 增长会话生成器的测试：布局字节、嫁接式增长往返、失败路径。
//!
//! 生成器与原生读侧互为对拍：生成的会话用 `disc_read` 的读侧读回（列举与内容），
//! 关键地址再从盘上字节里手工解析一遍，避免两边共享同一个误解。

use std::path::{Path, PathBuf};

use hadris_iso::sync::directory::DirDateTime;
use optiburn_mastering::{DiscProfile, ImageSpec, build_image};

use super::*;
use crate::disc_read::{BlockSource, is_joliet_root, open_session_view, walk_list};
use crate::readback::DiscEntry;

/// 内存块源：给手造的盘与合成盘做测试。
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
        let path = std::env::temp_dir().join(format!(
            "optiburn-grow-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
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

/// 用 mastering 建一张旧区段镜像（Joliet 开，含中文名、空文件与两级目录）。
fn old_image(tmp: &TempDir) -> Vec<u8> {
    let src = tmp.path().join("old-src");
    let sub = src.join("子目录");
    std::fs::create_dir_all(&sub).expect("create source tree");
    std::fs::write(src.join("a.md"), b"alpha notes").expect("write a.md");
    std::fs::write(sub.join("中文文件.txt"), "中文内容".as_bytes()).expect("write zh file");
    std::fs::write(src.join("空文件.bin"), b"").expect("write empty file");
    let iso = tmp.path().join("old.iso");
    build_image(
        &src,
        &iso,
        &ImageSpec {
            profile: DiscProfile::Dvd,
            volume_id: "OLDSESSION".to_string(),
            joliet: true,
        },
    )
    .expect("build image");
    std::fs::read(&iso).expect("read image")
}

/// 把一段字节放进盘缓冲的指定块处。
fn put(disc: &mut [u8], lba: u32, bytes: &[u8]) {
    disc[lba as usize * SECTOR_BYTES..lba as usize * SECTOR_BYTES + bytes.len()]
        .copy_from_slice(bytes);
}

/// 列出某个区段的条目（按路径排序，顺序本身不在契约里）。
fn list(disc: Vec<u8>, base: u32) -> Vec<DiscEntry> {
    let iso = open_session_view(MemoryBlocks(disc), base, u64::MAX, &CancelToken::default())
        .expect("open the session");
    let root = iso.root_dir();
    let joliet = is_joliet_root(&root.entry_type());
    let mut entries = Vec::new();
    walk_list(
        &iso,
        root.dir_ref(),
        "",
        joliet,
        &mut entries,
        &CancelToken::default(),
    )
    .expect("walk the session");
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

/// 直接用读侧抽一个文件的内容。
fn read_file(disc: Vec<u8>, base: u32, path: &str) -> Vec<u8> {
    let iso = open_session_view(MemoryBlocks(disc), base, u64::MAX, &CancelToken::default())
        .expect("open the session");
    let entry = iso
        .find_path(path)
        .expect("find path")
        .unwrap_or_else(|| panic!("{path} not found"));
    iso.read_file(&entry).expect("read file")
}

/// 手工解析会话字节：从描述符区取根目录记录，按路径逐段找记录，返回
/// (extent, data_len)。用生成器之外的代码验证地址约定。`session` 起点是盘上
/// `base` 处的会话起点，记录里的地址是盘级绝对地址，按 base 换算回会话内偏移。
/// `descriptor` 是描述符在区段里的序号（0 是 PVD，1 是 Joliet SVD）。
fn parse_entry(session: &[u8], base: u32, descriptor: u32, path: &[&str]) -> (u32, u32) {
    let descriptor_block = DESC_START + descriptor;
    let sector = &session[descriptor_block as usize * SECTOR_BYTES..][..SECTOR_BYTES];
    let mut extent = u32::from_le_bytes([sector[158], sector[159], sector[160], sector[161]]);
    let mut size = u32::from_le_bytes([sector[166], sector[167], sector[168], sector[169]]);
    for (index, wanted_name) in path.iter().enumerate() {
        let found = find_record(session, base, extent, size, wanted_name)
            .unwrap_or_else(|| panic!("{wanted_name} not found under {}", path[..index].join("/")));
        extent = found.0;
        size = found.1;
    }
    (extent, size)
}

/// 在一个目录的数据里找一条 Joliet 名匹配的记录。
fn find_record(
    session: &[u8],
    base: u32,
    extent: u32,
    size: u32,
    name: &str,
) -> Option<(u32, u32)> {
    assert!(extent >= base, "会话里的地址必须是盘级绝对地址");
    let mut offset = (extent - base) as usize * SECTOR_BYTES;
    let end = offset + size as usize;
    let wanted: Vec<u8> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
    while offset < end {
        let len = session[offset] as usize;
        if len == 0 {
            offset = (offset / SECTOR_BYTES + 1) * SECTOR_BYTES;
            continue;
        }
        let name_len = session[offset + 32] as usize;
        if session[offset + 33..offset + 33 + name_len] == wanted[..] {
            let extent = u32::from_le_bytes([
                session[offset + 2],
                session[offset + 3],
                session[offset + 4],
                session[offset + 5],
            ]);
            let size = u32::from_le_bytes([
                session[offset + 10],
                session[offset + 11],
                session[offset + 12],
                session[offset + 13],
            ]);
            return Some((extent, size));
        }
        offset += len;
    }
    None
}

#[test]
fn generated_session_addresses_are_absolute_and_sizes_are_session_relative() {
    const BASE: u32 = 1000;
    let tmp = TempDir::new("addresses");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("子目录")).expect("create source tree");
    std::fs::write(src.join("a.md"), b"alpha").expect("write a.md");
    std::fs::write(src.join("空文件.bin"), b"").expect("write empty file");
    std::fs::write(src.join("子目录/中文.txt"), "内容".as_bytes()).expect("write zh file");

    let plan = plan_session(None, &src, "GROWTV1".to_string(), BASE).expect("plan");
    let mut image = Vec::new();
    plan.write_image(&mut image, &CancelToken::default())
        .expect("write image");

    // 生成器记账：写出的字节数等于计划尺寸。
    assert_eq!(image.len() as u64, plan.total_bytes());
    let blocks = (image.len() / SECTOR_BYTES) as u64;

    // PVD：类型 1、CD001、卷标、卷空间大小是区段相对值（不含区段起点）。
    let pvd = &image[DESC_START as usize * SECTOR_BYTES..][..SECTOR_BYTES];
    assert_eq!(pvd[0], 1);
    assert_eq!(&pvd[1..6], b"CD001");
    assert_eq!(pvd[6], 1);
    assert_eq!(&pvd[40..47], b"GROWTV1");
    assert!(pvd[47..72].iter().all(|byte| *byte == b' '), "卷标补空格");
    assert_eq!(
        u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]) as u64,
        blocks,
        "卷空间大小是区段相对大小"
    );
    assert_eq!(
        u32::from_be_bytes([pvd[84], pvd[85], pvd[86], pvd[87]]) as u64,
        blocks
    );

    // 根目录记录：extent 是盘级绝对地址，data_len 是根目录数据的字节数（整块）。
    let root_extent = u32::from_le_bytes([pvd[158], pvd[159], pvd[160], pvd[161]]);
    let root_size = u32::from_le_bytes([pvd[166], pvd[167], pvd[168], pvd[169]]);
    assert_eq!(
        root_extent,
        BASE + 23,
        "根目录接在两套目录数据的开头（四张路径表各占一块）"
    );
    assert_eq!(root_size, SECTOR_BYTES as u32);
    assert_eq!(pvd[156], 34, "根目录记录长度");
    assert_eq!(pvd[188], 1, "根目录记录的标识符长度");
    assert_eq!(pvd[189], 0, "根目录记录的标识符");

    // 路径表：大小字段与实际字节数一致，位置是绝对地址。
    let path_table_size = u32::from_le_bytes([pvd[132], pvd[133], pvd[134], pvd[135]]) as usize;
    let l_table = u32::from_le_bytes([pvd[140], pvd[141], pvd[142], pvd[143]]);
    let m_table = u32::from_be_bytes([pvd[148], pvd[149], pvd[150], pvd[151]]);
    assert_eq!(l_table, BASE + 19, "路径表接在终止符之后");
    assert!(m_table > l_table, "两张表各占整数块");
    for table in [l_table, m_table] {
        let start = (table - BASE) as usize * SECTOR_BYTES;
        let table = &image[start..start + path_table_size];
        assert_eq!(table[0], 1, "根目录标识符长度");
        assert_eq!(table[8], 0, "根目录标识符");
        // 根记录长度 8 加标识符 1 加补位 1（标识符长度为奇数，总长恒为偶数）。
        assert_eq!(path_table_size % 2, 0, "路径表总长是偶数");
    }
    assert_eq!(m_table, l_table + 1, "L 表与 M 表各占一块");

    // Joliet SVD：类型 2、转义序列 `%/E`，卷标是 UTF-16BE。
    let svd = &image[(DESC_START + 1) as usize * SECTOR_BYTES..][..SECTOR_BYTES];
    assert_eq!(svd[0], 2);
    assert_eq!(&svd[88..91], b"%/E");
    assert_eq!(
        u32::from_le_bytes([svd[80], svd[81], svd[82], svd[83]]) as u64,
        blocks
    );
    assert_eq!(
        &svd[40..54],
        "GROWTV1"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<u8>>()
    );

    // 终止符。
    let terminator = &image[(DESC_START + 2) as usize * SECTOR_BYTES..][..SECTOR_BYTES];
    assert_eq!(terminator[0], 255);
    assert_eq!(&terminator[1..6], b"CD001");

    // 系统区全零。
    assert!(
        image[..DESC_START as usize * SECTOR_BYTES]
            .iter()
            .all(|b| *b == 0)
    );

    // 手工解析：PVD 与 Joliet 根目录里的条目 extent 都是盘级绝对地址（生成时就是
    // 这么写的），且都能被读侧读回来。
    // 布局：16 PVD、17 SVD、18 终止符、19-22 四张路径表、23-24 主命名空间目录、
    // 25-26 Joliet 目录，之后是新文件数据。
    let (extent, size) = parse_entry(&image, BASE, 1, &["a.md"]);
    assert_eq!(extent, BASE + 27, "第一个新文件接在两套目录数据之后");
    assert_eq!(size, 5);
    let (zh_extent, zh_size) = parse_entry(&image, BASE, 1, &["子目录", "中文.txt"]);
    assert_eq!(zh_extent, BASE + 28);
    assert_eq!(zh_size, "内容".len() as u32);

    let entries = list(
        {
            let mut disc = vec![0u8; (BASE as usize + image.len() / SECTOR_BYTES) * SECTOR_BYTES];
            put(&mut disc, BASE, &image);
            disc
        },
        BASE,
    );
    assert_eq!(
        entries,
        vec![
            DiscEntry {
                path: "/a.md".into(),
                size: 5,
                is_dir: false,
            },
            DiscEntry {
                path: "/子目录".into(),
                size: 0,
                is_dir: true,
            },
            DiscEntry {
                path: "/子目录/中文.txt".into(),
                size: "内容".len() as u64,
                is_dir: false,
            },
            DiscEntry {
                path: "/空文件.bin".into(),
                size: 0,
                is_dir: false,
            },
        ]
    );
}

#[test]
fn growth_references_old_blocks_and_reads_back() {
    const OLD_BASE: u32 = 1000;
    const NEW_BASE: u32 = 4000;
    let tmp = TempDir::new("roundtrip");
    let image = old_image(&tmp);

    // 合成盘：旧区段放在 1000 处。
    let mut disc = vec![0u8; 8192 * SECTOR_BYTES];
    put(&mut disc, OLD_BASE, &image);

    let old = {
        let disc = disc.clone();
        let mut blocks = MemoryBlocks(disc);
        let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
        read_old_session(&mut read, OLD_BASE, &CancelToken::default()).expect("read old session")
    };
    let old_root = &old.root;
    assert_eq!(old_root.files.len(), 2, "{old_root:?}");
    let old_file = old_root
        .files
        .iter()
        .find(|file| file.name == "a.md")
        .expect("a.md in the old session");
    assert_eq!(old_file.size, 11);
    assert!(old_file.extent >= OLD_BASE, "旧文件地址是盘级绝对地址");
    let old_extent = old_file.extent;

    // 源目录只带新文件：旧文件靠引用，不会被重写。
    let src = tmp.path().join("new-src");
    std::fs::create_dir_all(&src).expect("create source dir");
    std::fs::write(src.join("新文件.txt"), "new content".as_bytes()).expect("write new file");

    let plan = plan_session(Some(old), &src, "GROWTV1".to_string(), NEW_BASE).expect("plan");
    let mut session = Vec::new();
    plan.write_image(&mut session, &CancelToken::default())
        .expect("write image");
    // 旧文件的内容不出现在新区段的字节里：数据没有被重写。
    assert!(
        !session.windows(11).any(|window| window == b"alpha notes"),
        "新区段里不该出现旧文件的数据"
    );
    put(&mut disc, NEW_BASE, &session);

    // 读回：旧条目加新文件的合集，内容逐字节一致。
    let entries = list(disc.clone(), NEW_BASE);
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "/a.md",
            "/子目录",
            "/子目录/中文文件.txt",
            "/新文件.txt",
            "/空文件.bin"
        ]
    );
    assert_eq!(read_file(disc.clone(), NEW_BASE, "/a.md"), b"alpha notes");
    assert_eq!(
        read_file(disc.clone(), NEW_BASE, "/子目录/中文文件.txt"),
        "中文内容".as_bytes()
    );
    assert_eq!(
        read_file(disc.clone(), NEW_BASE, "/新文件.txt"),
        b"new content"
    );
    assert_eq!(read_file(disc.clone(), NEW_BASE, "/空文件.bin"), b"");

    // 旧文件的记录仍指向旧区段里的块（绝对地址），这次是独立解析盘上字节。
    let new_session = &disc[NEW_BASE as usize * SECTOR_BYTES..];
    let (extent, size) = parse_entry(new_session, NEW_BASE, 1, &["a.md"]);
    assert_eq!(
        extent, old_extent,
        "旧文件在新会话里必须原地引用旧块，而不是重写"
    );
    assert_eq!(size, 11);
    // 新文件的地址在新区段里。
    let (new_extent, new_size) = parse_entry(new_session, NEW_BASE, 1, &["新文件.txt"]);
    assert!(new_extent >= NEW_BASE, "{new_extent}");
    assert_eq!(new_size, "new content".len() as u32);
}

#[test]
fn old_session_without_joliet_is_refused() {
    let tmp = TempDir::new("nojoliet");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source dir");
    std::fs::write(src.join("a.md"), b"alpha").expect("write a.md");
    let iso = tmp.path().join("nojoliet.iso");
    build_image(
        &src,
        &iso,
        &ImageSpec {
            profile: DiscProfile::Dvd,
            volume_id: "NOJOLIET".to_string(),
            joliet: false,
        },
    )
    .expect("build image");
    let image = std::fs::read(&iso).expect("read image");

    let mut blocks = MemoryBlocks(image);
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    let error = read_old_session(&mut read, 0, &CancelToken::default())
        .expect_err("a primary-only session has no Joliet tree");
    assert!(
        matches!(&error, BurnError::GrowUnsupported(detail)
            if detail.contains("no Joliet directory tree")),
        "{error:?}"
    );
}

/// 手造一张最小可读的区段：PVD、Joliet SVD、终止符、根目录（`.`, `..` 与一条
/// 文件记录）、一块文件数据。`file_flags`、`extra_dir_extent` 与 `file_system_use`
/// 用来造异常形状（多 extent、目录环、符号链接）。
fn crafted_session(
    file_flags: u8,
    extra_dir_extent: Option<u32>,
    file_system_use: &[u8],
) -> Vec<u8> {
    let root = 19u32;
    let data = 20u32;
    let extra = 21u32;
    let blocks = if extra_dir_extent.is_some() { 22 } else { 21 };
    let mut disc = vec![0u8; blocks as usize * SECTOR_BYTES];
    let descriptor = |disc: &mut Vec<u8>, lba: u32, kind: u8| {
        let mut sector = [0u8; SECTOR_BYTES];
        sector[0] = kind;
        sector[1..6].copy_from_slice(b"CD001");
        sector[6] = 1;
        if kind == 2 {
            sector[88..91].copy_from_slice(b"%/E");
        }
        let record = directory_record(
            &[0x00],
            DirPlacement {
                extent: root,
                size: SECTOR_BYTES as u32,
            },
            FileFlags::DIRECTORY,
            DirDateTime::now(),
        );
        sector[156..190].copy_from_slice(record.to_bytes());
        let offset = lba as usize * SECTOR_BYTES;
        disc[offset..offset + SECTOR_BYTES].copy_from_slice(&sector);
    };
    let mut root_data = Vec::new();
    for name in [[0x00u8], [0x01u8]] {
        let record = directory_record(
            &name,
            DirPlacement {
                extent: root,
                size: SECTOR_BYTES as u32,
            },
            FileFlags::DIRECTORY,
            DirDateTime::now(),
        );
        root_data.extend_from_slice(record.to_bytes());
    }
    let file_name: Vec<u8> = "FILE.TXT"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    let file = directory_record_with_use(
        &file_name,
        DirPlacement {
            extent: data,
            size: 4,
        },
        FileFlags::from_bits_retain(file_flags),
        DirDateTime::now(),
        file_system_use,
    );
    root_data.extend_from_slice(file.to_bytes());
    if let Some(extent) = extra_dir_extent {
        let name: Vec<u8> = "LOOP".encode_utf16().flat_map(u16::to_be_bytes).collect();
        let record = directory_record(
            &name,
            DirPlacement {
                extent,
                size: SECTOR_BYTES as u32,
            },
            FileFlags::DIRECTORY,
            DirDateTime::now(),
        );
        root_data.extend_from_slice(record.to_bytes());
    }
    root_data.resize(SECTOR_BYTES, 0);
    let offset = root as usize * SECTOR_BYTES;
    disc[offset..offset + SECTOR_BYTES].copy_from_slice(&root_data);
    disc[data as usize * SECTOR_BYTES..data as usize * SECTOR_BYTES + 4].copy_from_slice(b"data");
    if extra_dir_extent.is_some() {
        // 循环目录的数据就是根目录本身的数据，读侧必须靠已访问集合拦住。
        let offset = extra as usize * SECTOR_BYTES;
        disc[offset..offset + SECTOR_BYTES].copy_from_slice(&root_data);
    }
    descriptor(&mut disc, 16, 1);
    descriptor(&mut disc, 17, 2);
    disc[18 * SECTOR_BYTES] = 255;
    disc
}

/// 带 SUSP/RRIP 系统用区的目录记录（手造符号链接等异常形状用）。
fn directory_record_with_use(
    name: &[u8],
    placement: DirPlacement,
    flags: FileFlags,
    date: DirDateTime,
    system_use: &[u8],
) -> hadris_iso::sync::directory::DirectoryRecord {
    let mut record = hadris_iso::sync::directory::DirectoryRecord::new(
        name,
        system_use,
        DirectoryRef {
            extent: LogicalSector(placement.extent as usize),
            size: placement.size as usize,
        },
        flags,
    );
    record.header_mut().date_time = date;
    record
}

#[test]
fn boot_record_in_the_last_session_is_refused() {
    let mut disc = crafted_session(0, None, &[]);
    disc[16 * SECTOR_BYTES] = 0; // 首条描述符换成启动记录
    let mut blocks = MemoryBlocks(disc);
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    let error = read_old_session(&mut read, 0, &CancelToken::default())
        .expect_err("a boot record blocks growth");
    assert!(
        matches!(&error, BurnError::GrowUnsupported(detail) if detail.contains("boot record")),
        "{error:?}"
    );
}

#[test]
fn symbolic_link_in_the_old_session_is_refused() {
    // SUSP 的 `SL` 条目：签名两个字节、长度一个字节（含这 4 字节头）、版本一个
    // 字节，再加一个字节的载荷。
    let disc = crafted_session(0, None, b"SL\x05\x01\x00");
    let mut blocks = MemoryBlocks(disc);
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    let error = read_old_session(&mut read, 0, &CancelToken::default())
        .expect_err("symbolic links are not grafted");
    assert!(
        matches!(&error, BurnError::GrowUnsupported(detail) if detail.contains("symbolic link")),
        "{error:?}"
    );
}

#[test]
fn multi_extent_old_file_is_refused() {
    let disc = crafted_session(0x80, None, &[]);
    let mut blocks = MemoryBlocks(disc);
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    let error = read_old_session(&mut read, 0, &CancelToken::default())
        .expect_err("multi-extent files are not grafted");
    assert!(
        matches!(&error, BurnError::GrowUnsupported(detail)
            if detail.contains("spans multiple extents")),
        "{error:?}"
    );
}

#[test]
fn directory_cycle_in_the_old_session_is_refused() {
    let disc = crafted_session(0, Some(19), &[]);
    let mut blocks = MemoryBlocks(disc);
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    let error = read_old_session(&mut read, 0, &CancelToken::default())
        .expect_err("a cycle must be caught");
    assert!(
        matches!(&error, BurnError::GrowUnsupported(detail) if detail.contains("cycle")),
        "{error:?}"
    );
}

#[test]
fn crafted_session_without_iso_reports_no_session() {
    let mut blocks = MemoryBlocks(vec![0u8; 64 * SECTOR_BYTES]);
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    assert!(matches!(
        read_old_session(&mut read, 0, &CancelToken::default()),
        Err(BurnError::NoIsoSession)
    ));
}

#[test]
fn empty_source_is_refused() {
    let tmp = TempDir::new("empty");
    let src = tmp.path().join("empty-src");
    std::fs::create_dir_all(&src).expect("create source dir");
    let error = plan_session(None, &src, "EMPTY".to_string(), 0).expect_err("no files to write");
    assert!(
        matches!(error, BurnError::NativeGap(NativeGap::EmptyGrowSource)),
        "{error:?}"
    );
}

#[test]
fn file_against_directory_is_a_conflict() {
    let tmp = TempDir::new("conflict");
    let old_src = tmp.path().join("old-src");
    std::fs::create_dir_all(old_src.join("相同名字")).expect("create old tree");
    std::fs::write(old_src.join("相同名字/内容.txt"), b"old").expect("write old file");
    let iso = tmp.path().join("conflict.iso");
    build_image(
        &old_src,
        &iso,
        &ImageSpec {
            profile: DiscProfile::Dvd,
            volume_id: "CONFLICT".to_string(),
            joliet: true,
        },
    )
    .expect("build image");
    let image = std::fs::read(&iso).expect("read image");
    let old = {
        let mut blocks = MemoryBlocks(image);
        let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
        read_old_session(&mut read, 0, &CancelToken::default()).expect("read old session")
    };

    let src = tmp.path().join("new-src");
    std::fs::create_dir_all(&src).expect("create source dir");
    std::fs::write(src.join("相同名字"), b"a file where a directory was").expect("write file");
    let error = plan_session(Some(old), &src, "CONFLICT".to_string(), 0)
        .expect_err("a file cannot replace a directory");
    assert!(
        matches!(&error, BurnError::GrowConflict(detail)
            if detail == "/相同名字: a new file replaces an existing directory"),
        "{error:?}"
    );
}

#[test]
fn primary_namespace_collision_is_reported() {
    let tmp = TempDir::new("collision");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source dir");
    // 两个中文名在 Level 2 主命名空间里都变成下划线，必须在合并时拦住。
    std::fs::write(src.join("中文甲.txt"), b"a").expect("write first");
    std::fs::write(src.join("中文乙.txt"), b"b").expect("write second");
    let error = plan_session(None, &src, "COLLIDE".to_string(), 0)
        .expect_err("the primary namespace cannot hold both names");
    assert!(
        matches!(&error, BurnError::GrowConflict(detail)
            if detail.contains("primary namespace") && detail.contains("中文甲.txt")),
        "{error:?}"
    );
}

#[test]
fn deep_source_is_refused() {
    let tmp = TempDir::new("deep");
    let mut src = tmp.path().join("deep-src");
    for level in 0..9 {
        src = src.join(format!("d{level}"));
    }
    std::fs::create_dir_all(&src).expect("create deep tree");
    std::fs::write(src.join("deep.txt"), b"deep").expect("write file");
    let top = tmp.path().join("deep-src");
    let error =
        plan_session(None, &top, "DEEP".to_string(), 0).expect_err("nesting is capped at 8 levels");
    assert!(
        matches!(&error, BurnError::GrowUnsupported(detail)
            if detail == "directory nesting deeper than 8 levels"),
        "{error:?}"
    );
}
