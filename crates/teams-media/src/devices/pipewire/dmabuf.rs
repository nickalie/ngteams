//! A shared screen's DMA-BUF read on the CPU. A compositor such as niri hands its screen cast only as DMA-BUFs it
//! doesn't mark mappable (its chunks' sizes 1 or 0, as PipeWire's DMA-BUF rules allow), so PipeWire maps none of them:
//! the screen's stream maps each one itself, only ever one with the linear layout its offer asked for, and brackets the
//! read with the kernel's CPU-access sync so the picture the GPU drew is the one read.

use std::os::fd::{AsRawFd as _, BorrowedFd};
use std::ptr::NonNull;

/// `DMA_BUF_IOCTL_SYNC` (`linux/dma-buf.h`: `_IOW('b', 0, struct dma_buf_sync)`) and its flags.
const DMA_BUF_IOCTL_SYNC: libc::Ioctl = 0x4008_6200;
const SYNC_READ: u64 = 1;
const SYNC_START: u64 = 0;
const SYNC_END: u64 = 4;

/// The bytes `offset..offset + len` of the DMA-BUF `fd`, read between the starting and ending sync; `None` when it
/// is shorter or can't be mapped. A buffer the kernel doesn't sync (a memfd the producer called a DMA-BUF) is read all
/// the same.
pub(super) fn read(fd: BorrowedFd<'_>, offset: usize, len: usize) -> Option<Vec<u8>> {
    let end = offset.checked_add(len)?;
    // a mapping read past the buffer's end faults (SIGBUS) rather than failing
    if size(fd)? < end {
        return None;
    }
    let mapped = Mapping::new(fd, end)?;
    sync(fd, SYNC_START);
    let bytes = mapped.bytes().get(offset..)?.to_vec();
    sync(fd, SYNC_END);
    Some(bytes)
}

/// The buffer's size (a DMA-BUF's inode carries it, as a memfd's does).
fn size(fd: BorrowedFd<'_>) -> Option<usize> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the descriptor is borrowed open for the call, and `stat` is writable room for the one struct it fills.
    let done = unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) };
    if done != 0 {
        return None;
    }
    // SAFETY: `fstat` succeeded, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    usize::try_from(stat.st_size).ok()
}

/// Tells the kernel the CPU starts or ends reading `fd`; a descriptor that isn't a DMA-BUF refuses it, harmlessly.
fn sync(fd: BorrowedFd<'_>, step: u64) {
    let flags: u64 = SYNC_READ | step;
    // SAFETY: the descriptor is borrowed open for the call, and the argument is the `struct dma_buf_sync` (one u64)
    // the request takes, alive for the call.
    let _ = unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &raw const flags) };
}

/// `len` bytes of `fd` mapped for reading, unmapped when dropped.
struct Mapping {
    at: NonNull<libc::c_void>,
    len: usize,
}

impl Mapping {
    fn new(fd: BorrowedFd<'_>, len: usize) -> Option<Self> {
        if len == 0 {
            return None;
        }
        // SAFETY: a fresh shared read-only mapping the kernel places; nothing else is touched, and a failure returns
        // `MAP_FAILED`.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if at == libc::MAP_FAILED {
            return None;
        }
        NonNull::new(at).map(|at| Self { at, len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `len` readable bytes are mapped at `at` until `self` drops, and the slice borrows `self`.
        unsafe { std::slice::from_raw_parts(self.at.as_ptr().cast::<u8>(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made, unmapped once; no slice of it outlives `self`.
        let _ = unsafe { libc::munmap(self.at.as_ptr(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;

    use super::*;

    #[test]
    fn a_buffer_s_bytes_from_its_offset_are_read_and_an_empty_or_short_one_is_none() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(read(file.as_fd(), 2, 4), Some(vec![3, 4, 5, 6]));
        assert_eq!(read(file.as_fd(), 0, 0), None);
        assert_eq!(read(file.as_fd(), 4, 8), None, "past the buffer's end");
        assert_eq!(read(file.as_fd(), usize::MAX, 2), None);
    }
}
