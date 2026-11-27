use crate::core::{Error, Result, Settings};
use axum::http::StatusCode;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
    read_count: AtomicUsize,
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
    pub fn read_count(&self) -> usize {
        self.read_count.load(Ordering::SeqCst)
    }
    pub fn reset_read_count(&self) {
        self.read_count.store(0, Ordering::SeqCst);
    }
    fn get(&self, doc_id: &str) -> Result<Value> {
        self.read_count.fetch_add(1, Ordering::SeqCst);
        self.docs
            .lock()
            .unwrap()
            .get(doc_id)
            .cloned()
            .ok_or_else(Error::missing)
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
                    if existing.as_ref().is_some_and(|document| {
                        immutable_remote_body_matches(doc_id, document, body)
                    }) {
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
            Self::Memory(mem) => mem.get(doc_id),
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

fn immutable_remote_body_matches(doc_id: &str, existing: &Value, expected: &Value) -> bool {
    let Some(object) = existing.as_object() else {
        return false;
    };
    if object.get("_id").and_then(Value::as_str) != Some(doc_id)
        || object
            .get("_rev")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return false;
    }
    let mut content = existing.clone();
    let Some(content) = content.as_object_mut() else {
        return false;
    };
    content.remove("_id");
    content.remove("_rev");
    &Value::Object(content.clone()) == expected
}
