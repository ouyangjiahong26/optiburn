//! MMC 命令层：把读侧 MMC 命令编码成 CDB，并把响应解析成结构化数据。
//!
//! 依赖 [`optiburn_transport::ScsiTransport`] 这一个硬件接缝，因此可以在没有光驱的
//! 机器上用替身完整测试。v0 只有读侧命令；写侧（RESERVE TRACK / WRITE(10) /
//! CLOSE TRACK）留给路线图里的原生 MMC 引擎，不在本 crate 留空壳。

use std::time::Duration;

use optiburn_transport::{Direction, ScsiTransport, TransportError};

/// 只读命令的超时：盘片寻道与转速切换都在这个量级内完成。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// INQUIRY 标准响应的长度（`SPC`：附加长度字段 + 8 + 16 + 4 字节）。
const INQUIRY_LEN: usize = 36;
/// READ DISC INFORMATION 标准响应（Data Type 000b）的长度。
const DISC_INFORMATION_LEN: usize = 34;

#[derive(Debug, thiserror::Error)]
pub enum MmcError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("device returned {got} bytes, need {need}")]
    ShortResponse { got: usize, need: usize },
}

/// 设备的 INQUIRY 标识字段；尾部填充的空格与 NUL 已去掉。
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
    /// 其它状态（例如 DVD-RAM 的 `others`）。
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
    /// 区段数（响应字节 4 的低 8 位；高 8 位在字节 9，实际盘片不会超过 99）。
    pub sessions: u8,
    /// 响应字节 3：最后一个区段的首轨号。
    pub first_track: u8,
}

/// 一条盘驱动器通道，持有已经打开的 [`ScsiTransport`]。
pub struct MmcDevice {
    transport: Box<dyn ScsiTransport>,
}

impl MmcDevice {
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

    /// 下发一条设备 → 主机的命令，并把“实际写入字节数 < 期望”当作错误。
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

/// 把 ASCII 字段解码成字符串，去掉尾部的空格与 NUL 填充。
fn decode_ascii(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|b| *b != b' ' && *b != 0)
        .map_or(0, |i| i + 1);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
