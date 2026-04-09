//! Platform-specific helpers for device access.
//!
//! On macOS, raw block devices (/dev/rdiskN) don't support `seek(End(0))`
//! to determine size. We use ioctl to query disk geometry instead.
//! F_NOCACHE and F_RDAHEAD are set to bypass the kernel buffer cache
//! (useless for FATX data) and disable kernel read-ahead.

#[allow(unused_imports)]
use log::{debug, info, warn};

/// Information about the underlying block device, queried via macOS ioctls.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Logical block size (typically 512)
    pub block_size: u32,
    /// Physical block size (often 4096 for modern drives)
    pub physical_block_size: u32,
    /// Total device size in bytes
    pub device_size: u64,
    /// Maximum single read transfer size the driver supports (0 = unknown)
    pub max_read_bytes: u64,
    /// Maximum single write transfer size the driver supports (0 = unknown)
    pub max_write_bytes: u64,
}

/// Get the size of a block device via macOS ioctls.
/// Returns None if the ioctls fail (e.g., not a block device, or not on macOS).
#[cfg(target_os = "macos")]
pub fn get_block_device_size(fd: std::os::unix::io::RawFd) -> Option<u64> {
    use nix::ioctl_read;

    // DKIOCGETBLOCKSIZE  = _IOR('d', 24, uint32_t)
    ioctl_read!(dkioc_get_block_size, b'd', 24, u32);
    // DKIOCGETBLOCKCOUNT = _IOR('d', 25, uint64_t)
    ioctl_read!(dkioc_get_block_count, b'd', 25, u64);

    let mut block_size: u32 = 0;
    let mut block_count: u64 = 0;

    unsafe {
        if let Err(e) = dkioc_get_block_size(fd, &mut block_size) {
            debug!("DKIOCGETBLOCKSIZE ioctl failed: {}", e);
            return None;
        }
        if let Err(e) = dkioc_get_block_count(fd, &mut block_count) {
            debug!("DKIOCGETBLOCKCOUNT ioctl failed: {}", e);
            return None;
        }
    }

    debug!(
        "ioctl: block_size={}, block_count={}, total={} bytes",
        block_size,
        block_count,
        block_size as u64 * block_count
    );
    Some(block_size as u64 * block_count)
}

/// Stub for non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn get_block_device_size(_fd: i32) -> Option<u64> {
    None
}

/// Query device I/O parameters and configure the file descriptor for optimal
/// FATX I/O: set F_NOCACHE (bypass kernel buffer cache) and F_RDAHEAD(0)
/// (disable kernel read-ahead — our app-level caches are more effective).
///
/// Returns `Some(DeviceInfo)` on success, `None` if not a block device or
/// ioctls fail (e.g., Cursor-backed test volumes).
#[cfg(target_os = "macos")]
pub fn configure_device_io(fd: std::os::unix::io::RawFd) -> Option<DeviceInfo> {
    use nix::ioctl_read;

    // Define ioctl readers
    ioctl_read!(dkioc_get_block_size, b'd', 24, u32);
    ioctl_read!(dkioc_get_block_count, b'd', 25, u64);
    // DKIOCGETPHYSICALBLOCKSIZE = _IOR('d', 77, uint32_t)
    ioctl_read!(dkioc_get_physical_block_size, b'd', 77, u32);
    // DKIOCGETMAXBYTECOUNTREAD = _IOR('d', 70, uint64_t)
    ioctl_read!(dkioc_get_max_read, b'd', 70, u64);
    // DKIOCGETMAXBYTECOUNTWRITE = _IOR('d', 71, uint64_t)
    ioctl_read!(dkioc_get_max_write, b'd', 71, u64);

    // Query basic block info (required)
    let mut block_size: u32 = 0;
    let mut block_count: u64 = 0;

    unsafe {
        if dkioc_get_block_size(fd, &mut block_size).is_err() {
            debug!("Not a block device (DKIOCGETBLOCKSIZE failed)");
            return None;
        }
        if dkioc_get_block_count(fd, &mut block_count).is_err() {
            debug!("DKIOCGETBLOCKCOUNT failed");
            return None;
        }
    }

    let device_size = block_size as u64 * block_count;

    // Query optional extended info
    let mut physical_block_size: u32 = block_size; // fallback to logical
    let mut max_read_bytes: u64 = 0;
    let mut max_write_bytes: u64 = 0;

    unsafe {
        let _ = dkioc_get_physical_block_size(fd, &mut physical_block_size);
        let _ = dkioc_get_max_read(fd, &mut max_read_bytes);
        let _ = dkioc_get_max_write(fd, &mut max_write_bytes);
    }

    // Set F_NOCACHE — bypass kernel buffer cache.
    // The kernel cache is useless for FATX data since macOS cannot interpret
    // the filesystem. Our app-level caches (fat_cache, file_cache, dir_cache)
    // are more effective. Requires 4KB-aligned I/O.
    // Note: F_NOCACHE and F_RDAHEAD are Apple-specific and not wrapped by nix,
    // so we use raw libc::fcntl for these two calls.
    unsafe {
        if libc::fcntl(fd, libc::F_NOCACHE, 1i32) == -1 {
            warn!(
                "F_NOCACHE failed (non-fatal): {}",
                std::io::Error::last_os_error()
            );
        } else {
            debug!("F_NOCACHE enabled");
        }

        // Disable kernel read-ahead — our app manages its own read patterns.
        if libc::fcntl(fd, libc::F_RDAHEAD, 0i32) == -1 {
            warn!(
                "F_RDAHEAD(0) failed (non-fatal): {}",
                std::io::Error::last_os_error()
            );
        } else {
            debug!("F_RDAHEAD disabled");
        }
    }

    let info = DeviceInfo {
        block_size,
        physical_block_size,
        device_size,
        max_read_bytes,
        max_write_bytes,
    };

    info!(
        "Device I/O configured: block_size={}, physical={}, size={} ({:.1} GB), max_read={}, max_write={}",
        info.block_size,
        info.physical_block_size,
        info.device_size,
        info.device_size as f64 / 1_073_741_824.0,
        info.max_read_bytes,
        info.max_write_bytes,
    );

    Some(info)
}

/// Stub for non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn configure_device_io(_fd: i32) -> Option<DeviceInfo> {
    None
}
