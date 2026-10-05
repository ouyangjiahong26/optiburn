//! Linux 实现：`/dev/sr*` 上的 `SG_IO` ioctl（`scsi/sg.h`）。
//!
//! 思路参考 vicr123/alight 的 `src/scsi/linux.rs`（该仓库无许可证，故本文件按
//! `sg_io_hdr` 的 C 布局自行编写，见 ADR-0001）。

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::time::Duration;

use crate::{Completion, Direction, MAX_CDB_LEN, MAX_SENSE_LEN, ScsiTransport, TransportError};

/// `SG_IO` 的 ioctl 请求码（`scsi/sg.h`）。
const SG_IO: libc::c_ulong = 0x2285;
/// `sg_io_hdr.dxfer_direction`：无数据阶段。
const SG_DXFER_NONE: i32 = -1;
/// `sg_io_hdr.dxfer_direction`：主机 → 设备。
const SG_DXFER_TO_DEV: i32 = -2;
/// `sg_io_hdr.dxfer_direction`：设备 → 主机。
const SG_DXFER_FROM_DEV: i32 = -3;

/// `sg_io_hdr` 的 C 布局（`scsi/sg.h`，64 位平台 sizeof 为 88）。
#[repr(C)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut core::ffi::c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut core::ffi::c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

/// Linux 上的一条 SCSI 通道：持有打开的 `/dev/sr*` 句柄。
pub struct LinuxSg {
    file: File,
    path: String,
}

impl LinuxSg {
    /// 以读写方式打开设备节点；`ENOENT` 映射为 [`TransportError::NotFound`]。
    pub fn open(device: &str) -> Result<Self, TransportError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => TransportError::NotFound(device.to_string()),
                _ => TransportError::Io(e),
            })?;
        Ok(Self {
            file,
            path: device.to_string(),
        })
    }

    /// 组装 `sg_io_hdr`（`scsi/sg.h` 的字段顺序）。指针指向调用方传入的缓冲区，由调用方
    /// 保证它们在 ioctl 期间存活。
    fn build_hdr(
        cdb: &[u8],
        cdb_buf: &mut [u8; MAX_CDB_LEN],
        dir: Direction,
        data: &mut [u8],
        sense: &mut [u8; MAX_SENSE_LEN],
        timeout: Duration,
    ) -> SgIoHdr {
        SgIoHdr {
            interface_id: i32::from(b'S'),
            dxfer_direction: match dir {
                Direction::None => SG_DXFER_NONE,
                Direction::ToDevice => SG_DXFER_TO_DEV,
                Direction::FromDevice => SG_DXFER_FROM_DEV,
            },
            cmd_len: cdb.len() as u8,
            mx_sb_len: MAX_SENSE_LEN as u8,
            iovec_count: 0,
            dxfer_len: data.len() as u32,
            dxferp: if data.is_empty() {
                std::ptr::null_mut()
            } else {
                data.as_mut_ptr().cast()
            },
            cmdp: cdb_buf.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: timeout.as_millis().min(u128::from(u32::MAX)) as u32,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        }
    }

    /// 把 ioctl 结果翻译成 [`Completion`]。
    ///
    /// 宿主机/驱动层报错时状态字节可能仍为 0，必须一并当失败，否则上层会把超时或设备
    /// 重置误判成成功。
    fn completion(
        cdb: &[u8],
        hdr: &SgIoHdr,
        sense: &[u8; MAX_SENSE_LEN],
    ) -> Result<Completion, TransportError> {
        let sense_len = usize::from(hdr.sb_len_wr).min(MAX_SENSE_LEN);
        if hdr.status != 0 || hdr.host_status != 0 || hdr.driver_status != 0 {
            return Err(TransportError::CommandFailed {
                cdb: cdb.to_vec(),
                scsi_status: hdr.status,
                sense: sense[..sense_len].to_vec(),
            });
        }

        Ok(Completion {
            scsi_status: hdr.status,
            sense: sense[..sense_len].to_vec(),
            // resid 是“未传送的字节数”；写方向下它可能为负，那不代表有残留。
            residual: hdr.resid.max(0) as usize,
        })
    }
}

impl ScsiTransport for LinuxSg {
    fn issue(
        &mut self,
        cdb: &[u8],
        dir: Direction,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<Completion, TransportError> {
        if cdb.len() > MAX_CDB_LEN {
            return Err(TransportError::CdbTooLong(cdb.len()));
        }

        let mut cdb_buf = [0u8; MAX_CDB_LEN];
        cdb_buf[..cdb.len()].copy_from_slice(cdb);
        let mut sense = [0u8; MAX_SENSE_LEN];

        let mut hdr = Self::build_hdr(cdb, &mut cdb_buf, dir, data, &mut sense, timeout);

        // SAFETY: hdr 的布局与 sg_io_hdr 一致（下方测试断言尺寸），其 cmdp/sbp/dxferp
        // 都指向本次调用内存中存活的缓冲区，ioctl 同步返回后才离开作用域。
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), SG_IO, &mut hdr as *mut SgIoHdr) };
        if rc < 0 {
            return Err(TransportError::Io(std::io::Error::last_os_error()));
        }

        Self::completion(cdb, &hdr, &sense)
    }

    fn device_path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sg_io_hdr_layout_matches_c() {
        // 布局一旦漂移，ioctl 会静默读到错误字段而不是报错。
        assert_eq!(std::mem::size_of::<SgIoHdr>(), 88);
        assert_eq!(std::mem::align_of::<SgIoHdr>(), 8);
    }
}
