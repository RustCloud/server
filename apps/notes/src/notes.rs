//! Notes are `.md` files in the folder the user gave the app (and its
//! subfolders). The list comes from an index in the app's AppData
//! (`index.json`): title and text of every note, refreshed only for files
//! whose content changed since (their etag differs).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::rustcloud::app::{appdata, files};

/// How deep subfolders are searched for notes.
const MAX_DEPTH: usize = 4;
/// Most notes listed; more than that is not what this app is for.
const MAX_NOTES: usize = 2000;
/// How much of a note's text the index keeps for search.
const INDEX_TEXT: usize = 64 * 1024;
const INDEX_FILE: &str = "index.json";

/// A note in the list.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    /// Subfolder path inside the notes folder, `""` at the top.
    pub folder: String,
    pub excerpt: String,
    pub updated_at: String,
}

/// A note opened for editing.
#[derive(Debug, Serialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    /// Sent back on save, so changes made elsewhere are not overwritten.
    pub etag: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    notes: HashMap<String, IndexEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexEntry {
    etag: String,
    title: String,
    text: String,
}

/// The notes folder: the first folder the user gave the app, if any.
pub fn folder() -> Result<Option<files::Node>, files::Error> {
    Ok(files::granted()?.into_iter().find(|node| node.is_dir))
}

/// All notes, newest first; with `query`, those whose title or text contains it.
pub fn list(folder: &files::Node, query: &str) -> Result<Vec<Summary>, files::Error> {
    let mut index = load_index();
    let mut changed = false;
    let mut found = Vec::new();
    let mut summaries = Vec::new();

    let mut level = vec![(folder.id.clone(), String::new())];
    for _ in 0..=MAX_DEPTH {
        let mut next = Vec::new();
        for (folder_id, path) in level {
            for node in files::children(&folder_id)? {
                if node.is_dir {
                    if !node.name.starts_with('.') {
                        next.push((node.id, join(&path, &node.name)));
                    }
                    continue;
                }
                if !is_note(&node.name) || summaries.len() >= MAX_NOTES {
                    continue;
                }
                let etag = node.etag.clone().unwrap_or_default();
                let fresh = index.notes.get(&node.id).is_some_and(|entry| entry.etag == etag);
                if !fresh {
                    let content = String::from_utf8_lossy(&files::read(&node.id)?).into_owned();
                    let entry =
                        IndexEntry { etag, title: title_of(&content, &node.name), text: cut(&content, INDEX_TEXT) };
                    index.notes.insert(node.id.clone(), entry);
                    changed = true;
                }
                let entry = &index.notes[&node.id];
                found.push(node.id.clone());
                summaries.push(Summary {
                    id: node.id.clone(),
                    title: entry.title.clone(),
                    folder: path.clone(),
                    excerpt: excerpt(&entry.text),
                    updated_at: node.updated_at.clone(),
                });
            }
        }
        level = next;
    }

    // Forget notes that are gone.
    let before = index.notes.len();
    index.notes.retain(|id, _| found.contains(id));
    if changed || index.notes.len() != before {
        save_index(&index);
    }

    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        summaries.retain(|summary| {
            let entry = &index.notes[&summary.id];
            entry.title.to_lowercase().contains(&query) || entry.text.to_lowercase().contains(&query)
        });
    }
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(summaries)
}

pub fn open(id: &str) -> Result<Note, files::Error> {
    let node = files::get(id)?;
    let content = String::from_utf8_lossy(&files::read(id)?).into_owned();
    Ok(Note { id: node.id, title: title_of(&content, &node.name), etag: node.etag.unwrap_or_default(), content })
}

/// Why a save did not happen.
pub enum SaveError {
    /// The file changed since it was opened; its current etag.
    Changed(String),
    Files(files::Error),
}

/// Saves a note opened with `etag`; refuses if the file changed since.
pub fn save(id: &str, content: &str, etag: &str) -> Result<Note, SaveError> {
    let current = files::get(id).map_err(SaveError::Files)?;
    let current_etag = current.etag.unwrap_or_default();
    if current_etag != etag {
        return Err(SaveError::Changed(current_etag));
    }
    let node = files::write(id, content.as_bytes()).map_err(SaveError::Files)?;
    Ok(Note {
        id: node.id,
        title: title_of(content, &node.name),
        etag: node.etag.unwrap_or_default(),
        content: content.to_owned(),
    })
}

/// Creates an empty note `name.md` (or `name 2.md`, … if taken) in the notes folder.
pub fn create(folder: &files::Node, name: &str) -> Result<Note, files::Error> {
    let name = name.trim().trim_end_matches(".md");
    let name = if name.is_empty() || name.contains('/') { "Note" } else { name };
    for n in 1..100 {
        let file_name = if n == 1 { format!("{name}.md") } else { format!("{name} {n}.md") };
        match files::create_file(&folder.id, &file_name, &[]) {
            Ok(node) => {
                return Ok(Note {
                    id: node.id,
                    title: title_of("", &file_name),
                    content: String::new(),
                    etag: node.etag.unwrap_or_default(),
                })
            }
            Err(files::Error::Conflict) => continue,
            Err(err) => return Err(err),
        }
    }
    Err(files::Error::Conflict)
}

pub fn delete(id: &str) -> Result<(), files::Error> {
    files::delete(id)
}

/// The first non-empty line without `#`s, or the file name without `.md`.
fn title_of(content: &str, file_name: &str) -> String {
    content
        .lines()
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(|line| cut(line, 200))
        .unwrap_or_else(|| file_name.trim_end_matches(".md").to_owned())
}

/// The start of the text after the title line, on one line.
fn excerpt(text: &str) -> String {
    let rest: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).skip(1).take(3).collect();
    cut(&rest.join(" "), 160)
}

fn is_note(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".md") && !name.starts_with('.')
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}/{name}")
    }
}

/// At most `max` bytes of `text`, cut at a character boundary.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The index from AppData; a missing or broken one is rebuilt from the files.
fn load_index() -> Index {
    appdata::read(INDEX_FILE).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
}

/// Saves the index; if that fails, the next listing rebuilds it.
fn save_index(index: &Index) {
    if let Ok(data) = serde_json::to_vec(index) {
        let _ = appdata::write(INDEX_FILE, &data);
    }
}
