// Conserve backup system.
// Copyright 2015, 2016, 2017 Martin Pool.

//! Archives holding backup material.
//!
//! Archives must be initialized before use, which creates the directory.
//!
//! Archives can contain a tree of bands, which themselves contain file versions.

use std;
use std::fs;
use std::fs::read_dir;
use std::path::{Path, PathBuf};

use super::*;
use super::io::{file_exists, require_empty_directory};
use super::jsonio;


const HEADER_FILENAME: &'static str = "CONSERVE";


/// An archive holding backup material.
#[derive(Clone, Debug)]
pub struct Archive {
    /// Top-level directory for the archive.
    path: PathBuf,

    /// Report for operations on this archive.
    report: Report,
}

#[derive(Debug, RustcDecodable, RustcEncodable)]
struct ArchiveHeader {
    conserve_archive_version: String,
}

impl Archive {
    /// Make a new directory to hold an archive, and write the header.
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Archive> {
        let path = path.as_ref();
        let archive = Archive {
            path: path.to_path_buf(),
            report: Report::new(),
        };
        require_empty_directory(&archive.path)?;
        let header = ArchiveHeader { conserve_archive_version: String::from(ARCHIVE_VERSION) };
        let header_filename = path.join(HEADER_FILENAME);
        jsonio::write(&header_filename, &header, &archive.report)
            .chain_err(|| {
                format!("Failed to write archive header: {:?}", header_filename)
            })?;
        info!("Created new archive in {:?}", path.display());
        Ok(archive)
    }

    /// Open an existing archive.
    ///
    /// Checks that the header is correct.
    pub fn open<P: AsRef<Path>>(path: P, report: &Report) -> Result<Archive> {
        let path = path.as_ref();
        let header_path = path.join(HEADER_FILENAME);
        if !file_exists(&header_path)? {
            return Err(ErrorKind::NotAnArchive(path.into()).into());
        }
        let header: ArchiveHeader = jsonio::read(&header_path, &report).chain_err(|| {
            format!("Failed to read archive header")
        })?;
        if header.conserve_archive_version != ARCHIVE_VERSION {
            return Err(
                ErrorKind::UnsupportedArchiveVersion(header.conserve_archive_version).into(),
            );
        }
        Ok(Archive {
            path: path.to_path_buf(),
            report: report.clone(),
        })
    }

    /// Returns a iterator of ids for bands currently present, in arbitrary order.
    pub fn iter_bands_unsorted(self: &Archive) -> Result<IterBands> {
        let read_dir = read_dir(&self.path).chain_err(|| {
            format!("failed reading directory {:?}", &self.path)
        })?;
        Ok(IterBands { dir_iter: read_dir })
    }

    /// Returns a vector of band ids, in sorted order from first to last.
    pub fn list_bands(self: &Archive) -> Result<Vec<BandId>> {
        // Note: For some reason `?` doesn't work here only `try!`.
        let mut band_ids: Vec<BandId> = try!(self.iter_bands_unsorted()?.collect());
        band_ids.sort();
        Ok(band_ids)
    }

    /// Returns the top-level directory for the archive.
    ///
    /// The top-level directory contains a `CONSERVE` header file, and zero or more
    /// band directories.
    pub fn path(self: &Archive) -> &Path {
        self.path.as_path()
    }

    /// Return the `BandId` of the highest-numbered band, or ArchiveEmpty,
    /// or an Err if any occurred reading the directory.
    pub fn last_band_id(self: &Archive) -> Result<BandId> {
        // Walk through list of bands; if any error return that, otherwise return the greatest.
        let mut accum: Option<BandId> = None;
        for next in self.iter_bands_unsorted()? {
            accum = Some(match (next, accum) {
                (Err(e), _) => return Err(e),
                (Ok(b), None) => b,
                (Ok(b), Some(a)) => std::cmp::max(b, a),
            })
        }
        accum.ok_or(ErrorKind::ArchiveEmpty.into())
    }

    /// Return the last completely-written band id.
    pub fn last_complete_band(self: &Archive) -> Result<Band> {
        for id in self.list_bands()?.iter().rev() {
            let b = Band::open(self, &id)?;
            if b.is_closed()? {
                return Ok(b);
            }
        }
        Err(ErrorKind::NoCompleteBands.into())
    }

    /// Return the Report that counts operations on this Archive and objects descended from it.
    pub fn report(&self) -> &Report {
        &self.report
    }
}


pub struct IterBands {
    dir_iter: fs::ReadDir,
}


impl Iterator for IterBands {
    type Item = Result<BandId>;

    fn next(&mut self) -> Option<Result<BandId>> {
        loop {
            let entry = match self.dir_iter.next() {
                None => return None,
                Some(Ok(entry)) => entry,
                Some(Err(e)) => {
                    return Some(Err(e.into()));
                }
            };
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    return Some(Err(e.into()));
                }
            };
            if !ft.is_dir() {
                // TODO: Complain about non-directory children other than the expected header?
                continue;
            }
            if let Ok(name_string) = entry.file_name().into_string() {
                if let Ok(band_id) = BandId::from_string(&name_string) {
                    return Some(Ok(band_id));
                } else {
                    warn!("unexpected archive subdirectory {:?}", &name_string);
                }
            } else {
                warn!(
                    "unexpected archive subdirectory with un-decodable name {:?}",
                    entry.file_name()
                )
            }
        }
    }
}


#[cfg(test)]
mod tests {
    extern crate tempdir;

    use std::fs;
    use std::io::Read;

    use super::*;
    use errors::ErrorKind;
    use {BandId, Report};
    use io::list_dir;
    use test_fixtures::ScratchArchive;

    #[test]
    fn create_then_open_archive() {
        let testdir = tempdir::TempDir::new("conserve-tests").unwrap();
        let arch_path = &testdir.path().join("arch");
        let arch = Archive::create(arch_path).unwrap();

        assert_eq!(arch.path(), arch_path.as_path());
        assert!(arch.list_bands().unwrap().is_empty());

        // We can re-open it.
        Archive::open(arch_path, &Report::new()).unwrap();
        assert!(arch.list_bands().unwrap().is_empty());
        assert!(arch.last_complete_band().is_err());
    }

    #[test]
    fn init_empty_dir() {
        let testdir = tempdir::TempDir::new("conserve-tests").unwrap();
        let arch_path = testdir.path();
        let arch = Archive::create(arch_path).unwrap();

        assert_eq!(arch.path(), arch_path);
        assert!(arch.list_bands().unwrap().is_empty());

        Archive::open(arch_path, &Report::new()).unwrap();
        assert!(arch.list_bands().unwrap().is_empty());
    }


    /// A new archive contains just one header file.
    /// The header is readable json containing only a version number.
    #[test]
    fn empty_archive() {
        let af = ScratchArchive::new();
        let (file_names, dir_names) = list_dir(af.path()).unwrap();
        assert_eq!(file_names.len(), 1);
        assert!(file_names.contains("CONSERVE"));
        assert_eq!(dir_names.len(), 0);

        let header_path = af.path().join("CONSERVE");
        let mut header_file = fs::File::open(&header_path).unwrap();
        let mut contents = String::new();
        header_file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "{\"conserve_archive_version\":\"0.4\"}\n");

        match *af.last_band_id().unwrap_err().kind() {
            ErrorKind::ArchiveEmpty => (),
            ref x => panic!("Unexpected error {:?}", x),
        }

        match *af.last_complete_band().unwrap_err().kind() {
            ErrorKind::NoCompleteBands => (),
            ref x => panic!("Unexpected error {:?}", x),
        }
    }

    /// Can create bands in an archive.
    #[test]
    fn create_bands() {
        use super::super::io::directory_exists;
        let af = ScratchArchive::new();

        // Make one band
        let _band1 = Band::create(&af).unwrap();
        assert!(directory_exists(af.path()).unwrap());
        let (_file_names, dir_names) = list_dir(af.path()).unwrap();
        println!("dirs: {:?}", dir_names);
        assert!(dir_names.contains("b0000"));

        assert_eq!(af.list_bands().unwrap(), vec![BandId::new(&[0])]);
        assert_eq!(af.last_band_id().unwrap(), BandId::new(&[0]));

        // Try creating a second band.
        let _band2 = Band::create(&af).unwrap();
        assert_eq!(
            af.list_bands().unwrap(),
            vec![BandId::new(&[0]), BandId::new(&[1])]
        );
        assert_eq!(af.last_band_id().unwrap(), BandId::new(&[1]));
    }
}
