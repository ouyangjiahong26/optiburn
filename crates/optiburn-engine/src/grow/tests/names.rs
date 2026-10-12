//! 文件名转换后的避重必须保留 Joliet 路径、文件内容与旧块引用。

use std::collections::BTreeSet;

use super::*;

const REPORTS: [&str; 2] = [
    "5-国防科技大学研究生阶段性学术成果等分鉴定表-ASR.doc",
    "5-国防科技大学研究生阶段性学术成果等分鉴定表-ASR.pdf",
];

#[test]
fn existing_chinese_names_do_not_block_an_unrelated_append() {
    const BASE: u32 = 1000;
    let tmp = TempDir::new("existing-names");
    let src = tmp.path().join("old-src");
    std::fs::create_dir_all(&src).expect("create old source");
    for (index, name) in REPORTS.iter().enumerate() {
        std::fs::write(src.join(name), [index as u8]).expect("write old file");
    }
    let iso = tmp.path().join("old.iso");
    build_image(&src, &iso, &ImageSpec::default()).expect("build old image");
    let mut disc = std::fs::read(iso).expect("read old image");
    let old = read_old(&disc, 0);
    let old_extents: Vec<_> = old.root.files.iter().map(|file| file.extent).collect();
    let source = tmp.path().join("new-src");
    std::fs::create_dir_all(&source).expect("create new source");
    std::fs::write(source.join("光驱.pdf"), b"new pdf").expect("write new file");
    let plan = plan_session(Some(old), &source, "NAMES".into(), BASE).expect("plan append");
    let mut session = Vec::new();
    plan.write_image(&mut session, &CancelToken::default())
        .expect("write session");
    disc.resize(BASE as usize * SECTOR_BYTES + session.len(), 0);
    put(&mut disc, BASE, &session);
    let entries = list(disc.clone(), BASE);
    assert_eq!(entries.len(), 3);
    for (index, name) in REPORTS.iter().enumerate() {
        assert_eq!(
            read_file(disc.clone(), BASE, &format!("/{name}")),
            [index as u8]
        );
        assert_eq!(
            parse_entry(&session, BASE, 1, &[name]).0,
            old_extents[index]
        );
    }
    assert_eq!(read_file(disc.clone(), BASE, "/光驱.pdf"), b"new pdf");
    assert_primary_names(&session, BASE, 3);
    // 生成区段还能再次作为嫁接源，旧文件的绝对地址不能丢。
    assert_eq!(read_old(&disc, BASE).root.files.len(), 3);
}

#[test]
fn new_chinese_names_and_truncated_names_read_back() {
    let tmp = TempDir::new("new-names");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source");
    let names = ["中文甲.txt", "中文乙.txt", REPORTS[0], REPORTS[1]];
    for (index, name) in names.iter().enumerate() {
        std::fs::write(src.join(name), [index as u8]).expect("write file");
    }
    let plan = plan_session(None, &src, "NAMES".into(), 0).expect("plan files");
    let mut image = Vec::new();
    plan.write_image(&mut image, &CancelToken::default())
        .expect("write image");
    assert_eq!(list(image.clone(), 0).len(), names.len());
    for (index, name) in names.iter().enumerate() {
        assert_eq!(
            read_file(image.clone(), 0, &format!("/{name}")),
            [index as u8]
        );
    }
    assert_primary_names(&image, 0, names.len());
}

#[test]
fn primary_aliases_skip_existing_suffixes_and_handle_many_collisions() {
    let tmp = TempDir::new("reserved-names");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source");
    // 每个字的 UTF-8 都占三个字节，主命名空间会得到同一个九下划线短名。
    let mut names = vec!["__________1.TXT".to_string()];
    for character in "甲乙丙丁戊己庚辛壬癸子丑".chars() {
        names.push(format!("中文{character}.txt"));
    }
    names.extend(["A中.txt".into(), "A文.txt".into(), "a____1.txt".into()]);
    for (index, name) in names.iter().enumerate() {
        std::fs::write(src.join(name), [index as u8]).expect("write file");
    }
    let plan = plan_session(None, &src, "NAMES".into(), 0).expect("plan files");
    let mut image = Vec::new();
    plan.write_image(&mut image, &CancelToken::default())
        .expect("write image");
    let primary = assert_primary_names(&image, 0, names.len());
    assert!(primary.contains(b"__________1.TXT;1".as_slice()));
    assert!(primary.contains(b"A____1.TXT;1".as_slice()));
    for (index, name) in names.iter().enumerate() {
        assert_eq!(
            read_file(image.clone(), 0, &format!("/{name}")),
            [index as u8]
        );
    }
}

#[test]
fn colliding_directory_names_keep_their_paths_and_children() {
    let tmp = TempDir::new("directory-names");
    let src = tmp.path().join("src");
    let names = ["中文甲", "中文乙", "中文丙"];
    for (index, name) in names.iter().enumerate() {
        std::fs::create_dir_all(src.join(name)).expect("create directory");
        std::fs::write(src.join(name).join("内容.txt"), [index as u8]).expect("write child");
    }
    let plan = plan_session(None, &src, "NAMES".into(), 0).expect("plan directories");
    let mut image = Vec::new();
    plan.write_image(&mut image, &CancelToken::default())
        .expect("write image");
    assert_primary_names(&image, 0, names.len());
    assert_eq!(read_old(&image, 0).root.dirs.len(), names.len());
    for (index, name) in names.iter().enumerate() {
        assert_eq!(
            read_file(image.clone(), 0, &format!("/{name}/内容.txt")),
            [index as u8]
        );
    }
}

#[test]
fn joliet_collisions_still_report_a_conflict() {
    let tmp = TempDir::new("joliet-names");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source");
    let prefix = "中".repeat(64);
    for suffix in ["甲.txt", "乙.txt"] {
        std::fs::write(src.join(format!("{prefix}{suffix}")), b"data").expect("write file");
    }
    let error = plan_session(None, &src, "NAMES".into(), 0).expect_err("Joliet names collide");
    assert!(
        matches!(error, BurnError::GrowConflict(detail) if detail.contains("Joliet namespace"))
    );
}

#[test]
fn colliding_names_with_long_extensions_stay_within_primary_limits() {
    let tmp = TempDir::new("long-extension");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create source");
    let names = [
        format!("甲.{}", "ext".repeat(12)),
        format!("乙.{}", "ext".repeat(12)),
    ];
    for (index, name) in names.iter().enumerate() {
        std::fs::write(src.join(name), [index as u8]).expect("write file");
    }
    let plan = plan_session(None, &src, "NAMES".into(), 0).expect("plan files");
    let mut image = Vec::new();
    plan.write_image(&mut image, &CancelToken::default())
        .expect("write image");
    assert_primary_names(&image, 0, names.len());
    for (index, name) in names.iter().enumerate() {
        assert_eq!(
            read_file(image.clone(), 0, &format!("/{name}")),
            [index as u8]
        );
    }
}

fn read_old(disc: &[u8], base: u32) -> OldSession {
    let mut blocks = MemoryBlocks(disc.to_vec());
    let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
    read_old_session(&mut read, base, &CancelToken::default()).expect("read old session")
}

/// 独立解析主目录记录，核对标识符唯一性、长度与版本后缀。
fn assert_primary_names(image: &[u8], base: u32, count: usize) -> BTreeSet<Vec<u8>> {
    let pvd = &image[DESC_START as usize * SECTOR_BYTES..];
    let extent = u32::from_le_bytes(pvd[158..162].try_into().unwrap());
    let size = u32::from_le_bytes(pvd[166..170].try_into().unwrap()) as usize;
    let start = (extent - base) as usize * SECTOR_BYTES;
    let mut offset = start;
    let mut names = BTreeSet::new();
    while offset < start + size {
        let length = image[offset] as usize;
        if length == 0 {
            offset = (offset / SECTOR_BYTES + 1) * SECTOR_BYTES;
            continue;
        }
        let name_len = image[offset + 32] as usize;
        let name = &image[offset + 33..offset + 33 + name_len];
        if name != [0] && name != [1] {
            assert!(names.insert(name.to_vec()), "主目录标识符重复：{name:?}");
            let directory = image[offset + 25] & 2 != 0;
            assert!(name_len <= if directory { 31 } else { 32 });
            if !directory {
                assert!(name.ends_with(b";1"));
            }
        }
        offset += length;
    }
    assert_eq!(names.len(), count);
    names
}
