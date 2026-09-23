//! Process-lifetime checksummed spill segments.

use crate::Error;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const SEGMENT_MAGIC: &[u8; 8] = b"TRCFSEG\0";
const SEGMENT_VERSION: u32 = 1;

#[derive(Clone)]
pub(super) struct SpillPointer {
    file: PathBuf,
    offset: u64,
    length: u64,
    checksum: [u8; 32],
}

pub(super) struct SpillStore {
    pub(super) directory: PathBuf,
    next_segment: AtomicU64,
    writer: Mutex<ActiveSegment>,
}

struct ActiveSegment {
    path: Option<PathBuf>,
    bytes: u64,
}

impl SpillStore {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            next_segment: AtomicU64::new(0),
            writer: Mutex::new(ActiveSegment {
                path: None,
                bytes: 0,
            }),
        }
    }

    pub(super) fn write_batch(&self, payloads: &[&[u8]]) -> Result<Vec<SpillPointer>, Error> {
        const MAX_SEGMENT_BYTES: u64 = 1024 * 1024;
        if fs::metadata(&self.directory)
            .map_err(|_| Error::SpillUnavailable)?
            .permissions()
            .readonly()
        {
            return Err(Error::SpillUnavailable);
        }
        let mut active = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes_to_write = payloads
            .iter()
            .map(|payload| 8 + 32 + payload.len() as u64)
            .sum::<u64>();
        if active.path.is_none()
            || (active.bytes > 12
                && active.bytes.saturating_add(bytes_to_write) > MAX_SEGMENT_BYTES)
        {
            let sequence = self.next_segment.fetch_add(1, Ordering::Relaxed);
            let path = self.directory.join(format!("segment-{sequence:020}.bin"));
            let mut file = File::create(&path).map_err(|_| Error::SpillUnavailable)?;
            file.write_all(SEGMENT_MAGIC)
                .map_err(|_| Error::SpillUnavailable)?;
            file.write_all(&SEGMENT_VERSION.to_le_bytes())
                .map_err(|_| Error::SpillUnavailable)?;
            active.path = Some(path);
            active.bytes = 12;
        }
        let path = active.path.clone().ok_or(Error::SpillUnavailable)?;
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|_| Error::SpillUnavailable)?;
        // Assemble each frame before writing. On an I/O failure truncate back
        // to this invocation's start so no partial frame is ever indexed.
        let frame_start = active.bytes;
        let mut pointers = Vec::with_capacity(payloads.len());
        let mut frames = Vec::with_capacity(payloads.len());
        let mut next_offset = frame_start;
        for payload in payloads {
            let checksum: [u8; 32] = Sha256::digest(payload).into();
            let mut frame = Vec::with_capacity(40 + payload.len());
            frame.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            frame.extend_from_slice(&checksum);
            frame.extend_from_slice(payload);
            pointers.push(SpillPointer {
                file: path.clone(),
                offset: next_offset + 40,
                length: payload.len() as u64,
                checksum,
            });
            next_offset += frame.len() as u64;
            frames.push(frame);
        }
        for frame in frames {
            if file.write_all(&frame).is_err() {
                if file.set_len(frame_start).is_err() {
                    // The physical tail is no longer trustworthy; force the
                    // next admission onto a fresh segment rather than using a
                    // stale logical offset into it.
                    active.path = None;
                    active.bytes = 0;
                }
                return Err(Error::SpillUnavailable);
            }
        }
        active.bytes = next_offset;
        Ok(pointers)
    }

    pub(super) fn read(&self, pointer: &SpillPointer) -> Result<Vec<u8>, Error> {
        let mut file = File::open(&pointer.file).map_err(|_| Error::CorruptSpill)?;
        let mut magic = [0; 8];
        let mut version = [0; 4];
        file.read_exact(&mut magic)
            .map_err(|_| Error::CorruptSpill)?;
        file.read_exact(&mut version)
            .map_err(|_| Error::CorruptSpill)?;
        if magic != *SEGMENT_MAGIC || u32::from_le_bytes(version) != SEGMENT_VERSION {
            return Err(Error::CorruptSpill);
        }
        let record_start = pointer.offset.checked_sub(40).ok_or(Error::CorruptSpill)?;
        file.seek(SeekFrom::Start(record_start))
            .map_err(|_| Error::CorruptSpill)?;
        let mut length = [0; 8];
        let mut checksum = [0; 32];
        file.read_exact(&mut length)
            .map_err(|_| Error::CorruptSpill)?;
        file.read_exact(&mut checksum)
            .map_err(|_| Error::CorruptSpill)?;
        if u64::from_le_bytes(length) != pointer.length || checksum != pointer.checksum {
            return Err(Error::CorruptSpill);
        }
        let mut bytes = vec![0; pointer.length.try_into().map_err(|_| Error::CorruptSpill)?];
        file.read_exact(&mut bytes)
            .map_err(|_| Error::CorruptSpill)?;
        if Sha256::digest(&bytes).as_slice() != pointer.checksum {
            return Err(Error::CorruptSpill);
        }
        Ok(bytes)
    }
}
