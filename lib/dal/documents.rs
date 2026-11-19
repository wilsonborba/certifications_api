use crate::core::{Error, Result, Settings};
use axum::http::StatusCode;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub enum Documents {
    Memory(Arc<MemoryDocs>),
    Remote(Arc<RemoteDocs>),
}

#[derive(Default)]
pub struct MemoryDocs {
    docs: Mutex<HashMap<String, Value>>,
    pub fail_delete: AtomicBool,
}

impl MemoryDocs {
    pub fn len(&self) -> usize {
        self.docs.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn contains(&self, doc_id: &str) -> bool {
        self.docs.lock().unwrap().contains_key(doc_id)
    }
}

pub struct RemoteDocs {
    base_url: String,
    database: String,
    user: String,
    password: String,
    http: reqwest::Client,
}

impl Documents {
    pub fn memory() -> Arc<MemoryDocs> {
        Arc::new(MemoryDocs::default())
    }

    pub fn remote(settings: &Settings, http: reqwest::Client) -> Self {
        Self::Remote(Arc::new(RemoteDocs {
            base_url: settings.couch_url.trim_end_matches('/').to_string(),
            database: settings.couch_database.clone(),
            user: settings.couch_user.clone(),
            password: settings.couch_password.clone(),
            http,
        }))
    }

    pub async fn put_immutable(&self, doc_id: &str, body: &Value) -> Result<()> {
        match self {
            Self::Memory(mem) => {
                let mut map = mem.docs.lock().unwrap();
                if let Some(existing) = map.get(doc_id) {
                    if existing == body {
                        return Ok(());
                    }
                    return Err(Error::conflict(
                        "document_exists: Immutable document already exists",
                    ));
                }
                map.insert(doc_id.to_string(), body.clone());
                Ok(())
            }
            Self::Remote(remote) => {
                if remote.base_url.is_empty() {
                    return Err(Error::unavailable());
                }
                let url = format!("{}/{}/{}", remote.base_url, remote.database, doc_id);
                let mut req = remote.http.put(&url).json(body);
                if !remote.user.is_empty() {
                    req = req.basic_auth(&remote.user, Some(&remote.password));
                }
                let resp = req.send().await.map_err(|_| Error::unavailable())?;
                if resp.status().is_success() {
                    return Ok(());
                }
                if resp.status() == StatusCode::CONFLICT {
                    let existing = self.get(doc_id).await.ok();
                    if existing.as_ref() == Some(body) {
                        return Ok(());
                    }
                    return Err(Error::conflict(
                        "document_exists: Immutable document already exists",
                    ));
                }
                Err(Error::unavailable())
            }
        }
    }

    pub async fn get(&self, doc_id: &str) -> Result<Value> {
        match self {
            Self::Memory(mem) => mem
                .docs
                .lock()
                .unwrap()
                .get(doc_id)
                .cloned()
                .ok_or_else(Error::missing),
            Self::Remote(remote) => {
                if remote.base_url.is_empty() {
                    return Err(Error::unavailable());
                }
                let url = format!("{}/{}/{}", remote.base_url, remote.database, doc_id);
                let mut req = remote.http.get(&url);
                if !remote.user.is_empty() {
                    req = req.basic_auth(&remote.user, Some(&remote.password));
                }
                let resp = req.send().await.map_err(|_| Error::unavailable())?;
                if resp.status() == StatusCode::NOT_FOUND {
                    return Err(Error::missing());
                }
                if !resp.status().is_success() {
                    return Err(Error::unavailable());
                }
                resp.json::<Value>().await.map_err(|_| Error::unavailable())
            }
        }
    }

    pub async fn delete(&self, doc_id: &str) -> Result<()> {
        match self {
            Self::Memory(mem) => {
                if mem.fail_delete.load(Ordering::SeqCst) {
                    return Err(Error::unavailable());
                }
                mem.docs.lock().unwrap().remove(doc_id);
                Ok(())
            }
            Self::Remote(remote) => {
                if remote.base_url.is_empty() {
                    return Err(Error::unavailable());
                }
                let doc = self.get(doc_id).await?;
                let rev = doc["_rev"].as_str().ok_or_else(Error::unavailable)?;
                let url = format!(
                    "{}/{}/{}?rev={}",
                    remote.base_url, remote.database, doc_id, rev
                );
                let mut req = remote.http.delete(&url);
                if !remote.user.is_empty() {
                    req = req.basic_auth(&remote.user, Some(&remote.password));
                }
                let resp = req.send().await.map_err(|_| Error::unavailable())?;
                if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                    Ok(())
                } else {
                    Err(Error::unavailable())
                }
            }
        }
    }
}
