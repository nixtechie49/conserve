// Conserve backup system.
// Copyright 2015, 2016, 2017, 2018 Martin Pool.

//! Make a backup by walking a source directory and copying the contents
//! into an archive.

use super::*;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use futures::Future;
use futures::future;
use futures::future::FutureResult;
use futures_cpupool;


#[derive(Debug)]
pub struct BackupOptions {
}


impl BackupOptions {
    pub fn default() -> Self {
        BackupOptions { }
    }
}


/// Make a new backup from a source tree into a band in this archive.
pub fn make_backup(source: &LiveTree, archive: &Archive, _: &BackupOptions) -> Result<()> {
    tree::copy_tree(source, &mut BackupWriter::begin(archive)?)
}


/// Accepts files to write in the archive (in apath order.)
#[derive(Debug)]
struct BackupWriter {
    band: Band,
    block_dir: BlockDir,
    report: Report,
    ib: IndexBuilder,

    // TODO: Seems like we shouldn't quite need Arc and Mutex here, because the entries are almost
    // plain-old-data. However they do contain heap-allocated strings and vecs, and those are
    // probably not safe to share across threads without protection. One way around this, perhaps,
    // would be to treat the IndexBuilder as Arc/Mutex and have the entries push themselves into
    // it, but that's not obviously any better.
    pend: VecDeque<futures_cpupool::CpuFuture<Arc<Mutex<IndexEntry>>, Error>>,
    pool: futures_cpupool::CpuPool,
}


impl BackupWriter {
    /// Create a new BackupWriter.
    ///
    /// This currently makes a new top-level band.
    fn begin(archive: &Archive) -> Result<BackupWriter> {
        let band = Band::create(archive)?;
        let block_dir = band.block_dir();
        let index_builder = band.index_builder();
        Ok(BackupWriter {
            band: band,
            block_dir: block_dir,
            ib: index_builder,
            report: archive.report().clone(),
            pend: VecDeque::new(),
            pool: futures_cpupool::CpuPool::new_num_cpus(),
        })
    }

    /// Append a future that will later produce an IndexEntry, and spawn it in the cpupool.
    fn defer<F, R>(&mut self, f: F)
        where F: FnOnce() -> R + Send + 'static,
              R: futures::Future<Item = Arc<Mutex<IndexEntry>>, Error = Error> + Send + 'static,
    {
        // TODO: Peek into pend, and flush as many futures as have already completed.
        // TODO: Test that with many entries, multiple index blocks are generated. This won't
        // happen with the current code, which only flushes them at the end.
        self.pend.push_back(self.pool.spawn_fn(f));
        //self.index_builder.push(index_entry);
        //self.index_builder.maybe_flush(&self.report)?;
    }
}


/// Make a future that just immediately returns an IndexEntry.
fn immediate_result(ie: IndexEntry) ->
    FutureResult<Arc<Mutex<IndexEntry>>, Error> {
    future::ok(Arc::new(Mutex::new(ie)))
}


impl tree::WriteTree for BackupWriter {
    fn finish(&mut self) -> Result<()> {
        // TODO: Flush pending futures
        for p in self.pend.drain(..) {
            let arc = p.wait()?;
            // By the time the future completes, it should have released the mutex.
            self.ib.push(
                Arc::try_unwrap(arc).unwrap()
                .into_inner().unwrap());
        }
        self.ib.finish_hunk(&self.report)?;
        self.band.close(&self.report)?;
        Ok(())
    }

    fn write_dir(&mut self, source_entry: &Entry) -> Result<()> {
        self.report.increment("dir", 1);
        let apath = source_entry.apath().to_string().clone();
        let mtime = source_entry.unix_mtime();
        self.defer(move || immediate_result(
            IndexEntry {
                apath: apath,
                mtime: mtime,
                kind: Kind::Dir,
                addrs: vec![],
                blake2b: None,
                target: None,
            }));
        Ok(())
    }

    fn write_file(&mut self, source_entry: &Entry, content: &mut std::io::Read) -> Result<()> {
        self.report.increment("file", 1);
        // TODO: Cope graciously if the file disappeared after readdir.
        let (addrs, body_hash) = self.block_dir.store(content, &self.report)?;
        let apath = source_entry.apath().to_string().clone();
        let mtime = source_entry.unix_mtime();
        self.defer(move || immediate_result(IndexEntry {
            apath: apath,
            mtime: mtime,
            kind: Kind::File,
            addrs: addrs,
            blake2b: Some(body_hash),
            target: None,
        }));
        Ok(())
    }

    fn write_symlink(&mut self, source_entry: &Entry) -> Result<()> {
        self.report.increment("symlink", 1);
        let target = source_entry.symlink_target();
        assert!(target.is_some());
        let apath = source_entry.apath().to_string().clone();
        let mtime = source_entry.unix_mtime();
        self.defer(move || immediate_result(IndexEntry {
            apath: apath,
            mtime: mtime,
            kind: Kind::Symlink,
            addrs: vec![],
            blake2b: None,
            target: target,
        }));
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::super::*;
    use test_fixtures::{ScratchArchive, TreeFixture};

    #[cfg(unix)]
    #[test]
    pub fn symlink() {
        let af = ScratchArchive::new();
        let srcdir = TreeFixture::new();
        srcdir.create_symlink("symlink", "/a/broken/destination");
        make_backup(
            &LiveTree::open(srcdir.path(), &Report::new()).unwrap(),
            &af,
            &BackupOptions::default()).unwrap();
        let report = af.report();
        assert_eq!(0, report.get_count("block.write"));
        assert_eq!(0, report.get_count("file"));
        assert_eq!(1, report.get_count("symlink"));
        assert_eq!(0, report.get_count("skipped.unsupported_file_kind"));

        let band_ids = af.list_bands().unwrap();
        assert_eq!(1, band_ids.len());
        assert_eq!("b0000", band_ids[0].as_string());

        let band = Band::open(&af, &band_ids[0]).unwrap();
        assert!(band.is_closed().unwrap());

        let index_entries = band.index_iter(&excludes::excludes_nothing(), &report)
            .unwrap()
            .filter_map(|i| i.ok())
            .collect::<Vec<IndexEntry>>();
        assert_eq!(2, index_entries.len());

        let e2 = &index_entries[1];
        assert_eq!(e2.kind(), Kind::Symlink);
        assert_eq!(e2.apath, "/symlink");
        assert_eq!(e2.target.as_ref().unwrap(), "/a/broken/destination");
    }

    #[test]
    pub fn excludes() {
        let af = ScratchArchive::new();
        let srcdir = TreeFixture::new();

        srcdir.create_dir("test");
        srcdir.create_dir("foooooo");
        srcdir.create_file("foo");
        srcdir.create_file("fooBar");
        srcdir.create_file("foooooo/test");
        srcdir.create_file("test/baz");
        srcdir.create_file("baz");
        srcdir.create_file("bar");

        let report = af.report();
        let lt = &LiveTree::open(srcdir.path(), &report).unwrap()
            .with_excludes(
                excludes::from_strings(
                    &["/**/foo*", "/**/baz"],
                ).unwrap(),
            );
        make_backup(
            &lt,
            &af,
            &BackupOptions::default()).unwrap();

        assert_eq!(1, report.get_count("block.write"));
        assert_eq!(1, report.get_count("file"));
        assert_eq!(2, report.get_count("dir"));
        assert_eq!(0, report.get_count("symlink"));
        assert_eq!(0, report.get_count("skipped.unsupported_file_kind"));
        assert_eq!(4, report.get_count("skipped.excluded.files"));
        assert_eq!(1, report.get_count("skipped.excluded.directories"));
    }

    #[test]
    pub fn empty_file_uses_zero_blocks() {
        use std::io::Read;

        let af = ScratchArchive::new();
        let srcdir = TreeFixture::new();
        srcdir.create_file_with_contents("empty", &[]);
        make_backup(
            &srcdir.live_tree(),
            &af,
            &BackupOptions::default()).unwrap();
        let report = af.report();

        assert_eq!(0, report.get_count("block.write"));
        assert_eq!(1, report.get_count("file"), "file count");

        // Read back the empty file
        let st = StoredTree::open_last(&af).unwrap();
        let empty_entry = st.iter_entries()
            .unwrap()
            .map(|i| i.unwrap())
            .find(|ref i| i.apath == "/empty")
            .expect("found one entry");
        let mut sf = st.file_contents(&empty_entry).unwrap();
        let mut s = String::new();
        assert_eq!(sf.read_to_string(&mut s).unwrap(), 0);
        assert_eq!(s.len(), 0);
    }
}
