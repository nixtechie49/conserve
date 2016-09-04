// Conserve backup system.
// Copyright 2015, 2016 Martin Pool.

//! File contents are stored in data blocks within an archive band.
//!
//! Blocks are required to be less than 1GB uncompressed, so they can be held
//! entirely in memory on a typical machine.

use std::fs;
use std::io;
use std::io::prelude::*;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::time;

use blake2_rfc::blake2b;
use blake2_rfc::blake2b::Blake2b;
use brotli2::write::BrotliEncoder;
use rustc_serialize::hex::ToHex;

use tempfile;

use super::io::{read_and_decompress};
use super::report::Report;

/// Use a moderate Brotli compression level.
///
/// TODO: Is this a good tradeoff?
const BROTLI_COMPRESSION_LEVEL: u32 = 4;

/// Use the maximum 64-byte hash.
const BLAKE_HASH_SIZE_BYTES: usize = 64;

/// Take this many characters from the block hash to form the subdirectory name.
const SUBDIR_NAME_CHARS: usize = 3;

/// The unique identifier for a block: its hexadecimal `BLAKE2b` hash.
pub type BlockHash = String;


/// Points to some compressed data inside the block dir.
///
/// Identifiers are: which file contains it, at what (pre-compression) offset,
/// and what (pre-compression) length.
#[derive(Debug, PartialEq, RustcDecodable, RustcEncodable)]
pub struct Reference {
    /// ID of the block storing this info (in future, salted.)
    pub hash: String,

    /// Position in this block where data begins.
    pub start: u64,

    /// Length of this block to be used.
    pub len: u64,
}


/// A readable, writable directory within a band holding data blocks.
pub struct BlockDir {
    pub path: PathBuf,
}

fn block_name_to_subdirectory(block_hash: &str) -> &str {
    &block_hash[..SUBDIR_NAME_CHARS]
}

impl BlockDir {
    /// Create a BlockDir accessing `path`, which must exist as a directory.
    pub fn new(path: &Path) -> BlockDir {
        BlockDir { path: path.to_path_buf() }
    }

    /// Return the subdirectory in which we'd put a file called `hash_hex`.
    fn subdir_for(self: &BlockDir, hash_hex: &str) -> PathBuf {
        let mut buf = self.path.clone();
        buf.push(block_name_to_subdirectory(hash_hex));
        buf
    }

    /// Return the full path for a file called `hex_hash`.
    fn path_for_file(self: &BlockDir, hash_hex: &str) -> PathBuf {
        let mut buf = self.subdir_for(hash_hex);
        buf.push(hash_hex);
        buf
    }

    pub fn store_file(&self, from_file: &mut fs::File, length_advice: u64, report: &mut Report)
    -> io::Result<(Vec<Reference>, BlockHash)> {
        // TODO: Don't read the whole thing in one go, use smaller buffers to cope with
        //       large files.

        let tempf = try!(tempfile::NamedTempFileOptions::new()
            .prefix("tmp").create_in(&self.path));
        let mut encoder = BrotliEncoder::new(tempf, BROTLI_COMPRESSION_LEVEL);
        let mut hasher = Blake2b::new(BLAKE_HASH_SIZE_BYTES);
        let mut uncompressed_length: u64 = 0;

        // Use the stat size as guidance for a buffer, but always read the whole thing.
        let mut buf = Vec::<u8>::with_capacity(length_advice as usize);

        let start_read = time::Instant::now();
        try!(from_file.read_to_end(&mut buf));
        report.increment_duration("source.read", start_read.elapsed());

        let start_compress = time::Instant::now();
        try!(encoder.write_all(&buf));
        report.increment_duration("block.compress", start_compress.elapsed());

        uncompressed_length += buf.len() as u64;

        let start_hash = time::Instant::now();
        hasher.update(&buf);
        report.increment_duration("block.hash", start_hash.elapsed());

        let mut tempf = try!(encoder.finish());
        let hex_hash = hasher.finalize().as_bytes().to_hex();

        // TODO: Update this when the stored blocks can be different from body hash.
        let refs = vec![Reference {
            hash: hex_hash.clone(),
            start: 0,
            len: uncompressed_length,
        }];
        if try!(self.contains(&hex_hash)) {
            report.increment("block.write.already_present", 1);
            return Ok((refs, hex_hash));
        }
        let compressed_length: u64 = try!(tempf.seek(SeekFrom::Current(0)));
        try!(super::io::ensure_dir_exists(&self.subdir_for(&hex_hash)));
        if let Err(e) = tempf.persist_noclobber(&self.path_for_file(&hex_hash)) {
            if e.error.kind() == ErrorKind::AlreadyExists {
                // Suprising we saw this rather than detecting it above.
                warn!("Unexpected late detection of existing block {:?}", hex_hash);
                report.increment("block.write.already_present", 1);
                return Ok((refs, hex_hash));
            } else {
                return Err(e.error);
            }
        }
        report.increment("block.write.count", 1);
        report.increment_size("block.write", uncompressed_length, compressed_length);
        Ok((refs, hex_hash))
    }

    /// True if the named block is present in this directory.
    pub fn contains(self: &BlockDir, hash: &str) -> io::Result<bool> {
        match fs::metadata(self.path_for_file(hash)) {
            Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// Read back the contents of a block, as a byte array.
    #[allow(unused)]
    pub fn get(self: &BlockDir, hash: &str, report: &mut Report) -> io::Result<Vec<u8>> {
        let path = self.path_for_file(hash);
        let decompressed = match read_and_decompress(&path) {
            Ok(d) => d,
            Err(e) => {
                error!("Block file {:?} couldn't be decompressed: {:?}", path, e);
                report.increment("block.read.corrupt", 1);
                return Err(e);
            }
        };
        report.increment("block.read.count", 1);

        let actual_hash = blake2b::blake2b(BLAKE_HASH_SIZE_BYTES, &[], &decompressed)
            .as_bytes()
            .to_hex();
        if actual_hash != *hash {
            report.increment("block.read.misplaced", 1);
            error!("Block file {:?} has actual decompressed hash {:?}",
                   path,
                   actual_hash);
            return Err(io::Error::new(ErrorKind::InvalidData, "block.read.misplaced"));
        }
        Ok(decompressed)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::SeekFrom;
    use std::io::prelude::*;
    use tempdir;
    use tempfile;
    use super::{BlockDir};
    use super::super::report::Report;

    const EXAMPLE_TEXT: &'static [u8] = b"hello!";
    const EXAMPLE_BLOCK_HASH: &'static str =
        "66ad1939a9289aa9f1f1d9ad7bcee694293c7623affb5979bd3f844ab4adcf2145b117b7811b3cee\
        31e130efd760e9685f208c2b2fb1d67e28262168013ba63c";

    fn make_example_file() -> tempfile::NamedTempFile {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(EXAMPLE_TEXT).unwrap();
        tf.flush().unwrap();
        tf.seek(SeekFrom::Start(0)).unwrap();
        tf
    }

    fn setup() -> (tempdir::TempDir, BlockDir) {
        let testdir = tempdir::TempDir::new("block_test").unwrap();
        let block_dir = BlockDir::new(testdir.path());
        (testdir, block_dir)
    }

    #[test]
    pub fn write_to_file() {
        let expected_hash = EXAMPLE_BLOCK_HASH.to_string();
        let mut report = Report::new();
        let (testdir, block_dir) = setup();
        let mut example_file = make_example_file();

        assert_eq!(block_dir.contains(&expected_hash).unwrap(), false);

        let (refs, hash_hex) = block_dir.store_file(&mut example_file, 0, &mut report).unwrap();
        assert_eq!(hash_hex, EXAMPLE_BLOCK_HASH);

        // Should be in one block, and as it's currently unsalted the hash is the same.
        assert_eq!(1, refs.len());
        assert_eq!(0, refs[0].start);
        assert_eq!(EXAMPLE_BLOCK_HASH, refs[0].hash);

        // Subdirectory and file should exist
        let expected_file = testdir.path().join("66a").join(EXAMPLE_BLOCK_HASH);
        let attr = fs::metadata(expected_file).unwrap();
        assert!(attr.is_file());

        // TODO: Inspect `refs`

        assert_eq!(block_dir.contains(&expected_hash).unwrap(), true);

        assert_eq!(report.get_count("block.write.already_present"), 0);
        assert_eq!(report.get_count("block.write.count"), 1);
        assert_eq!(report.get_size("block.write"), (6, 10));

        // Try to read back
        assert_eq!(report.get_count("block.read.count"), 0);
        let back = block_dir.get(&expected_hash, &mut report).unwrap();
        assert_eq!(back, EXAMPLE_TEXT);
        assert_eq!(report.get_count("block.read.count"), 1);
    }

    #[test]
    pub fn write_same_data_again() {
        let mut report = Report::new();
        let (_testdir, block_dir) = setup();

        let mut example_file = make_example_file();
        let (refs1, hash1) = block_dir.store_file(&mut example_file, 0, &mut report).unwrap();
        assert_eq!(report.get_count("block.write.already_present"), 0);
        assert_eq!(report.get_count("block.write.count"), 1);

        let mut example_file = make_example_file();
        let (refs2, hash2) = block_dir.store_file(&mut example_file, 0, &mut report).unwrap();
        assert_eq!(report.get_count("block.write.already_present"), 1);
        assert_eq!(report.get_count("block.write.count"), 1);

        assert_eq!(hash1, hash2);
        assert_eq!(refs1, refs2);
    }
}
