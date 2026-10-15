//! 真机测试：需要光驱，用环境变量指定设备后手动跑。
//!
//! - 只读侦察：`cargo test -p optiburn-engine -- --ignored inspect_real_media --nocapture`
//! - 门禁余量读数：`OPTIBURN_DEVICE=D: cargo test -p optiburn-engine -- --ignored inspect_session_gaps --nocapture`
//!   打印门禁会用的区段开销与盘上相邻区段的实测差额（CD 余量默认值的来源，ADR-0019 修订）。
//! - 真机写盘：`OPTIBURN_DEVICE=D: OPTIBURN_IMAGE=x.iso cargo test -p optiburn-engine -- --ignored native_burn_real --nocapture`
//!   写入会占用一张空白或可覆写的盘，别拿有数据的盘跑。

use super::*;
use optiburn_mmc::{MmcDevice, wait_until_ready};

#[test]
#[ignore = "needs optical drive"]
fn inspect_real_media() {
    let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
    let transport = optiburn_transport::open(&device).expect("open device");
    let mut mmc = MmcDevice::new(transport);
    wait_until_ready(&mut mmc).expect("device ready");
    let info = mmc.read_disc_information().expect("disc information");
    let profile = mmc.get_configuration().expect("current profile");
    let capacity = mmc.read_capacity();
    println!("device: {device}");
    println!("disc: status={:?} sessions={}", info.status, info.sessions);
    println!(
        "profile: {:#06x} kind={:?}",
        profile.0,
        profile.media_kind()
    );
    println!("capacity: {capacity:?}");
    // 轨道与区段：追加能否继续看这几个字段（损坏位置位时 NWA_V 会被清零）。
    if let Ok(track) = mmc.read_track_information(crate::native::LAST_TRACK) {
        println!(
            "track 0xFF: start={} nwa={} nwa_valid={} damaged={} free={} track_blocks={}",
            track.start_lba,
            track.next_writable_address,
            track.nwa_valid,
            track.damaged,
            track.free_blocks,
            track.track_blocks
        );
    }
    if let Ok(Some(session)) = mmc.read_toc_session_info() {
        println!("session: last_session_start={}", session.last_session_start);
    }
    if let Ok(tracks) = mmc.read_toc_tracks() {
        let listed: Vec<String> = tracks
            .iter()
            .map(|track| {
                format!(
                    "{}@{}{}",
                    track.track,
                    track.start_lba,
                    if track.is_lead_out { "L" } else { "" }
                )
            })
            .collect();
        println!("tracks: {}", listed.join(" "));
    }
}

#[test]
#[ignore = "needs optical drive"]
fn inspect_session_gaps() {
    // 只读侦察：打印门禁会用的区段开销余量，以及盘上相邻区段的地址差额（下一区段
    // 起点减上一区段数据末尾，数据末尾取该区段 PVD 的卷空间大小）。CD 的余量默认值
    // 就是按这个读数定的，换驱动器或换介质后重跑这条核对（ADR-0019 修订）。
    let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
    let transport = optiburn_transport::open(&device).expect("open device");
    let mut mmc = MmcDevice::new(transport);
    wait_until_ready(&mut mmc).expect("device ready");
    let info = mmc.read_disc_information().expect("disc information");
    let profile = mmc.get_configuration().expect("current profile");
    println!("disc: status={:?} sessions={}", info.status, info.sessions);
    if let Ok(track) = mmc.read_track_information(crate::native::LAST_TRACK) {
        println!(
            "track 0xFF: nwa={} nwa_valid={} damaged={} free={} track_blocks={}",
            track.next_writable_address,
            track.nwa_valid,
            track.damaged,
            track.free_blocks,
            track.track_blocks
        );
    }
    if let Some(kind) = profile.media_kind() {
        println!(
            "headroom: append={} close={}",
            crate::capacity::session_headroom(&mut mmc, kind, info.status, false),
            crate::capacity::session_headroom(&mut mmc, kind, info.status, true)
        );
    }
    let Ok(tracks) = mmc.read_toc_tracks() else {
        // 空白盘的 READ TOC 会被驱动器拒绝（实测），此时没有区段可量。
        println!("track list: unavailable on this disc");
        return;
    };
    let starts: Vec<u32> = tracks.iter().map(|track| track.start_lba).collect();
    let mut block = [0u8; optiburn_mmc::SECTOR_BYTES];
    for pair in starts.windows(2) {
        let (start, next) = (pair[0], pair[1]);
        if let Ok(()) = mmc.read_blocks(start + crate::capacity::PVD_BLOCK_OFFSET, &mut block) {
            let space = crate::capacity::parse_pvd_volume_space(&block);
            println!(
                "session at {start}: data={space:?} gap={:?}",
                space.map(|space| next - start - space)
            );
        }
    }
}

#[test]
#[ignore = "needs optical drive and a writable disc"]
fn native_burn_real() {
    let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. /dev/sr0");
    let image = std::env::var("OPTIBURN_IMAGE").expect("set OPTIBURN_IMAGE to an .iso path");
    let mut last = 0.0f32;
    NativeEngine
        .burn(
            &BurnJob {
                image: std::path::PathBuf::from(image),
                device,
                speed: None,
                multi: false,
            },
            &mut |f| last = f,
            &CancelToken::default(),
        )
        .expect("burn failed");
    assert!((last - 1.0).abs() < 1e-6);
}

/// 增长模式真机：把盘上现有路径集合记下来，追加两个小文件（其中一个中文名）
/// 到卷标 GROWTV1，再读回对拍。默认不封盘，盘上原有区段不动。
///
/// `OPTIBURN_GROW_DUMP` 给路径时另写一份合成镜像（`[区段起点块零] + 会话字节`），
/// 供 Linux 上 `mount -t iso9660 -o loop,ro,sbsector=<起点>` 做独立判据。
#[test]
#[ignore = "needs optical drive and an appendable disc"]
fn native_grow_and_read_back_real() {
    let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
    let before: Vec<String> = crate::list_tree(&device)
        .expect("list the disc before growing")
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    println!("盘上原有 {} 个条目：{:?}", before.len(), before);

    let src = std::env::temp_dir().join(format!("optiburn-grow-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).expect("create source dir");
    let ascii = b"optiburn native grow probe\n".to_vec();
    let chinese = "中文追加测试\n".as_bytes().to_vec();
    std::fs::write(src.join("grow-probe.txt"), &ascii).expect("write ascii file");
    std::fs::write(src.join("中文追加.txt"), &chinese).expect("write chinese file");

    let job = GrowJob {
        src: src.clone(),
        device: device.clone(),
        speed: None,
        volume_id: "GROWTV1".to_string(),
        close_disc: false,
        allow_damaged_last_session: false,
    };
    let mut last = 0.0f32;
    grow(
        &job,
        &mut |fraction| last = fraction,
        &CancelToken::default(),
    )
    .expect("grow failed");
    assert!((last - 1.0).abs() < 1e-6, "进度收在 1.0");
    assert_eq!(
        crate::read_volume_id(&device).expect("volume id"),
        "GROWTV1"
    );

    // 读回：旧路径集合是新集合的子集，两个新文件逐字节一致。
    let after: Vec<String> = crate::list_tree(&device)
        .expect("list the disc after growing")
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    println!("追加后 {} 个条目：{:?}", after.len(), after);
    for path in &before {
        assert!(after.contains(path), "旧路径 {path} 在新会话里不见了");
    }
    let out = std::env::temp_dir().join(format!("optiburn-grow-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("create output dir");
    crate::extract_paths(
        &device,
        &["/grow-probe.txt".to_string(), "/中文追加.txt".to_string()],
        &out,
        &CancelToken::default(),
    )
    .expect("extract the appended files");
    assert_eq!(std::fs::read(out.join("grow-probe.txt")).unwrap(), ascii);
    assert_eq!(std::fs::read(out.join("中文追加.txt")).unwrap(), chinese);
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&src);

    if let Ok(dump) = std::env::var("OPTIBURN_GROW_DUMP") {
        dump_session(&device, Path::new(&dump)).expect("dump the session");
        println!("会话镜像已写到 {dump}");
    }
}

/// 把盘上末区段（起点块 + 区段字节）读成一份合成镜像：起点块用零填充，末尾
/// 补上会话的卷空间大小。Linux 上可用 `mount -t iso9660 -o loop,ro,sbsector=`
/// 复核绝对地址约定。
fn dump_session(device: &str, path: &Path) -> Result<(), BurnError> {
    let mut mmc = MmcDevice::new(
        optiburn_transport::open(device).map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?,
    );
    wait_until_ready_for(&mut mmc, Duration::from_secs(60))?;
    let base = mmc
        .read_toc_session_info()?
        .ok_or_else(|| BurnError::ReadFailed("no session to dump".to_string()))?
        .last_session_start;
    let mut pvd = vec![0u8; SECTOR_BYTES];
    mmc.read_blocks(base + 16, &mut pvd)?;
    if pvd[1..6] != *b"CD001" {
        return Err(BurnError::NoIsoSession);
    }
    let blocks = u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]);
    let mut image = vec![0u8; base as usize * SECTOR_BYTES + blocks as usize * SECTOR_BYTES];
    mmc.read_blocks(
        base,
        &mut image[base as usize * SECTOR_BYTES..base as usize * SECTOR_BYTES + SECTOR_BYTES],
    )?;
    let start = base as usize * SECTOR_BYTES;
    let mut offset = SECTOR_BYTES;
    while offset < blocks as usize * SECTOR_BYTES {
        let here = (blocks as usize * SECTOR_BYTES - offset)
            .min(optiburn_mmc::MAX_READ_BLOCKS * SECTOR_BYTES);
        let from = base + (offset / SECTOR_BYTES) as u32;
        let target = start + offset;
        mmc.read_blocks(from, &mut image[target..target + here])?;
        offset += here;
    }
    std::fs::write(path, &image)?;
    println!(
        "区段起点 {base}，卷空间 {blocks} 块，dump {} 字节",
        image.len()
    );
    Ok(())
}

/// 写盘 + 读回对拍：写完把刚写的块用 READ(10) 读回来，与镜像逐字节比较。
/// 这是在没有原生读盘能力之前能拿到的最强验证。默认不封盘（multi），
/// 盘上已有的区段不会被覆写，新会话接在 NWA 后面。
#[test]
#[ignore = "needs optical drive and a writable disc"]
fn native_burn_and_read_back_real() {
    let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
    let image = std::env::var("OPTIBURN_IMAGE").expect("set OPTIBURN_IMAGE to an .iso path");
    let expected = std::fs::read(&image).expect("read the image");
    let blocks = expected.len().div_ceil(SECTOR_BYTES);

    // 与引擎同一套起点判断：空盘 0，可追加盘 NWA。
    let start = {
        let mut mmc = MmcDevice::new(optiburn_transport::open(&device).expect("open device"));
        wait_until_ready(&mut mmc).expect("device ready");
        let info = mmc.read_disc_information().expect("disc information");
        match info.status {
            DiscStatus::Appendable => {
                mmc.read_track_information(0xFF)
                    .expect("track information")
                    .next_writable_address
            }
            _ => 0,
        }
    };
    println!("writing {} blocks at LBA {start}", blocks);

    NativeEngine
        .burn(
            &BurnJob {
                image: std::path::PathBuf::from(&image),
                device: device.clone(),
                speed: None,
                multi: true,
            },
            &mut |_| {},
            &CancelToken::default(),
        )
        .expect("burn failed");

    let mut mmc = MmcDevice::new(optiburn_transport::open(&device).expect("open device"));
    wait_until_ready_for(&mut mmc, Duration::from_secs(60)).expect("device ready after close");
    let mut readback = vec![0u8; blocks * SECTOR_BYTES];
    mmc.read_blocks(start, &mut readback).expect("read back");
    assert_eq!(
        &readback[..expected.len()],
        &expected[..],
        "读回的字节与镜像不一致"
    );
    assert!(
        readback[expected.len()..].iter().all(|b| *b == 0),
        "末块补零"
    );
    println!("read back {} bytes, identical", readback.len());
}
