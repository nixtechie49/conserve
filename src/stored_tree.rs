// Copyright 2017, 2018 Martin Pool.

//! Access a versioned tree stored in the archive.
//!
//! Through this interface you can iterate the contents and retrieve file contents.
//!
//! This is the preferred higher-level interface for reading stored versions. It'll abstract
//! across incremental backups, hiding from the caller that data may be distributed across
//! multiple index files, bands, and blocks.

use super::*;
use super::stored_file::StoredFile;


/// Read index and file contents for a version stored in the archive.
#[derive(Debug)]
pub struct StoredTree {
    archive: Archive,
    band: Band,
    excludes: GlobSet,
}


impl StoredTree {
    /// Open the last complete version in the archive.
    pub fn open_last(archive: &Archive) -> Result<StoredTree> {
        Ok(StoredTree {
            archive: archive.clone(),
            band: archive.last_complete_band()?,
            excludes: excludes::excludes_nothing(),
        })
    }

    /// Open a specified version.
    ///
    /// It's an error if it's not complete.
    pub fn open_version(archive: &Archive, band_id: &BandId) -> Result<StoredTree> {
        let band = Band::open(archive, band_id)?;
        if !band.is_closed()? {
            return Err(ErrorKind::BandIncomplete(band_id.clone()).into());
        }
        Ok(StoredTree {
            archive: archive.clone(),
            band: band,
            excludes: excludes::excludes_nothing(),
        })
    }

    /// Open a specified version.
    ///
    /// This function allows opening incomplete versions, which might contain only a partial copy
    /// of the source tree, or maybe nothing at all.
    pub fn open_incomplete_version(archive: &Archive, band_id: &BandId) -> Result<StoredTree> {
        let band = Band::open(archive, band_id)?;
        Ok(StoredTree {
            archive: archive.clone(),
            band: band,
            excludes: excludes::excludes_nothing(),
        })
    }

    pub fn with_excludes(self, excludes: GlobSet) -> StoredTree {
        StoredTree {
            excludes: excludes,
            .. self
        }
    }

    pub fn band(&self) -> &Band {
        &self.band
    }

    pub fn archive(&self) -> &Archive {
        &self.archive
    }

    pub fn is_closed(&self) -> Result<bool> {
        self.band.is_closed()
    }

    // TODO: Perhaps add a way to open a file by name, bearing in mind this might be slow to
    // call if it reads the whole index.
}


impl ReadTree for StoredTree {
    type E = index::IndexEntry;
    type I = index::Iter;
    type R = stored_file::StoredFile;

    /// Return an iter of index entries in this stored tree.
    fn iter_entries(&self) -> Result<index::Iter> {
        self.band.index_iter(&self.excludes, self.archive.report())
    }

    fn file_contents(&self, entry: &Self::E) -> Result<Self::R> {
        Ok(StoredFile::open(self.band.block_dir(), entry.addrs.clone(), self.archive.report()))
    }
}


#[cfg(test)]
mod test {
    use super::super::*;
    use super::super::test_fixtures::*;

    #[test]
    pub fn open_stored_tree() {
        let af = ScratchArchive::new();
        af.store_two_versions();

        let last_band_id = af.last_band_id().unwrap();
        let st = StoredTree::open_last(&af).unwrap();

        assert_eq!(st.band().id(), last_band_id);

        let names: Vec<String> = st.iter_entries()
            .unwrap()
            .map(|e| e.unwrap().apath)
            .collect();
        let expected = if SYMLINKS_SUPPORTED {
            vec![
                "/",
                "/hello",
                "/hello2",
                "/link",
                "/subdir",
                "/subdir/subfile",
            ]
        } else {
            vec!["/", "/hello", "/hello2", "/subdir", "/subdir/subfile"]
        };
        assert_eq!(expected, names);
    }

    #[test]
    pub fn cant_open_no_versions() {
        let af = ScratchArchive::new();
        assert!(StoredTree::open_last(&af).is_err());
    }
}
