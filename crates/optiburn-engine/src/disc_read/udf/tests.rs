//! UDF 读侧测试：自造 UDF-only 镜像（hadris-cd）与两种多区段地址约定的合成夹具。
//!
//! 没有 Windows 写的纯 UDF 实物盘可用，纯 UDF 的“盘级绝对”约定按内核 fs/udf
//! 的语义在测试里合成：见 [`displace_to_disc_absolute`]（ADR-0021）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hadris_cd::{Directory, FileEntry, FileTree, OpticalImageOptions, OpticalImageWriter};
use optiburn_mastering::{DiscProfile, ImageSpec, build_image};

use crate::disc_read::{DiscSession, NativeRead, is_joliet_root, open_session, walk_list};
use crate::readback::ReadBackend;

use super::*;

/// 夹具镜像的卷标。
const FIXTURE_VOLUME_ID: &str = "UDFONLY";

/// 临时目录守卫：建在系统临时目录，析构时删除。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("optiburn-udf-{tag}-{}", std::process::id()));
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

/// 内存块源：给多区段合成夹具用（`disc_read::tests` 的同款是那个模块私有的）。
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

/// 中文名文件的内容。
fn zh_content() -> Vec<u8> {
    "中文内容".as_bytes().to_vec()
}

/// 跨扇区的二进制内容（第二个扇区的字节与第一个不同，能暴露只读首扇区的实现）。
fn bin_content() -> Vec<u8> {
    (0..5000u32).map(|index| (index % 251) as u8).collect()
}

/// 夹具目录树：中文名、空文件、两级目录、跨扇区文件。
fn fixture_tree() -> FileTree {
    let mut tree = FileTree::new();
    tree.add_file(FileEntry::from_buffer("自述.txt", zh_content()));
    tree.add_file(FileEntry::from_buffer("空.txt", Vec::new()));
    let mut docs = Directory::new("资料");
    docs.add_file(FileEntry::from_buffer(
        "说明.md",
        b"top level notes".to_vec(),
    ));
    let mut photos = Directory::new("照片");
    photos.add_file(FileEntry::from_buffer("清单.bin", bin_content()));
    docs.add_subdir(photos);
    tree.add_dir(docs);
    tree
}

/// 夹具树应有的盘上形态：路径到内容（目录记为 `None`）。键序即路径字节序，
/// 与 `list_tree` 的排序口径一致。
fn expected_tree() -> BTreeMap<String, Option<Vec<u8>>> {
    BTreeMap::from([
        ("/空.txt".to_string(), Some(Vec::new())),
        ("/自述.txt".to_string(), Some(zh_content())),
        ("/资料".to_string(), None),
        (
            "/资料/说明.md".to_string(),
            Some(b"top level notes".to_vec()),
        ),
        ("/资料/照片".to_string(), None),
        ("/资料/照片/清单.bin".to_string(), Some(bin_content())),
    ])
}

/// 用 hadris-cd 造一张 UDF-only（UDF 1.02）镜像，写到临时文件。
fn write_udf_only_image(tmp: &TempDir) -> PathBuf {
    let path = tmp.path().join("udf-only.iso");
    let options = OpticalImageOptions::default()
        .volume_id(FIXTURE_VOLUME_ID)
        .udf_only();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create the image file");
    OpticalImageWriter::new(file, options)
        .finish(fixture_tree())
        .expect("write the UDF-only image");
    relocate_vrs(&path);
    path
}

/// 把卷识别序列从逻辑块 19 搬到块 16。
///
/// 上游 hadris-cd 的 `udf_only()` 沿用了 Bridge 的偏移，把卷识别序列写在块 19，
/// 而 ECMA-167 要求它从块 16 起（`udfinfo` 与内核的 udf 驱动都据此定位，2026-10-11
/// 实测：块 19 的布局被两个独立工具拒绝）。夹具按标准形态改位，读侧的扫描覆盖块
/// 16 到 31，两种位置都认得。
fn relocate_vrs(path: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open the fixture for the VRS move");
    let mut vrs = vec![0u8; 3 * SECTOR_BYTES];
    file.seek(SeekFrom::Start(19 * SECTOR_BYTES as u64))
        .expect("seek to the VRS");
    file.read_exact(&mut vrs).expect("read the VRS");
    file.seek(SeekFrom::Start(16 * SECTOR_BYTES as u64))
        .expect("seek to block 16");
    file.write_all(&vrs).expect("write the VRS");
    file.seek(SeekFrom::Start(19 * SECTOR_BYTES as u64))
        .expect("seek back to block 19");
    file.write_all(&vec![0u8; 3 * SECTOR_BYTES])
        .expect("clear the old VRS");
}

/// 造一张 ISO 9660 + Joliet + UDF Bridge 镜像（mastering 的 Dvd profile）。
fn write_bridge_image(tmp: &TempDir) -> PathBuf {
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("子目录")).expect("create source tree");
    std::fs::write(src.join("a.md"), b"alpha notes").expect("write a.md");
    std::fs::write(src.join("子目录/中文.txt"), "中文".as_bytes()).expect("write zh file");
    let image = tmp.path().join("bridge.iso");
    build_image(
        &src,
        &image,
        &ImageSpec {
            profile: DiscProfile::Dvd,
            volume_id: "BRIDGE".to_string(),
            joliet: true,
        },
    )
    .expect("build the bridge image");
    image
}

/// 把整棵 UDF 树读成 路径 -> 内容 的表，顺带钉住“文件内容长度等于条目 size”。
fn read_tree(
    volume: &UdfVolume<UdfSource>,
    _cancel: &CancelToken,
) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(
        volume: &UdfVolume<UdfSource>,
        dir: &UdfDir,
        prefix: &str,
        out: &mut BTreeMap<String, Option<Vec<u8>>>,
    ) {
        for entry in dir.entries() {
            let path = format!("{prefix}/{}", entry.name);
            if entry.is_directory {
                out.insert(path.clone(), None);
                let sub = volume.read_directory(&entry.icb).expect("read a directory");
                walk(volume, &sub, &path, out);
            } else {
                let data = volume.read_file(entry).expect("read a file");
                assert_eq!(
                    data.len() as u64,
                    entry.size,
                    "文件内容长度与目录条目的 size 不一致: {path}"
                );
                out.insert(path, Some(data));
            }
        }
    }

    let root = volume.root_dir().expect("read the UDF root");
    let mut out = BTreeMap::new();
    walk(volume, &root, "", &mut out);
    out
}

/// 把镜像放进一块更大的内存盘：镜像原样落在 `base` 处，返回整盘缓冲。
fn disc_with_image_at(image: &[u8], base: u32) -> Vec<u8> {
    let mut disc = vec![0u8; base as usize * SECTOR_BYTES];
    disc.extend_from_slice(image);
    disc
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn write_u32(bytes: &mut [u8], value: u32) {
    bytes[..4].copy_from_slice(&value.to_le_bytes());
}

fn tag_id(block: &[u8]) -> u16 {
    u16::from_le_bytes([block[0], block[1]])
}

fn tag_location(block: &[u8]) -> u32 {
    read_u32(&block[12..16])
}

fn set_tag_location(block: &mut [u8], location: u32) {
    write_u32(&mut block[12..16], location);
}

/// tag 校验和是否为字节 0 到 3 与 5 到 15 之和（与 hadris-udf 同口径）。
fn refresh_tag_checksum(block: &mut [u8]) {
    block[4] = block[..16]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
}

fn tag_checksum_ok(block: &[u8]) -> bool {
    let computed = block[..16]
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 4)
        .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
    computed == block[4]
}

/// CRC-16-ITU（多项式 0x1021），与 hadris-udf 用来校验描述符 payload 的实现
/// 同口径。
fn crc16_itu(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        let mut x = ((crc >> 8) ^ u16::from(byte)) & 0xFF;
        x ^= x >> 4;
        crc = (crc << 8) ^ (x << 12) ^ (x << 5) ^ x;
    }
    crc
}

/// 改过 payload 的描述符要重算 CRC（hadris-udf 的 `validate_bytes` 会校验）。
fn refresh_crc(block: &mut [u8]) {
    let crc_length = u16::from_le_bytes([block[10], block[11]]) as usize;
    if crc_length == 0 {
        return;
    }
    let crc = crc16_itu(&block[16..16 + crc_length]);
    block[8..10].copy_from_slice(&crc.to_le_bytes());
}

fn crc_ok(block: &[u8]) -> bool {
    let crc_length = u16::from_le_bytes([block[10], block[11]]) as usize;
    if crc_length == 0 {
        return true;
    }
    crc16_itu(&block[16..16 + crc_length]) == u16::from_le_bytes([block[8], block[9]])
}

/// 块数允许时，镜像里可能有 AVDP 的位置：固定位置 256，以及末尾的 N-256 与 N-1。
fn anchor_blocks(blocks: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if blocks > 256 {
        out.push(256);
        out.push(blocks - 256);
    }
    if blocks > 1 {
        out.push(blocks - 1);
    }
    out
}

/// 找第一个 tag 标识与校验和都自洽的块。
fn find_block_with_tag(image: &[u8], id: u16) -> u32 {
    let blocks = image.len() / SECTOR_BYTES;
    (0..blocks)
        .find(|block| {
            let start = block * SECTOR_BYTES;
            tag_id(&image[start..]) == id && tag_checksum_ok(&image[start..])
        })
        .map(|block| block as u32)
        .unwrap_or_else(|| panic!("夹具里没有 tag 标识 {id} 的描述符"))
}

/// 把一张单会话 UDF 镜像的字节转成“盘级绝对”形态（测试夹具）。
///
/// 依据内核 fs/udf 的语义（`super.c` 只在区段起点加 16、加 256、加 512 三处做固定
/// 位置发现，`udf_read_ptagged` 把描述符内容里的地址当物理块用、拿“分区内逻辑块号”
/// 校验分区内描述符的 tag）：卷结构描述符（tag 标识 1 到 9，位于分区之外）的
/// tag_location 与内容里的卷空间地址（AVDP 的两个 extent、PD 的
/// partitionStartingLocation、LVD 的完整性 extent、USD 的分配描述符）都加 `shift`；
/// 分区内描述符（FSD、File Entry、FID）的 tag_location 不动。改过 payload 的重算
/// CRC，改过 tag 的重算 tag 校验和。
fn displace_to_disc_absolute(image: &mut [u8], shift: u32) {
    let blocks = image.len() / SECTOR_BYTES;
    let mut extents: Vec<(u32, u32)> = Vec::new();
    for block in anchor_blocks(blocks) {
        let start = block * SECTOR_BYTES;
        if tag_id(&image[start..]) != AVDP_TAG {
            continue;
        }
        // 先把两个 extent 记下来再位移，否则位移后的 location 找不到 VDS。
        extents.push((
            read_u32(&image[start + 16..]),
            read_u32(&image[start + 20..]),
        ));
        extents.push((
            read_u32(&image[start + 24..]),
            read_u32(&image[start + 28..]),
        ));
        for field in [20usize, 28] {
            let value = read_u32(&image[start + field..]);
            write_u32(&mut image[start + field..], value + shift);
        }
        set_tag_location(&mut image[start..], block as u32 + shift);
        refresh_crc(&mut image[start..]);
        refresh_tag_checksum(&mut image[start..]);
    }

    let mut integrity_location = None;
    for (length, location) in extents {
        let count = (length as usize / SECTOR_BYTES).min(blocks.saturating_sub(location as usize));
        for index in 0..count {
            let block = location as usize + index;
            let start = block * SECTOR_BYTES;
            let id = tag_id(&image[start..]);
            if !(1..=9).contains(&id) {
                continue;
            }
            set_tag_location(&mut image[start..], block as u32 + shift);
            match id {
                // Partition Descriptor：分区的物理起始位置。
                5 => {
                    let value = read_u32(&image[start + 188..]);
                    write_u32(&mut image[start + 188..], value + shift);
                }
                // Logical Volume Descriptor：完整性序列 extent（在分区之外）。
                6 => {
                    let value = read_u32(&image[start + 436..]);
                    write_u32(&mut image[start + 436..], value + shift);
                    integrity_location = Some(value);
                }
                // Unallocated Space Descriptor：各分配描述符的 location。
                7 => {
                    let descriptors = read_u32(&image[start + 20..]) as usize;
                    for index in 0..descriptors.min(60) {
                        let field = start + 24 + index * 8 + 4;
                        let value = read_u32(&image[field..]);
                        write_u32(&mut image[field..], value + shift);
                    }
                }
                _ => {}
            }
            refresh_crc(&mut image[start..]);
            refresh_tag_checksum(&mut image[start..]);
        }
    }

    // 完整性序列在 VDS 之外，单独补它的 tag_location（内容 hadris 不读）。
    if let Some(location) = integrity_location {
        let block = location as usize;
        if block < blocks && tag_id(&image[block * SECTOR_BYTES..]) == 9 {
            let start = block * SECTOR_BYTES;
            set_tag_location(&mut image[start..], block as u32 + shift);
            refresh_tag_checksum(&mut image[start..]);
        }
    }
}

#[test]
fn udf_only_image_volume_id_list_and_extract() {
    let tmp = TempDir::new("udf-only");
    let image = write_udf_only_image(&tmp);
    let cancel = CancelToken::default();

    // 夹具必须是标准形态：卷识别序列从逻辑块 16 起（独立工具 udfinfo 与内核 udf
    // 都按这个位置定位，见 relocate_vrs）。
    let bytes = std::fs::read(&image).expect("read the fixture");
    assert_eq!(
        &bytes[16 * SECTOR_BYTES + 1..16 * SECTOR_BYTES + 6],
        b"BEA01"
    );
    assert_eq!(
        &bytes[17 * SECTOR_BYTES + 1..17 * SECTOR_BYTES + 6],
        b"NSR02"
    );
    assert!(
        bytes[19 * SECTOR_BYTES..22 * SECTOR_BYTES]
            .iter()
            .all(|byte| *byte == 0),
        "原位置的卷识别序列要清掉"
    );

    let volume = open_udf_session(&image, &cancel).expect("open the UDF-only image");
    assert_eq!(volume_id(&volume).unwrap(), FIXTURE_VOLUME_ID);

    // 列举：路径集合（含中文名）与目录 size 为 0。
    let mut listed = list_tree(&volume, &cancel).expect("list the tree");
    listed.sort_by(|a, b| a.path.cmp(&b.path));
    let expected_paths: Vec<String> = expected_tree().keys().cloned().collect();
    assert_eq!(
        listed.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        expected_paths
    );
    for entry in &listed {
        if entry.is_dir {
            assert_eq!(entry.size, 0, "目录不报大小: {}", entry.path);
        }
    }

    // 整树抽取逐字节比对（含空文件）。
    let tree_out = tmp.path().join("tree-out");
    extract_tree(&volume, &tree_out, &cancel).expect("extract the tree");
    assert_eq!(
        std::fs::read(tree_out.join("自述.txt")).unwrap(),
        zh_content()
    );
    assert_eq!(
        std::fs::read(tree_out.join("资料/说明.md")).unwrap(),
        b"top level notes"
    );
    assert_eq!(
        std::fs::read(tree_out.join("资料/照片/清单.bin")).unwrap(),
        bin_content()
    );
    assert_eq!(std::fs::read(tree_out.join("空.txt")).unwrap(), b"");

    // 按路径抽取：一个文件加一个目录，没要的不抽。
    let paths_out = tmp.path().join("paths-out");
    extract_paths(
        &volume,
        &["/自述.txt".to_string(), "/资料".to_string()],
        &paths_out,
        &cancel,
    )
    .expect("extract by path");
    assert_eq!(
        std::fs::read(paths_out.join("自述.txt")).unwrap(),
        zh_content()
    );
    assert_eq!(
        std::fs::read(paths_out.join("资料/照片/清单.bin")).unwrap(),
        bin_content()
    );
    assert!(!paths_out.join("空.txt").exists(), "没要的不抽");
    assert!(matches!(
        extract_paths(&volume, &["/没有这个".to_string()], &paths_out, &cancel),
        Err(BurnError::ReadFailed(_))
    ));
    assert!(matches!(
        extract_paths(&volume, &["/../逃逸".to_string()], &paths_out, &cancel),
        Err(BurnError::UnsafePath(_))
    ));

    // 整树内容与预期表逐字节一致（附带钉住 size 与内容长度一致）。
    assert_eq!(read_tree(&volume, &cancel), expected_tree());

    // 接缝层也走一遍：纯 UDF 盘的读侧入口必须是 UDF 分支，而追加门禁仍把它当
    // “末区段不是 ISO 9660”。
    let source = image.to_str().expect("utf-8 temp path");
    let backend = NativeRead;
    assert_eq!(backend.read_volume_id(source).unwrap(), FIXTURE_VOLUME_ID);
    assert!(!backend.last_session_is_iso(source).unwrap());
    let mut listed = backend.list_tree(source).expect("list through the backend");
    listed.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        listed.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        expected_paths
    );
    let seam_out = tmp.path().join("seam-out");
    backend
        .extract_tree(&image, &seam_out, &cancel)
        .expect("extract through the backend");
    assert_eq!(
        std::fs::read(seam_out.join("资料/照片/清单.bin")).unwrap(),
        bin_content()
    );
}

#[test]
fn bridge_image_takes_the_iso_branch() {
    let tmp = TempDir::new("udf-bridge");
    let image = write_bridge_image(&tmp);
    let cancel = CancelToken::default();

    // Bridge 盘两套文件系统都在，读侧必须走 ISO 分支（与追加门禁同一语义）。
    let mut paths = match open_session(&image, &cancel).expect("open the bridge image") {
        DiscSession::Iso(iso) => {
            let root = iso.root_dir();
            let joliet = is_joliet_root(&root.entry_type());
            let mut entries = Vec::new();
            walk_list(&iso, root.dir_ref(), "", joliet, &mut entries, &cancel).expect("walk");
            entries
                .into_iter()
                .map(|entry| (entry.path, entry.is_dir))
                .collect::<Vec<_>>()
        }
        DiscSession::Udf(_) => panic!("Bridge 镜像必须走 ISO 分支"),
    };
    paths.sort();
    assert_eq!(
        paths,
        vec![
            ("/a.md".to_string(), false),
            ("/子目录".to_string(), true),
            ("/子目录/中文.txt".to_string(), false),
        ]
    );

    // UDF 侧确实可读：iso-first 不是“只有 ISO 能开”的副产物。
    assert!(
        open_udf_session(&image, &cancel).is_ok(),
        "Bridge 镜像的 UDF 侧本身应能打开"
    );
}

#[test]
fn session_relative_multi_session_fixture_reads() {
    const BASE: u32 = 1000;
    let tmp = TempDir::new("udf-relative");
    let image = std::fs::read(write_udf_only_image(&tmp)).expect("read the fixture image");
    let disc = disc_with_image_at(&image, BASE);

    let cancel = CancelToken::default();
    let volume = open_udf_view(
        Box::new(MemoryBlocks(disc.clone())),
        BASE,
        disc.len() as u64,
        &cancel,
    )
    .expect("open the session-relative fixture");
    assert_eq!(volume_id(&volume).unwrap(), FIXTURE_VOLUME_ID);
    assert_eq!(read_tree(&volume, &cancel), expected_tree());
}

#[test]
fn disc_absolute_multi_session_fixture_reads() {
    const BASE: u32 = 4000;
    let tmp = TempDir::new("udf-absolute");
    let image = std::fs::read(write_udf_only_image(&tmp)).expect("read the fixture image");
    let cancel = CancelToken::default();

    // 基准：未位移的镜像 base 0 读一遍。
    let plain = open_udf_view(
        Box::new(MemoryBlocks(image.clone())),
        0,
        image.len() as u64,
        &cancel,
    )
    .expect("open the plain fixture");
    assert_eq!(read_tree(&plain, &cancel), expected_tree());
    drop(plain);

    // 位移成盘级绝对形态，先钉住转换本身：tag_location 与 CRC/校验和都改过，
    // 分区内描述符的 tag_location 不动。
    let mut converted = image.clone();
    displace_to_disc_absolute(&mut converted, BASE);

    let anchor = 256 * SECTOR_BYTES;
    assert_eq!(tag_location(&converted[anchor..]), 256 + BASE);
    assert!(
        tag_checksum_ok(&converted[anchor..]),
        "AVDP 的 tag 校验和要重算"
    );
    assert!(crc_ok(&converted[anchor..]), "AVDP 的 CRC 要重算");
    assert_eq!(
        read_u32(&converted[anchor + 20..]),
        read_u32(&image[anchor + 20..]) + BASE,
        "AVDP 的主 VDS extent 要位移"
    );
    assert_eq!(
        read_u32(&converted[anchor + 28..]),
        read_u32(&image[anchor + 28..]) + BASE,
        "AVDP 的备用 VDS extent 要位移"
    );

    let pd_block = find_block_with_tag(&image, 5) as usize;
    let pd_start = pd_block * SECTOR_BYTES;
    assert_eq!(tag_location(&converted[pd_start..]), pd_block as u32 + BASE);
    assert_eq!(
        read_u32(&converted[pd_start + 188..]),
        read_u32(&image[pd_start + 188..]) + BASE,
        "分区起始位置要位移"
    );
    assert!(
        crc_ok(&converted[pd_start..]),
        "PD 的 CRC 要随 payload 重算"
    );

    let lvd_block = find_block_with_tag(&image, 6) as usize;
    let lvd_start = lvd_block * SECTOR_BYTES;
    assert_eq!(
        read_u32(&converted[lvd_start + 436..]),
        read_u32(&image[lvd_start + 436..]) + BASE,
        "LVD 的完整性序列 extent 要位移"
    );
    assert!(
        crc_ok(&converted[lvd_start..]),
        "LVD 的 CRC 要随 payload 重算"
    );
    // 完整性序列（LVID）本身在 VDS 之外，它的 tag_location 也要位移。
    let integrity = read_u32(&image[lvd_start + 436..]) as usize;
    assert_eq!(
        tag_location(&converted[integrity * SECTOR_BYTES..]),
        integrity as u32 + BASE
    );
    assert!(tag_checksum_ok(&converted[integrity * SECTOR_BYTES..]));
    // Unallocated Space Descriptor 的 tag 与 CRC 同样要自洽（本夹具 0 个分配描述符）。
    let usd_block = find_block_with_tag(&image, 7) as usize;
    let usd_start = usd_block * SECTOR_BYTES;
    assert_eq!(
        tag_location(&converted[usd_start..]),
        usd_block as u32 + BASE
    );
    assert!(crc_ok(&converted[usd_start..]));

    let partition_start = read_u32(&image[pd_start + 188..]) as usize;
    let fsd_rel = read_u32(&image[lvd_block * SECTOR_BYTES + 252..]);
    let fsd_block = partition_start + fsd_rel as usize;
    let fsd_start = fsd_block * SECTOR_BYTES;
    assert_eq!(tag_id(&converted[fsd_start..]), 256, "FSD 的 tag 标识");
    assert_eq!(
        tag_location(&converted[fsd_start..]),
        fsd_rel,
        "分区内描述符的 tag_location 是分区内块号，不位移"
    );

    // 位移版读出同一棵树（同路径、同内容、同 size）。
    let disc = disc_with_image_at(&converted, BASE);
    let volume = open_udf_view(
        Box::new(MemoryBlocks(disc.clone())),
        BASE,
        disc.len() as u64,
        &cancel,
    )
    .expect("open the disc-absolute fixture");
    assert_eq!(volume_id(&volume).unwrap(), FIXTURE_VOLUME_ID);
    assert_eq!(read_tree(&volume, &cancel), expected_tree());
}

#[test]
fn source_seek_end_works_with_the_unknown_length_sentinel() {
    // 设备路径的 len 是哨兵值；hadris 读文件前会 `seek(End(0))` 做越界判断，
    // 哨兵值转 i64 必须仍是正数。
    let mut source = UdfSource::new(
        Box::new(MemoryBlocks(vec![0u8; 4 * SECTOR_BYTES])),
        0,
        UdfMapping::Single,
        UNKNOWN_LENGTH,
        CancelToken::default(),
    );
    assert_eq!(source.seek(SeekFrom::End(0)).unwrap(), UNKNOWN_LENGTH);
    assert_eq!(source.seek(SeekFrom::Start(0)).unwrap(), 0);
}

#[test]
fn missing_vrs_reports_no_session() {
    let cancel = CancelToken::default();
    let disc = vec![0u8; 300 * SECTOR_BYTES];
    let result = open_udf_view(
        Box::new(MemoryBlocks(disc)),
        0,
        300 * SECTOR_BYTES as u64,
        &cancel,
    );
    assert!(
        matches!(result, Err(BurnError::NoIsoSession)),
        "没有 VRS 的合成盘"
    );
}

#[test]
fn vrs_without_anchor_reports_unsupported_udf() {
    let tmp = TempDir::new("udf-no-anchor");
    let mut image = std::fs::read(write_udf_only_image(&tmp)).expect("read the fixture image");
    // VRS 保留，把固定位置 256 的 AVDP 清零（512 处本来就没有锚点）。
    image[256 * SECTOR_BYTES..257 * SECTOR_BYTES].fill(0);

    let cancel = CancelToken::default();
    let result = open_udf_view(
        Box::new(MemoryBlocks(image.clone())),
        0,
        image.len() as u64,
        &cancel,
    );
    assert!(
        matches!(result, Err(BurnError::UnsupportedUdf(_))),
        "VRS 在但锚点不在：是 UDF，结构不受支持"
    );
}
