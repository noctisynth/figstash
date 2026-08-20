//! Content-addressed zstd blob storage.

use crate::error::{corrupt_error, io_error};
use figstash_core::AppResult;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn put_bytes(&self, bytes: &[u8]) -> AppResult<String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let mut reader = bytes;
        self.write_compressed(&hash, &mut reader)?;
        Ok(hash)
    }

    pub(crate) fn put_file(&self, path: &Path) -> AppResult<(String, u64)> {
        let mut input = File::open(path)
            .map_err(|error| io_error("Failed to open a staged response.", error))?;
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|error| io_error("Failed to read a staged response.", error))?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read).map_err(|error| {
                corrupt_error("A staged response size cannot be represented.", error)
            })?;
            size = size.checked_add(read_u64).ok_or_else(|| {
                corrupt_error(
                    "A staged response exceeds the supported size.",
                    "size overflow",
                )
            })?;
            hasher.update(&buffer[..read]);
        }
        let hash = hasher.finalize().to_hex().to_string();
        let mut input = File::open(path)
            .map_err(|error| io_error("Failed to reopen a staged response.", error))?;
        self.write_compressed(&hash, &mut input)?;
        Ok((hash, size))
    }

    pub(crate) fn get(&self, hash: &str) -> AppResult<Vec<u8>> {
        validate_hash(hash)?;
        let path = self.path(hash);
        let input = File::open(&path)
            .map_err(|error| io_error("Failed to open a snapshot blob.", error))?;
        let mut decoder = zstd::Decoder::new(input)
            .map_err(|error| io_error("Failed to initialize blob decompression.", error))?;
        let mut bytes = Vec::new();
        decoder
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("Failed to decompress a snapshot blob.", error))?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != hash {
            return Err(corrupt_error(
                "A snapshot blob failed its BLAKE3 integrity check.",
                format!("expected {hash}, got {actual}"),
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn path(&self, hash: &str) -> PathBuf {
        self.root
            .join("b3")
            .join(&hash[..2])
            .join(format!("{hash}.zst"))
    }

    fn write_compressed(&self, hash: &str, reader: &mut dyn Read) -> AppResult<()> {
        validate_hash(hash)?;
        let destination = self.path(hash);
        if destination.exists() {
            return Ok(());
        }
        let parent = destination
            .parent()
            .ok_or_else(|| corrupt_error("A blob destination has no parent directory.", hash))?;
        fs::create_dir_all(parent)
            .map_err(|error| io_error("Failed to create a blob directory.", error))?;
        super::catalog::restrict_directory(parent)?;
        let temporary = parent.join(format!(".{hash}.{}.tmp", Uuid::new_v4()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| io_error("Failed to create a temporary blob.", error))?;
        super::catalog::restrict_file(&temporary)?;
        let mut encoder = zstd::Encoder::new(file, 3)
            .map_err(|error| io_error("Failed to initialize blob compression.", error))?;
        std::io::copy(reader, &mut encoder)
            .map_err(|error| io_error("Failed to compress a snapshot blob.", error))?;
        let mut file = encoder
            .finish()
            .map_err(|error| io_error("Failed to finish blob compression.", error))?;
        file.flush()
            .map_err(|error| io_error("Failed to flush a snapshot blob.", error))?;
        file.sync_all()
            .map_err(|error| io_error("Failed to sync a snapshot blob.", error))?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => Ok(()),
            Err(error) if destination.exists() => {
                fs::remove_file(&temporary).map_err(|remove_error| {
                    io_error("Failed to remove a duplicate temporary blob.", remove_error)
                })?;
                let _ = error;
                Ok(())
            }
            Err(error) => Err(io_error("Failed to commit a snapshot blob.", error)),
        }
    }
}

fn validate_hash(hash: &str) -> AppResult<()> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(corrupt_error(
            "A catalog blob hash is invalid.",
            hash.to_owned(),
        ))
    }
}
