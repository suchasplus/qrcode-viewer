use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: u64,
    pub text: String,
    pub source: String,
    pub area_px: u32,
    pub total_in_frame: usize,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    pub entries: Vec<HistoryEntry>,
    next_id: u64,
    #[serde(skip)]
    storage_path: Option<PathBuf>,
}

impl HistoryStore {
    pub fn default_storage_path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|d| d.join("qrcode-viewer").join("history.json"))
    }

    pub fn load_or_default() -> Self {
        let path = Self::default_storage_path();
        Self::load_from_path(path)
    }

    pub fn load_from_path(path: Option<PathBuf>) -> Self {
        if let Some(ref p) = path {
            if let Ok(raw) = std::fs::read_to_string(p) {
                if let Ok(mut store) = serde_json::from_str::<HistoryStore>(&raw) {
                    store.storage_path = path;
                    if store.next_id == 0 {
                        store.next_id = store.entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
                    }
                    return store;
                }
            }
        }
        Self {
            entries: Vec::new(),
            next_id: 1,
            storage_path: path,
        }
    }

    pub fn save(&self) {
        let Some(ref path) = self.storage_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// 记录一条新的识别结果；若与最近一条内容相同则仅刷新时间与来源，返回是否为新内容
    pub fn record(
        &mut self,
        text: &str,
        source: &str,
        area_px: u32,
        total_in_frame: usize,
    ) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        if let Some(first) = self.entries.first_mut() {
            if first.text == trimmed {
                first.timestamp = now_str;
                first.source = source.to_string();
                first.area_px = area_px;
                first.total_in_frame = total_in_frame;
                self.save();
                return false;
            }
        }

        // 若历史列表中已有相同内容，将其移至顶部更新
        if let Some(pos) = self.entries.iter().position(|e| e.text == trimmed) {
            let mut existing = self.entries.remove(pos);
            existing.timestamp = now_str;
            existing.source = source.to_string();
            existing.area_px = area_px;
            existing.total_in_frame = total_in_frame;
            self.entries.insert(0, existing);
            self.save();
            return true;
        }

        let id = if self.next_id == 0 { 1 } else { self.next_id };
        self.next_id = id + 1;

        self.entries.insert(
            0,
            HistoryEntry {
                id,
                text: trimmed.to_string(),
                source: source.to_string(),
                area_px,
                total_in_frame,
                timestamp: now_str,
            },
        );

        if self.entries.len() > 200 {
            self.entries.truncate(200);
        }
        self.save();
        true
    }

    pub fn remove_by_id(&mut self, id: u64) {
        self.entries.retain(|e| e.id != id);
        self.save();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }
}
