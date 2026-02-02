use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GroupBook {
    pub groups: Vec<GroupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    pub gssi: u32,
    #[serde(default)]
    pub members: Vec<u32>,
}

static BOOK: OnceLock<Mutex<GroupBook>> = OnceLock::new();
static BOOK_PATH: OnceLock<PathBuf> = OnceLock::new();

fn default_path() -> PathBuf {
    std::env::var("TETRA_HTTP_GROUPBOOK_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("tetra_http_groups.json"))
}

fn path() -> &'static PathBuf {
    BOOK_PATH.get_or_init(default_path)
}

fn load_from_disk(p: &Path) -> GroupBook {
    let Ok(bytes) = fs::read(p) else { return GroupBook::default(); };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_to_disk(p: &Path, book: &GroupBook) {
    if let Ok(s) = serde_json::to_vec_pretty(book) {
        let _ = fs::write(p, s);
    }
}

fn book() -> &'static Mutex<GroupBook> {
    BOOK.get_or_init(|| Mutex::new(load_from_disk(path())))
}

pub fn get() -> GroupBook {
    book().lock().unwrap().clone()
}

pub fn upsert_group(name: String, gssi: u32) -> GroupBook {
    let mut g = book().lock().unwrap();
    if let Some(e) = g.groups.iter_mut().find(|x| x.gssi == gssi) {
        e.name = name;
    } else {
        g.groups.push(GroupEntry { name, gssi, members: Vec::new() });
        g.groups.sort_by_key(|x| x.gssi);
    }
    save_to_disk(path(), &g);
    g.clone()
}

pub fn delete_group(gssi: u32) -> GroupBook {
    let mut g = book().lock().unwrap();
    g.groups.retain(|x| x.gssi != gssi);
    save_to_disk(path(), &g);
    g.clone()
}

pub fn set_members(gssi: u32, members: Vec<u32>) -> GroupBook {
    let mut g = book().lock().unwrap();
    if let Some(e) = g.groups.iter_mut().find(|x| x.gssi == gssi) {
        e.members = members;
        e.members.sort_unstable();
        e.members.dedup();
    }
    save_to_disk(path(), &g);
    g.clone()
}

pub fn move_member(issi: u32, to_gssi: u32) -> GroupBook {
    let mut g = book().lock().unwrap();
    // remove from any group
    for e in g.groups.iter_mut() {
        e.members.retain(|&m| m != issi);
    }
    // add to target
    if let Some(e) = g.groups.iter_mut().find(|x| x.gssi == to_gssi) {
        e.members.push(issi);
        e.members.sort_unstable();
        e.members.dedup();
    }
    save_to_disk(path(), &g);
    g.clone()
}
