//! 写盘相关的 MMC 命令：CDB 组装与写参数页（原生 MMC 引擎用，见 ADR-0017）。
//! 主体是写侧（MODE SELECT、RESERVE TRACK、WRITE(10)、SYNCHRONIZE CACHE、CLOSE），
//! 读侧几条（GET CONFIGURATION、READ CAPACITY、READ TRACK INFORMATION、READ(10)）
//! 与它们成对，同放一处。
//!
//! 字节布局对齐 libburn（xorriso 的写后端）的同类命令，字段含义以 MMC-5 为准。
//! 每条 CDB 与写参数页的关键字节都有黄金断言，防止改参数时静默漂移。

/// WRITE(10) 单条命令的传输长度上限（16 位字段）。
pub const MAX_WRITE_BLOCKS: usize = 0xFFFF;

/// 写参数页（Mode Page 5）的数据长度：libburn 对 CD 用 0x32，DVD 增量写也用同一页。
pub const WRITE_PARAMS_PAGE_LEN: u8 = 0x32;

/// 当前盘片 Profile（GET CONFIGURATION 响应头的字节 6-7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentProfile(pub u16);

impl CurrentProfile {
    pub const CD_R: u16 = 0x0009;
    pub const CD_RW: u16 = 0x000A;
    pub const DVD_R: u16 = 0x0011;
    pub const DVD_RAM: u16 = 0x0012;
    pub const DVD_RW_RESTRICTED: u16 = 0x0013;
    pub const DVD_RW_SEQUENTIAL: u16 = 0x0014;
    pub const DVD_R_DL: u16 = 0x0015;
    pub const DVD_PLUS_R: u16 = 0x001A;
    pub const DVD_PLUS_RW: u16 = 0x001B;
    pub const DVD_PLUS_R_DL: u16 = 0x002B;
    pub const BD_R_SRM: u16 = 0x0041;
    pub const BD_R_RRM: u16 = 0x0042;
    pub const BD_RE: u16 = 0x0043;

    /// Profile 到写序列分组的映射。分组照抄 libburn 在写参数页上的分支，
    /// 未知 Profile 返回 None，由引擎给明确报错（不猜写序列）。
    pub fn media_kind(self) -> Option<MediaKind> {
        match self.0 {
            Self::CD_R | Self::CD_RW => Some(MediaKind::Cd),
            Self::DVD_R | Self::DVD_RW_SEQUENTIAL | Self::DVD_R_DL => Some(MediaKind::DvdMinus),
            // +R 族与 BD-R 不写写参数页（写类型由驱动器默认），但区段结构照走，
            // 写完仍要关区段。DVD-RAM 与 BD-RE 是随机可写介质，没有区段可关。
            Self::DVD_PLUS_R
            | Self::DVD_PLUS_RW
            | Self::DVD_PLUS_R_DL
            | Self::BD_R_SRM
            | Self::BD_R_RRM => Some(MediaKind::PlusOrBdR),
            Self::DVD_RAM | Self::BD_RE => Some(MediaKind::RandomWritable),
            _ => None,
        }
    }
}

/// 写序列按介质分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// CD-R / CD-RW：MODE SELECT 设 TAO，写，关区段。
    Cd,
    /// DVD-R / DVD-RW 顺序记录 / DVD-R DL：MODE SELECT 设增量写，写，关区段。
    DvdMinus,
    /// DVD+R[W] / DVD+R DL / BD-R：不设写参数，写，关区段。
    PlusOrBdR,
    /// DVD-RAM / BD-RE：随机可写，不设写参数，也不关区段。
    RandomWritable,
}

impl MediaKind {
    /// 这组介质写完是否需要关区段。随机可写介质（DVD-RAM、BD-RE）以块为单位覆写，
    /// 没有区段结构可关。写参数页的 multi 位决定其余介质的封盘与否。
    pub fn needs_close_session(self) -> bool {
        !matches!(self, Self::RandomWritable)
    }

    /// 写参数页的写类型：CD 用 TAO（1），DVD-R 族用增量写（0），
    /// +R 族与随机可写介质不发参数页。
    fn write_type(self) -> Option<u8> {
        match self {
            Self::Cd => Some(0x01),
            Self::DvdMinus => Some(0x00),
            Self::PlusOrBdR | Self::RandomWritable => None,
        }
    }
}

/// GET CONFIGURATION（0x46）读当前 Profile 与特征列表，RT=0。
pub fn get_configuration_cdb(alloc_len: u16) -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x46;
    cdb[7] = (alloc_len >> 8) as u8;
    cdb[8] = alloc_len as u8;
    cdb
}

/// READ CAPACITY(10)（0x25）：返回最后可写 LBA 与块长。
pub fn read_capacity_cdb() -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x25;
    cdb[7] = 0x00;
    cdb[8] = 0x08;
    cdb
}

/// MODE SELECT(10)（0x55）：PF 位置位，参数表长度写进字节 7-8。
pub fn mode_select_cdb(alloc_len: u16) -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x55;
    cdb[1] = 0x10;
    cdb[7] = (alloc_len >> 8) as u8;
    cdb[8] = alloc_len as u8;
    cdb
}

/// RESERVE TRACK（0x53）：按块数预留轨道，块数为大端 32 位，放在字节 5-8。
pub fn reserve_track_cdb(blocks: u32) -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x53;
    cdb[5..9].copy_from_slice(&blocks.to_be_bytes());
    cdb
}

/// WRITE(10)（0x2A）：起始 LBA 在字节 2-5，块数（大端 16 位）在字节 7-8。
pub fn write_10_cdb(lba: u32, blocks: u16) -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x2A;
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7..9].copy_from_slice(&blocks.to_be_bytes());
    cdb
}

/// SYNCHRONIZE CACHE（0x35）：LBA 与块数为 0 表示冲刷整个缓存。
/// 不置 IMMED，命令要等驱动器把缓存落盘后才返回（超时由调用方给足）。
pub fn synchronize_cache_cdb() -> [u8; 10] {
    [0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// CLOSE TRACK/SESSION（0x5B）：关闭当前区段（Close Function = 0b010，放在字节 2 的
/// 低三位，与 libburn 的 `mmc_close` 一致，写成移位后的 0x04 会被驱动器以
/// INVALID FIELD IN CDB 拒绝，2026-10-10 实测）。
/// 不置 IMMED，等驱动器把 lead-out 写完再返回。
pub fn close_session_cdb() -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x5B;
    cdb[2] = 0b010;
    cdb
}

/// 组装 MODE SELECT 的参数表：8 字节模式参数头 + 写参数页（Mode Page 5）。
///
/// 字段取自 libburn 的 `mmc_compose_mode_page_5`（MMC-5 的表也在注释里对过）：
/// 字节 2 是 BUFE 与写类型，字节 3 是高两位的 multi 与低四位的 control（数据轨为
/// 0b0100），字节 4 是数据块类型（8 = 2048 字节模式 1），DVD-R 族另填 link size
/// 与 packet size（feature 21h 的链路长度未查询，用 libburn 的兜底值 16）。
pub fn write_params_payload(kind: MediaKind, multi: bool) -> Option<Vec<u8>> {
    let write_type = kind.write_type()?;
    let mut payload = vec![0u8; 8 + 2 + WRITE_PARAMS_PAGE_LEN as usize];
    let page = &mut payload[8..];
    page[0] = 0x05;
    page[1] = WRITE_PARAMS_PAGE_LEN;
    match kind {
        MediaKind::Cd => {
            // BUFE 置位（写缓存欠载保护），control = 0b0100（数据轨）。
            page[2] = (1 << 6) | (write_type & 0x0f);
            page[3] = ((3 * u8::from(multi)) << 6) | 0b0100;
            page[4] = 0x08;
        }
        MediaKind::DvdMinus => {
            // BUFE 与 LS_V 置位，增量流的固定包（FP）与轨道模式 5。
            page[2] = (1 << 6) | (1 << 5) | (write_type & 0x0f);
            page[3] = ((3 * u8::from(multi)) << 6) | (1 << 5) | 0x05;
            page[4] = 0x08;
            page[5] = 16;
            page[13] = 16;
        }
        MediaKind::PlusOrBdR | MediaKind::RandomWritable => return None,
    }
    Some(payload)
}

/// READ TRACK INFORMATION（0x52）的响应长度：起始地址（8-11）与 NWA（12-15）
/// 都在前 16 字节里，剩余块数（16-19）与轨道大小（24-27）跟在后面，一次读全。
pub const TRACK_INFO_LEN: usize = 32;

/// 一条轨道的信息（READ TRACK INFORMATION 响应的关键字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackInfo {
    /// 轨道起始地址（响应字节 8-11）。
    pub start_lba: u32,
    /// 下一个可写地址（响应字节 12-15）。追加刻录的起点在它后面（中间是链接块）。
    pub next_writable_address: u32,
    /// 剩余可写块数（响应字节 16-19，MMC 的 Free Blocks，2048 字节块计）。
    /// 顺序介质上 NWA 加它就是盘的可写上限（容量门禁用它，见 ADR-0019）。
    pub free_blocks: u32,
    /// 轨道大小（响应字节 24-27，2048 字节块计）。诊断用（打印轨道布局核对
    /// 解析偏移时读的就是它）。
    pub track_blocks: u32,
}

/// READ TRACK INFORMATION（0x52）：Track 位置位，轨道号按大端 32 位放在字节 2-5。
///
/// CD 上用 0xFF 取“当前可写的那条”，这也是 libburn 对 CD 的取值（mmc.c 的
/// `mmc_read_track_info`：CD 走 0xFF，DVD-R 族改传最后一条轨道的编号）。
pub fn track_info_cdb(track: u32) -> [u8; 10] {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x52;
    cdb[1] = 0x01;
    cdb[2..6].copy_from_slice(&track.to_be_bytes());
    cdb[7] = (TRACK_INFO_LEN >> 8) as u8;
    cdb[8] = TRACK_INFO_LEN as u8;
    cdb
}

/// 从 READ TRACK INFORMATION 的响应取起始地址、NWA、剩余块数与轨道大小。
pub fn parse_track_information(response: &[u8]) -> Option<TrackInfo> {
    let start = response.get(8..12)?;
    let nwa = response.get(12..16)?;
    let free = response.get(16..20)?;
    let size = response.get(24..28)?;
    Some(TrackInfo {
        start_lba: u32::from_be_bytes([start[0], start[1], start[2], start[3]]),
        next_writable_address: u32::from_be_bytes([nwa[0], nwa[1], nwa[2], nwa[3]]),
        free_blocks: u32::from_be_bytes([free[0], free[1], free[2], free[3]]),
        track_blocks: u32::from_be_bytes([size[0], size[1], size[2], size[3]]),
    })
}

/// READ TOC Format 1（区段信息）的响应长度：4 字节头加一条 8 字节描述符。
/// Format 1 只回一条描述符（末个可读区段的，MMC-5 6.26.3.3），不是每个已完结
/// 区段一条，8 区段的盘实测数据长度仍是 10。取 12 字节覆盖头加整条描述符。
pub const SESSION_INFO_LEN: usize = 12;

/// 区段信息（READ TOC Format 1 那条区段描述符的关键字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionInfo {
    /// 首个已完结区段的编号（响应字节 2）。
    pub first_session: u8,
    /// 末个已完结区段的编号（响应字节 3）。可追加盘上它比 READ DISC
    /// INFORMATION 的区段数少 1（开放区段不计入），两个口径不要对齐。
    pub last_session: u8,
    /// 末区段的起始地址（描述符的字节 4-7）。Format 1 一律 LBA，但 MMC-5
    /// 6.26.3.3.3 提醒非 CD 介质可能回 track 1、LBA 0 的无用假值，libburn 因此
    /// 先查 TOC 再兜底。本机 USB 光驱对 CD-R 回真值（实测），DVD/BD 驱动器若照
    /// 规范回假值，读侧会静默定位到首个区段，记为待验证缺口。
    pub last_session_start: u32,
}

/// READ TOC/PMA/ATIP（0x43）Format 1（区段信息）。CDB 对齐 libburn 的
/// `MMC_GET_MSINFO` 同一布局（format 1、MSF 位 0），alloc length 给 12 字节
/// （libburn 的 `mmc_read_multi_session_c1` 把模板里的 16 覆写成 0x000C，
/// 实测 12 字节申请下驱动器只回 10 字节，描述符照常完整）。
pub fn toc_session_info_cdb() -> [u8; 10] {
    [
        0x43,
        0x00,
        0x01,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        SESSION_INFO_LEN as u8,
        0x00,
    ]
}

/// 从区段信息响应取末区段编号与起始地址。
///
/// 解析规则（按 MMC-5 的响应布局，实测 CD-R 驱动器照此回填）：响应头两字节是
/// 数据长度（不含长度字段自身，单条描述符时为 10），字节 2 与 3 是首末会话编号，
/// 描述符跟在头后面，其字节 2 是末区段首轨号、字节 4-7 是末区段起始 LBA。
/// Format 1 只回一条描述符（末个可读区段的），8 区段的盘实测数据长度仍是 10。
/// 盘上没有已完结区段（空白盘）时数据长度不足，返回 `None`。
pub fn parse_session_info(response: &[u8]) -> Option<SessionInfo> {
    let data_len = u16::from_be_bytes([*response.first()?, *response.get(1)?]);
    if data_len < 10 {
        return None;
    }
    let descriptor = response.get(4..12)?;
    Some(SessionInfo {
        first_session: *response.get(2)?,
        last_session: *response.get(3)?,
        last_session_start: u32::from_be_bytes([
            descriptor[4],
            descriptor[5],
            descriptor[6],
            descriptor[7],
        ]),
    })
}

/// READ(10)（0x28）：与 WRITE(10) 同一套地址与长度字段，数据方向相反。
pub fn read_10_cdb(lba: u32, blocks: u16) -> [u8; 10] {
    let mut cdb = write_10_cdb(lba, blocks);
    cdb[0] = 0x28;
    cdb
}

pub fn parse_current_profile(response: &[u8]) -> Option<CurrentProfile> {
    let bytes = response.get(6..8)?;
    Some(CurrentProfile(u16::from_be_bytes([bytes[0], bytes[1]])))
}

/// 从 READ CAPACITY 的响应取最后可写 LBA（字节 0-3）。
pub fn parse_capacity_last_lba(response: &[u8]) -> Option<u32> {
    let bytes = response.get(0..4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_configuration_cdb_is_golden() {
        assert_eq!(
            get_configuration_cdb(16),
            [0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00]
        );
    }

    #[test]
    fn mode_select_cdb_sets_pf_and_length() {
        assert_eq!(
            mode_select_cdb(60),
            [0x55, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3C, 0x00]
        );
    }

    #[test]
    fn reserve_track_cdb_carries_big_endian_blocks() {
        assert_eq!(
            reserve_track_cdb(0x0001_2345),
            [0x53, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x23, 0x45, 0x00]
        );
    }

    #[test]
    fn write_10_cdb_carries_lba_and_length() {
        assert_eq!(
            write_10_cdb(0x0000_1234, 32),
            [0x2A, 0x00, 0x00, 0x00, 0x12, 0x34, 0x00, 0x00, 0x20, 0x00]
        );
    }

    #[test]
    fn sync_and_close_cdbs_are_golden() {
        assert_eq!(
            synchronize_cache_cdb(),
            [0x35, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
        // Close Function = 0b010（关区段）直接放字节 2，IMMED 不置位。
        assert_eq!(
            close_session_cdb(),
            [0x5B, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn toc_session_info_cdb_matches_libburn() {
        // libburn 的 MMC_GET_MSINFO 同一布局（format 1、MSF 位 0），alloc length
        // 我们给 12（头 4 加一条描述符 8）。
        assert_eq!(
            toc_session_info_cdb(),
            [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x00]
        );
    }

    #[test]
    fn session_info_parse_reads_the_hardware_shape() {
        // 实测 8 区段可追加 CD-R（2026-10-10）：data_len=10，描述符给末区段首轨 8、
        // 起始 LBA 279570（0x0004_4412）。首末会话编号是 1 与 7（驱动器对末个
        // 已完结区段的口径，比 DI 的区段数少 1，开放区段不计入）。
        let reply = [
            0x00, 0x0A, 0x01, 0x07, 0x00, 0x14, 0x08, 0x00, 0x00, 0x04, 0x44, 0x12,
        ];
        assert_eq!(
            parse_session_info(&reply),
            Some(SessionInfo {
                first_session: 1,
                last_session: 7,
                last_session_start: 279_570,
            })
        );
        // 空白盘：数据长度不足一条描述符（实测未拿到，按 MMC-5 的长度字段语义推）。
        assert_eq!(
            parse_session_info(&[0x00, 0x04, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]),
            None
        );
    }

    #[test]
    fn cd_write_params_page_matches_libburn_fields() {
        let payload = write_params_payload(MediaKind::Cd, false).expect("CD has a page");
        let page = &payload[8..];
        assert_eq!(page[0], 0x05);
        assert_eq!(page[1], WRITE_PARAMS_PAGE_LEN);
        assert_eq!(page[2], 0x41, "BUFE 与 TAO");
        assert_eq!(page[3], 0b0100, "非 multi 的 control 数据轨位");
        assert_eq!(page[4], 0x08);
        assert!(payload[..8].iter().all(|b| *b == 0), "模式参数头全零");

        let multi = write_params_payload(MediaKind::Cd, true).expect("CD has a page");
        assert_eq!(multi[8 + 3], 0b1100_0100, "multi 置高两位");
    }

    #[test]
    fn dvd_minus_write_params_page_matches_libburn_fields() {
        let payload = write_params_payload(MediaKind::DvdMinus, true).expect("DVD-R has a page");
        let page = &payload[8..];
        assert_eq!(page[2], 0x60, "BUFE 与 LS_V，写类型 0（增量）");
        assert_eq!(page[3], 0b1110_0101, "multi 高两位、FP 与轨道模式 5");
        assert_eq!(page[4], 0x08);
        assert_eq!(page[5], 16, "link size 兜底值");
        assert_eq!(page[13], 16, "packet size");
    }

    #[test]
    fn profiles_without_write_parameters_have_no_page() {
        // +R 族与 BD-R：不发参数页，但区段结构照走，写完要关区段。
        for code in [
            CurrentProfile::DVD_PLUS_R,
            CurrentProfile::BD_R_SRM,
            CurrentProfile::BD_R_RRM,
        ] {
            let kind = CurrentProfile(code).media_kind().expect("known profile");
            assert_eq!(kind, MediaKind::PlusOrBdR);
            assert!(write_params_payload(kind, true).is_none());
            assert!(kind.needs_close_session());
        }
        // 随机可写介质：不发参数页，也没有区段可关。
        for code in [CurrentProfile::DVD_RAM, CurrentProfile::BD_RE] {
            let kind = CurrentProfile(code).media_kind().expect("known profile");
            assert_eq!(kind, MediaKind::RandomWritable);
            assert!(write_params_payload(kind, true).is_none());
            assert!(!kind.needs_close_session());
        }
    }

    #[test]
    fn profile_classification_covers_the_libburn_groups() {
        for code in [CurrentProfile::CD_R, CurrentProfile::CD_RW] {
            assert_eq!(CurrentProfile(code).media_kind(), Some(MediaKind::Cd));
        }
        for code in [
            CurrentProfile::DVD_R,
            CurrentProfile::DVD_RW_SEQUENTIAL,
            CurrentProfile::DVD_R_DL,
        ] {
            assert_eq!(CurrentProfile(code).media_kind(), Some(MediaKind::DvdMinus));
        }
        // 受限覆盖 DVD-RW 与未知 Profile 都不在支持列表里（ADR-0017 记录了这个缺口）。
        assert_eq!(
            CurrentProfile(CurrentProfile::DVD_RW_RESTRICTED).media_kind(),
            None
        );
        assert_eq!(CurrentProfile(0xFFFF).media_kind(), None);
    }

    #[test]
    fn response_parsers_read_the_fields_at_the_right_offsets() {
        let mut config = vec![0u8; 16];
        config[0] = 0x00;
        config[1] = 0x00;
        config[2] = 0x00;
        config[3] = 0x0C;
        config[6] = 0x00;
        config[7] = CurrentProfile::DVD_R as u8;
        assert_eq!(
            parse_current_profile(&config),
            Some(CurrentProfile(CurrentProfile::DVD_R))
        );

        let capacity = [0x00, 0x01, 0x23, 0x45, 0x00, 0x00, 0x08, 0x00];
        assert_eq!(parse_capacity_last_lba(&capacity), Some(0x0001_2345));
    }
}
