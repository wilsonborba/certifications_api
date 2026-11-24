use crate::core::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait FsmClient: Send + Sync {
    fn put_bytes<'a>(&'a self, ref_key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<String>>;
    fn get_bytes<'a>(&'a self, ref_key: &'a str) -> BoxFuture<'a, Result<Vec<u8>>>;
    fn get_bytes_bounded<'a>(
        &'a self,
        ref_key: &'a str,
        max_bytes: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>>>;
    fn delete_ref<'a>(&'a self, ref_key: &'a str) -> BoxFuture<'a, Result<()>>;
}

#[derive(Default)]
pub struct MemoryFsm {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    pub put_calls: AtomicUsize,
    pub get_calls: AtomicUsize,
    pub fail_delete: AtomicBool,
}

impl MemoryFsm {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn put_count(&self) -> usize {
        self.put_calls.load(Ordering::SeqCst)
    }
    pub fn get_count(&self) -> usize {
        self.get_calls.load(Ordering::SeqCst)
    }
    pub fn has_blob(&self, ref_key: &str) -> bool {
        self.blobs.lock().unwrap().contains_key(ref_key)
    }
    pub fn seed(&self, ref_key: &str, bytes: &[u8]) {
        self.blobs
            .lock()
            .unwrap()
            .insert(ref_key.to_string(), bytes.to_vec());
    }
}

impl FsmClient for MemoryFsm {
    fn put_bytes<'a>(&'a self, ref_key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            self.put_calls.fetch_add(1, Ordering::SeqCst);
            self.blobs
                .lock()
                .unwrap()
                .insert(ref_key.to_string(), bytes.to_vec());
            Ok(ref_key.to_string())
        })
    }
    fn get_bytes<'a>(&'a self, ref_key: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            self.blobs
                .lock()
                .unwrap()
                .get(ref_key)
                .cloned()
                .ok_or_else(Error::missing)
        })
    }
    fn get_bytes_bounded<'a>(
        &'a self,
        ref_key: &'a str,
        max_bytes: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            let bytes = self
                .blobs
                .lock()
                .unwrap()
                .get(ref_key)
                .cloned()
                .ok_or_else(Error::missing)?;
            if bytes.len() > max_bytes {
                return Err(Error::unprocessable(
                    "quota_exceeded: Stored source exceeds byte limit",
                ));
            }
            Ok(bytes)
        })
    }
    fn delete_ref<'a>(&'a self, ref_key: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(Error::unavailable());
            }
            self.blobs.lock().unwrap().remove(ref_key);
            Ok(())
        })
    }
}

pub struct HttpFsm {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl HttpFsm {
    pub fn client(base_url: &str, api_key: &str, http: reqwest::Client) -> Arc<dyn FsmClient> {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return MemoryFsm::new();
        }
        Arc::new(Self {
            base_url: base.to_string(),
            api_key: api_key.to_string(),
            http,
        })
    }
}

impl FsmClient for HttpFsm {
    fn put_bytes<'a>(&'a self, ref_key: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut req = self
                .http
                .put(format!("{}/files/{}", self.base_url, ref_key))
                .body(bytes.to_vec());
            if !self.api_key.is_empty() {
                req = req.header("x-fsm-key", &self.api_key);
            }
            let resp = req.send().await.map_err(|_| Error::unavailable())?;
            if resp.status().is_success() {
                Ok(ref_key.to_string())
            } else {
                Err(Error::unavailable())
            }
        })
    }
    fn get_bytes<'a>(&'a self, ref_key: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        self.get_bytes_bounded(ref_key, 100 * 1024 * 1024)
    }
    fn get_bytes_bounded<'a>(
        &'a self,
        ref_key: &'a str,
        max_bytes: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            let mut req = self
                .http
                .get(format!("{}/files/{}", self.base_url, ref_key));
            if !self.api_key.is_empty() {
                req = req.header("x-fsm-key", &self.api_key);
            }
            let mut resp = req.send().await.map_err(|_| Error::unavailable())?;
            if resp.status() == axum::http::StatusCode::NOT_FOUND {
                return Err(Error::missing());
            }
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            if let Some(cl) = resp.content_length()
                && cl > max_bytes as u64
            {
                return Err(Error::unprocessable(
                    "quota_exceeded: Stored source exceeds byte limit",
                ));
            }
            let mut out = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| Error::unavailable())? {
                if out.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(Error::unprocessable(
                        "quota_exceeded: Stored source exceeds byte limit",
                    ));
                }
                out.extend_from_slice(&chunk);
            }
            Ok(out)
        })
    }
    fn delete_ref<'a>(&'a self, ref_key: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut req = self
                .http
                .delete(format!("{}/files/{}", self.base_url, ref_key));
            if !self.api_key.is_empty() {
                req = req.header("x-fsm-key", &self.api_key);
            }
            let resp = req.send().await.map_err(|_| Error::unavailable())?;
            if resp.status().is_success() || resp.status() == axum::http::StatusCode::NOT_FOUND {
                Ok(())
            } else {
                Err(Error::unavailable())
            }
        })
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
