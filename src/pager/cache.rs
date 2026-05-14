//! 頁面快取與 Bloom Filter

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

pub const DIR_DEFAULT_CACHE_SIZE: usize = 64 * 1024 * 1024; // 64MB

/// LRU 頁面快取
pub struct PageCache {
    cache: Mutex<HashMap<usize, (Vec<u8>, usize)>>,
    lru: Mutex<VecDeque<usize>>,
    max_size: usize,
    current_size: Mutex<usize>,
}

impl PageCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            lru: Mutex::new(VecDeque::new()),
            max_size,
            current_size: Mutex::new(0),
        }
    }

    pub fn get(&self, page_id: usize) -> Option<Vec<u8>> {
        let mut cache = self.cache.lock().unwrap();
        if let Some((data, _)) = cache.get(&page_id) {
            let mut lru = self.lru.lock().unwrap();
            lru.retain(|&id| id != page_id);
            lru.push_back(page_id);
            return Some(data.clone());
        }
        None
    }

    pub fn put(&self, page_id: usize, data: Vec<u8>) {
        let size = data.len();
        let mut current = self.current_size.lock().unwrap();
        
        if size > self.max_size / 10 { return; }
        
        while *current + size > self.max_size {
            if let Some(evict_id) = self.lru.lock().unwrap().pop_front() {
                if let Some((_, s)) = self.cache.lock().unwrap().remove(&evict_id) {
                    *current -= s;
                }
            } else { break; }
        }
        
        self.cache.lock().unwrap().insert(page_id, (data.clone(), size));
        self.lru.lock().unwrap().push_back(page_id);
        *current += size;
    }

    pub fn invalidate(&self, page_id: usize) {
        if let Some((_, size)) = self.cache.lock().unwrap().remove(&page_id) {
            *self.current_size.lock().unwrap() -= size;
        }
        self.lru.lock().unwrap().retain(|&id| id != page_id);
    }

    pub fn clear(&self) {
        self.cache.lock().unwrap().clear();
        self.lru.lock().unwrap().clear();
        *self.current_size.lock().unwrap() = 0;
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let len = self.cache.lock().unwrap().len();
        let size = *self.current_size.lock().unwrap();
        (len, size, self.max_size)
    }
}

/// Bloom Filter for fast key existence check
pub struct BloomFilter {
    bits: Vec<u8>,
    size: usize,
    hashes: usize,
}

impl BloomFilter {
    pub fn new(size: usize, hashes: usize) -> Self {
        Self { bits: vec![0; size], size, hashes }
    }

    pub fn insert(&mut self, key: &[u8]) {
        for i in 0..self.hashes {
            let idx = self.hash(key, i) % self.size;
            self.bits[idx / 8] |= 1 << (idx % 8);
        }
    }

    pub fn might_contain(&self, key: &[u8]) -> bool {
        for i in 0..self.hashes {
            let idx = self.hash(key, i) % self.size;
            if self.bits[idx / 8] & (1 << (idx % 8)) == 0 { return false; }
        }
        true
    }

    fn hash(&self, key: &[u8], seed: usize) -> usize {
        let mut h = 14695981039346656037u64;
        h ^= seed as u64;
        for &b in key {
            h = h.wrapping_mul(1099511628211u64);
            h ^= b as u64;
        }
        h as usize % self.size
    }
}