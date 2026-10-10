//! MMC 命令层：把读侧 MMC 命令编码成 CDB，并把响应解析成结构化数据。
//!
//! 依赖 [`optiburn_transport::ScsiTransport`] 这一个硬件接缝，因此可以在没有光驱的
//! 机器上用替身完整测试。v0 只有读侧命令。写侧（RESERVE TRACK / WRITE(10) /
//! CLOSE TRACK）留给路线图里的原生 MMC 引擎，不在本 crate 留空壳。

use std::thread::sleep;
use std::time::{Duration, Instant};

use optiburn_transport::{Direction, ScsiTransport, TransportError};

/// 只读命令的超时：盘片寻道与转速切换都在这个量级内完成。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// INQUIRY 标准响应的长度（`SPC`：附加长度字段 + 8 + 16 + 4 字节）。
const INQUIRY_LEN: usize = 36;
/// READ DISC INFORMATION 标准响应（Data Type 000b）的长度。
const DISC_INFORMATION_LEN: usize = 34;
/// READ FORMAT CAPACITIES 的请求长度：4 字节响应头加 3 个 8 字节容量描述符。
/// 驱动器通常只报前两三个描述符，多请求的部分按协议零填充或计入 residual。
const FORMAT_CAPACITIES_LEN: usize = 28;

#[derive(Debug, thiserror::Error)]
pub enum MmcError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("device returned {got} bytes, need {need}")]
    ShortResponse { got: usize, need: usize },
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
    /// “最后一个区段的首轨号”在字节 5，本结构不解析。
    pub first_track: u8,
}

/// READ FORMAT CAPACITIES 解析出的盘片容量，字节口径（ADR-0017）。
///
/// 字段是 [`Option`]：描述符缺失时容量口径不可知。CD 介质普遍不报容量描述符，
/// 此时上层显示“未知”并跳过容量门禁，而不是拒绝刻录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscCapacity {
    /// 介质最大容量，来自描述符类型 0（最大可格式化容量）。
    pub total: Option<u64>,
    /// 已写入部分，来自描述符类型 1（当前已格式化容量）。差值即可用容量。
    pub used: Option<u64>,
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
        })
    }

    /// READ FORMAT CAPACITIES（op 0x23）：介质总容量与已写入量（ADR-0017）。
    ///
    /// 响应是变长列表，驱动器可以少给：实机（HL-DT-ST GP70N）对 28 字节请求
    /// 只回 12 字节并计 16 字节 residual。因此不走 [`Self::read_into`] 的满长
    /// 判定，按实际返回长度解析，有效区间再由头部列表长度圈定。
    pub fn read_format_capacities(&mut self) -> Result<DiscCapacity, MmcError> {
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
    poll_until_ready(mmc, Duration::from_secs(20), Duration::from_millis(500))
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

/// 解析 READ FORMAT CAPACITIES 响应。
///
/// 响应是 4 字节头加若干 8 字节描述符：头部字节 3 是描述符列表总字节数，
/// 每个描述符里块数是大端 u32、描述符类型在字节 4 的高 2 位、块长在字节
/// 5–7（大端）。类型 0 与类型 1 分别是最大容量与已写入量，其余类型是缺陷
/// 索引与格式化参数，不参与容量口径。有效区间以头部声明为准并与实际响应
/// 长度取小，超出请求被截断的描述符不读。
fn parse_format_capacities(data: &[u8]) -> DiscCapacity {
    let mut capacity = DiscCapacity {
        total: None,
        used: None,
    };
    if data.len() < 4 {
        return capacity;
    }
    // 列表长度是 8 的倍数，防御性对齐避免越过响应边界。
    let list = (data[3] as usize).min(data.len() - 4) & !7;
    for offset in (4..4 + list).step_by(8) {
        let descriptor = &data[offset..offset + 8];
        let blocks =
            u32::from_be_bytes([descriptor[0], descriptor[1], descriptor[2], descriptor[3]]) as u64;
        let block_len = u32::from_be_bytes([0, descriptor[5], descriptor[6], descriptor[7]]) as u64;
        match (descriptor[4] >> 6) & 0b11 {
            0 => capacity.total = Some(blocks * block_len),
            1 => capacity.used = Some(blocks * block_len),
            _ => {}
        }
    }
    capacity
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
        reply: Vec<u8>,
        residual: usize,
    }

    impl ScsiTransport for FakeTransport {
        fn issue(
            &mut self,
            cdb: &[u8],
            _dir: Direction,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<optiburn_transport::Completion, TransportError> {
            self.issued.borrow_mut().push(cdb.to_vec());
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

    /// 建一台只回放 `reply` 的假驱动器，返回设备与它收到的 CDB 日志。
    fn device(reply: Vec<u8>) -> (MmcDevice, Rc<RefCell<Vec<Vec<u8>>>>) {
        device_with_residual(reply, 0)
    }

    fn device_with_residual(
        reply: Vec<u8>,
        residual: usize,
    ) -> (MmcDevice, Rc<RefCell<Vec<Vec<u8>>>>) {
        let issued = Rc::new(RefCell::new(Vec::new()));
        let fake = FakeTransport {
            issued: Rc::clone(&issued),
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

    /// 组一个 8 字节容量描述符：块数、描述符类型、块长。
    fn descriptor(reply: &mut [u8], offset: usize, blocks: u32, kind: u8, block_len: u32) {
        reply[offset..offset + 4].copy_from_slice(&blocks.to_be_bytes());
        // 类型在高 2 位；低 6 位塞满脏位，解析必须只认高 2 位。
        reply[offset + 4] = (kind << 6) | 0b0011_1111;
        reply[offset + 5..offset + 8].copy_from_slice(&block_len.to_be_bytes()[1..]);
    }

    #[test]
    fn read_format_capacities_decodes_both_descriptors() {
        // DVD+R 4.7 GB 盘的典型形态：类型 0 是全盘容量，类型 1 是已写入量。
        let mut reply = vec![0u8; FORMAT_CAPACITIES_LEN];
        reply[3] = 16;
        descriptor(&mut reply, 4, 2295104, 0, 2048);
        descriptor(&mut reply, 12, 100000, 1, 2048);
        let (mut dev, _) = device(reply);

        assert_eq!(
            dev.read_format_capacities().unwrap(),
            DiscCapacity {
                total: Some(2295104 * 2048),
                used: Some(100000 * 2048),
            }
        );
    }

    #[test]
    fn read_format_capacities_without_descriptors_reports_none() {
        // CD 介质的典型形态：容量列表长度为 0，两个口径都读不到。
        let (mut dev, _) = device(vec![0u8; FORMAT_CAPACITIES_LEN]);
        assert_eq!(
            dev.read_format_capacities().unwrap(),
            DiscCapacity {
                total: None,
                used: None,
            }
        );
    }

    #[test]
    fn read_format_capacities_keeps_missing_side_as_none() {
        // 只有类型 1（已写入）没有类型 0：total 保持 None，上层据此跳过容量门禁。
        let mut reply = vec![0u8; FORMAT_CAPACITIES_LEN];
        reply[3] = 8;
        descriptor(&mut reply, 4, 5000, 1, 2048);
        let (mut dev, _) = device(reply);

        assert_eq!(
            dev.read_format_capacities().unwrap(),
            DiscCapacity {
                total: None,
                used: Some(5000 * 2048),
            }
        );
    }

    #[test]
    fn parse_format_capacities_trusts_the_declared_list_length() {
        // 头部声明 8 字节时，第二个描述符即使有数据也不读。
        let mut data = vec![0u8; FORMAT_CAPACITIES_LEN];
        data[3] = 8;
        descriptor(&mut data, 4, 100, 0, 2048);
        descriptor(&mut data, 12, 200, 1, 2048);
        assert_eq!(
            parse_format_capacities(&data),
            DiscCapacity {
                total: Some(100 * 2048),
                used: None,
            }
        );
        // 声明超出响应边界时按实际长度截断，不越界。
        let mut overflow = vec![0u8; 12];
        overflow[3] = 64;
        descriptor(&mut overflow, 4, 100, 0, 2048);
        assert_eq!(
            parse_format_capacities(&overflow),
            DiscCapacity {
                total: Some(100 * 2048),
                used: None,
            }
        );
        assert_eq!(
            parse_format_capacities(&[]),
            DiscCapacity {
                total: None,
                used: None,
            }
        );
    }

    #[test]
    fn read_format_capacities_tolerates_the_measured_short_response() {
        // 实机形态（HL-DT-ST GP70N，CD-R 80 分钟）：对 28 字节请求只回 12 字节
        // 并计 16 字节 residual，内容是头加一个描述符。该描述符类型位是 0，数值
        // 257820 块却与盘总容量 359847 块对不上、恰等于 xorriso 读出的已写块数
        // （readable 257820）：CD 家族介质没有格式化容量语义，驱动器把已写口径
        // 填进了类型 0。单一描述符凑不齐 total 与 used，上层按“读不到容量”跳过
        // 门禁，数字不进界面，这种怪癖数据不会被当成可用容量。
        let reply = vec![
            0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0xEF, 0x1C, 0x02, 0x00, 0x08, 0x00,
        ];
        let (mut dev, _) = device_with_residual(reply, 16);
        assert_eq!(
            dev.read_format_capacities().unwrap(),
            DiscCapacity {
                total: Some(257820 * 2048),
                used: None,
            }
        );
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
            let (mut dev, _) = device(reply);

            assert_eq!(
                dev.read_disc_information().unwrap(),
                DiscInformation {
                    status: expected,
                    sessions: 3,
                    first_track: 2,
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
}
