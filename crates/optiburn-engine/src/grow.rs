//! 增长模式（嫁接式）：把源目录并进盘上末区段，生成新区段的 ISO 9660 + Joliet
//! 镜像（ADR-0020）。旧文件的数据块原地引用，既不重读也不重写，只重写目录结构与
//! 新文件数据。
//!
//! 地址约定：写进目录记录、路径表与描述符的地址一律是盘级绝对地址（区段起点加
//! 会话内相对块号），只有描述符里的卷空间大小是区段相对值。标准读取器（Linux 内核
//! 的 isofs 与 Windows 的光盘文件系统）把目录记录里的 extent 直接当盘上块号用，
//! xorriso 增长模式写的区段也是这个约定（ADR-0018 实机验证）。镜像文件路径的区段
//! 起点是 0，两种约定重合。
//!
//! 只写 ISO 9660 与 Joliet，不写 UDF：追加不改动盘上原有 UDF 分区，新区段的卷识别
//! 序列也不再提供 UDF 描述符（ADR-0020 记录了取舍）。

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hadris_iso::file::EntryType;
use hadris_iso::joliet::JolietLevel;
use hadris_iso::sync::directory::{DirDateTime, DirectoryRecord, DirectoryRef, FileFlags};
use hadris_iso::sync::io::LogicalSector;

use optiburn_mmc::SECTOR_BYTES;

use crate::{BurnError, CancelToken, NativeGap};

/// 文件数据的读写分块。
const FILE_CHUNK: usize = 64 * 1024;
/// 描述符区的起始逻辑块。
const DESC_START: u32 = 16;
/// 目录递归深度上限（ECMA-119 建议不超过 8 层）。
const MAX_DEPTH: usize = 8;
/// 单条目录记录的定长部分加名字与补位后的长度（与 `DirectoryRecord::new` 一致）。
const fn record_len(name_len: usize) -> usize {
    (name_len + 34) & !1
}

mod old_session;
#[cfg(test)]
mod tests;

pub(crate) use old_session::{DirNode, OldSession, read_old_session};

/// 命名空间：主标识（Level 2 大写名）或 Joliet。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Namespace {
    Primary,
    Joliet,
}

impl Namespace {
    fn directory_id(self, dir: &PlanDir) -> &[u8] {
        match self {
            Self::Primary => &dir.primary_id,
            Self::Joliet => &dir.joliet_id,
        }
    }

    fn file_id(self, file: &PlanFile) -> &[u8] {
        match self {
            Self::Primary => &file.primary_id,
            Self::Joliet => &file.joliet_id,
        }
    }

    fn placement(self, dir: &PlanDir) -> DirPlacement {
        match self {
            Self::Primary => dir.primary,
            Self::Joliet => dir.joliet,
        }
    }

    fn placement_mut(self, dir: &mut PlanDir) -> &mut DirPlacement {
        match self {
            Self::Primary => &mut dir.primary,
            Self::Joliet => &mut dir.joliet,
        }
    }
}

/// 一条目录在某个命名空间里的落点。`extent` 是盘级绝对块号。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DirPlacement {
    extent: u32,
    size: u32,
}

/// 合并后的计划目录。
#[derive(Debug, Clone)]
struct PlanDir {
    /// Joliet 名（根为空串）。
    name: String,
    /// 本目录的 Joliet 标识符（根是单字节 0）。
    joliet_id: Vec<u8>,
    /// 本目录的主命名空间标识符（根是单字节 0）。
    primary_id: Vec<u8>,
    dirs: Vec<PlanDir>,
    files: Vec<PlanFile>,
    primary: DirPlacement,
    joliet: DirPlacement,
}

/// 计划里的一个文件。`source` 为 `None` 表示旧区段里的文件，数据原地引用。
#[derive(Debug, Clone)]
struct PlanFile {
    name: String,
    joliet_id: Vec<u8>,
    primary_id: Vec<u8>,
    /// 盘级绝对块号（旧文件来自旧区段，新文件在布局阶段填写）。
    extent: u32,
    size: u64,
    date: DirDateTime,
    source: Option<PathBuf>,
}

impl PlanFile {
    fn blocks(&self) -> u32 {
        (self.size.div_ceil(SECTOR_BYTES as u64)) as u32
    }
}

/// 布局里的目录顺序（路径表顺序）：根第一，随后按层序，同层按父目录的路径表序号，
/// 同父按标识符字节序。
#[derive(Debug, Clone)]
struct OrderedDir {
    /// 从根到该目录的子目录序号路径，用来在树上找节点。
    path: Vec<usize>,
    /// 父目录的路径表序号（根指向自己）。
    parent: u16,
}

/// 会话的块布局。地址都是盘级绝对块号。
#[derive(Debug, Clone)]
struct PlanLayout {
    primary_l_table: u32,
    primary_m_table: u32,
    joliet_l_table: u32,
    joliet_m_table: u32,
    primary_path_table_size: u32,
    joliet_path_table_size: u32,
    primary_order: Vec<OrderedDir>,
    joliet_order: Vec<OrderedDir>,
}

/// 一次增长会话的完整计划：布局已定，可以算尺寸，也可以生成镜像字节。
#[derive(Debug, Clone)]
pub(crate) struct SessionPlan {
    volume_id: String,
    root: PlanDir,
    layout: PlanLayout,
    /// 会话块数，写进描述符的卷空间大小（区段相对）。
    blocks: u32,
}

impl SessionPlan {
    /// 会话字节数，容量门禁用。
    pub(crate) fn total_bytes(&self) -> u64 {
        u64::from(self.blocks) * SECTOR_BYTES as u64
    }

    /// 生成会话镜像，字节数等于 [`Self::total_bytes`]。
    pub(crate) fn write_image(
        &self,
        sink: &mut dyn Write,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        let mut written = CountingSink {
            inner: sink,
            count: 0,
        };
        written.write_all(&vec![0u8; DESC_START as usize * SECTOR_BYTES])?;
        written.write_all(&self.descriptor(Namespace::Primary))?;
        written.write_all(&self.descriptor(Namespace::Joliet))?;
        written.write_all(&descriptor_terminator())?;
        for namespace in [Namespace::Primary, Namespace::Joliet] {
            for big_endian in [false, true] {
                let mut table =
                    path_table_bytes(&self.root, self.order(namespace), namespace, big_endian);
                table.resize(table.len().div_ceil(SECTOR_BYTES) * SECTOR_BYTES, 0);
                written.write_all(&table)?;
            }
        }
        for namespace in [Namespace::Primary, Namespace::Joliet] {
            for entry in self.order(namespace) {
                written.write_all(&self.directory_data(&entry.path, namespace))?;
            }
        }
        for entry in &self.layout.primary_order {
            let dir = lookup(&self.root, &entry.path);
            for index in sorted_files(dir, Namespace::Joliet) {
                let file = &dir.files[index];
                if let Some(source) = &file.source {
                    copy_file(source, file.size, &mut written, cancel)?;
                }
            }
        }
        let total = self.total_bytes();
        // 尾部补零：零长度新文件不占数据块，extent 落在文件数据之后，布局给它们
        // 留了一块零块（见 layout）。正常只补这一块，其余记账偏差一并兜住。
        if written.count < total {
            written.write_all(&vec![0u8; (total - written.count) as usize])?;
        }
        debug_assert_eq!(written.count, total);
        Ok(())
    }

    fn order(&self, namespace: Namespace) -> &[OrderedDir] {
        match namespace {
            Namespace::Primary => &self.layout.primary_order,
            Namespace::Joliet => &self.layout.joliet_order,
        }
    }

    /// 生成一条卷描述符（PVD 或 Joliet SVD，ECMA-119 8.4 与 8.5 逐字段填充）。
    fn descriptor(&self, namespace: Namespace) -> [u8; SECTOR_BYTES] {
        let mut block = [0u8; SECTOR_BYTES];
        let joliet = namespace == Namespace::Joliet;
        block[0] = if joliet { 2 } else { 1 };
        block[1..6].copy_from_slice(b"CD001");
        block[6] = 1;
        block[8..40].fill(b' ');
        if joliet {
            write_utf16_name(&mut block[40..72], &self.volume_id);
        } else {
            write_d_name(&mut block[40..72], &self.volume_id);
        }
        write_u32_both(&mut block[80..88], u64::from(self.blocks));
        if joliet {
            block[88..91].copy_from_slice(b"%/E");
        }
        write_u16_both(&mut block[120..124], 1);
        write_u16_both(&mut block[124..128], 1);
        write_u16_both(&mut block[128..132], SECTOR_BYTES as u64);
        let (path_table_size, l_table, m_table) = match namespace {
            Namespace::Primary => (
                self.layout.primary_path_table_size,
                self.layout.primary_l_table,
                self.layout.primary_m_table,
            ),
            Namespace::Joliet => (
                self.layout.joliet_path_table_size,
                self.layout.joliet_l_table,
                self.layout.joliet_m_table,
            ),
        };
        write_u32_both(&mut block[132..140], u64::from(path_table_size));
        block[140..144].copy_from_slice(&l_table.to_le_bytes());
        block[148..152].copy_from_slice(&m_table.to_be_bytes());
        let root = namespace.placement(&self.root);
        let record = directory_record(&[0x00], root, FileFlags::DIRECTORY, DirDateTime::now());
        block[156..190].copy_from_slice(record.to_bytes());
        // 卷集标识、出版者、制作者、应用标识：128 字节一段，空格填充。
        block[190..702].fill(b' ');
        block[574..582].copy_from_slice(b"OPTIBURN");
        // 版权、摘要、书目文件标识：37 字节一段。
        block[702..813].fill(b' ');
        let now = utc_now();
        write_dec_datetime(&mut block[813..830], &now);
        write_dec_datetime(&mut block[830..847], &now);
        block[881] = 1;
        block
    }

    /// 生成一条目录的数据（整块补零到布局尺寸）。
    fn directory_data(&self, path: &[usize], namespace: Namespace) -> Vec<u8> {
        let dir = lookup(&self.root, path);
        let placement = namespace.placement(dir);
        let mut out = Vec::with_capacity(placement.size as usize);
        let now = DirDateTime::now();
        let parent = match path.split_last() {
            Some((_, rest)) => namespace.placement(lookup(&self.root, rest)),
            None => placement,
        };
        let dot = directory_record(&[0x00], placement, FileFlags::DIRECTORY, now);
        out.extend_from_slice(dot.to_bytes());
        let dot_dot = directory_record(&[0x01], parent, FileFlags::DIRECTORY, now);
        out.extend_from_slice(dot_dot.to_bytes());
        let mut dirs: Vec<&PlanDir> = dir.dirs.iter().collect();
        dirs.sort_by(|left, right| {
            namespace
                .directory_id(left)
                .cmp(namespace.directory_id(right))
        });
        for child in dirs {
            let record = directory_record(
                namespace.directory_id(child),
                namespace.placement(child),
                FileFlags::DIRECTORY,
                now,
            );
            out.extend_from_slice(record.to_bytes());
        }
        for index in sorted_files(dir, namespace) {
            let file = &dir.files[index];
            let placement = DirPlacement {
                extent: file.extent,
                size: file.size as u32,
            };
            let record = directory_record(
                namespace.file_id(file),
                placement,
                FileFlags::empty(),
                file.date,
            );
            out.extend_from_slice(record.to_bytes());
        }
        debug_assert!(out.len() <= placement.size as usize);
        out.resize(placement.size as usize, 0);
        out
    }
}

/// 带计数的写入端：生成结束时核对写出的字节数与计划一致。
struct CountingSink<'a> {
    inner: &'a mut dyn Write,
    count: u64,
}

impl Write for CountingSink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.count += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// 合并与校验
// ---------------------------------------------------------------------------

/// 按 `src` 目录与盘上旧树合并出会话计划。`base_lba` 是新区段的起点。
pub(crate) fn plan_session(
    old: Option<OldSession>,
    src: &Path,
    volume_id: String,
    base_lba: u32,
) -> Result<SessionPlan, BurnError> {
    let mut root = match old {
        Some(session) => from_old(session.root),
        None => PlanDir::root(),
    };
    let mut any_file = false;
    merge_fs(&mut root, src, "/", 1, &mut any_file)?;
    if !any_file {
        return Err(BurnError::NativeGap(NativeGap::EmptyGrowSource));
    }
    validate_dir(&root, "/")?;
    SessionPlan::layout(root, volume_id, base_lba)
}

/// 旧树转成计划树：旧条目的名字按本仓库的命名规则重新编码（Joliet 为 UTF-16BE、
/// 最多 64 个码元；主命名空间为 Level 2 大写名）。
fn from_old(node: DirNode) -> PlanDir {
    let mut dir = match node.name.is_empty() {
        true => PlanDir::root(),
        false => PlanDir::child(&node.name),
    };
    for child in node.dirs {
        dir.dirs.push(from_old(child));
    }
    for file in node.files {
        dir.files.push(PlanFile {
            joliet_id: joliet_id(&file.name),
            primary_id: primary_file_id(&file.name),
            name: file.name,
            extent: file.extent,
            size: file.size,
            date: file.date,
            source: None,
        });
    }
    dir
}

/// 把源目录的内容并进计划树。符号链接与特殊文件跳过，与 `build_image` 的
/// `FileTree::from_fs` 一致。`path` 是 `dir` 的显示路径（根是 `/`）。
fn merge_fs(
    dir: &mut PlanDir,
    src: &Path,
    path: &str,
    depth: usize,
    any_file: &mut bool,
) -> Result<(), BurnError> {
    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        let is_dir = metadata.is_dir();
        if !is_dir && !metadata.is_file() {
            continue;
        }
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            path,
            is_dir,
        ));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, path_buf, is_dir) in entries {
        if is_dir {
            if depth + 1 > MAX_DEPTH {
                return Err(unsupported("directory nesting deeper than 8 levels"));
            }
            let child_here = child_path(path, &name);
            match dir.dirs.iter_mut().find(|child| child.name == name) {
                Some(child) => merge_fs(child, &path_buf, &child_here, depth + 1, any_file)?,
                None => {
                    if dir.files.iter().any(|file| file.name == name) {
                        return Err(conflict(format!(
                            "{child_here}: a new directory replaces an existing file"
                        )));
                    }
                    let mut child = PlanDir::child(&name);
                    merge_fs(&mut child, &path_buf, &child_here, depth + 1, any_file)?;
                    dir.dirs.push(child);
                }
            }
            continue;
        }
        *any_file = true;
        if dir.dirs.iter().any(|child| child.name == name) {
            return Err(conflict(format!(
                "{}: a new file replaces an existing directory",
                child_path(path, &name)
            )));
        }
        let size = std::fs::metadata(&path_buf)?.len();
        if size > u64::from(u32::MAX) {
            return Err(unsupported(&format!(
                "the new file {} is larger than 4 GiB",
                child_path(path, &name)
            )));
        }
        let file = PlanFile {
            joliet_id: joliet_id(&name),
            primary_id: primary_file_id(&name),
            name: name.clone(),
            extent: 0,
            size,
            date: DirDateTime::now(),
            source: Some(path_buf),
        };
        match dir.files.iter_mut().find(|existing| existing.name == name) {
            Some(existing) => *existing = file,
            None => dir.files.push(file),
        }
    }
    Ok(())
}

/// 合并后的校验：两个命名空间内标识符各自唯一。
fn validate_dir(dir: &PlanDir, path: &str) -> Result<(), BurnError> {
    for namespace in [Namespace::Joliet, Namespace::Primary] {
        let mut seen: Vec<(&[u8], &str)> = Vec::new();
        let children = dir
            .dirs
            .iter()
            .map(|child| (namespace.directory_id(child), child.name.as_str()))
            .chain(
                dir.files
                    .iter()
                    .map(|file| (namespace.file_id(file), file.name.as_str())),
            );
        for (id, name) in children {
            if let Some((_, other)) = seen.iter().find(|(seen_id, _)| *seen_id == id) {
                let namespace = match namespace {
                    Namespace::Primary => "primary",
                    Namespace::Joliet => "Joliet",
                };
                return Err(conflict(format!(
                    "name collision in the {namespace} namespace under {path}: {other} and {name}"
                )));
            }
            seen.push((id, name));
        }
    }
    for child in &dir.dirs {
        validate_dir(child, &child_path(path, &child.name))?;
    }
    Ok(())
}

impl PlanDir {
    fn root() -> Self {
        Self {
            name: String::new(),
            joliet_id: vec![0],
            primary_id: vec![0],
            dirs: Vec::new(),
            files: Vec::new(),
            primary: DirPlacement::default(),
            joliet: DirPlacement::default(),
        }
    }

    fn child(name: &str) -> Self {
        Self {
            name: name.to_string(),
            joliet_id: joliet_id(name),
            primary_id: primary_directory_id(name),
            dirs: Vec::new(),
            files: Vec::new(),
            primary: DirPlacement::default(),
            joliet: DirPlacement::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// 布局与生成
// ---------------------------------------------------------------------------

impl SessionPlan {
    /// 定布局：先把两个命名空间的目录尺寸算出来，再依次分配路径表、两套目录数据与
    /// 新文件数据的块号。
    fn layout(mut root: PlanDir, volume_id: String, base_lba: u32) -> Result<Self, BurnError> {
        let primary_order = path_table_order(&root, Namespace::Primary);
        let joliet_order = path_table_order(&root, Namespace::Joliet);
        assign_sizes(&mut root);

        let primary_path_table_size =
            path_table_bytes(&root, &primary_order, Namespace::Primary, false).len() as u32;
        let joliet_path_table_size =
            path_table_bytes(&root, &joliet_order, Namespace::Joliet, false).len() as u32;
        let mut cursor = DESC_START + 3;
        let reserve = |size: u32, cursor: &mut u32| {
            let start = *cursor;
            *cursor += size.div_ceil(SECTOR_BYTES as u32);
            start
        };
        let primary_l_table = reserve(primary_path_table_size, &mut cursor);
        let primary_m_table = reserve(primary_path_table_size, &mut cursor);
        let joliet_l_table = reserve(joliet_path_table_size, &mut cursor);
        let joliet_m_table = reserve(joliet_path_table_size, &mut cursor);

        for namespace in [Namespace::Primary, Namespace::Joliet] {
            let order = match namespace {
                Namespace::Primary => primary_order.clone(),
                Namespace::Joliet => joliet_order.clone(),
            };
            for entry in &order {
                let dir = lookup_mut(&mut root, &entry.path);
                let size = dir_size(dir, namespace);
                let extent = absolute(base_lba, cursor)?;
                let placement = namespace.placement_mut(dir);
                placement.extent = extent;
                placement.size = size;
                cursor += size.div_ceil(SECTOR_BYTES as u32);
            }
        }

        // 新文件数据：目录按主命名空间的路径表顺序，同目录内按 Joliet 标识符升序。
        let mut zero_length_new_file = false;
        for entry in primary_order.clone() {
            let dir = lookup_mut(&mut root, &entry.path);
            for index in sorted_files(dir, Namespace::Joliet) {
                let file = &mut dir.files[index];
                if file.source.is_none() {
                    continue;
                }
                if file.size == 0 {
                    zero_length_new_file = true;
                }
                file.extent = absolute(base_lba, cursor)?;
                cursor += file.blocks();
            }
        }
        // 零长度新文件的 extent 落在文件数据末尾之后会越过卷尾，末尾补一块零块兜住。
        let blocks = cursor + u32::from(zero_length_new_file);

        Ok(Self {
            volume_id,
            root,
            layout: PlanLayout {
                primary_l_table: absolute(base_lba, primary_l_table)?,
                primary_m_table: absolute(base_lba, primary_m_table)?,
                joliet_l_table: absolute(base_lba, joliet_l_table)?,
                joliet_m_table: absolute(base_lba, joliet_m_table)?,
                primary_path_table_size,
                joliet_path_table_size,
                primary_order,
                joliet_order,
            },
            blocks,
        })
    }
}

/// 会话内相对块号到盘级绝对块号。
fn absolute(base_lba: u32, block: u32) -> Result<u32, BurnError> {
    base_lba
        .checked_add(block)
        .ok_or_else(|| unsupported("the new session is beyond the addressable 2048-byte blocks"))
}

/// 目录数据的字节数：两条自引用记录加各子条目，向上取整到块，最小一块。
fn dir_size(dir: &PlanDir, namespace: Namespace) -> u32 {
    let mut bytes = 2 * record_len(1);
    for child in &dir.dirs {
        bytes += record_len(namespace.directory_id(child).len());
    }
    for file in &dir.files {
        bytes += record_len(namespace.file_id(file).len());
    }
    bytes.div_ceil(SECTOR_BYTES).max(1) as u32 * SECTOR_BYTES as u32
}

/// 递归算两个命名空间的目录尺寸（父目录的尺寸要用到子目录的尺寸）。
fn assign_sizes(dir: &mut PlanDir) {
    for child in &mut dir.dirs {
        assign_sizes(child);
    }
    dir.primary.size = dir_size(dir, Namespace::Primary);
    dir.joliet.size = dir_size(dir, Namespace::Joliet);
}

/// 目录顺序（路径表顺序）：根第一，随后按层序，同层按父目录的路径表序号升序，
/// 同父按标识符字节序升序。
fn path_table_order(root: &PlanDir, namespace: Namespace) -> Vec<OrderedDir> {
    let mut order = vec![OrderedDir {
        path: Vec::new(),
        parent: 1,
    }];
    let mut queue: VecDeque<(Vec<usize>, u16)> = VecDeque::new();
    queue.push_back((Vec::new(), 1));
    while let Some((path, number)) = queue.pop_front() {
        let dir = lookup(root, &path);
        let mut children: Vec<(usize, &[u8])> = dir
            .dirs
            .iter()
            .enumerate()
            .map(|(index, child)| (index, namespace.directory_id(child)))
            .collect();
        children.sort_by(|left, right| left.1.cmp(right.1));
        for (index, _) in children {
            let mut child_path = path.clone();
            child_path.push(index);
            let child_number = order.len() as u16 + 1;
            order.push(OrderedDir {
                path: child_path.clone(),
                parent: number,
            });
            queue.push_back((child_path, child_number));
        }
    }
    order
}

/// 按路径取节点。
fn lookup<'a>(root: &'a PlanDir, path: &[usize]) -> &'a PlanDir {
    let mut node = root;
    for index in path {
        node = &node.dirs[*index];
    }
    node
}

/// 按路径取节点（可写）。
fn lookup_mut<'a>(root: &'a mut PlanDir, path: &[usize]) -> &'a mut PlanDir {
    let mut node = root;
    for index in path {
        node = &mut node.dirs[*index];
    }
    node
}

/// 目录内文件的排序：按本命名空间标识符字节序。
fn sorted_files(dir: &PlanDir, namespace: Namespace) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..dir.files.len()).collect();
    indices.sort_by(|left, right| {
        namespace
            .file_id(&dir.files[*left])
            .cmp(namespace.file_id(&dir.files[*right]))
    });
    indices
}

/// 生成一张路径表（ECMA-119 9.4）：长度、扩展属性长度 0、extent 四字节、父目录号
/// 两字节、标识符，标识符长度为奇数时补一个 0 字节（记录总长恒为偶数）。
fn path_table_bytes(
    root: &PlanDir,
    order: &[OrderedDir],
    namespace: Namespace,
    big_endian: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in order {
        let dir = lookup(root, &entry.path);
        let id = namespace.directory_id(dir);
        let extent = namespace.placement(dir).extent;
        out.push(id.len() as u8);
        out.push(0);
        if big_endian {
            out.extend_from_slice(&extent.to_be_bytes());
            out.extend_from_slice(&entry.parent.to_be_bytes());
        } else {
            out.extend_from_slice(&extent.to_le_bytes());
            out.extend_from_slice(&entry.parent.to_le_bytes());
        }
        out.extend_from_slice(id);
        if id.len() % 2 == 1 {
            out.push(0);
        }
    }
    out
}

/// 终止符（类型 255）。
fn descriptor_terminator() -> [u8; SECTOR_BYTES] {
    let mut block = [0u8; SECTOR_BYTES];
    block[0] = 255;
    block[1..6].copy_from_slice(b"CD001");
    block[6] = 1;
    block
}

/// 一条目录记录。
fn directory_record(
    name: &[u8],
    placement: DirPlacement,
    flags: FileFlags,
    date: DirDateTime,
) -> DirectoryRecord {
    let mut record = DirectoryRecord::new(
        name,
        &[],
        DirectoryRef {
            extent: LogicalSector(placement.extent as usize),
            size: placement.size as usize,
        },
        flags,
    );
    record.header_mut().date_time = date;
    record
}

/// 主命名空间（Level 2 大写名）与 Joliet 的标识符。命名转换一律用上游实现：
/// 越界字符替换与截断规则都在 hadris-iso 里，与 `build_image` 的镜像一致。
fn primary_entry_type() -> EntryType {
    EntryType::Level2 {
        supports_lowercase: false,
        supports_rrip: false,
    }
}

fn joliet_entry_type() -> EntryType {
    EntryType::Joliet {
        level: JolietLevel::Level3,
        supports_rrip: false,
    }
}

fn primary_file_id(name: &str) -> Vec<u8> {
    primary_entry_type().convert_name(name).as_bytes().to_vec()
}

fn primary_directory_id(name: &str) -> Vec<u8> {
    primary_entry_type()
        .convert_directory_name(name)
        .as_bytes()
        .to_vec()
}

fn joliet_id(name: &str) -> Vec<u8> {
    joliet_entry_type().convert_name(name).as_bytes().to_vec()
}

/// 目录的显示路径：根是 `/`。
fn child_path(parent: &str, name: &str) -> String {
    match parent {
        "/" => format!("/{name}"),
        "" => format!("/{name}"),
        other => format!("{other}/{name}"),
    }
}

/// 写小端加大端 32 位字段（ECMA-119 的冗余表示）。
fn write_u32_both(field: &mut [u8], value: u64) {
    let value = value as u32;
    field[..4].copy_from_slice(&value.to_le_bytes());
    field[4..8].copy_from_slice(&value.to_be_bytes());
}

/// 写小端加大端 16 位字段。
fn write_u16_both(field: &mut [u8], value: u64) {
    let value = value as u16;
    field[..2].copy_from_slice(&value.to_le_bytes());
    field[2..4].copy_from_slice(&value.to_be_bytes());
}

/// PVD 的 d 字符串卷标：UTF-8 字节截断到字段长度，其余补空格。
fn write_d_name(field: &mut [u8], name: &str) {
    field.fill(b' ');
    let bytes = name.as_bytes();
    let len = bytes.len().min(field.len());
    field[..len].copy_from_slice(&bytes[..len]);
}

/// Joliet 的卷标：UTF-16BE 截断到字段容量（字段 32 字节即 16 个码元），其余补空格。
fn write_utf16_name(field: &mut [u8], name: &str) {
    let (units, _) = field.as_chunks_mut::<2>();
    let capacity = units.len();
    for unit in units.iter_mut() {
        unit.copy_from_slice(&0x0020u16.to_be_bytes());
    }
    for (index, character) in name.chars().take(capacity).enumerate() {
        let unit = u16::try_from(character as u32).unwrap_or(b'_' as u16);
        units[index].copy_from_slice(&unit.to_be_bytes());
    }
}

/// 描述符的日期时间字段（17 字节）：ASCII `YYYYMMDDHHMMSSCC` 加时区字节。
fn write_dec_datetime(field: &mut [u8], time: &CivilTime) {
    let text = format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}00",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    );
    field[..16].copy_from_slice(text.as_bytes());
    field[16] = 0;
}

/// 当前的 UTC 时刻（只用到年月日时分秒）。
#[derive(Debug, Clone, Copy)]
struct CivilTime {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

fn utc_now() -> CivilTime {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    CivilTime {
        year,
        month,
        day,
        hour: (rest / 3600) as u32,
        minute: (rest % 3600 / 60) as u32,
        second: (rest % 60) as u32,
    }
}

/// 1970-01-01 起的天数换算成公历年月日（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// 把一个文件按布局尺寸写进镜像，不足整块补零。
fn copy_file(
    source: &Path,
    size: u64,
    sink: &mut dyn Write,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let mut file = File::open(source)?;
    let mut buffer = vec![0u8; FILE_CHUNK];
    let mut remaining = size;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let want = FILE_CHUNK.min(remaining as usize);
        file.read_exact(&mut buffer[..want]).map_err(|error| {
            // 计划时的文件长度与现在不一致（文件在写入前被截断）按 io 错误上报。
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                std::io::Error::other(format!(
                    "{} is shorter than when the session was planned",
                    source.display()
                ))
            } else {
                error
            }
        })?;
        sink.write_all(&buffer[..want])?;
        remaining -= want as u64;
    }
    let padding = (size.div_ceil(SECTOR_BYTES as u64) * SECTOR_BYTES as u64) - size;
    sink.write_all(&vec![0u8; padding as usize])?;
    Ok(())
}

fn unsupported(detail: &str) -> BurnError {
    BurnError::GrowUnsupported(detail.to_string())
}

fn conflict(detail: String) -> BurnError {
    BurnError::GrowConflict(detail)
}
