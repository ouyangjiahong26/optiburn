//! 抢救与 zip 重建的单元测试：写入边界扫描、文件定性、抽取兜底、半截 zip 的处置。

use super::*;

#[test]
fn salvage_classification_uses_the_written_boundary() {
    const BOUNDARY: u32 = 1100;
    // 整段在边界之前：完整。
    assert_eq!(
        classify_salvage_state(1000, 2048, BOUNDARY),
        SalvageState::Complete
    );
    // 跨过边界：半截，能读多少算多少。
    assert_eq!(
        classify_salvage_state(1099, 4096, BOUNDARY),
        SalvageState::Truncated {
            readable_bytes: 2048
        }
    );
    // 起点就在边界之后：一个字节都没写。
    assert_eq!(
        classify_salvage_state(1100, 4096, BOUNDARY),
        SalvageState::Missing
    );
    // 零长度文件不需要数据，算完整。
    assert_eq!(
        classify_salvage_state(2000, 0, BOUNDARY),
        SalvageState::Complete
    );
}

#[test]
fn written_boundary_binary_searches_the_first_unreadable_block() {
    const START: u32 = 301_170;
    const WRITTEN: u32 = 9_253;
    let mut reads = 0u32;
    let mut read = |lba: u32, out: &mut [u8]| {
        reads += 1;
        if lba >= START + WRITTEN {
            return Err(BurnError::ReadFailed("未写位置读失败".to_string()));
        }
        out.fill(0xAB);
        Ok(())
    };
    let boundary = written_boundary(&mut read, START, 23_438, &CancelToken::default())
        .expect("扫描要给出边界");
    assert_eq!(boundary, START + WRITTEN);
    // 二分：两万三千多块只读十几次，线性扫要读九千多块。
    assert!(reads <= 20, "读次数 {reads} 应是对数级");
}

#[test]
fn written_boundary_counts_all_zero_blocks_as_written() {
    // 会话开头的系统区就是全零，文件内容也可以是零：全零不算中断点。
    let mut read = |lba: u32, out: &mut [u8]| {
        if lba >= 1700 {
            return Err(BurnError::ReadFailed("未写位置读失败".to_string()));
        }
        out.fill(0);
        Ok(())
    };
    let boundary =
        written_boundary(&mut read, 1000, 5000, &CancelToken::default()).expect("扫描要给出边界");
    assert_eq!(boundary, 1700);
}

#[test]
fn written_boundary_is_the_start_when_the_first_block_fails() {
    let mut read = |_lba: u32, _out: &mut [u8]| Err(BurnError::ReadFailed("读失败".to_string()));
    let boundary =
        written_boundary(&mut read, 500, 100, &CancelToken::default()).expect("扫描要给出边界");
    assert_eq!(boundary, 500);
}

#[test]
fn written_boundary_reaches_the_session_end_when_everything_reads() {
    let mut read = |_lba: u32, out: &mut [u8]| {
        out.fill(7);
        Ok(())
    };
    let boundary =
        written_boundary(&mut read, 500, 100, &CancelToken::default()).expect("扫描要给出边界");
    assert_eq!(boundary, 600);
}

#[test]
fn extraction_that_reads_nothing_leaves_no_empty_file() {
    // 边界扫描假定可读性单调；固件在已写区中间留坏块（或对未写位置回零）时，
    // 定性可能比实际乐观。文件的第一块就读不出来：报告记“没写”，dest 里不能
    // 留 0 字节的空壳。零长度文件不同：readable_bytes 为 0，抽出的 0 字节就是
    // 它的全部内容，要留住。
    let dir = TempDir::new("salvage-empty-extract");
    let disc = ScriptedDisc::new(vec![0u8; 4 * SECTOR_BYTES], 0, vec![]);
    let mut mmc = MmcDevice::new(Box::new(disc));
    let token = CancelToken::default();

    let broken = dir.path().join("broken.bin");
    let written =
        extract_salvage_file(&mut mmc, 9, 3 * SECTOR_BYTES as u64, &broken, &token).unwrap();
    assert_eq!(written, 0, "首块读失败一个字节都写不下");
    assert!(!broken.exists(), "没写的文件不能在 dest 留空壳");

    let empty = dir.path().join("empty.bin");
    let written = extract_salvage_file(&mut mmc, 9, 0, &empty, &token).unwrap();
    assert_eq!(written, 0);
    assert!(empty.exists(), "零长度文件抽出 0 字节也算完整");
}

/// 中央目录与 EOCD（存储与描述符两种造法共用；方法都是存储，区别只在标志位）。
fn push_directory_and_eocd(
    out: &mut Vec<u8>,
    records: &[(&str, u16, u32, u32, u32)], // (名字, 标志, CRC, 大小, 本地偏移)
) {
    let directory_offset = out.len() as u32;
    let directory_start = out.len();
    for (name, flags, crc, size, offset) in records {
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&20u16.to_le_bytes()); // 制作版本
        out.extend_from_slice(&20u16.to_le_bytes()); // 解压所需版本
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 方法：存储
        out.extend_from_slice(&0u16.to_le_bytes()); // 时间与日期留零
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 额外字段长度
        out.extend_from_slice(&0u16.to_le_bytes()); // 注释长度
        out.extend_from_slice(&0u16.to_le_bytes()); // 起始磁盘号
        out.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
        out.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    let directory_size = (out.len() - directory_start) as u32;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes()); // 磁盘号
    out.extend_from_slice(&0u16.to_le_bytes()); // 中央目录起始磁盘号
    out.extend_from_slice(&(records.len() as u16).to_le_bytes());
    out.extend_from_slice(&(records.len() as u16).to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // 注释长度
}

/// 手造一个“存储”（不压缩）的 zip：多个条目加中央目录与 EOCD。
fn build_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut records = Vec::new();
    for (name, payload) in entries {
        let offset = out.len() as u32;
        let crc = zip_recover::crc32(payload);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 标志：长度就在本地头里
        out.extend_from_slice(&0u16.to_le_bytes()); // 方法：不压缩
        out.extend_from_slice(&0u16.to_le_bytes()); // 时间与日期留零
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 额外字段长度
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(payload);
        records.push((*name, 0u16, crc, payload.len() as u32, offset));
    }
    push_directory_and_eocd(&mut out, &records);
    out
}

/// 手造一个带数据描述符（通用位 bit 3）的 zip：本地头的长度字段是零，真实长度在
/// 数据后的描述符里。`with_signature` 决定描述符带不带 `PK\x07\x08` 签名。
fn build_descriptor_zip(entries: &[(&str, &[u8])], with_signature: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut records = Vec::new();
    for (name, payload) in entries {
        let offset = out.len() as u32;
        let crc = zip_recover::crc32(payload);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0b1000u16.to_le_bytes()); // bit 3：长度写在描述符里
        out.extend_from_slice(&0u16.to_le_bytes()); // 方法：不压缩
        out.extend_from_slice(&0u16.to_le_bytes()); // 时间与日期留零
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // CRC 与两个长度都是零
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 额外字段长度
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(payload);
        if with_signature {
            out.extend_from_slice(b"PK\x07\x08");
        }
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        records.push((*name, 0b1000u16, crc, payload.len() as u32, offset));
    }
    push_directory_and_eocd(&mut out, &records);
    out
}

#[test]
fn truncated_zip_keeps_only_complete_entries() {
    let zip = build_stored_zip(&[
        ("a.txt", b"aaaa"),
        ("b.txt", b"bbbbbbbb"),
        ("c.txt", b"cccccccccccc"),
    ]);
    // 砍在第三个条目的数据中间：前两条完整，第三条不完整，中央目录全丢。
    let keep = 30 + 5 + 4 + 30 + 5 + 8 + 30 + 5 + 6;
    let truncated = zip[..keep].to_vec();

    let dir = std::env::temp_dir().join("optiburn-zip-recover-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.zip");
    std::fs::write(&path, &truncated).unwrap();

    let salvaged = zip_recover::recover_truncated_zip(&path)
        .unwrap()
        .expect("应重建");
    assert_eq!(salvaged.entries, 2);
    assert_eq!(salvaged.stored_bytes, 12);
    assert_eq!(salvaged.dropped_entry.as_deref(), Some("c.txt"));

    // 重建后的文件应等于“两条条目的原样字节 + 中央目录与 EOCD”。
    let rebuilt = std::fs::read(&path).unwrap();
    let complete = build_stored_zip(&[("a.txt", b"aaaa"), ("b.txt", b"bbbbbbbb")]);
    let eocd = &rebuilt[rebuilt.len() - 22..];
    assert_eq!(&eocd[..4], b"PK\x05\x06", "末尾要有 EOCD");
    assert_eq!(
        u16::from_le_bytes([eocd[10], eocd[11]]),
        2,
        "EOCD 报两条条目"
    );
    assert_eq!(
        &rebuilt[..rebuilt.len() - 22],
        &complete[..complete.len() - 22]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn intact_zip_is_not_rebuilt() {
    let zip = build_stored_zip(&[("a.txt", b"aaaa")]);
    let dir = std::env::temp_dir().join("optiburn-zip-recover-test-3");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("whole.zip");
    std::fs::write(&path, &zip).unwrap();
    assert!(zip_recover::recover_truncated_zip(&path).unwrap().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), zip);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn descriptor_style_zip_keeps_only_complete_entries() {
    for with_signature in [true, false] {
        let zip = build_descriptor_zip(
            &[
                ("a.txt", b"aaaa"),
                ("b.txt", b"bbbbbbbb"),
                ("c.txt", b"cccccc"),
            ],
            with_signature,
        );
        // 砍在第三个条目的数据中间（它的本地头 30 + 名字 5 + 数据 6 只留 3 字节）。
        let keep =
            zip.len() - 22 - (3 * (46 + 5)) - (6 + 4 + 4 + if with_signature { 4 } else { 0 }) + 3;
        let cut = zip[..keep].to_vec();
        let dir = std::env::temp_dir().join("optiburn-zip-descriptor-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.zip");
        std::fs::write(&path, &cut).unwrap();

        let salvaged = zip_recover::recover_truncated_zip(&path)
            .unwrap()
            .unwrap_or_else(|| panic!("带签名={with_signature} 时应识别出 zip 链"));
        assert_eq!(salvaged.entries, 2, "带签名={with_signature}");
        assert_eq!(salvaged.stored_bytes, 12, "带签名={with_signature}");
        assert_eq!(salvaged.dropped_entry.as_deref(), Some("c.txt"));
        // 重建后的 zip 里两条条目的长度字段要靠描述符取得（本地头是零）。
        let rebuilt = std::fs::read(&path).unwrap();
        let directory_start = rebuilt
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("重建后要有中央目录");
        let first = &rebuilt[directory_start..];
        assert_eq!(
            u32::from_le_bytes([first[20], first[21], first[22], first[23]]),
            4,
            "第一条的压缩长度应来自描述符"
        );
        assert_eq!(
            u32::from_le_bytes([first[24], first[25], first[26], first[27]]),
            4,
            "第一条的解压长度应来自描述符"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn zip_cut_inside_the_first_entry_reports_it_without_rewriting() {
    let zip = build_stored_zip(&[("big.bin", &[7u8; 4000])]);
    let cut = zip[..100].to_vec();
    let dir = std::env::temp_dir().join("optiburn-zip-first-entry-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.zip");
    std::fs::write(&path, &cut).unwrap();

    let salvaged = zip_recover::recover_truncated_zip(&path)
        .unwrap()
        .expect("应识别出 zip 链");
    assert_eq!(salvaged.entries, 0);
    assert_eq!(salvaged.dropped_entry.as_deref(), Some("big.bin"));
    assert_eq!(std::fs::read(&path).unwrap(), cut, "没有完整条目就不重写");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn zip_name_check_survives_multibyte_names() {
    // 中文名截断或没写时也会走这个判断：按字符切后缀会踩非字符边界而 panic。
    assert!(!is_zip_name("附件"));
    assert!(!is_zip_name("会议记录"));
    assert!(!is_zip_name("文件ab"));
    assert!(is_zip_name("x.zip"));
    assert!(is_zip_name("档案.ZIP"));
    assert!(!is_zip_name("zip"));
}

#[test]
fn final_state_treats_an_empty_file_as_complete() {
    // 零长度文件抽出 0 字节就是完整，不能因为读出来是零就报“一个字节都没写”。
    assert_eq!(final_salvage_state(0, 0), SalvageState::Complete);
    assert_eq!(final_salvage_state(2048, 2048), SalvageState::Complete);
    assert_eq!(final_salvage_state(4096, 2048), SalvageState::Complete);
    assert_eq!(final_salvage_state(0, 4096), SalvageState::Missing);
    assert_eq!(
        final_salvage_state(1024, 4096),
        SalvageState::Truncated {
            readable_bytes: 1024
        }
    );
}

#[test]
fn descriptor_scan_skips_a_false_header_inside_stored_data() {
    // 存储条目的数据里出现 PK\x03\x04（zip 套 zip、docx、apk 都会）。取第一个签名
    // 当条目结尾会把这一条连同后面完整的条目一起丢掉，所以要按描述符自洽性挑。
    let nested = b"PK\x03\x04\x14\x00\x00\x00\x00\x00junkjunkjunk";
    let zip = build_descriptor_zip(&[("outer.bin", nested), ("next.txt", b"tail")], false);
    let keep = zip.len() - 22 - 2 * (46 + 5) - (4 + 4 + 4) + 2;
    let cut = zip[..keep].to_vec();
    let dir = std::env::temp_dir().join("optiburn-zip-nested-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.zip");
    std::fs::write(&path, &cut).unwrap();

    let salvaged = zip_recover::recover_truncated_zip(&path)
        .unwrap()
        .expect("应识别出 zip 链");
    assert_eq!(salvaged.entries, 1, "假签名不该截断第一条");
    assert_eq!(salvaged.dropped_entry.as_deref(), Some("next.txt"));
    assert!(salvaged.rebuilt);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn zip_with_too_many_entries_keeps_the_raw_bytes() {
    // 中央目录与 EOCD 的条目数是 16 位：装不下就不重建，不写自相矛盾的 EOCD。
    let mut zip = Vec::new();
    let total = u16::MAX as usize + 1;
    for index in 0..total {
        let name = format!("{index:05}");
        zip.extend_from_slice(b"PK\x03\x04");
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&zip_recover::crc32(b"a").to_le_bytes());
        zip.extend_from_slice(&1u32.to_le_bytes());
        zip.extend_from_slice(&1u32.to_le_bytes());
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(name.as_bytes());
        zip.extend_from_slice(b"a");
    }
    let dir = std::env::temp_dir().join("optiburn-zip-many-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("many.zip");
    std::fs::write(&path, &zip).unwrap();

    let salvaged = zip_recover::recover_truncated_zip(&path)
        .unwrap()
        .expect("应识别出 zip 链");
    assert_eq!(salvaged.entries as usize, total);
    assert!(!salvaged.rebuilt, "超出表达范围时不该重建");
    assert_eq!(std::fs::read(&path).unwrap(), zip, "原始字节要留着");
    std::fs::remove_dir_all(&dir).ok();
}

/// 驱动器登记了未关闭轨道但数据区一个块都没写：描述符区读不出来应按没有可抢救
/// 内容处理，而不是以 SCSI 错误退出（issue #51）。
#[test]
fn unwritten_open_track_is_nothing_to_salvage() {
    let dir = TempDir::new("salvage-unwritten-track");
    let disc = ScriptedDisc::new(
        vec![0u8; 2000 * SECTOR_BYTES],
        1000,
        vec![(1, 0), (2, 1000), (3, 2000)],
    )
    .with_open_track(2000);
    let mmc = MmcDevice::new(Box::new(disc));
    let report = salvage_unclosed_session_on_device(mmc, dir.path(), &CancelToken::default())
        .expect("预期内的探测失败不该报成错误");
    assert!(report.is_none(), "数据区没落盘应判为没有可抢救内容");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "没有可抢救内容时目标目录应为空"
    );
}

/// 描述符区可读的未关闭轨道仍照旧抢救：列出文件、定性、抽取。
#[test]
fn readable_open_track_still_salvages() {
    const BASE: u32 = 2000;
    let session = session_bytes(BASE, "SALVAGE", &[("keep.txt", "kept")]);
    let blocks = BASE + (session.len() / SECTOR_BYTES) as u32;
    let disc = ScriptedDisc::new(
        disc_with_session(BASE, blocks, &session),
        1000,
        vec![(1, 0), (2, 1000), (3, BASE)],
    )
    .with_open_track(BASE);
    let mmc = MmcDevice::new(Box::new(disc));
    let dir = TempDir::new("salvage-readable-track");
    let report = salvage_unclosed_session_on_device(mmc, dir.path(), &CancelToken::default())
        .expect("可读的未关闭轨道应能抢救")
        .expect("应产出报告");
    assert!(
        report
            .files
            .iter()
            .any(|file| file.path == "/keep.txt" && file.state == SalvageState::Complete),
        "{:?}",
        report.files
    );
    assert_eq!(std::fs::read(dir.path().join("keep.txt")).unwrap(), b"kept");
}

/// 轨道起点不晚于末个已关闭区段：不是未关闭轨道，返回 None。
#[test]
fn open_track_not_after_last_session_is_none() {
    let dir = TempDir::new("salvage-no-open-track");
    let disc = ScriptedDisc::new(vec![], 2000, vec![(1, 0), (2, 2000)]).with_open_track(2000);
    let mmc = MmcDevice::new(Box::new(disc));
    assert!(
        salvage_unclosed_session_on_device(mmc, dir.path(), &CancelToken::default())
            .unwrap()
            .is_none()
    );
}

/// 描述符区读得出来但没有 CD001：与改动前一致，返回 None。
#[test]
fn non_iso_descriptor_on_open_track_is_none() {
    let dir = TempDir::new("salvage-non-iso-track");
    let disc = ScriptedDisc::new(
        vec![0u8; 2100 * SECTOR_BYTES],
        1000,
        vec![(1, 0), (2, 1000), (3, 2000)],
    )
    .with_open_track(2000);
    let mmc = MmcDevice::new(Box::new(disc));
    assert!(
        salvage_unclosed_session_on_device(mmc, dir.path(), &CancelToken::default())
            .unwrap()
            .is_none()
    );
}

/// READ TRACK INFORMATION 失败仍是错误，不并进 None。
#[test]
fn track_info_failure_is_still_an_error() {
    let dir = TempDir::new("salvage-track-info-fail");
    let disc = ScriptedDisc::new(vec![], 1000, vec![(1, 0), (2, 1000)]);
    let mmc = MmcDevice::new(Box::new(disc));
    assert!(matches!(
        salvage_unclosed_session_on_device(mmc, dir.path(), &CancelToken::default()),
        Err(BurnError::Mmc(_))
    ));
}
