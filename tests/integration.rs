// Copyright 2015, 2016, 2017 Martin Pool.

/// Test Conserve through its public API.

extern crate conserve;

extern crate tempdir;

use std::fs::File;
use std::io::prelude::*;

use conserve::*;
use conserve::test_fixtures::ScratchArchive;
use conserve::test_fixtures::TreeFixture;


#[test]
pub fn simple_backup() {
    let af = ScratchArchive::new();
    let srcdir = TreeFixture::new();
    srcdir.create_file("hello");
    // TODO: Include a symlink only on Unix.
    make_backup(&srcdir.live_tree(), &af, &BackupOptions::default()).unwrap();
    check_backup(&af, &af.report());
    check_restore(&af);
}


#[test]
pub fn simple_backup_with_excludes() {
    let af = ScratchArchive::new();
    let srcdir = TreeFixture::new();
    srcdir.create_file("hello");
    srcdir.create_file("foooo");
    srcdir.create_file("bar");
    srcdir.create_file("baz");
    // TODO: Include a symlink only on Unix.
    let backup_options = BackupOptions::default();
    let lt = srcdir.live_tree()
        .with_excludes(
            excludes::from_strings(
                &["/**/baz", "/**/bar", "/**/fooo*"],
            ).unwrap());
    make_backup(&lt, &af, &backup_options).unwrap();
    check_backup(&af, &af.report());
    check_restore(&af);
}

fn check_backup(af: &ScratchArchive, report: &Report) {
    assert_eq!(1, report.get_count("block.write"));
    assert_eq!(1, report.get_count("file"));
    assert_eq!(1, report.get_count("dir"));
    assert_eq!(0, report.get_count("skipped.unsupported_file_kind"));

    let band_ids = af.list_bands().unwrap();
    assert_eq!(1, band_ids.len());
    assert_eq!("b0000", band_ids[0].as_string());
    assert_eq!(af.last_complete_band().unwrap().id(), BandId::new(&[0]));

    let dur = report.get_duration("source.read");
    let read_us = (dur.subsec_nanos() as u64) / 1000u64 + dur.as_secs() * 1000000u64;
    assert!(read_us > 0);

    let band = Band::open(&af, &band_ids[0]).unwrap();
    assert!(band.is_closed().unwrap());

    let index_entries = band.index_iter(&excludes::excludes_nothing(), &report)
        .unwrap()
        .filter_map(|i| i.ok())
        .collect::<Vec<IndexEntry>>();
    assert_eq!(2, index_entries.len());

    let root_entry = &index_entries[0];
    assert_eq!("/", root_entry.apath);
    assert_eq!(Kind::Dir, root_entry.kind);
    assert!(root_entry.mtime.unwrap() > 0);

    let file_entry = &index_entries[1];
    assert_eq!("/hello", file_entry.apath);
    assert_eq!(Kind::File, file_entry.kind);
    assert!(file_entry.mtime.unwrap() > 0);
    let hash = file_entry.blake2b.as_ref().unwrap();
    assert_eq!(
        "9063990e5c5b2184877f92adace7c801a549b00c39cd7549877f06d5dd0d3a6ca6eee\
        42d5896bdac64831c8114c55cee664078bd105dc691270c92644ccb2ce7",
        hash
    );
}

fn check_restore(af: &ScratchArchive) {
    // TODO: Read back contents of that file.
    let restore_dir = TreeFixture::new();

    let restore_report = Report::new();
    let restore_a = Archive::open(af.path(), &restore_report).unwrap();
    restore_tree(
        &StoredTree::open_last(&restore_a).unwrap(),
        restore_dir.path(),
        &RestoreOptions::default(),
    ).unwrap();

    let block_sizes = restore_report.get_size("block");
    assert!(
        block_sizes.uncompressed == 8 && block_sizes.compressed == 10,
        format!("{:?}", block_sizes)
    );
    let index_sizes = restore_report.get_size("index");
    assert_eq!(
        index_sizes.uncompressed,
        462,
        "index_sizes.uncompressed on restore"
    );
    assert!(index_sizes.compressed <= 292, index_sizes.compressed);
    // TODO: Check what was restored.
}


/// Store and retrieve large files.
#[test]
fn large_file() {
    let af = ScratchArchive::new();

    let tf = TreeFixture::new();
    let large_content = String::from("a sample large file\n").repeat(1000000);
    tf.create_file_with_contents("large", &large_content.as_bytes());

    make_backup(&tf.live_tree(), &af, &BackupOptions::default()).unwrap();
    let report = af.report();
    assert_eq!(report.get_count("file"), 1);
    assert_eq!(report.get_count("file.large"), 1);

    // Try to restore it
    let rd = tempdir::TempDir::new("conserve_test_restore").unwrap();
    let restore_report = Report::new();
    let restore_archive = Archive::open(af.path(), &restore_report).unwrap();
    restore_tree(
        &StoredTree::open_last(&restore_archive).unwrap(),
        rd.path(),
        &RestoreOptions::default(),
    ).unwrap();

    assert_eq!(report.get_count("file"), 1);

    // TODO: Restore should also set file.large etc.
    let mut content = String::new();
    File::open(rd.path().join("large"))
        .unwrap()
        .read_to_string(&mut content)
        .unwrap();
    assert_eq!(large_content, content);
}
