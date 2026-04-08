//! fatx-mount: Mount Xbox FATX/XTAF file systems via a local NFS server.
//!
//! Starts a localhost NFSv3 server backed by a FATX volume, then mounts it
//! so it appears as a regular volume in Finder.
//!
//! Usage:
//!   sudo fatx-mount /dev/rdisk4 --partition "360 Data"
//!   # Drive appears in Finder. Unmount from Finder or:
//!   umount /Volumes/Xbox\ Drive

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use clap::Parser;
use log::{debug, error, info, warn};
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

use fatxlib::partition::{detect_xbox_partitions, format_size};
use fatxlib::types::{DirectoryEntry, FileAttributes, FIRST_CLUSTER};
use fatxlib::volume::FatxVolume;

const ROOT_FILEID: fileid3 = 1; // FATX root cluster is FIRST_CLUSTER (1)

/// Convert a FATX cluster number to an NFS file ID.
fn cluster_to_id(cluster: u32) -> fileid3 {
    cluster as fileid3
}

fn id_to_cluster(id: fileid3) -> u32 {
    id as u32
}

/// Convert FATX packed date+time to nfstime3.
fn fatx_to_nfstime(date: u16, time: u16) -> nfstime3 {
    let year = ((date >> 9) & 0x7F) as i32 + 1980;
    let month = ((date >> 5) & 0x0F) as u32;
    let day = (date & 0x1F) as u32;
    let hour = ((time >> 11) & 0x1F) as u32;
    let min = ((time >> 5) & 0x3F) as u32;
    let sec = ((time & 0x1F) * 2) as u32;

    let days_approx = (year - 1970) as u64 * 365
        + ((year - 1969) / 4) as u64
        + month_day_offset(month, year) as u64
        + (day.saturating_sub(1)) as u64;
    let secs = days_approx * 86400 + hour as u64 * 3600 + min as u64 * 60 + sec as u64;

    nfstime3 {
        seconds: secs as u32,
        nseconds: 0,
    }
}

fn month_day_offset(month: u32, year: i32) -> u32 {
    let days = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let base = if (1..=12).contains(&month) {
        days[(month - 1) as usize]
    } else {
        0
    };
    if month > 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
        base + 1
    } else {
        base
    }
}

fn now_nfstime() -> nfstime3 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    nfstime3 {
        seconds: d.as_secs() as u32,
        nseconds: d.subsec_nanos(),
    }
}

/// Build NFS file attributes from a FATX directory entry.
fn dirent_to_fattr(entry: &DirectoryEntry) -> fattr3 {
    let is_dir = entry.attributes.contains(FileAttributes::DIRECTORY);
    let ftype = if is_dir {
        ftype3::NF3DIR
    } else {
        ftype3::NF3REG
    };
    let mode: u32 = if is_dir { 0o755 } else { 0o644 };
    let size = entry.file_size as u64;

    let ctime = fatx_to_nfstime(entry.creation_date, entry.creation_time);
    let mtime = fatx_to_nfstime(entry.write_date, entry.write_time);
    let atime = fatx_to_nfstime(entry.access_date, entry.access_time);

    fattr3 {
        ftype,
        mode,
        nlink: if is_dir { 2 } else { 1 },
        uid: 501, // default macOS user
        gid: 20,  // staff group
        size,
        used: size,
        rdev: specdata3 {
            specdata1: 0,
            specdata2: 0,
        },
        fsid: 1,
        fileid: cluster_to_id(entry.first_cluster),
        atime,
        mtime,
        ctime,
    }
}

fn root_fattr() -> fattr3 {
    let now = now_nfstime();
    fattr3 {
        ftype: ftype3::NF3DIR,
        mode: 0o755,
        nlink: 2,
        uid: 501,
        gid: 20,
        size: 0,
        used: 0,
        rdev: specdata3 {
            specdata1: 0,
            specdata2: 0,
        },
        fsid: 1,
        fileid: ROOT_FILEID,
        atime: now,
        mtime: now,
        ctime: now,
    }
}

/// The NFS filesystem backed by a FatxVolume.
///
/// All blocking I/O (USB reads/writes via FatxVolume) is dispatched to
/// `tokio::task::spawn_blocking` so the async NFS event loop never stalls.
struct FatxNfs {
    vol: Arc<Mutex<FatxVolume<File>>>,
    /// Cache: parent_cluster -> Vec<DirectoryEntry>
    dir_cache: Arc<Mutex<HashMap<u32, Vec<DirectoryEntry>>>>,
    /// Reverse lookup: cluster -> (parent_cluster, name)
    inode_parents: Arc<Mutex<HashMap<u32, (u32, String)>>>,
    /// File data cache: cluster -> file bytes (avoids re-reading entire file per NFS chunk)
    file_cache: Arc<Mutex<HashMap<u32, Vec<u8>>>>,
    /// Whether the volume was opened read-only
    readonly: bool,
}

impl FatxNfs {
    fn new(vol: FatxVolume<File>, readonly: bool) -> Self {
        FatxNfs {
            vol: Arc::new(Mutex::new(vol)),
            dir_cache: Arc::new(Mutex::new(HashMap::new())),
            inode_parents: Arc::new(Mutex::new(HashMap::new())),
            file_cache: Arc::new(Mutex::new(HashMap::new())),
            readonly,
        }
    }

    /// Read directory entries for a cluster, populating caches.
    /// Returns cached data on hit; only goes to USB on cache miss.
    async fn get_dir_entries(&self, cluster: u32) -> Result<Vec<DirectoryEntry>, nfsstat3> {
        // Fast path: check cache without blocking I/O
        {
            let cache = self.dir_cache.lock().unwrap();
            if let Some(entries) = cache.get(&cluster) {
                return Ok(entries.clone());
            }
        }

        // Cache miss — read from USB via spawn_blocking
        let vol = Arc::clone(&self.vol);
        let dir_cache = Arc::clone(&self.dir_cache);
        let inode_parents = Arc::clone(&self.inode_parents);

        tokio::task::spawn_blocking(move || {
            // Double-check cache inside the blocking task (another task may have populated it)
            {
                let cache = dir_cache.lock().unwrap();
                if let Some(entries) = cache.get(&cluster) {
                    return Ok(entries.clone());
                }
            }

            let t0 = Instant::now();
            let mut vol = vol.lock().unwrap();
            let entries = if cluster == FIRST_CLUSTER {
                vol.read_root_directory()
            } else {
                vol.read_directory(cluster)
            };
            let elapsed = t0.elapsed();

            match entries {
                Ok(entries) => {
                    info!(
                        "dir read cluster {} -> {} entries ({:.1}ms USB)",
                        cluster,
                        entries.len(),
                        elapsed.as_secs_f64() * 1000.0
                    );
                    {
                        let mut parents = inode_parents.lock().unwrap();
                        for e in &entries {
                            parents.insert(e.first_cluster, (cluster, e.filename()));
                        }
                    }
                    {
                        let mut cache = dir_cache.lock().unwrap();
                        cache.insert(cluster, entries.clone());
                    }
                    Ok(entries)
                }
                Err(e) => {
                    warn!(
                        "readdir cluster {} ({:.1}ms): {}",
                        cluster,
                        elapsed.as_secs_f64() * 1000.0,
                        e
                    );
                    Err(nfsstat3::NFS3ERR_IO)
                }
            }
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))
    }

    /// Resolve a full FATX path from parent fileid + name.
    fn resolve_fatx_path(&self, parent_id: fileid3, name: &str) -> String {
        let parent_cluster = id_to_cluster(parent_id);
        let mut parts = vec![name.to_string()];
        let mut current = parent_cluster;
        let parents = self.inode_parents.lock().unwrap();
        while current != FIRST_CLUSTER {
            if let Some((grandparent, dir_name)) = parents.get(&current) {
                parts.push(dir_name.clone());
                current = *grandparent;
            } else {
                break;
            }
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// Check if a filename is macOS metadata that should be silently rejected.
    /// Finder creates .DS_Store and ._ (AppleDouble) files on any writeable volume.
    /// These are meaningless on an Xbox drive and waste clusters.
    fn is_macos_metadata(name: &str) -> bool {
        name == ".DS_Store"
            || name == ".Spotlight-V100"
            || name == ".Trashes"
            || name == ".fseventsd"
            || name.starts_with("._")
    }

    /// Invalidate the directory cache for a parent cluster, plus any cached
    /// file data for children of that directory (they may have new clusters
    /// after a delete+recreate write cycle).
    fn invalidate_dir(&self, parent_cluster: u32) {
        // Remove child file caches
        {
            let dir_cache = self.dir_cache.lock().unwrap();
            if let Some(entries) = dir_cache.get(&parent_cluster) {
                let mut fc = self.file_cache.lock().unwrap();
                for e in entries {
                    fc.remove(&e.first_cluster);
                }
            }
        }
        let mut cache = self.dir_cache.lock().unwrap();
        cache.remove(&parent_cluster);
    }

    /// Invalidate a single file's data cache (e.g. after write).
    fn invalidate_file(&self, cluster: u32) {
        let mut cache = self.file_cache.lock().unwrap();
        cache.remove(&cluster);
    }

    /// Check if readonly and return appropriate error.
    fn check_writable(&self) -> Result<(), nfsstat3> {
        if self.readonly {
            Err(nfsstat3::NFS3ERR_ROFS)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl NFSFileSystem for FatxNfs {
    fn capabilities(&self) -> VFSCapabilities {
        if self.readonly {
            VFSCapabilities::ReadOnly
        } else {
            VFSCapabilities::ReadWrite
        }
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_FILEID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let t0 = Instant::now();
        let cluster = id_to_cluster(dirid);
        let name_str =
            std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_NOENT)?;

        debug!("NFS lookup: dir={} name=\"{}\"", dirid, name_str);

        // Handle . and ..
        if name_str == "." {
            return Ok(dirid);
        }
        if name_str == ".." {
            let parents = self.inode_parents.lock().unwrap();
            if let Some((parent, _)) = parents.get(&cluster) {
                return Ok(cluster_to_id(*parent));
            }
            return Ok(ROOT_FILEID);
        }

        let entries = self.get_dir_entries(cluster).await?;
        for entry in &entries {
            if entry.filename().eq_ignore_ascii_case(name_str) {
                let id = cluster_to_id(entry.first_cluster);
                debug!(
                    "NFS lookup: \"{}\" -> id={} ({:.1}ms)",
                    name_str,
                    id,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(id);
            }
        }
        debug!(
            "NFS lookup: \"{}\" -> NOENT ({:.1}ms)",
            name_str,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        Err(nfsstat3::NFS3ERR_NOENT)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let t0 = Instant::now();
        if id == ROOT_FILEID {
            debug!(
                "NFS getattr: id={} (root) ({:.1}ms)",
                id,
                t0.elapsed().as_secs_f64() * 1000.0
            );
            return Ok(root_fattr());
        }

        let cluster = id_to_cluster(id);
        let parent_cluster = {
            let parents = self.inode_parents.lock().unwrap();
            parents.get(&cluster).map(|(p, _)| *p)
        };

        if let Some(pc) = parent_cluster {
            let entries = self.get_dir_entries(pc).await?;
            for entry in &entries {
                if entry.first_cluster == cluster {
                    debug!(
                        "NFS getattr: id={} \"{}\" size={} ({:.1}ms)",
                        id,
                        entry.filename(),
                        entry.file_size,
                        t0.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok(dirent_to_fattr(entry));
                }
            }
        }
        debug!(
            "NFS getattr: id={} -> NOENT ({:.1}ms)",
            id,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        Err(nfsstat3::NFS3ERR_NOENT)
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        debug!("NFS setattr: id={}", _id);
        // FATX has limited attribute support; return current attrs
        self.getattr(_id).await
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let t0 = Instant::now();
        let cluster = id_to_cluster(id);

        // Fast path: serve from file cache without any USB I/O
        {
            let cache = self.file_cache.lock().unwrap();
            if let Some(data) = cache.get(&cluster) {
                let start = offset as usize;
                let end = (start + count as usize).min(data.len());
                if start >= data.len() {
                    debug!(
                        "NFS read: id={} offset={} count={} -> EOF (cached, {:.1}ms)",
                        id,
                        offset,
                        count,
                        t0.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok((vec![], true));
                }
                let eof = end >= data.len();
                debug!(
                    "NFS read: id={} offset={} count={} -> {} bytes, eof={} (cached, {:.1}ms)",
                    id,
                    offset,
                    count,
                    end - start,
                    eof,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((data[start..end].to_vec(), eof));
            }
        }

        // Cache miss — find the directory entry, then read the whole file once
        let parent_cluster = {
            let parents = self.inode_parents.lock().unwrap();
            parents.get(&cluster).map(|(p, _)| *p)
        };

        let entry = if let Some(pc) = parent_cluster {
            let entries = self.get_dir_entries(pc).await?;
            entries.into_iter().find(|e| e.first_cluster == cluster)
        } else {
            None
        };

        let entry = entry.ok_or(nfsstat3::NFS3ERR_NOENT)?;

        let vol = Arc::clone(&self.vol);
        let file_cache = Arc::clone(&self.file_cache);
        let data = tokio::task::spawn_blocking(move || {
            let t0 = Instant::now();
            let mut vol = vol.lock().unwrap();
            match vol.read_file(&entry) {
                Ok(data) => {
                    let elapsed = t0.elapsed();
                    info!(
                        "file read cluster {} ({} bytes) in {:.1}ms from USB",
                        cluster,
                        data.len(),
                        elapsed.as_secs_f64() * 1000.0
                    );
                    // Cache the file data for subsequent chunk reads
                    let mut cache = file_cache.lock().unwrap();
                    cache.insert(cluster, data.clone());
                    Ok(data)
                }
                Err(e) => {
                    warn!("read cluster {}: {}", cluster, e);
                    Err(nfsstat3::NFS3ERR_IO)
                }
            }
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))?;

        let start = offset as usize;
        let end = (start + count as usize).min(data.len());
        if start >= data.len() {
            Ok((vec![], true))
        } else {
            let eof = end >= data.len();
            Ok((data[start..end].to_vec(), eof))
        }
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        let t0 = Instant::now();
        self.check_writable()?;

        let cluster = id_to_cluster(id);
        info!("NFS write: id={} offset={} len={}", id, offset, data.len());

        // Find the entry and its parent — drop MutexGuard before any .await
        let parent_cluster = {
            let parents = self.inode_parents.lock().unwrap();
            parents.get(&cluster).map(|(p, _)| *p)
        };

        let entry = if let Some(pc) = parent_cluster {
            let entries = self.get_dir_entries(pc).await?;
            entries.into_iter().find(|e| e.first_cluster == cluster)
        } else {
            None
        };

        let parent_cluster = parent_cluster.ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let entry = entry.ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let path = self.resolve_fatx_path(cluster_to_id(parent_cluster), &entry.filename());

        // For FATX, we need to read the whole file, modify it, delete and rewrite.
        // Try to use cached file data to avoid an extra USB read.
        let cached_data = {
            let cache = self.file_cache.lock().unwrap();
            cache.get(&cluster).cloned()
        };

        // Run all blocking vol I/O on a dedicated thread.
        let vol = Arc::clone(&self.vol);
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            let t0 = Instant::now();
            let mut vol = vol.lock().unwrap();
            let mut file_data = if let Some(cached) = cached_data {
                info!(
                    "write: using cached data for cluster {} ({} bytes)",
                    cluster,
                    cached.len()
                );
                cached
            } else {
                vol.read_file(&entry).map_err(|e| {
                    warn!("read for write cluster {}: {}", cluster, e);
                    nfsstat3::NFS3ERR_IO
                })?
            };

            let write_end = offset as usize + data.len();
            if write_end > file_data.len() {
                file_data.resize(write_end, 0);
            }
            file_data[offset as usize..write_end].copy_from_slice(&data);

            let _ = vol.delete(&path);
            vol.create_file(&path, &file_data).map_err(|e| {
                warn!("write cluster {}: {}", cluster, e);
                nfsstat3::NFS3ERR_IO
            })?;
            let _ = vol.flush();
            let elapsed = t0.elapsed();
            info!(
                "write cluster {} ({} bytes at offset {}) in {:.1}ms",
                cluster,
                data.len(),
                offset,
                elapsed.as_secs_f64() * 1000.0
            );
            Ok::<(), nfsstat3>(())
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))?;

        self.invalidate_file(cluster);
        self.invalidate_dir(parent_cluster);
        info!(
            "NFS write: id={} complete ({:.1}ms)",
            id,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        self.getattr(id).await
    }

    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let t0 = Instant::now();
        self.check_writable()?;

        let name_str =
            std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;

        // Block macOS metadata files from being created on Xbox drive
        if Self::is_macos_metadata(name_str) {
            debug!("NFS create: blocked macOS metadata \"{}\"", name_str);
            return Err(nfsstat3::NFS3ERR_PERM);
        }
        info!("NFS create: dir={} name=\"{}\"", dirid, name_str);

        let path = self.resolve_fatx_path(dirid, name_str);
        let parent_cluster = id_to_cluster(dirid);

        let vol = Arc::clone(&self.vol);
        let path_clone = path.clone();
        tokio::task::spawn_blocking(move || {
            let mut vol = vol.lock().unwrap();
            vol.create_file(&path_clone, &[]).map_err(|e| {
                warn!("create '{}': {}", path_clone, e);
                nfsstat3::NFS3ERR_IO
            })?;
            let _ = vol.flush();
            Ok::<(), nfsstat3>(())
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))?;

        self.invalidate_dir(parent_cluster);

        // Look up the new entry
        let entries = self.get_dir_entries(parent_cluster).await?;
        for entry in &entries {
            if entry.filename().eq_ignore_ascii_case(name_str) {
                info!(
                    "NFS create: \"{}\" -> id={} ({:.1}ms)",
                    name_str,
                    cluster_to_id(entry.first_cluster),
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((cluster_to_id(entry.first_cluster), dirent_to_fattr(entry)));
            }
        }
        error!(
            "NFS create: \"{}\" created but not found in dir listing!",
            name_str
        );
        Err(nfsstat3::NFS3ERR_IO)
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        let (id, _) = self.create(dirid, filename, sattr3::default()).await?;
        Ok(id)
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let t0 = Instant::now();
        self.check_writable()?;

        let name_str =
            std::str::from_utf8(dirname.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;

        if Self::is_macos_metadata(name_str) {
            debug!("NFS mkdir: blocked macOS metadata \"{}\"", name_str);
            return Err(nfsstat3::NFS3ERR_PERM);
        }
        info!("NFS mkdir: dir={} name=\"{}\"", dirid, name_str);
        let path = self.resolve_fatx_path(dirid, name_str);
        let parent_cluster = id_to_cluster(dirid);

        let vol = Arc::clone(&self.vol);
        let path_clone = path.clone();
        tokio::task::spawn_blocking(move || {
            let mut vol = vol.lock().unwrap();
            vol.create_directory(&path_clone).map_err(|e| {
                warn!("mkdir '{}': {}", path_clone, e);
                nfsstat3::NFS3ERR_IO
            })?;
            let _ = vol.flush();
            Ok::<(), nfsstat3>(())
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))?;

        self.invalidate_dir(parent_cluster);

        let entries = self.get_dir_entries(parent_cluster).await?;
        for entry in &entries {
            if entry.filename().eq_ignore_ascii_case(name_str) {
                info!(
                    "NFS mkdir: \"{}\" -> id={} ({:.1}ms)",
                    name_str,
                    cluster_to_id(entry.first_cluster),
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((cluster_to_id(entry.first_cluster), dirent_to_fattr(entry)));
            }
        }
        error!(
            "NFS mkdir: \"{}\" created but not found in dir listing!",
            name_str
        );
        Err(nfsstat3::NFS3ERR_IO)
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        let t0 = Instant::now();
        self.check_writable()?;

        let name_str =
            std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        info!("NFS remove: dir={} name=\"{}\"", dirid, name_str);
        let path = self.resolve_fatx_path(dirid, name_str);
        let parent_cluster = id_to_cluster(dirid);

        let vol = Arc::clone(&self.vol);
        let path_clone = path.clone();
        tokio::task::spawn_blocking(move || {
            let mut vol = vol.lock().unwrap();
            vol.delete(&path_clone).map_err(|e| {
                warn!("remove '{}': {}", path_clone, e);
                nfsstat3::NFS3ERR_IO
            })?;
            let _ = vol.flush();
            Ok::<(), nfsstat3>(())
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))?;

        self.invalidate_dir(parent_cluster);
        info!(
            "NFS remove: \"{}\" done ({:.1}ms)",
            name_str,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        let t0 = Instant::now();
        self.check_writable()?;

        // FATX only supports same-directory rename
        if from_dirid != to_dirid {
            warn!("NFS rename: cross-directory rename not supported");
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }

        let from_name =
            std::str::from_utf8(from_filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        let to_name =
            std::str::from_utf8(to_filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        info!("NFS rename: \"{}\" -> \"{}\"", from_name, to_name);
        let path = self.resolve_fatx_path(from_dirid, from_name);
        let parent_cluster = id_to_cluster(from_dirid);

        let vol = Arc::clone(&self.vol);
        let path_clone = path.clone();
        let to_name_owned = to_name.to_string();
        tokio::task::spawn_blocking(move || {
            let mut vol = vol.lock().unwrap();
            vol.rename(&path_clone, &to_name_owned).map_err(|e| {
                warn!("rename '{}' -> '{}': {}", path_clone, to_name_owned, e);
                nfsstat3::NFS3ERR_IO
            })?;
            let _ = vol.flush();
            Ok::<(), nfsstat3>(())
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))?;

        self.invalidate_dir(parent_cluster);
        info!(
            "NFS rename: \"{}\" -> \"{}\" done ({:.1}ms)",
            from_name,
            to_name,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let t0 = Instant::now();
        let cluster = id_to_cluster(dirid);
        debug!(
            "NFS readdir: dir={} start_after={} max={}",
            dirid, start_after, max_entries
        );
        let entries = self.get_dir_entries(cluster).await?;

        let mut result = Vec::new();

        // Build full listing: . and .. first, then entries
        let mut full_list: Vec<(fileid3, fattr3, String)> = Vec::new();

        // Add . entry
        let self_attr = self.getattr(dirid).await?;
        full_list.push((dirid, self_attr, ".".to_string()));

        // Add .. entry
        let parent_id = {
            let parents = self.inode_parents.lock().unwrap();
            parents
                .get(&cluster)
                .map(|(p, _)| cluster_to_id(*p))
                .unwrap_or(ROOT_FILEID)
        };
        let parent_attr = self.getattr(parent_id).await?;
        full_list.push((parent_id, parent_attr, "..".to_string()));

        // Add real entries
        for entry in &entries {
            full_list.push((
                cluster_to_id(entry.first_cluster),
                dirent_to_fattr(entry),
                entry.filename(),
            ));
        }

        // Pagination: skip entries until we pass start_after
        let mut found_start = start_after == 0;
        for (id, attr, name) in &full_list {
            if !found_start {
                if *id == start_after {
                    found_start = true;
                }
                continue;
            }
            if result.len() >= max_entries {
                return Ok(ReadDirResult {
                    entries: result,
                    end: false,
                });
            }
            result.push(DirEntry {
                fileid: *id,
                name: name.as_bytes().into(),
                attr: *attr,
            });
        }

        debug!(
            "NFS readdir: dir={} returning {} entries, end=true ({:.1}ms)",
            dirid,
            result.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        Ok(ReadDirResult {
            entries: result,
            end: true,
        })
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        // FATX does not support symlinks
        Err(nfsstat3::NFS3ERR_NOTSUPP)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_NOTSUPP)
    }
}

// ===========================================================================
// CLI
// ===========================================================================

fn parse_hex_or_dec(s: &str) -> Result<u64, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u64>().map_err(|e| e.to_string())
    }
}

/// Get the size of a device, handling macOS raw block devices correctly.
fn get_device_size(file: &mut File) -> u64 {
    if let Ok(size) = file.seek(SeekFrom::End(0)) {
        if size > 0 {
            let _ = file.seek(SeekFrom::Start(0));
            return size;
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        if let Some(size) = fatxlib::platform::get_block_device_size(file.as_raw_fd()) {
            let _ = file.seek(SeekFrom::Start(0));
            return size;
        }
    }

    let _ = file.seek(SeekFrom::Start(0));
    0
}

#[derive(Parser)]
#[command(
    name = "fatx-mount",
    about = "Mount Xbox FATX/XTAF file systems (shows in Finder)",
    version
)]
struct Cli {
    /// Device or disk image to mount
    #[arg(required_unless_present = "cleanup")]
    device: Option<PathBuf>,

    /// Partition name (e.g. "360 Data", "Data (E)")
    #[arg(long)]
    partition: Option<String>,

    /// Manual partition offset (hex or decimal)
    #[arg(long, value_parser = parse_hex_or_dec, default_value = "0")]
    offset: u64,

    /// Manual partition size (hex or decimal, 0 = auto)
    #[arg(long, value_parser = parse_hex_or_dec, default_value = "0")]
    size: u64,

    /// NFS server port
    #[arg(long, default_value = "11111")]
    port: u16,

    /// Mount point (default: /Volumes/Xbox Drive)
    #[arg(long)]
    mountpoint: Option<PathBuf>,

    /// Enable verbose logging (info + NFS operations)
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Enable trace logging (debug-level: every NFS lookup/getattr/read)
    #[arg(long)]
    trace: bool,

    /// Mount read-only
    #[arg(long)]
    readonly: bool,

    /// Actually mount in Finder (off by default for safety).
    /// Without this flag, only the NFS server starts and you can
    /// test with: showmount -e localhost
    #[arg(long)]
    mount: bool,

    /// Emergency cleanup: kill stale NFS mounts and exit.
    /// Use this if a previous fatx-mount session left a zombie mount.
    #[arg(long)]
    cleanup: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let log_level = if cli.trace {
        "debug"
    } else if cli.verbose {
        "info"
    } else {
        "warn"
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp_millis()
        .init();

    info!("fatx-mount starting (log_level={})", log_level);

    // --cleanup: emergency recovery from stale NFS mounts
    if cli.cleanup {
        eprintln!("[cleanup] Emergency cleanup of stale fatx-mount NFS mounts...");

        eprintln!("[cleanup] Killing mount_nfs processes...");
        let _ = std::process::Command::new("killall")
            .args(["-9", "mount_nfs"])
            .output();
        eprintln!("[cleanup] Killing fatx-mount processes...");
        let _ = std::process::Command::new("killall")
            .args(["-9", "fatx-mount"])
            .output();

        // Force-unmount any localhost NFS mounts
        let mount_output = std::process::Command::new("mount")
            .output()
            .expect("failed to run mount");
        let mount_list = String::from_utf8_lossy(&mount_output.stdout);
        for line in mount_list.lines() {
            if line.contains("localhost") || line.contains("127.0.0.1") {
                // Extract mount point (third field in mount output)
                let parts: Vec<&str> = line.split(" on ").collect();
                if parts.len() >= 2 {
                    let mp = parts[1].split(' ').next().unwrap_or("");
                    if !mp.is_empty() {
                        eprintln!("  Force-unmounting: {}", mp);
                        let _ = std::process::Command::new("umount")
                            .args(["-f", mp])
                            .output();
                    }
                }
            }
        }

        // Clean up mount point directories
        let default_mps = ["/Volumes/Xbox Drive", "/Volumes/TestFATX"];
        for mp in &default_mps {
            if std::path::Path::new(mp).exists() {
                eprintln!("  Removing mount point: {}", mp);
                let _ = std::fs::remove_dir(mp);
            }
        }
        if let Some(ref mp) = cli.mountpoint {
            if mp.exists() {
                eprintln!("  Removing mount point: {}", mp.display());
                let _ = std::fs::remove_dir(mp);
            }
        }

        eprintln!("Cleanup complete.");
        std::process::exit(0);
    }

    let device_path = cli.device.as_ref().unwrap_or_else(|| {
        eprintln!("Device path is required (unless using --cleanup)");
        std::process::exit(1);
    });

    // Open the device
    let mut file = if cli.readonly {
        File::open(device_path)
    } else {
        OpenOptions::new().read(true).write(true).open(device_path)
    }
    .unwrap_or_else(|e| {
        eprintln!("Error opening '{}': {}", device_path.display(), e);
        eprintln!(
            "Try: sudo fatx-mount {} --partition \"360 Data\"",
            device_path.display()
        );
        std::process::exit(1);
    });

    let device_size = get_device_size(&mut file);

    // Resolve partition
    let (offset, size) = if let Some(ref name) = cli.partition {
        let partitions = detect_xbox_partitions(&mut file, device_size).unwrap_or_else(|e| {
            eprintln!("Error scanning partitions: {}", e);
            std::process::exit(1);
        });

        let found = partitions
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name));

        match found {
            Some(p) => {
                info!("Using partition '{}' at offset 0x{:X}", p.name, p.offset);
                (p.offset, p.size)
            }
            None => {
                eprintln!("Partition '{}' not found. Available:", name);
                for p in &partitions {
                    eprintln!(
                        "  {} (offset 0x{:X}, size {})",
                        p.name,
                        p.offset,
                        format_size(p.size)
                    );
                }
                std::process::exit(1);
            }
        }
    } else {
        (cli.offset, cli.size)
    };

    // Open the FATX volume
    let vol = FatxVolume::open(file, offset, size).unwrap_or_else(|e| {
        eprintln!("Error opening FATX volume: {}", e);
        std::process::exit(1);
    });

    let cluster_size = vol.superblock.cluster_size();
    let total_clusters = vol.total_clusters;
    info!(
        "FATX volume: {} clusters x {} = {}",
        total_clusters,
        format_size(cluster_size),
        format_size(total_clusters as u64 * cluster_size)
    );

    let mode = if cli.readonly {
        "read-only"
    } else {
        "read-write"
    };
    info!("Creating NFS filesystem adapter (mode={})", mode);
    let fs = FatxNfs::new(vol, cli.readonly);

    let bind_addr = format!("127.0.0.1:{}", cli.port);
    info!("Binding NFS server to {}...", bind_addr);
    let listener = NFSTcpListener::bind(&bind_addr, fs)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to bind NFS server on {}: {}", bind_addr, e);
            eprintln!("Is port {} already in use?", cli.port);
            std::process::exit(1);
        });

    let port = listener.get_listen_port();
    println!("NFS server listening on 127.0.0.1:{}", port);
    info!("NFS server ready on port {}", port);

    // Resolve mountpoint string once — used by mount logic and Ctrl+C handler
    let mp_str = if cli.mount {
        cli.mountpoint
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("/Volumes/Xbox Drive"))
            .display()
            .to_string()
    } else {
        String::new()
    };

    // Auto-mount unless --no-mount
    if cli.mount {
        let mountpoint = PathBuf::from(&mp_str);

        // Create mount point
        if !mountpoint.exists() {
            if let Err(e) = std::fs::create_dir_all(&mountpoint) {
                eprintln!("Failed to create mount point '{}': {}", mp_str, e);
                eprintln!("Try: sudo mkdir -p \"{}\"", mp_str);
                std::process::exit(1);
            }
        }

        // First, clean up any stale mount at this mountpoint
        let _ = std::process::Command::new("umount")
            .arg("-f")
            .arg(&mp_str)
            .output();

        // Mount in background with a timeout so it can't hang forever
        let mp_clone = mp_str.clone();
        tokio::spawn(async move {
            // Small delay to let the NFS server start accepting connections
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let opts = format!(
                "nolocks,noresvport,vers=3,tcp,rsize=131072,wsize=131072,actimeo=2,intr,soft,retrans=2,timeo=10,port={port},mountport={port}"
            );

            info!(
                "Running: mount_nfs -o {} localhost:/ \"{}\"",
                opts, mp_clone
            );

            // Use tokio timeout so a hanging mount_nfs doesn't block forever
            let mount_future = tokio::process::Command::new("mount_nfs")
                .args(["-o", &opts, "localhost:/", &mp_clone])
                .output();

            match tokio::time::timeout(std::time::Duration::from_secs(10), mount_future).await {
                Ok(Ok(o)) if o.status.success() => {
                    println!("Mounted at {}", mp_clone);
                    println!("The drive should appear in Finder.");
                    println!("Unmount with: umount \"{}\"", mp_clone);
                }
                Ok(Ok(o)) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    error!("mount_nfs failed: {}", stderr);
                    eprintln!("Mount failed: {}", stderr);
                    eprintln!(
                        "Try manually: mount_nfs -o nolocks,noresvport,vers=3,tcp,port={port},mountport={port} localhost:/ \"{}\"",
                        mp_clone
                    );
                }
                Ok(Err(e)) => {
                    error!("Failed to run mount_nfs: {}", e);
                }
                Err(_) => {
                    eprintln!("Mount timed out after 10s. Killing mount_nfs...");
                    let _ = std::process::Command::new("killall")
                        .args(["-9", "mount_nfs"])
                        .output();
                    eprintln!(
                        "Try manually: mount_nfs -o nolocks,noresvport,vers=3,tcp,port={port},mountport={port} localhost:/ \"{}\"",
                        mp_clone
                    );
                }
            }
        });
    } else {
        println!("NFS server running (no auto-mount). To mount in Finder:");
        println!(
            "  sudo mount_nfs -o nolocks,noresvport,vers=3,tcp,soft,intr,retrans=2,timeo=10,port={port},mountport={port} localhost:/ /Volumes/Xbox\\ Drive"
        );
        println!("To unmount:");
        println!("  sudo umount -f /Volumes/Xbox\\ Drive");
        println!("Pass --mount to auto-mount in Finder.");
    }

    println!("Press Ctrl+C to stop.");

    // CRITICAL: The shutdown sequence must be:
    //   1. Unmount WHILE the NFS server is still running (so umount can talk to it)
    //   2. Then kill the NFS server
    //   3. Then exit
    //
    // If we kill the server first, umount hangs trying to talk to a dead server,
    // which freezes Finder and can require a reboot.
    //
    // We use a dedicated thread with raw signal handling because tokio's
    // ctrl_c() can't fire when the event loop is blocked by NFS I/O.
    {
        let mp_for_signal = if cli.mount {
            Some(mp_str.clone())
        } else {
            None
        };

        std::thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            ctrlc_channel(tx);
            let _ = rx.recv(); // blocks until SIGINT

            eprintln!("\n[shutdown] Signal received, beginning clean shutdown...");

            // Step 1: Unmount FIRST while the NFS server is still alive.
            // This lets umount cleanly disconnect from the server.
            if let Some(ref mp) = mp_for_signal {
                eprintln!(
                    "[shutdown] Step 1/3: Unmounting {} (server still running)...",
                    mp
                );
                let umount = std::process::Command::new("umount").arg(mp).output();
                match umount {
                    Ok(o) if o.status.success() => {
                        eprintln!("[shutdown] Clean unmount succeeded.");
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        eprintln!(
                            "[shutdown] Clean unmount failed ({}), trying force...",
                            stderr.trim()
                        );
                        let force = std::process::Command::new("umount")
                            .args(["-f", mp])
                            .output();
                        match force {
                            Ok(o) if o.status.success() => {
                                eprintln!("[shutdown] Force unmount succeeded.")
                            }
                            Ok(o) => eprintln!(
                                "[shutdown] Force unmount failed: {}",
                                String::from_utf8_lossy(&o.stderr).trim()
                            ),
                            Err(e) => eprintln!("[shutdown] Force unmount error: {}", e),
                        }
                    }
                    Err(e) => {
                        eprintln!("[shutdown] umount error: {}", e);
                    }
                }

                // Give macOS a moment to finish the unmount
                eprintln!("[shutdown] Step 2/3: Waiting for macOS to release mount...");
                std::thread::sleep(std::time::Duration::from_millis(300));

                // Clean up the mount point directory
                eprintln!("[shutdown] Step 3/3: Cleaning up mount point directory...");
                match std::fs::remove_dir(mp) {
                    Ok(_) => eprintln!("[shutdown] Removed mount point {}.", mp),
                    Err(e) => {
                        eprintln!("[shutdown] Could not remove mount point: {} (non-fatal)", e)
                    }
                }
            } else {
                eprintln!("[shutdown] No mount to clean up (server-only mode).");
            }

            // Step 2: Now it's safe to exit — no stale mount left behind
            eprintln!("[shutdown] Shutdown complete. Exiting.");
            std::process::exit(0);
        });
    }

    // Run the NFS server forever (Ctrl+C handled by the thread above)
    if let Err(e) = listener.handle_forever().await {
        error!("NFS server error: {}", e);
    }
}

/// Set up a channel-based Ctrl+C (SIGINT) listener using raw signals.
fn ctrlc_channel(tx: std::sync::mpsc::Sender<()>) {
    unsafe {
        libc::signal(
            libc::SIGINT,
            sigint_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            sigint_handler as *const () as libc::sighandler_t,
        );
    }
    *SIGINT_TX.lock().unwrap() = Some(tx);
}

static SIGINT_TX: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
    std::sync::Mutex::new(None);

extern "C" fn sigint_handler(_sig: libc::c_int) {
    if let Ok(guard) = SIGINT_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_macos_metadata tests ──

    #[test]
    fn test_blocks_ds_store() {
        assert!(FatxNfs::is_macos_metadata(".DS_Store"));
    }

    #[test]
    fn test_blocks_spotlight() {
        assert!(FatxNfs::is_macos_metadata(".Spotlight-V100"));
    }

    #[test]
    fn test_blocks_trashes() {
        assert!(FatxNfs::is_macos_metadata(".Trashes"));
    }

    #[test]
    fn test_blocks_fseventsd() {
        assert!(FatxNfs::is_macos_metadata(".fseventsd"));
    }

    #[test]
    fn test_blocks_resource_fork_prefix() {
        assert!(FatxNfs::is_macos_metadata("._anything"));
        assert!(FatxNfs::is_macos_metadata("._Icon\r"));
        assert!(FatxNfs::is_macos_metadata("._"));
    }

    #[test]
    fn test_allows_normal_files() {
        assert!(!FatxNfs::is_macos_metadata("game.bin"));
        assert!(!FatxNfs::is_macos_metadata("Content"));
        assert!(!FatxNfs::is_macos_metadata(".hidden"));
        assert!(!FatxNfs::is_macos_metadata("DS_Store")); // no leading dot
    }

    // ── inode/cluster mapping tests ──

    #[test]
    fn test_root_inode_is_one() {
        // The NFS root inode should be 1 (standard NFS convention)
        assert_eq!(FIRST_CLUSTER, 1);
    }

    // ── check_writable tests ──
    // These require constructing a FatxNfs instance, which needs a real volume.
    // We test the logic indirectly through the NFS integration tests.

    // ── cache invalidation logic ──

    #[test]
    fn test_cache_structures() {
        // Verify that HashMap<u32, Vec<DirectoryEntry>> and HashMap<u32, Vec<u8>>
        // are the correct types by constructing them
        let dir_cache: HashMap<u32, Vec<DirectoryEntry>> = HashMap::new();
        let file_cache: HashMap<u32, Vec<u8>> = HashMap::new();
        assert!(dir_cache.is_empty());
        assert!(file_cache.is_empty());
    }
}
