//! MMC 命令层：把读侧与写侧的 MMC 命令编码成 CDB，并把响应解析成结构化数据。
//!
//! 依赖 [`optiburn_transport::ScsiTransport`] 这一个硬件接缝，因此可以在没有光驱的
//! 机器上用替身完整测试。读侧有 INQUIRY、TEST UNIT READY、READ DISC INFORMATION。
//! 写侧（GET CONFIGURATION、MODE SELECT 写参数页、RESERVE TRACK、WRITE(10)、
//! SYNCHRONIZE CACHE、CLOSE TRACK/SESSION）在 [`write`] 模块里，服务原生 MMC 引擎
//! （ADR-0017）。

use std::thread::sleep;
use std::time::{Duration, Instant};

use optiburn_transport::{Direction, ScsiTransport, TransportError};

mod write;

pub use write::{CurrentProfile, MAX_WRITE_BLOCKS, MediaKind, SessionInfo, TrackInfo};

/// 数据扇区的字节数。写侧与镜像都按这个块长对齐（ISO 9660 的逻辑块）。
pub const SECTOR_BYTES: usize = 2048;

/// 单条 READ(10) 最多带的块数：SPTI 的单次传输上限在 64 KiB 这个量级，
/// 统一按它切块（见 [`MmcDevice::read_blocks`] 的实测记录）。
pub const MAX_READ_BLOCKS: usize = 32;

/// 只读命令的超时：盘片寻道与转速切换都在这个量级内完成。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// 写命令超时：单条 WRITE(10) 最大 2 MiB，慢速介质也够用（libburn 取 200 秒）。
const WRITE_TIMEOUT: Duration = Duration::from_secs(200);
/// RESERVE TRACK 超时：驱动器要先做写入参数协商。
const RESERVE_TIMEOUT: Duration = Duration::from_secs(200);
/// SYNCHRONIZE CACHE 与 CLOSE 不置 IMMED，命令要等缓存落盘与 lead-out 写完，
/// 整盘封口在慢速介质上可能要一分钟以上。
const LONG_OP_TIMEOUT: Duration = Duration::from_secs(300);

/// INQUIRY 标准响应的长度（`SPC`：附加长度字段 + 8 + 16 + 4 字节）。
const INQUIRY_LEN: usize = 36;
/// READ DISC INFORMATION 标准响应（Data Type 000b）的长度。
const DISC_INFORMATION_LEN: usize = 34;
/// READ FORMAT CAPACITIES 的请求长度：4 字节响应头加 3 个 8 字节容量描述符。
/// 驱动器通常只报前两三个描述符，多请求的部分按协议零填充或计入 residual。
const FORMAT_CAPACITIES_LEN: usize = 28;
/// GET CONFIGURATION 读取长度：8 字节响应头加 8 字节特征描述符就够取当前 Profile。
const CONFIGURATION_LEN: usize = 16;
/// READ CAPACITY(10) 的响应长度：最后 LBA 与块长各 4 字节。
const CAPACITY_LEN: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum MmcError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("device returned {got} bytes, need {need}")]
    ShortResponse { got: usize, need: usize },
    #[error("device response too short to parse")]
    MalformedResponse,
    #[error("{len} bytes is not a whole number of 2048-byte blocks")]
    MisalignedBlocks { len: usize },
    #[error("{blocks} blocks exceed the transfer limit of {max} blocks per command")]
    TooManyBlocks { blocks: usize, max: usize },
    #[error("device not ready within timeout")]
    NotReady,
}

/// 设备的 INQUIRY 标识字段。尾部填充的空格与 NUL 已去掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inquiry {
    pub vendor: String,
    pub product: String,
    pub revision: String,
}

/// 盘片状态，对应 READ DISC INFORMATION 响应字节 2 的低 2 位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscStatus {
    /// 空盘。
    Empty,
    /// 已写但未封口，可以继续追加区段。
    Appendable,
    /// 已封口。
    Finalized,
    /// 随机可写介质（MMC 状态位 0b11，例如 DVD-RAM、BD-RE）。
    Other(u8),
}

impl DiscStatus {
    fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0 => Self::Empty,
            1 => Self::Appendable,
            2 => Self::Finalized,
            other => Self::Other(other),
        }
    }
}

/// READ DISC INFORMATION 标准响应里的关键字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscInformation {
    pub status: DiscStatus,
    /// 区段数（响应字节 4 的低 8 位，高 8 位在字节 9，实际盘片不会超过 99）。
    pub sessions: u8,
    /// 响应字节 3：盘上首轨号（第一个轨道的编号，通常为 1）。
    pub first_track: u8,
    /// 响应字节 5：末区段的首轨号。
    ///
    /// 可追加盘上它指向尚未写入的开放区段（实测 2026-10-10，CD-R：盘上 8 条轨道，
    /// 它给 9，即 NWA 处的隐形轨道，读那里会落在空区），末区段定位不能用它，
    /// 读侧走 [`MmcDevice::read_toc_session_info`]。
    pub last_session_first_track: u8,
}

/// READ FORMAT CAPACITIES 解析出的格式化容量，字节口径（ADR-0019）。
///
/// 首条描述符（MMC-5 6.24.3.2 的 Current/Maximum Capacity Descriptor）字节 4
/// 的低 2 位是描述符类型：1 未格式化介质（数值是最大可格式化容量），2 已格式化
/// 介质（数值是当前格式化容量），3 无介质或容量未知。字节 5–7 是类型相关参数，
/// 不是块长，块数一律乘 2048 字节（libburn 同口径）。后续描述符是 Formattable
/// Capacity Descriptor（字节 4 的高 6 位是格式类型），不进容量口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FormatCapacity {
    /// 未格式化介质可格式化到的最大容量（描述符类型 1）。
    pub max_formattable: Option<u64>,
    /// 已格式化介质的当前格式化容量（描述符类型 2）。只作总容量退回，不参与
    /// 可用容量口径（一次写介质上它是已写成型的范围，不是剩余空间）。
    pub formatted: Option<u64>,
}

/// 盘片容量：总容量与可用容量（字节），读不到的口径为 None（ADR-0019）。
///
/// 可用容量优先取 READ TRACK INFORMATION 的剩余块数，退回 READ FORMAT
/// CAPACITIES 时只有未格式化介质能给出可用容量。规则见
/// [`MmcDevice::read_disc_capacity`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscCapacity {
    /// 介质总容量（字节）。
    pub total: Option<u64>,
    /// 还能写入的容量（字节），写前容量门禁的比较基准。
    pub free: Option<u64>,
}

/// 一条盘驱动器通道，持有已经打开的 [`ScsiTransport`]。
pub struct MmcDevice {
    transport: Box<dyn ScsiTransport>,
}

impl MmcDevice {
    /// 用一条已打开的传输通道建一台盘驱动器。
    pub fn new(transport: Box<dyn ScsiTransport>) -> Self {
        Self { transport }
    }

    /// INQUIRY（op 0x12）：厂商、型号、固件版本。
    pub fn inquiry(&mut self) -> Result<Inquiry, MmcError> {
        let data = self.read_into(&[0x12, 0, 0, 0, INQUIRY_LEN as u8, 0], INQUIRY_LEN)?;
        Ok(Inquiry {
            vendor: decode_ascii(&data[8..16]),
            product: decode_ascii(&data[16..32]),
            revision: decode_ascii(&data[32..36]),
        })
    }

    /// TEST UNIT READY（op 0x00）：设备就绪且介质可访问。
    pub fn test_unit_ready(&mut self) -> Result<(), MmcError> {
        self.transport.issue(
            &[0x00, 0, 0, 0, 0, 0],
            Direction::None,
            &mut [],
            DEFAULT_TIMEOUT,
        )?;
        Ok(())
    }

    /// READ DISC INFORMATION（op 0x51，Data Type 0）：盘片状态与区段数。
    pub fn read_disc_information(&mut self) -> Result<DiscInformation, MmcError> {
        let cdb = [
            0x51,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            (DISC_INFORMATION_LEN >> 8) as u8,
            DISC_INFORMATION_LEN as u8,
            0x00,
        ];
        let data = self.read_into(&cdb, DISC_INFORMATION_LEN)?;
        Ok(DiscInformation {
            status: DiscStatus::from_bits(data[2]),
            sessions: data[4],
            first_track: data[3],
            last_session_first_track: data[5],
        })
    }

    /// READ FORMAT CAPACITIES（op 0x23）：介质可格式化到的容量（ADR-0019）。
    ///
    /// 响应是变长列表，驱动器可以少给：实机（HL-DT-ST GP70N）对 28 字节请求
    /// 只回 12 字节并计 16 字节 residual。因此不走 [`Self::read_into`] 的满长
    /// 判定，按实际返回长度解析，有效区间再由头部列表长度圈定。
    pub fn read_format_capacities(&mut self) -> Result<FormatCapacity, MmcError> {
        let cdb = [
            0x23,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            (FORMAT_CAPACITIES_LEN >> 8) as u8,
            FORMAT_CAPACITIES_LEN as u8,
            0x00,
        ];
        let mut data = vec![0u8; FORMAT_CAPACITIES_LEN];
        let completion =
            self.transport
                .issue(&cdb, Direction::FromDevice, &mut data, DEFAULT_TIMEOUT)?;
        // residual 是“未传送的字节数”；Windows 的 SPTI 不回传 residual（恒为 0），
        // 那里 data 按满长解释，多出的部分是零，不影响按列表长度圈出的解析。
        let written =
            FORMAT_CAPACITIES_LEN.saturating_sub(completion.residual.min(FORMAT_CAPACITIES_LEN));
        data.truncate(written);
        Ok(parse_format_capacities(&data))
    }

    /// 读一次盘片容量（[`DiscCapacity`]）：总容量与可用容量（字节）。
    ///
    /// 可用容量优先取 READ TRACK INFORMATION（op 0x52）的剩余块数：顺序介质
    /// （CD-R/RW、DVD±R、BD-R）上下一可写地址加剩余块数就是盘的可写上限，即
    /// 总容量（libburn 的 `media_lba_limit` 同口径，实测与 xorriso 读出的整体
    /// 容量一致）。轨道号用 0xFF（MMC 对 CD 与 DVD+R 族的取值），驱动器不认时
    /// 退回 READ FORMAT CAPACITIES：类型 1 的最大可格式化容量同时是总容量与
    /// 可用容量（未格式化介质整盘待写），类型 2 的当前格式化容量只作总容量，
    /// 不参与门禁（一次写介质上它是已写范围，不是剩余空间）。两条路都读不到
    /// 时两个口径都是 None，由调用方决定跳过显示与门禁。
    pub fn read_disc_capacity(&mut self) -> DiscCapacity {
        let track = self.read_track_information(0xFF).ok();
        let formats = self.read_format_capacities().ok();
        disc_capacity_of(track.as_ref(), formats.as_ref())
    }

    /// GET CONFIGURATION（0x46）：当前 Profile。写序列按它分组（见 [`MediaKind`]）。
    pub fn get_configuration(&mut self) -> Result<CurrentProfile, MmcError> {
        let data = self.read_into(
            &write::get_configuration_cdb(CONFIGURATION_LEN as u16),
            CONFIGURATION_LEN,
        )?;
        write::parse_current_profile(&data).ok_or(MmcError::MalformedResponse)
    }

    /// READ CAPACITY(10)（0x25）：最后可写 LBA。介质不报告容量时返回错误，
    /// 由调用方决定这次写入是否必须知道容量。
    pub fn read_capacity(&mut self) -> Result<u32, MmcError> {
        let data = self.read_into(&write::read_capacity_cdb(), CAPACITY_LEN)?;
        write::parse_capacity_last_lba(&data).ok_or(MmcError::MalformedResponse)
    }

    /// READ TRACK INFORMATION（0x52）：轨道起始地址与下一个可写地址（NWA）。
    /// `track` 用 0xFF 取 CD 上“当前可写的那条”（libburn 对 CD 的取值）。
    pub fn read_track_information(&mut self, track: u32) -> Result<TrackInfo, MmcError> {
        let data = self.read_into(&write::track_info_cdb(track), write::TRACK_INFO_LEN)?;
        write::parse_track_information(&data).ok_or(MmcError::MalformedResponse)
    }

    /// READ TOC/PMA/ATIP（0x43）Format 1：区段信息，末区段起始地址在唯一一条
    /// 区段描述符里。读侧用它定位末区段（[`DiscInformation::last_session_first_track`]
    /// 为什么不能用见那里的说明）。盘上没有已完结区段（空白盘）时数据不足一条
    /// 描述符，返回 `Ok(None)`，与命令失败区分开。
    pub fn read_toc_session_info(&mut self) -> Result<Option<SessionInfo>, MmcError> {
        let data = self.read_into(&write::toc_session_info_cdb(), write::SESSION_INFO_LEN)?;
        Ok(write::parse_session_info(&data))
    }

    /// READ(10)（0x28）：从 `lba` 读一段数据，长度必须是整块。写侧命令的读侧对偶，
    /// 写盘后的回读校验与将来的原生读盘都用它。
    ///
    /// 超过 [`MAX_READ_BLOCKS`] 的请求自动拆成多条命令：Windows 的 SPTI 单次传输
    /// 卡在 64 KiB 这个量级，实测 81 块（165888 字节）的 READ(10) 直接被以
    /// ERROR_INVALID_PARAMETER 拒绝（2026-10-10）。
    pub fn read_blocks(&mut self, lba: u32, data: &mut [u8]) -> Result<(), MmcError> {
        if !data.len().is_multiple_of(SECTOR_BYTES) {
            return Err(MmcError::MisalignedBlocks { len: data.len() });
        }
        let mut offset = 0usize;
        let mut next = lba;
        while offset < data.len() {
            let blocks = ((data.len() - offset) / SECTOR_BYTES).min(MAX_READ_BLOCKS);
            let cdb = write::read_10_cdb(next, blocks as u16);
            let end = offset + blocks * SECTOR_BYTES;
            self.transport.issue(
                &cdb,
                Direction::FromDevice,
                &mut data[offset..end],
                DEFAULT_TIMEOUT,
            )?;
            offset = end;
            next += blocks as u32;
        }
        Ok(())
    }

    /// MODE SELECT(10)（0x55）：下发写参数页（Write Type 与 multi 位）。
    /// `multi` 为真表示这次写的区段之后还要继续追加（不封盘）。
    /// 返回是否真的发送了参数页：不写参数的介质组返回 `false`。
    pub fn set_write_parameters(&mut self, kind: MediaKind, multi: bool) -> Result<bool, MmcError> {
        let Some(mut payload) = write::write_params_payload(kind, multi) else {
            return Ok(false);
        };
        let cdb = write::mode_select_cdb(payload.len() as u16);
        self.transport
            .issue(&cdb, Direction::ToDevice, &mut payload, DEFAULT_TIMEOUT)?;
        Ok(true)
    }

    /// RESERVE TRACK（0x53）：为即将写入的数据预留轨道（块数按 2048 字节计）。
    /// 顺序介质（CD-R、DVD-R 族）必须先预留，否则驱动器不给写。
    pub fn reserve_track(&mut self, blocks: u32) -> Result<(), MmcError> {
        let cdb = write::reserve_track_cdb(blocks);
        self.transport
            .issue(&cdb, Direction::None, &mut [], RESERVE_TIMEOUT)?;
        Ok(())
    }

    /// WRITE(10)（0x2A）：从 `lba` 起写入一段数据。长度必须是整块，块数上限
    /// [`MAX_WRITE_BLOCKS`]（超出的调用方自己拆成多条）。
    pub fn write_blocks(&mut self, lba: u32, data: &mut [u8]) -> Result<(), MmcError> {
        if !data.len().is_multiple_of(SECTOR_BYTES) {
            return Err(MmcError::MisalignedBlocks { len: data.len() });
        }
        let blocks = data.len() / SECTOR_BYTES;
        if blocks > MAX_WRITE_BLOCKS {
            return Err(MmcError::TooManyBlocks {
                blocks,
                max: MAX_WRITE_BLOCKS,
            });
        }
        let cdb = write::write_10_cdb(lba, blocks as u16);
        self.transport
            .issue(&cdb, Direction::ToDevice, data, WRITE_TIMEOUT)?;
        Ok(())
    }

    /// SYNCHRONIZE CACHE（0x35）：等驱动器把写缓存落盘，返回时数据已在介质上。
    pub fn synchronize_cache(&mut self) -> Result<(), MmcError> {
        let cdb = write::synchronize_cache_cdb();
        self.transport
            .issue(&cdb, Direction::None, &mut [], LONG_OP_TIMEOUT)?;
        Ok(())
    }

    /// CLOSE TRACK/SESSION（0x5B）：关闭当前区段（写完 lead-out 才返回）。
    /// 是否封盘由 MODE SELECT 的 multi 位决定，不在这里。
    pub fn close_session(&mut self) -> Result<(), MmcError> {
        let cdb = write::close_session_cdb();
        self.transport
            .issue(&cdb, Direction::None, &mut [], LONG_OP_TIMEOUT)?;
        Ok(())
    }

    /// 下发一条数据从设备发回主机的命令，并把“实际写入字节数 < 期望”当作错误。
    fn read_into(&mut self, cdb: &[u8], need: usize) -> Result<Vec<u8>, MmcError> {
        let mut data = vec![0u8; need];
        let completion =
            self.transport
                .issue(cdb, Direction::FromDevice, &mut data, DEFAULT_TIMEOUT)?;
        // residual 是“未传送的字节数”，所以实际长度 = 请求长度 - residual。
        // Windows 的 SPTI 不回传 residual（恒为 0），那里无法察觉短响应。
        let written = need.saturating_sub(completion.residual.min(need));
        if written < need {
            return Err(MmcError::ShortResponse { got: written, need });
        }
        Ok(data)
    }
}

/// 等介质就绪：盘片上电与识别要几秒，这期间 TEST UNIT READY 报错。
/// 20 秒内每 500 ms 重试一次，超时报 [`MmcError::NotReady`]，把决定权交还给人。
pub fn wait_until_ready(mmc: &mut MmcDevice) -> Result<(), MmcError> {
    wait_until_ready_for(mmc, Duration::from_secs(20))
}

/// 自定义时限的就绪等待：关闭区段这类长操作之后驱动器还要忙一阵，调用方给更长的时限。
pub fn wait_until_ready_for(mmc: &mut MmcDevice, deadline: Duration) -> Result<(), MmcError> {
    poll_until_ready(mmc, deadline, Duration::from_millis(500))
}

/// 按 deadline 与 interval 轮询就绪。公开入口只填真实硬件的两个常量，
/// 测试才能用毫秒级参数走同一条路径，不必为假设备等真秒数。
fn poll_until_ready(
    mmc: &mut MmcDevice,
    deadline: Duration,
    interval: Duration,
) -> Result<(), MmcError> {
    let start = Instant::now();
    loop {
        if mmc.test_unit_ready().is_ok() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(MmcError::NotReady);
        }
        sleep(interval);
    }
}

/// 写盘被拒绝的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WriteBlock {
    #[error("appendable disc requires grow mode")]
    NeedGrowMode,
    #[error("disc is finalized")]
    Finalized,
}

/// 写前门禁：可追加盘不接受镜像写入（单区段镜像不带前面区段的目录树，会遮住
/// 已有文件），已封口盘一律拒绝；空盘与随机可写（Other，无多区段遮蔽问题）放行。
/// `accept_appendable` 只留给走增长模式的追加路径。
pub fn approve_write(info: &DiscInformation, accept_appendable: bool) -> Result<(), WriteBlock> {
    match info.status {
        DiscStatus::Appendable if !accept_appendable => Err(WriteBlock::NeedGrowMode),
        DiscStatus::Finalized => Err(WriteBlock::Finalized),
        _ => Ok(()),
    }
}

/// 把 ASCII 字段解码成字符串，去掉尾部的空格与 NUL 填充。
fn decode_ascii(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|b| *b != b' ' && *b != 0)
        .map_or(0, |i| i + 1);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// 解析 READ FORMAT CAPACITIES 响应（ADR-0019）。
///
/// 头部字节 3 是描述符列表的总字节数。首条描述符是 Current/Maximum Capacity
/// Descriptor：字节 0–3 是块数（大端），字节 4 的低 2 位是描述符类型（1 未格式
/// 化、2 已格式化、3 无介质或容量未知），字节 5–7 是类型相关参数。列表里其余
/// 描述符是 Formattable Capacity Descriptor（字节 4 的高 6 位是格式类型），与
/// 容量口径无关，不参与解析。有效区间以头部声明为准并与实际响应长度取小，
/// 超出请求被截断的描述符不读。
fn parse_format_capacities(data: &[u8]) -> FormatCapacity {
    let mut capacity = FormatCapacity::default();
    if data.len() < 12 {
        return capacity;
    }
    // 列表长度是 8 的倍数，防御性对齐避免越过响应边界。不足一条描述符时不读。
    let list = (data[3] as usize).min(data.len() - 4) & !7;
    if list < 8 {
        return capacity;
    }
    let descriptor = &data[4..12];
    let blocks = u64::from(u32::from_be_bytes([
        descriptor[0],
        descriptor[1],
        descriptor[2],
        descriptor[3],
    ]));
    let bytes = blocks * 2048;
    match descriptor[4] & 0b11 {
        1 => capacity.max_formattable = Some(bytes),
        2 => capacity.formatted = Some(bytes),
        // 0 保留，3 是无介质或容量未知，都不构成容量口径。
        _ => {}
    }
    capacity
}

/// 把两个命令的读数汇成 [`DiscCapacity`]，规则见 [`MmcDevice::read_disc_capacity`]。
fn disc_capacity_of(track: Option<&TrackInfo>, formats: Option<&FormatCapacity>) -> DiscCapacity {
    if let Some(track) = track
        && track.free_blocks > 0
    {
        let limit = u64::from(track.next_writable_address) + u64::from(track.free_blocks);
        return DiscCapacity {
            total: Some(limit * 2048),
            free: Some(u64::from(track.free_blocks) * 2048),
        };
    }
    let Some(formats) = formats else {
        return DiscCapacity::default();
    };
    DiscCapacity {
        total: formats.max_formattable.or(formats.formatted),
        free: formats.max_formattable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 记录下发的 CDB，并按固定脚本回填响应。
    #[derive(Default)]
    struct FakeTransport {
        issued: Rc<RefCell<Vec<Vec<u8>>>>,
        /// 方向为写往设备的命令，记录其数据载荷。
        writes: Rc<RefCell<Vec<Vec<u8>>>>,
        reply: Vec<u8>,
        residual: usize,
    }

    impl ScsiTransport for FakeTransport {
        fn issue(
            &mut self,
            cdb: &[u8],
            dir: Direction,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<optiburn_transport::Completion, TransportError> {
            self.issued.borrow_mut().push(cdb.to_vec());
            if dir == Direction::ToDevice {
                self.writes.borrow_mut().push(data.to_vec());
            }
            let n = self.reply.len().min(data.len());
            data[..n].copy_from_slice(&self.reply[..n]);
            Ok(optiburn_transport::Completion {
                scsi_status: 0,
                sense: Vec::new(),
                residual: self.residual,
            })
        }

        fn device_path(&self) -> &str {
            "/dev/fake"
        }
    }

    /// 假驱动器收到的命令日志（CDB 与写往设备的数据载荷）。
    type Log = Rc<RefCell<Vec<Vec<u8>>>>;

    /// 建一台只回放 `reply` 的假驱动器，返回设备与它收到的 CDB 日志。
    fn device(reply: Vec<u8>) -> (MmcDevice, Log) {
        device_with_residual(reply, 0)
    }

    /// 建一台假驱动器，返回设备、CDB 日志与写往设备的数据载荷日志。
    fn device_capturing(reply: Vec<u8>) -> (MmcDevice, Log, Log) {
        let issued = Rc::new(RefCell::new(Vec::new()));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let fake = FakeTransport {
            issued: Rc::clone(&issued),
            writes: Rc::clone(&writes),
            reply,
            residual: 0,
        };
        (MmcDevice::new(Box::new(fake)), issued, writes)
    }

    fn device_with_residual(reply: Vec<u8>, residual: usize) -> (MmcDevice, Log) {
        let issued = Rc::new(RefCell::new(Vec::new()));
        let fake = FakeTransport {
            issued: Rc::clone(&issued),
            writes: Rc::new(RefCell::new(Vec::new())),
            reply,
            residual,
        };
        (MmcDevice::new(Box::new(fake)), issued)
    }

    #[test]
    fn inquiry_sends_cdb_and_parses_fields() {
        let mut reply = vec![0u8; INQUIRY_LEN];
        reply[0] = 0x05; // 有数据的直接访问设备
        reply[8..16].copy_from_slice(b"OPTIBURN");
        reply[16..32].copy_from_slice(b"FAKE DRIVE      ");
        reply[32..36].copy_from_slice(b"1.0 ");
        let (mut dev, _) = device(reply);

        let inq = dev.inquiry().unwrap();
        assert_eq!(
            inq,
            Inquiry {
                vendor: "OPTIBURN".into(),
                product: "FAKE DRIVE".into(),
                revision: "1.0".into(),
            }
        );
    }

    #[test]
    fn inquiry_cdb_is_golden() {
        let (mut dev, cdbs) = device(vec![0u8; INQUIRY_LEN]);
        dev.inquiry().unwrap();
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![0x12, 0x00, 0x00, 0x00, 0x24, 0x00]]
        );
    }

    #[test]
    fn test_unit_ready_cdb_is_golden() {
        let (mut dev, cdbs) = device(Vec::new());
        dev.test_unit_ready().unwrap();
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00]]
        );
    }

    #[test]
    fn read_disc_information_cdb_is_golden() {
        let (mut dev, cdbs) = device(vec![0u8; DISC_INFORMATION_LEN]);
        dev.read_disc_information().unwrap();
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x51, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x22, 0x00
            ]]
        );
    }

    #[test]
    fn read_format_capacities_cdb_is_golden() {
        let (mut dev, cdbs) = device(vec![0u8; FORMAT_CAPACITIES_LEN]);
        dev.read_format_capacities().unwrap();
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x23, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c, 0x00
            ]]
        );
    }

    /// 组一条容量描述符：块数、描述符类型（字节 4 的低 2 位）、类型相关参数。
    fn format_descriptor(reply: &mut [u8], offset: usize, blocks: u32, kind: u8, parameter: u32) {
        reply[offset..offset + 4].copy_from_slice(&blocks.to_be_bytes());
        // 类型在低 2 位。高 6 位塞满脏位，解析必须只认低 2 位。
        reply[offset + 4] = (kind & 0b11) | 0b1111_1100;
        reply[offset + 5..offset + 8].copy_from_slice(&parameter.to_be_bytes()[1..]);
    }

    #[test]
    fn parse_format_capacities_reads_the_descriptor_type_from_the_low_two_bits() {
        // 类型 1（未格式化介质）给最大可格式化容量。
        let mut reply = vec![0u8; FORMAT_CAPACITIES_LEN];
        reply[3] = 8;
        format_descriptor(&mut reply, 4, 2295104, 1, 0x2000);
        assert_eq!(
            parse_format_capacities(&reply),
            FormatCapacity {
                max_formattable: Some(2295104 * 2048),
                formatted: None,
            }
        );
        // 类型 2（已格式化介质）给当前格式化容量，数值乘固定 2048 字节块长，
        // 字节 5–7 的类型相关参数不参与换算。
        let mut reply = vec![0u8; FORMAT_CAPACITIES_LEN];
        reply[3] = 8;
        format_descriptor(&mut reply, 4, 100_000, 2, 0x0800);
        assert_eq!(
            parse_format_capacities(&reply),
            FormatCapacity {
                max_formattable: None,
                formatted: Some(100_000 * 2048),
            }
        );
    }

    #[test]
    fn parse_format_capacities_type_three_and_empty_lists_report_nothing() {
        // 类型 3 是无介质或容量未知，不构成容量口径。
        let mut reply = vec![0u8; FORMAT_CAPACITIES_LEN];
        reply[3] = 8;
        format_descriptor(&mut reply, 4, 0, 3, 0);
        assert_eq!(parse_format_capacities(&reply), FormatCapacity::default());
        // 列表长度为 0 时一条描述符都没有。
        assert_eq!(
            parse_format_capacities(&[0u8; FORMAT_CAPACITIES_LEN]),
            FormatCapacity::default()
        );
    }

    #[test]
    fn parse_format_capacities_ignores_formattable_descriptors() {
        // 首条已格式化描述符后跟 Formattable Capacity Descriptor（字节 4 的高 6 位
        // 是格式类型，如 DVD-RW 的 0x10），它不进容量口径。
        let mut reply = vec![0u8; FORMAT_CAPACITIES_LEN];
        reply[3] = 16;
        format_descriptor(&mut reply, 4, 2295104, 2, 0x0800);
        reply[12..16].copy_from_slice(&11_564_032u32.to_be_bytes());
        reply[16] = 0x10 << 2;
        reply[17..20].copy_from_slice(&[0x30, 0x00, 0x00]);
        assert_eq!(
            parse_format_capacities(&reply),
            FormatCapacity {
                max_formattable: None,
                formatted: Some(2295104 * 2048),
            }
        );
    }

    #[test]
    fn parse_format_capacities_trusts_the_declared_list_length() {
        // 头部声明 8 字节时，第二个描述符即使有数据也不读。
        let mut data = vec![0u8; FORMAT_CAPACITIES_LEN];
        data[3] = 8;
        format_descriptor(&mut data, 4, 100, 1, 0);
        format_descriptor(&mut data, 12, 200, 2, 0);
        assert_eq!(
            parse_format_capacities(&data),
            FormatCapacity {
                max_formattable: Some(100 * 2048),
                formatted: None,
            }
        );
        // 声明超出响应边界时按实际长度截断，不越界。
        let mut overflow = vec![0u8; 12];
        overflow[3] = 64;
        format_descriptor(&mut overflow, 4, 100, 1, 0);
        assert_eq!(
            parse_format_capacities(&overflow),
            FormatCapacity {
                max_formattable: Some(100 * 2048),
                formatted: None,
            }
        );
        assert_eq!(parse_format_capacities(&[]), FormatCapacity::default());
    }

    #[test]
    fn read_format_capacities_tolerates_the_measured_response() {
        // 实测形态（HL-DT-ST GP70N，CD-R 80 分钟，2026-10-10，8 区段）：请求
        // 28 字节，响应是头加一条描述符，字节 4 = 0x02 即类型 2（已格式化介质），
        // 数值 279870 块是盘的已写范围。变长响应按 residual 截断，短响应不作失败。
        let reply = vec![
            0x00, 0x00, 0x00, 0x08, 0x00, 0x04, 0x45, 0x3E, 0x02, 0x00, 0x08, 0x00,
        ];
        let (mut dev, _) = device_with_residual(reply, 16);
        assert_eq!(
            dev.read_format_capacities().unwrap(),
            FormatCapacity {
                max_formattable: None,
                formatted: Some(279_870 * 2048),
            }
        );
    }

    /// 实测形状（HL-DT-ST GP70N，CD-R 80 分钟，2026-10-10，8 区段）的轨道 9：
    /// 起始 286770、NWA 286770、剩余 73077、轨道大小 73077。NWA 加剩余 359847
    /// 块与 xorriso 读出的整体容量一致。
    fn measured_track_info() -> TrackInfo {
        TrackInfo {
            start_lba: 286_770,
            next_writable_address: 286_770,
            free_blocks: 73_077,
            track_blocks: 73_077,
        }
    }

    #[test]
    fn disc_capacity_uses_track_info_free_blocks_first() {
        // 剩余块数可用时取它：总容量是 NWA 加剩余块数，可用容量是剩余块数，
        // 格式化容量（这里是类型 2 的已写范围）不参与。
        let formats = FormatCapacity {
            max_formattable: None,
            formatted: Some(279_870 * 2048),
        };
        assert_eq!(
            disc_capacity_of(Some(&measured_track_info()), Some(&formats)),
            DiscCapacity {
                total: Some(359_847 * 2048),
                free: Some(73_077 * 2048),
            }
        );
    }

    #[test]
    fn disc_capacity_falls_back_to_format_capacities() {
        // 未格式化介质整盘待写，最大可格式化容量同时是总容量与可用容量。
        let unformatted = FormatCapacity {
            max_formattable: Some(2295104 * 2048),
            formatted: None,
        };
        assert_eq!(
            disc_capacity_of(None, Some(&unformatted)),
            DiscCapacity {
                total: Some(2295104 * 2048),
                free: Some(2295104 * 2048),
            }
        );
        // 类型 2 只作总容量展示，不参与门禁（一次写介质上它是已写范围）。
        let formatted = FormatCapacity {
            max_formattable: None,
            formatted: Some(2295104 * 2048),
        };
        assert_eq!(
            disc_capacity_of(None, Some(&formatted)),
            DiscCapacity {
                total: Some(2295104 * 2048),
                free: None,
            }
        );
        // 剩余块数为 0 的轨道读数不构成口径，照样退回格式化容量。
        let empty_track = TrackInfo {
            start_lba: 0,
            next_writable_address: 0,
            free_blocks: 0,
            track_blocks: 0,
        };
        assert_eq!(
            disc_capacity_of(Some(&empty_track), Some(&unformatted)),
            DiscCapacity {
                total: Some(2295104 * 2048),
                free: Some(2295104 * 2048),
            }
        );
        // 两条路都没有读数时两个口径都不可知。
        assert_eq!(disc_capacity_of(None, None), DiscCapacity::default());
    }

    #[test]
    fn read_disc_information_decodes_all_status_bits() {
        for (bits, expected) in [
            (0b00u8, DiscStatus::Empty),
            (0b01, DiscStatus::Appendable),
            (0b10, DiscStatus::Finalized),
            (0b11, DiscStatus::Other(3)),
        ] {
            let mut reply = vec![0u8; DISC_INFORMATION_LEN];
            // 同一字节里还带 state-of-last-session(3:2) 与 erasable(4)，解析必须只取低 2 位。
            reply[2] = bits | (0b10 << 2) | 0b0001_0000;
            reply[3] = 2;
            reply[4] = 3;
            reply[5] = 7;
            let (mut dev, _) = device(reply);

            assert_eq!(
                dev.read_disc_information().unwrap(),
                DiscInformation {
                    status: expected,
                    sessions: 3,
                    first_track: 2,
                    last_session_first_track: 7,
                }
            );
        }
    }

    #[test]
    fn short_response_is_an_error() {
        let (mut dev, _) = device_with_residual(vec![0u8; INQUIRY_LEN], 4);
        assert!(matches!(
            dev.inquiry(),
            Err(MmcError::ShortResponse { got: 32, need: 36 })
        ));
    }

    #[test]
    fn ascii_decoding_strips_trailing_fill() {
        assert_eq!(decode_ascii(b"OPTIBURN"), "OPTIBURN");
        assert_eq!(decode_ascii(b"FAKE DRIVE      "), "FAKE DRIVE");
        assert_eq!(decode_ascii(b"1.0 \0\0"), "1.0");
        assert_eq!(decode_ascii(b"        "), "");
    }

    /// TEST UNIT READY（CDB 首字节 0x00）先报固定次数的“命令失败”，之后放行；
    /// 其余命令一律成功。模拟盘片上电识别期。
    struct FlakyReadyTransport {
        failures_left: Rc<Cell<u32>>,
    }

    impl ScsiTransport for FlakyReadyTransport {
        fn issue(
            &mut self,
            cdb: &[u8],
            _dir: Direction,
            _data: &mut [u8],
            _timeout: Duration,
        ) -> Result<optiburn_transport::Completion, TransportError> {
            if cdb[0] == 0x00 && self.failures_left.get() > 0 {
                self.failures_left.set(self.failures_left.get() - 1);
                // CHECK CONDITION 的典型形态：状态 2，sense 里带“未就绪”（2/04）。
                return Err(TransportError::CommandFailed {
                    cdb: cdb.to_vec(),
                    scsi_status: 2,
                    sense: vec![0x70, 0x00, 0x02, 0x04],
                });
            }
            Ok(optiburn_transport::Completion {
                scsi_status: 0,
                sense: Vec::new(),
                residual: 0,
            })
        }

        fn device_path(&self) -> &str {
            "/dev/fake"
        }
    }

    #[test]
    fn approve_write_truth_table() {
        let info = |status| DiscInformation {
            status,
            sessions: 3,
            first_track: 1,
            last_session_first_track: 3,
        };
        assert_eq!(approve_write(&info(DiscStatus::Empty), false), Ok(()));
        assert_eq!(approve_write(&info(DiscStatus::Appendable), true), Ok(()));
        assert_eq!(
            approve_write(&info(DiscStatus::Appendable), false),
            Err(WriteBlock::NeedGrowMode)
        );
        assert_eq!(
            approve_write(&info(DiscStatus::Finalized), true),
            Err(WriteBlock::Finalized)
        );
        assert_eq!(approve_write(&info(DiscStatus::Other(3)), false), Ok(()));
    }

    #[test]
    fn poll_until_ready_retries_until_the_device_answers() {
        let failures = Rc::new(Cell::new(2));
        let mut dev = MmcDevice::new(Box::new(FlakyReadyTransport {
            failures_left: Rc::clone(&failures),
        }));

        poll_until_ready(&mut dev, Duration::from_secs(1), Duration::from_millis(10))
            .expect("device must become ready");
        assert_eq!(failures.get(), 0, "both injected failures must be retried");
    }

    #[test]
    fn poll_until_ready_times_out_when_the_device_never_readies() {
        let mut dev = MmcDevice::new(Box::new(FlakyReadyTransport {
            failures_left: Rc::new(Cell::new(u32::MAX)),
        }));

        assert!(matches!(
            poll_until_ready(
                &mut dev,
                Duration::from_millis(30),
                Duration::from_millis(10)
            ),
            Err(MmcError::NotReady)
        ));
    }

    #[test]
    fn get_configuration_parses_the_current_profile() {
        let mut reply = vec![0u8; CONFIGURATION_LEN];
        reply[3] = 0x0C;
        reply[6] = 0x00;
        reply[7] = 0x09; // CD-R
        let (mut dev, cdbs) = device(reply);

        assert_eq!(dev.get_configuration().unwrap(), CurrentProfile(0x0009));
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00
            ]]
        );
    }

    #[test]
    fn read_capacity_parses_the_last_writable_lba() {
        let reply = vec![0x00, 0x00, 0x4F, 0xFF, 0x00, 0x00, 0x08, 0x00];
        let (mut dev, cdbs) = device(reply);

        assert_eq!(dev.read_capacity().unwrap(), 0x4FFF);
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x25, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00
            ]]
        );
    }

    #[test]
    fn set_write_parameters_sends_the_page_to_the_device() {
        let (mut dev, cdbs, writes) = device_capturing(Vec::new());

        assert!(dev.set_write_parameters(MediaKind::Cd, false).unwrap());
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x55, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3C, 0x00
            ]]
        );
        let payload = &writes.borrow()[0];
        assert_eq!(payload.len(), 8 + 2 + 0x32);
        assert!(payload[..8].iter().all(|b| *b == 0));
        assert_eq!(&payload[8..10], &[0x05, 0x32]);
        assert_eq!(payload[10], 0x41);
    }

    #[test]
    fn set_write_parameters_is_a_no_op_without_a_page() {
        let (mut dev, cdbs, writes) = device_capturing(Vec::new());

        assert!(
            !dev.set_write_parameters(MediaKind::PlusOrBdR, true)
                .unwrap()
        );
        assert!(cdbs.borrow().is_empty());
        assert!(writes.borrow().is_empty());
    }

    #[test]
    fn write_blocks_rejects_unaligned_and_oversized_lengths() {
        let (mut dev, _, _) = device_capturing(Vec::new());

        let mut odd = vec![0u8; SECTOR_BYTES + 1];
        assert!(matches!(
            dev.write_blocks(0, &mut odd),
            Err(MmcError::MisalignedBlocks { len }) if len == SECTOR_BYTES + 1
        ));
        let mut huge = vec![0u8; (MAX_WRITE_BLOCKS + 1) * SECTOR_BYTES];
        assert!(matches!(
            dev.write_blocks(0, &mut huge),
            Err(MmcError::TooManyBlocks {
                blocks,
                max: MAX_WRITE_BLOCKS
            }) if blocks == MAX_WRITE_BLOCKS + 1
        ));
    }

    #[test]
    fn write_blocks_sends_golden_write_10_with_the_payload() {
        let (mut dev, cdbs, writes) = device_capturing(Vec::new());

        let mut data = vec![0xABu8; 2 * SECTOR_BYTES];
        dev.write_blocks(3, &mut data).unwrap();
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x2A, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x02, 0x00
            ]]
        );
        assert_eq!(writes.borrow()[0], data);
    }

    #[test]
    fn read_track_information_cdb_and_parse_are_golden() {
        let mut reply = vec![0u8; write::TRACK_INFO_LEN];
        reply[8..12].copy_from_slice(&0x0001_2345u32.to_be_bytes());
        reply[12..16].copy_from_slice(&0x0001_2400u32.to_be_bytes());
        reply[16..20].copy_from_slice(&1234u32.to_be_bytes());
        reply[24..28].copy_from_slice(&81u32.to_be_bytes());
        let (mut dev, cdbs) = device(reply);

        let info = dev.read_track_information(0xFF).unwrap();
        assert_eq!(info.start_lba, 0x0001_2345);
        assert_eq!(info.next_writable_address, 0x0001_2400);
        assert_eq!(info.free_blocks, 1234);
        assert_eq!(info.track_blocks, 81);
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x52, 0x01, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x20, 0x00
            ]]
        );
    }

    #[test]
    fn track_info_parse_reads_the_measured_free_blocks() {
        // 实测形状（HL-DT-ST GP70N，CD-R 80 分钟，2026-10-10）：轨道 9、区段 8，
        // 起始 286770、NWA 286770、剩余 73077、轨道大小 73077。
        let reply = vec![
            0x00, 0x22, 0x09, 0x08, 0x00, 0x04, 0x4F, 0x01, 0x00, 0x04, 0x60, 0x32, 0x00, 0x04,
            0x60, 0x32, 0x00, 0x01, 0x1D, 0x75, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x1D, 0x75,
            0x00, 0x00, 0x00, 0x00,
        ];
        let info = write::parse_track_information(&reply).expect("measured reply must parse");
        assert_eq!(info.start_lba, 286_770);
        assert_eq!(info.next_writable_address, 286_770);
        assert_eq!(info.free_blocks, 73_077);
        assert_eq!(info.track_blocks, 73_077);
    }

    #[test]
    fn read_toc_session_info_cdb_and_parse_are_golden() {
        // 实测形状（8 区段 CD-R）：data_len=10，描述符末区段首轨 8、起始 279570。
        let mut reply = vec![0u8; write::SESSION_INFO_LEN];
        reply[0] = 0x00;
        reply[1] = 0x0A;
        reply[2] = 0x01;
        reply[3] = 0x07;
        reply[4] = 0x00;
        reply[5] = 0x14;
        reply[6] = 0x08;
        reply[8..12].copy_from_slice(&279_570u32.to_be_bytes());
        let (mut dev, cdbs) = device(reply);

        let info = dev
            .read_toc_session_info()
            .unwrap()
            .expect("disc with a closed session");
        assert_eq!(
            info,
            write::SessionInfo {
                first_session: 1,
                last_session: 7,
                last_session_start: 279_570,
            }
        );
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x00
            ]]
        );

        // 空白盘：数据长度不足一条描述符，返回 None 而不是错误。
        let (mut blank, _) = device(vec![0u8; write::SESSION_INFO_LEN]);
        assert_eq!(blank.read_toc_session_info().unwrap(), None);
    }

    #[test]
    fn read_blocks_sends_golden_read_10() {
        let (mut dev, cdbs, _) = device_capturing(Vec::new());

        let mut data = vec![0u8; SECTOR_BYTES];
        dev.read_blocks(0x0000_1234, &mut data).unwrap();
        assert_eq!(
            cdbs.borrow().as_slice(),
            &[vec![
                0x28, 0x00, 0x00, 0x00, 0x12, 0x34, 0x00, 0x00, 0x01, 0x00
            ]]
        );
    }

    #[test]
    fn read_blocks_splits_requests_at_the_transport_limit() {
        let (mut dev, cdbs, _) = device_capturing(Vec::new());

        // 100 块拆成 32 + 32 + 32 + 4，LBA 依次跟进。
        let mut data = vec![0u8; 100 * SECTOR_BYTES];
        dev.read_blocks(0x0000_0010, &mut data).unwrap();
        let lengths: Vec<u16> = cdbs
            .borrow()
            .iter()
            .map(|cdb| u16::from_be_bytes([cdb[7], cdb[8]]))
            .collect();
        assert_eq!(lengths, vec![32, 32, 32, 4]);
        let lbas: Vec<u32> = cdbs
            .borrow()
            .iter()
            .map(|cdb| u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]))
            .collect();
        assert_eq!(lbas, vec![0x10, 0x30, 0x50, 0x70]);
    }

    #[test]
    fn read_blocks_rejects_unaligned_lengths() {
        let (mut dev, _, _) = device_capturing(Vec::new());

        let mut odd = vec![0u8; SECTOR_BYTES - 1];
        assert!(matches!(
            dev.read_blocks(0, &mut odd),
            Err(MmcError::MisalignedBlocks { .. })
        ));
    }
}
