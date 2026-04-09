use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    inode_id: usize,
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        inode_id: usize,
        block_id: usize,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            inode_id,
            block_id,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        if let Some(inode_id) = self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode)
        }) {
            let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
            Some(Arc::new(Self::new(
                inode_id as usize,
                block_id as usize,
                block_offset,
                self.fs.clone(),
                self.block_device.clone(),
            )))
        } else {
            None
        }
    }
    /// Adjust the size of a disk inode (support shrink for directory unlink)
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size == disk_inode.size {
            return;
        }
        if new_size > disk_inode.size {
            let blocks_needed = disk_inode.blocks_num_needed(new_size);
            let mut v: Vec<u32> = Vec::new();
            for _ in 0..blocks_needed {
                v.push(fs.alloc_data());
            }
            disk_inode.increase_size(new_size, v, &self.block_device);
        } else {
            // shrink: just update size (partial dealloc omitted for simplicity;
            // full dealloc happens in clear() when nlink reaches 0)
            disk_inode.size = new_size;
        }
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
                new_inode.nlink = 1; // nlink is 1 when created. TODO: but what if it failed below? 
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            new_inode_id as usize,
            block_id as usize,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// Link a new path to current inode
    /// only root inode should call this function
    pub fn link(&self, old_name: &str, new_name: &str) -> isize {
        // check if old_name and new_name are the same
        if old_name == new_name {
            return -1;
        }

        // check if old_name exists
        // we don't check if new_name exists. we define it as undefined.
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(old_name, root_inode)
        };
        let old_inode_id = self.read_disk_inode(op);
        // maybe we don't need to check again / should we check it here or at a higher level?
        if old_inode_id.is_none() {
            return -1;
        }

        // increase nlink of old inode by 1
        let (inode_block_id, inode_block_offset) = fs.get_disk_inode_pos(old_inode_id.unwrap());
        get_block_cache(inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(inode_block_offset, |old_inode: &mut DiskInode| {
                old_inode.nlink += 1;
            });

        // create new dirent with the same inode id as old_name
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(
                new_name, 
                old_inode_id.unwrap()
            );
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });
        0
    }
    /// Unlink a path to current inode
    /// only root inode should call this function
    pub fn unlink(&self, name: &str) -> isize {
        // check if file exists
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        let inode_id = self.read_disk_inode(op);
        if inode_id.is_none() {
            return -1;
        }
        let inode_id = inode_id.unwrap();

        // decrease nlink of target inode by 1
        let (inode_block_id, inode_block_offset) = fs.get_disk_inode_pos(inode_id);
        get_block_cache(inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(inode_block_offset, |target_inode: &mut DiskInode| {
                target_inode.nlink -= 1;
            });
        
        // delete dirent with the same name from parent directory
        self.modify_disk_inode(|root_inode| {
            // find the dirent with the same name
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                assert_eq!(
                    root_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if dirent.name() == name {
                    // delete this dirent by moving the last dirent to this place and decreasing size by 1 dirent
                    let last_dirent_offset = (file_count - 1) * DIRENT_SZ;
                    if last_dirent_offset != i * DIRENT_SZ {
                        assert_eq!(
                            root_inode.read_at(last_dirent_offset, dirent.as_bytes_mut(), &self.block_device,),
                            DIRENT_SZ,
                        );
                        root_inode.write_at(i * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
                    }
                    // decrease size by 1 dirent (now handled by adjusted increase_size)
                    let new_size = (file_count - 1) * DIRENT_SZ;
                    self.increase_size(new_size as u32, root_inode, &mut fs);
                    break;
                }
            }
        });

        // delete inode and its data if nlink reaches 0
        let (inode_block_id, inode_block_offset) = fs.get_disk_inode_pos(inode_id);
        get_block_cache(inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(inode_block_offset, |inode: &mut DiskInode| {
                if inode.nlink == 0 {
                    let size = inode.size;
                    // dealloc data blocks
                    let data_blocks_dealloc = inode.clear_size(&self.block_device);
                    assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
                    for data_block in data_blocks_dealloc.into_iter() {
                        fs.dealloc_data(data_block);
                    }
                    // dealloc inode 
                    fs.inode_bitmap.dealloc(&self.block_device, inode_id as usize);
                }
            });
        block_cache_sync_all();
        0
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
    /// Get the id of current inode
    pub fn get_inode_id(&self) -> usize {
        self.inode_id
    }
    /// Get the type of current inode
    pub fn get_inode_type(&self) -> DiskInodeType {
        self.read_disk_inode(|disk_inode| disk_inode.is_dir()).then(|| DiskInodeType::Directory).unwrap_or(DiskInodeType::File)
    }
    /// Get nlink of current inode (always reads from disk to ensure correctness for hard links)
    pub fn get_nlink(&self) -> usize {
        self.read_disk_inode(|disk_inode| disk_inode.nlink as usize)
    }
}
