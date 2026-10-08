//! SCSI 传输层：把 CDB 交给设备，取回状态、sense 与残余长度。
//!
//! 这是全仓库唯一的硬件抽象点：上层（`optiburn-mmc`，以及路线图里的原生 MMC 写入
//! 引擎）只依赖 [`ScsiTransport`]，不感知 Linux `SG_IO` 与 Windows SPTI 的差异。
//! v0 支持 Linux 与 Windows，其余平台在 [`open`] 返回 [`TransportError::Unsupported`]。

use std::time::Duration;

/// 单个 CDB 的最大长度：SPTI 的 `SCSI_PASS_THROUGH_DIRECT.Cdb` 固定 16 字节。
pub const MAX_CDB_LEN: usize = 16;
/// sense 缓冲区上限：SCSI 标准中固定格式 sense 最长 32 字节。
pub const MAX_SENSE_LEN: usize = 32;

/// 数据传送方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 无数据阶段，例如 TEST UNIT READY。
    None,
    /// 数据从主机发往设备。
    ToDevice,
    /// 数据从设备发回主机。
    FromDevice,
}

/// 一次命令的结果。
///
/// `scsi_status == 0` 即命令级成功。sense 不做解释，原样交给上层判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// SCSI 状态字节。
    pub scsi_status: u8,
    /// 设备返回的 sense 数据，未解释，最多 [`MAX_SENSE_LEN`] 字节。
    pub sense: Vec<u8>,
    /// 未传送的字节数。
    pub residual: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("CDB too long: {0} bytes (max 16)")]
    CdbTooLong(usize),
    #[error("SCSI command {cdb:02x?} failed: status={scsi_status}, sense={sense:02x?}")]
    CommandFailed {
        cdb: Vec<u8>,
        scsi_status: u8,
        sense: Vec<u8>,
    },
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("device not found: {0}")]
    NotFound(String),
    #[error("unsupported on this platform")]
    Unsupported,
}

/// 一条同步 SCSI 通道。
///
/// 实现只需保证：CDB 原样下发、`data` 按 `dir` 收发、sense 不超过
/// [`MAX_SENSE_LEN`] 字节。命令级失败（状态字节非 0，或宿主机/驱动层报错）返回
/// [`TransportError::CommandFailed`]，设备把 sense 放在 `sense` 字段里。
pub trait ScsiTransport {
    /// 下发一条 CDB 并等待完成。
    fn issue(
        &mut self,
        cdb: &[u8],
        dir: Direction,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<Completion, TransportError>;

    /// 打开时使用的设备路径，用于报错与日志。
    fn device_path(&self) -> &str;
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// 按平台打开设备，返回可用的 [`ScsiTransport`]。
#[cfg(target_os = "linux")]
pub fn open(device: &str) -> Result<Box<dyn ScsiTransport>, TransportError> {
    Ok(Box::new(linux::LinuxSg::open(device)?))
}

/// 按平台打开设备，返回可用的 [`ScsiTransport`]。
#[cfg(windows)]
pub fn open(device: &str) -> Result<Box<dyn ScsiTransport>, TransportError> {
    Ok(Box::new(windows::WindowsSpti::open(device)?))
}

/// 按平台打开设备，返回可用的 [`ScsiTransport`]。
#[cfg(not(any(target_os = "linux", windows)))]
pub fn open(_device: &str) -> Result<Box<dyn ScsiTransport>, TransportError> {
    Err(TransportError::Unsupported)
}

/// 枚举本机的光驱设备路径，按名字排序，路径可直接交给 [`open`]。
///
/// Linux 枚举 `/dev/sr*` 设备节点，Windows 枚举盘符位掩码里类型为光驱的盘符；
/// 其它平台返回空列表，上层按“未发现光驱”处理。
/// 枚举与 [`open`] 一样按平台收在本 crate，调用方不需要平台分支。
#[cfg(target_os = "linux")]
pub fn list_optical_devices() -> Vec<String> {
    linux::optical_devices()
}

/// 枚举本机的光驱设备路径，按名字排序，路径可直接交给 [`open`]。
///
/// Windows 枚举盘符位掩码里 `GetDriveTypeW` 判为光驱的盘符；
/// 其它平台返回空列表，上层按“未发现光驱”处理。
/// 枚举与 [`open`] 一样按平台收在本 crate，调用方不需要平台分支。
#[cfg(windows)]
pub fn list_optical_devices() -> Vec<String> {
    windows::list_optical_devices()
}

/// 枚举本机的光驱设备路径，按名字排序，路径可直接交给 [`open`]。
///
/// 除 Linux 与 Windows 外的平台暂无枚举实现，返回空列表，上层按“未发现光驱”处理。
/// 枚举与 [`open`] 一样按平台收在本 crate，调用方不需要平台分支。
#[cfg(not(any(target_os = "linux", windows)))]
pub fn list_optical_devices() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn open_missing_device_reports_not_found() {
        match open("/nonexistent-optiburn-device") {
            Err(TransportError::NotFound(path)) => {
                assert_eq!(path, "/nonexistent-optiburn-device");
            }
            Err(other) => panic!("expected NotFound, got {other:?}"),
            Ok(_) => panic!("expected NotFound, opened a device that cannot exist"),
        }
    }

    #[test]
    fn optical_device_list_is_sorted_and_matches_platform() {
        let devices = list_optical_devices();
        assert!(devices.windows(2).all(|w| w[0] < w[1]), "{devices:?}");
        #[cfg(target_os = "linux")]
        for device in &devices {
            assert!(device.starts_with("/dev/sr"), "{device}");
        }
        #[cfg(windows)]
        for device in &devices {
            // Windows 侧产出的是 `X:` 形式的盘符路径。
            let bytes = device.as_bytes();
            assert_eq!(bytes.len(), 2, "{device}");
            assert!(
                bytes[0].is_ascii_uppercase() && bytes[1] == b':',
                "{device}"
            );
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        assert!(devices.is_empty());
    }
}
