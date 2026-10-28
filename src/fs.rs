use core::fmt::Write;
#[cfg(test)]
#[path = "disk.rs"]
mod disk;
#[cfg(test)]
#[path = "auth.rs"]
mod auth;
use crate::disk::{BlockDevice, SECTOR_SIZE};
use crate::auth::{AccountStore, Credentials, ROOT_UID, USER_UID};
#[cfg(test)]
use crate::disk::DiskError;

pub const MAX_NODES: usize = 32;
pub const MAX_NAME: usize = 24;
pub const MAX_FILE_SIZE: usize = 256;
const NODE_SECTORS: u32 = 3;
const DATA_START: u32 = 4;
pub const FS_START_LBA: u32 = 2048;
const MAX_PATH_DEPTH: usize = 32;
const MAGIC: &[u8; 8] = b"YALIFS02";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError { InvalidPath, Missing, NotDirectory, IsDirectory, AlreadyExists, DirectoryNotEmpty, NoSpace, NameTooLong, FileTooLarge, NotFormatted, Disk, PermissionDenied, Authentication }

#[derive(Clone, Copy)]
enum NodeKind { Free, Directory, File }

#[derive(Clone, Copy)]
struct Node { kind: NodeKind, parent: usize, name: [u8; MAX_NAME], name_len: usize, file_len: usize, owner: u32, mode: u16 }

impl Node { const fn empty() -> Self { Self { kind: NodeKind::Free, parent: 0, name: [0; MAX_NAME], name_len: 0, file_len: 0, owner: 0, mode: 0 } } }

pub struct FileSystem<D: BlockDevice> { disk: D, nodes: [Node; MAX_NODES], cwd: usize, mounted: bool, accounts: AccountStore }

impl<D: BlockDevice> FileSystem<D> {
    pub fn new(disk: D) -> Self {
        let mut nodes = [Node::empty(); MAX_NODES]; nodes[0].kind = NodeKind::Directory; nodes[0].mode = 0o755;
        Self { disk, nodes, cwd: 0, mounted: false, accounts: AccountStore::new() }
    }

    pub fn mount(&mut self) -> Result<(), FsError> {
        let mut sector = [0u8; SECTOR_SIZE]; self.read(0, &mut sector)?;
        if &sector[..MAGIC.len()] != MAGIC || sector[8] != 2 { return Err(FsError::NotFormatted); }
        self.accounts.deserialize(&sector).map_err(|_| FsError::Disk)?;
        self.load_nodes()?;
        if !matches!(self.nodes[0].kind, NodeKind::Directory) { return Err(FsError::Disk); }
        self.cwd = 0; self.mounted = true; Ok(())
    }

    pub fn format(&mut self) -> Result<(), FsError> {
        let mut superblock = [0u8; SECTOR_SIZE]; superblock[..MAGIC.len()].copy_from_slice(MAGIC); superblock[8] = 2; superblock[9] = MAX_NODES as u8;
        self.accounts = AccountStore::new(); self.accounts.serialize(&mut superblock); self.write(0, &superblock)?; self.nodes = [Node::empty(); MAX_NODES]; self.nodes[0].kind = NodeKind::Directory; self.nodes[0].mode = 0o755; self.cwd = 0; self.mounted = true; self.sync_nodes()
    }

    pub fn users_configured(&self) -> bool { self.accounts.configured() }
    pub fn authenticate(&self, name: &str, password: &str) -> Option<Credentials> { self.accounts.authenticate(name, password) }
    pub fn setup_users(&mut self, root_password: &str, user_password: &str, nonce: u64) -> Result<(), FsError> {
        self.require_mount()?; self.accounts.configure(root_password, user_password, nonce).map_err(|_| FsError::Authentication)?;
        self.create_node_internal(0, "home", NodeKind::Directory, ROOT_UID, 0o755)?;
        let home = self.find_child(0, b"home").ok_or(FsError::Missing)?;
        self.create_node_internal(home, "user", NodeKind::Directory, USER_UID, 0o700)?;
        self.sync_super()
    }

    pub fn chmod_as(&mut self, uid: u32, path: &str, mode: u16) -> Result<(), FsError> {
        if uid != ROOT_UID || mode > 0o777 { return Err(FsError::PermissionDenied); }
        let node = self.resolve_as(uid, path)?; self.nodes[node].mode = mode; self.sync_nodes()
    }

    pub fn is_mounted(&self) -> bool { self.mounted }

    pub fn change_dir(&mut self, path: &str) -> Result<(), FsError> {
        self.change_dir_as(ROOT_UID, path)
    }
    pub fn change_dir_as(&mut self, uid: u32, path: &str) -> Result<(), FsError> {
        self.require_mount()?; let node = self.resolve_as(uid, path)?;
        self.check_access(node, uid, 1)?;
        if !matches!(self.nodes[node].kind, NodeKind::Directory) { return Err(FsError::NotDirectory); }
        self.cwd = node; Ok(())
    }

    pub fn make_dir(&mut self, path: &str) -> Result<(), FsError> {
        self.make_dir_as(ROOT_UID, path)
    }
    pub fn make_dir_as(&mut self, uid: u32, path: &str) -> Result<(), FsError> {
        self.require_mount()?; let (parent, name) = self.parent_and_name_as(uid, path)?; self.check_access(parent, uid, 3)?; self.create_node_internal(parent, name, NodeKind::Directory, uid, 0o755).map(|_| ())
    }

    pub fn touch(&mut self, path: &str) -> Result<(), FsError> {
        self.touch_as(ROOT_UID, path)
    }
    pub fn touch_as(&mut self, uid: u32, path: &str) -> Result<(), FsError> {
        self.require_mount()?;
        match self.resolve_as(uid, path) {
            Ok(node) if matches!(self.nodes[node].kind, NodeKind::Directory) => Err(FsError::IsDirectory),
            Ok(node) => { let parent = self.nodes[node].parent; self.check_access(parent, uid, 3) },
            Err(FsError::Missing) => { let (parent, name) = self.parent_and_name_as(uid, path)?; self.check_access(parent, uid, 3)?; self.create_node_internal(parent, name, NodeKind::File, uid, 0o644).map(|_| ()) }
            Err(error) => Err(error),
        }
    }

    pub fn write_file(&mut self, path: &str, text: &str) -> Result<(), FsError> {
        self.write_file_as(ROOT_UID, path, text)
    }
    pub fn write_file_as(&mut self, uid: u32, path: &str, text: &str) -> Result<(), FsError> {
        self.require_mount()?; if text.len() > MAX_FILE_SIZE { return Err(FsError::FileTooLarge); }
        let node = match self.resolve_as(uid, path) {
            Ok(node) => node,
            Err(FsError::Missing) => { let (parent, name) = self.parent_and_name_as(uid, path)?; self.check_access(parent, uid, 3)?; self.create_node_internal(parent, name, NodeKind::File, uid, 0o644)?; self.find_child(parent, name.as_bytes()).ok_or(FsError::Missing)? }
            Err(error) => return Err(error),
        };
        if !matches!(self.nodes[node].kind, NodeKind::File) { return Err(FsError::IsDirectory); }
        self.check_access(node, uid, 2)?;
        let mut sector = [0u8; SECTOR_SIZE]; sector[..text.len()].copy_from_slice(text.as_bytes()); self.write(DATA_START + node as u32, &sector)?;
        self.nodes[node].file_len = text.len(); self.sync_nodes()
    }

    pub fn remove(&mut self, path: &str) -> Result<(), FsError> {
        self.remove_as(ROOT_UID, path)
    }
    pub fn remove_as(&mut self, uid: u32, path: &str) -> Result<(), FsError> {
        self.require_mount()?; let node = self.resolve_as(uid, path)?;
        if node == 0 || node == self.cwd { return Err(FsError::InvalidPath); }
        self.check_access(self.nodes[node].parent, uid, 3)?;
        if matches!(self.nodes[node].kind, NodeKind::Directory) && self.has_children(node) { return Err(FsError::DirectoryNotEmpty); }
        self.nodes[node] = Node::empty(); self.sync_nodes()
    }

    pub fn move_path(&mut self, source: &str, target: &str) -> Result<(), FsError> {
        self.move_path_as(ROOT_UID, source, target)
    }
    pub fn move_path_as(&mut self, uid: u32, source: &str, target: &str) -> Result<(), FsError> {
        self.require_mount()?; let node = self.resolve_as(uid, source)?; if node == 0 { return Err(FsError::InvalidPath); }
        let old_parent = self.nodes[node].parent; self.check_access(old_parent, uid, 3)?; let (parent, name) = self.parent_and_name_as(uid, target)?; self.check_access(parent, uid, 3)?; if self.find_child(parent, name.as_bytes()).is_some() { return Err(FsError::AlreadyExists); }
        self.nodes[node].parent = parent; self.set_name(node, name); self.sync_nodes()
    }

    pub fn copy_file(&mut self, source: &str, target: &str) -> Result<(), FsError> {
        self.copy_file_as(ROOT_UID, source, target)
    }
    pub fn copy_file_as(&mut self, uid: u32, source: &str, target: &str) -> Result<(), FsError> {
        self.require_mount()?; let source_node = self.resolve_as(uid, source)?; if !matches!(self.nodes[source_node].kind, NodeKind::File) { return Err(FsError::IsDirectory); } self.check_access(source_node, uid, 4)?;
        let (parent, name) = self.parent_and_name_as(uid, target)?; self.check_access(parent, uid, 3)?; if self.find_child(parent, name.as_bytes()).is_some() { return Err(FsError::AlreadyExists); }
        let node = self.create_node_internal(parent, name, NodeKind::File, uid, 0o644)?; let mut sector = [0u8; SECTOR_SIZE]; self.read(DATA_START + source_node as u32, &mut sector)?; self.write(DATA_START + node as u32, &sector)?;
        self.nodes[node].file_len = self.nodes[source_node].file_len; self.sync_nodes()
    }

    pub fn print_pwd<W: Write>(&self, out: &mut W) {
        self.write_pwd(out);
        let _ = out.write_str("\n");
    }

    pub fn write_pwd<W: Write>(&self, out: &mut W) {
        let mut path = [0usize; MAX_PATH_DEPTH]; let mut count = 0; let mut node = self.cwd;
        while node != 0 && count < MAX_PATH_DEPTH { path[count] = node; count += 1; node = self.nodes[node].parent; }
        let _ = out.write_str("/"); for index in (0..count).rev() { if index != count - 1 { let _ = out.write_str("/"); } self.write_name(out, path[index]); }
    }

    pub fn print_ls<W: Write>(&self, path: Option<&str>, show_hidden: bool, long: bool, out: &mut W) -> Result<(), FsError> { self.print_ls_as(ROOT_UID, path, show_hidden, long, out) }
    pub fn print_ls_as<W: Write>(&self, uid: u32, path: Option<&str>, show_hidden: bool, long: bool, out: &mut W) -> Result<(), FsError> {
        self.require_mount()?; let node = match path { Some(path) if !path.is_empty() => self.resolve_as(uid, path)?, _ => self.cwd };
        self.check_access(node, uid, 5)?;
        if !matches!(self.nodes[node].kind, NodeKind::Directory) { return Err(FsError::NotDirectory); }
        for index in 1..MAX_NODES {
            if matches!(self.nodes[index].kind, NodeKind::Free) || self.nodes[index].parent != node { continue; }
            if !show_hidden && self.nodes[index].name_len > 0 && self.nodes[index].name[0] == b'.' { continue; }
            if long {
                let _ = out.write_str(if matches!(self.nodes[index].kind, NodeKind::Directory) { "dir " } else { "file " });
                self.write_name(out, index);
                let _ = out.write_str(" owner="); write_number(out, self.nodes[index].owner as usize); let _ = out.write_str(" mode="); write_octal(out, self.nodes[index].mode);
                if matches!(self.nodes[index].kind, NodeKind::File) { let _ = out.write_str(" size="); write_number(out, self.nodes[index].file_len); }
            } else {
                let _ = out.write_str(if matches!(self.nodes[index].kind, NodeKind::Directory) { "d " } else { "f " });
                self.write_name(out, index);
            }
            let _ = out.write_str("\n");
        }
        Ok(())
    }

    pub fn print_file<W: Write>(&mut self, path: &str, out: &mut W) -> Result<(), FsError> { self.print_file_as(ROOT_UID, path, out) }
    pub fn print_file_as<W: Write>(&mut self, uid: u32, path: &str, out: &mut W) -> Result<(), FsError> {
        self.require_mount()?; let node = self.resolve_as(uid, path)?; if !matches!(self.nodes[node].kind, NodeKind::File) { return Err(FsError::IsDirectory); } self.check_access(node, uid, 4)?;
        let mut sector = [0u8; SECTOR_SIZE]; self.read(DATA_START + node as u32, &mut sector)?; let text = core::str::from_utf8(&sector[..self.nodes[node].file_len]).map_err(|_| FsError::Disk)?; let _ = out.write_str(text); let _ = out.write_str("\n"); Ok(())
    }

    fn require_mount(&self) -> Result<(), FsError> { if self.mounted { Ok(()) } else { Err(FsError::NotFormatted) } }

    fn resolve(&self, path: &str) -> Result<usize, FsError> { self.resolve_as(ROOT_UID, path) }
    fn resolve_as(&self, uid: u32, path: &str) -> Result<usize, FsError> {
        if path.is_empty() { return Err(FsError::InvalidPath); } let mut current = if path.as_bytes()[0] == b'/' { 0 } else { self.cwd }; let mut saw = false;
        for component in path.split('/') { if component.is_empty() || component == "." { continue; } saw = true; self.check_access(current, uid, 1)?; if component == ".." { current = if current == 0 { 0 } else { self.nodes[current].parent }; continue; } if !matches!(self.nodes[current].kind, NodeKind::Directory) { return Err(FsError::NotDirectory); } current = self.find_child(current, component.as_bytes()).ok_or(FsError::Missing)?; }
        if !saw && path != "/" && path != "." && path != "./" { return Err(FsError::InvalidPath); } Ok(current)
    }

    fn parent_and_name<'a>(&self, path: &'a str) -> Result<(usize, &'a str), FsError> { self.parent_and_name_as(ROOT_UID, path) }
    fn parent_and_name_as<'a>(&self, uid: u32, path: &'a str) -> Result<(usize, &'a str), FsError> {
        if path.is_empty() || path == "/" || path.ends_with('/') { return Err(FsError::InvalidPath); } let slash = path.rfind('/'); let (parent_path, name) = match slash { Some(0) => ("/", &path[1..]), Some(index) => (&path[..index], &path[index + 1..]), None => (".", path) };
        if name.is_empty() || name == "." || name == ".." { return Err(FsError::InvalidPath); } if name.len() > MAX_NAME || !name.is_ascii() { return Err(FsError::NameTooLong); } let parent = self.resolve_as(uid, parent_path)?; if !matches!(self.nodes[parent].kind, NodeKind::Directory) { return Err(FsError::NotDirectory); } Ok((parent, name))
    }

    fn create_node(&mut self, parent: usize, name: &str, kind: NodeKind) -> Result<(), FsError> { self.create_node_returning(parent, name, kind).map(|_| ()) }
    fn create_node_returning(&mut self, parent: usize, name: &str, kind: NodeKind) -> Result<usize, FsError> {
        self.create_node_internal(parent, name, kind, ROOT_UID, if matches!(kind, NodeKind::Directory) { 0o755 } else { 0o644 }).map(|index| index)
    }
    fn create_node_internal(&mut self, parent: usize, name: &str, kind: NodeKind, owner: u32, mode: u16) -> Result<usize, FsError> {
        if self.find_child(parent, name.as_bytes()).is_some() { return Err(FsError::AlreadyExists); } let index = (1..MAX_NODES).find(|&i| matches!(self.nodes[i].kind, NodeKind::Free)).ok_or(FsError::NoSpace)?; self.nodes[index] = Node { kind, parent, name: [0; MAX_NAME], name_len: name.len(), file_len: 0, owner, mode }; self.set_name(index, name); self.sync_nodes()?; Ok(index)
    }
    fn check_access(&self, node: usize, uid: u32, required: u16) -> Result<(), FsError> {
        if uid == ROOT_UID { return Ok(()); }
        let shift = if self.nodes[node].owner == uid { 6 } else { 0 };
        if ((self.nodes[node].mode >> shift) & required) == required { Ok(()) } else { Err(FsError::PermissionDenied) }
    }
    fn find_child(&self, parent: usize, name: &[u8]) -> Option<usize> { (1..MAX_NODES).find(|&i| !matches!(self.nodes[i].kind, NodeKind::Free) && self.nodes[i].parent == parent && self.nodes[i].name_len == name.len() && self.nodes[i].name[..name.len()] == name[..]) }
    fn has_children(&self, parent: usize) -> bool { (1..MAX_NODES).any(|i| !matches!(self.nodes[i].kind, NodeKind::Free) && self.nodes[i].parent == parent) }
    fn set_name(&mut self, node: usize, name: &str) { self.nodes[node].name = [0; MAX_NAME]; self.nodes[node].name[..name.len()].copy_from_slice(name.as_bytes()); self.nodes[node].name_len = name.len(); }
    fn write_name<W: Write>(&self, out: &mut W, node: usize) { let _ = out.write_str(core::str::from_utf8(&self.nodes[node].name[..self.nodes[node].name_len]).unwrap_or("<invalid>")); }

    fn load_nodes(&mut self) -> Result<(), FsError> {
        let mut raw = [0u8; SECTOR_SIZE * NODE_SECTORS as usize]; for sector in 0..NODE_SECTORS { let part: &mut [u8; SECTOR_SIZE] = (&mut raw[sector as usize * SECTOR_SIZE..][..SECTOR_SIZE]).try_into().unwrap(); self.read(1 + sector, part)?; }
        for i in 0..MAX_NODES { let p = i * 40; self.nodes[i] = Node::empty(); self.nodes[i].kind = match raw[p] { 1 => NodeKind::Directory, 2 => NodeKind::File, _ => NodeKind::Free }; self.nodes[i].parent = u16::from_le_bytes([raw[p + 1], raw[p + 2]]) as usize; self.nodes[i].file_len = u16::from_le_bytes([raw[p + 3], raw[p + 4]]) as usize; self.nodes[i].name_len = (raw[p + 5] as usize).min(MAX_NAME); self.nodes[i].name.copy_from_slice(&raw[p + 6..p + 6 + MAX_NAME]); self.nodes[i].owner = u32::from_le_bytes([raw[p + 30], raw[p + 31], raw[p + 32], raw[p + 33]]); self.nodes[i].mode = u16::from_le_bytes([raw[p + 34], raw[p + 35]]); }
        Ok(())
    }
    fn sync_nodes(&mut self) -> Result<(), FsError> {
        let mut raw = [0u8; SECTOR_SIZE * NODE_SECTORS as usize]; for i in 0..MAX_NODES { let p = i * 40; raw[p] = match self.nodes[i].kind { NodeKind::Free => 0, NodeKind::Directory => 1, NodeKind::File => 2 }; raw[p + 1..p + 3].copy_from_slice(&(self.nodes[i].parent as u16).to_le_bytes()); raw[p + 3..p + 5].copy_from_slice(&(self.nodes[i].file_len as u16).to_le_bytes()); raw[p + 5] = self.nodes[i].name_len as u8; raw[p + 6..p + 6 + MAX_NAME].copy_from_slice(&self.nodes[i].name); raw[p + 30..p + 34].copy_from_slice(&self.nodes[i].owner.to_le_bytes()); raw[p + 34..p + 36].copy_from_slice(&self.nodes[i].mode.to_le_bytes()); } for sector in 0..NODE_SECTORS { let part: &[u8; SECTOR_SIZE] = (&raw[sector as usize * SECTOR_SIZE..][..SECTOR_SIZE]).try_into().unwrap(); self.write(1 + sector, part)?; } Ok(())
    }
    fn sync_super(&mut self) -> Result<(), FsError> { let mut block = [0u8; SECTOR_SIZE]; self.read(0, &mut block)?; self.accounts.serialize(&mut block); self.write(0, &block) }
    fn read(&mut self, lba: u32, sector: &mut [u8; SECTOR_SIZE]) -> Result<(), FsError> { self.disk.read_sector(FS_START_LBA + lba, sector).map_err(|_| FsError::Disk) }
    fn write(&mut self, lba: u32, sector: &[u8; SECTOR_SIZE]) -> Result<(), FsError> { self.disk.write_sector(FS_START_LBA + lba, sector).map_err(|_| FsError::Disk) }
    #[cfg(test)] pub fn into_disk(self) -> D { self.disk }
}

fn write_number<W: Write>(out: &mut W, mut number: usize) {
    let mut digits = [0u8; 20]; let mut length = 0;
    if number == 0 { let _ = out.write_str("0"); return; }
    while number > 0 { digits[length] = b'0' + (number % 10) as u8; length += 1; number /= 10; }
    for index in (0..length).rev() { let _ = out.write_char(digits[index] as char); }
}

fn write_octal<W: Write>(out: &mut W, number: u16) {
    for shift in [6, 3, 0] { let digit = b'0' + ((number >> shift) & 7) as u8; let _ = out.write_char(digit as char); }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct MockDisk { sectors: std::vec::Vec<[u8; SECTOR_SIZE]> }
    impl MockDisk { fn new() -> Self { Self { sectors: vec![[0; SECTOR_SIZE]; 2100] } } }
    impl BlockDevice for MockDisk { fn read_sector(&mut self, lba: u32, b: &mut [u8; SECTOR_SIZE]) -> Result<(), DiskError> { *b = self.sectors[lba as usize]; Ok(()) } fn write_sector(&mut self, lba: u32, b: &[u8; SECTOR_SIZE]) -> Result<(), DiskError> { self.sectors[lba as usize] = *b; Ok(()) } }

    #[test] fn data_survives_unmount_and_mount() { let mut fs = FileSystem::new(MockDisk::new()); assert_eq!(fs.mount(), Err(FsError::NotFormatted)); fs.format().unwrap(); fs.make_dir("/docs").unwrap(); fs.write_file("/docs/readme", "persistent").unwrap(); let disk = fs.into_disk(); let mut rebooted = FileSystem::new(disk); rebooted.mount().unwrap(); let mut out = String::new(); rebooted.print_file("/docs/readme", &mut out).unwrap(); assert_eq!(out, "persistent\n"); }
    #[test] fn paths_and_copy_move_work() { let mut fs = FileSystem::new(MockDisk::new()); fs.format().unwrap(); fs.make_dir("/a").unwrap(); fs.change_dir("/a").unwrap(); fs.write_file("x", "one").unwrap(); fs.copy_file("x", "y").unwrap(); fs.move_path("y", "z").unwrap(); let mut out = String::new(); fs.print_file("../a/z", &mut out).unwrap(); assert_eq!(out, "one\n"); }
    #[test] fn ls_options_and_absolute_relative_paths_work() { let mut fs = FileSystem::new(MockDisk::new()); fs.format().unwrap(); fs.make_dir("/docs").unwrap(); fs.touch("/.hidden").unwrap(); fs.write_file("/docs/readme", "hello").unwrap(); let mut out = String::new(); fs.print_ls(Some("/"), false, false, &mut out).unwrap(); assert!(out.contains("d docs\n")); assert!(!out.contains(".hidden")); out.clear(); fs.print_ls(Some("/"), true, true, &mut out).unwrap(); assert!(out.contains("file .hidden owner=0 mode=644 size=0\n")); fs.change_dir("/docs").unwrap(); fs.change_dir("..").unwrap(); fs.change_dir("./docs").unwrap(); assert_eq!(fs.resolve(".").unwrap(), fs.resolve("/docs").unwrap()); }
    #[test] fn accounts_ownership_permissions_and_persistence_work() { let mut fs = FileSystem::new(MockDisk::new()); fs.format().unwrap(); fs.setup_users("root-password", "user-password", 7).unwrap(); assert_eq!(fs.authenticate("root", "root-password").unwrap().uid, ROOT_UID); assert_eq!(fs.authenticate("user", "user-password").unwrap().uid, USER_UID); fs.write_file_as(ROOT_UID, "/admin.txt", "root-only").unwrap(); assert_eq!(fs.write_file_as(USER_UID, "/admin.txt", "blocked"), Err(FsError::PermissionDenied)); fs.write_file_as(USER_UID, "/home/user/note", "allowed").unwrap(); fs.chmod_as(ROOT_UID, "/admin.txt", 0o600).unwrap(); assert_eq!(fs.print_file_as(USER_UID, "/admin.txt", &mut String::new()), Err(FsError::PermissionDenied)); let disk = fs.into_disk(); let mut rebooted = FileSystem::new(disk); rebooted.mount().unwrap(); assert!(rebooted.authenticate("root", "root-password").is_some()); assert_eq!(rebooted.write_file_as(USER_UID, "/admin.txt", "blocked"), Err(FsError::PermissionDenied)); }
}
