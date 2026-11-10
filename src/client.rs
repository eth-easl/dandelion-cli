//! HTTP transport and BSON encoding/decoding of the Dandelion API documents.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document, doc};

use crate::config::Settings;
use crate::error::{ApiError, ConnectionError};

pub const BSON_CONTENT_TYPE: &str = "application/bson";

// ---------------------------------------------------------------------------------------------
// Data model

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub identifier: String,
    pub key: i64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Set {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct InvokeResponse {
    pub sets: Vec<Set>,
    /// JSON-encoded string, passed through as received.
    pub timestamps: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputSet {
    pub name: String,
    /// `None` is sent as BSON null; `Some` as an array of `[item_name, item_bytes]` pairs.
    pub static_items: Option<Vec<(String, Vec<u8>)>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionRegistration {
    pub name: String,
    pub context_size: u64,
    pub engine_type: String,
    pub local_path: String,
    pub binary: Vec<u8>,
    pub input_sets: Vec<InputSet>,
    pub output_sets: Vec<String>,
    pub min_set_bytes: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InvokeTarget {
    Name(String),
    Composition(String),
}

// ---------------------------------------------------------------------------------------------
// Encoding

fn binary(bytes: &[u8]) -> Bson {
    Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: bytes.to_vec() })
}

fn to_i64(v: u64, what: &str) -> Result<i64> {
    i64::try_from(v).map_err(|_| anyhow!("{what} {v} is too large"))
}

impl FunctionRegistration {
    pub fn to_document(&self) -> Result<Document> {
        let input_sets: Vec<Bson> = self
            .input_sets
            .iter()
            .map(|set| {
                let statics = match &set.static_items {
                    None => Bson::Null,
                    Some(items) => Bson::Array(
                        items
                            .iter()
                            .map(|(name, data)| Bson::Array(vec![name.clone().into(), binary(data)]))
                            .collect(),
                    ),
                };
                Bson::Array(vec![set.name.clone().into(), statics])
            })
            .collect();
        let min_set_bytes = self
            .min_set_bytes
            .iter()
            .map(|v| to_i64(*v, "min_set_bytes value").map(Bson::Int64))
            .collect::<Result<Vec<_>>>()?;

        Ok(doc! {
            "name": &self.name,
            "context_size": to_i64(self.context_size, "context_size")?,
            "engine_type": &self.engine_type,
            "local_path": &self.local_path,
            "binary": binary(&self.binary),
            "input_sets": input_sets,
            "output_sets": &self.output_sets,
            "min_set_bytes": min_set_bytes,
        })
    }
}

pub fn composition_document(text: &str) -> Document {
    doc! { "composition": text }
}

pub fn invoke_document(target: &InvokeTarget, sets: &[Set]) -> Document {
    let mut d = match target {
        InvokeTarget::Name(n) => doc! { "name": n },
        InvokeTarget::Composition(c) => doc! { "composition": c },
    };
    let sets: Vec<Bson> = sets
        .iter()
        .map(|s| {
            let items: Vec<Bson> = s
                .items
                .iter()
                .map(|i| {
                    Bson::Document(doc! {
                        "identifier": &i.identifier,
                        "key": i.key,
                        "data": binary(&i.data),
                    })
                })
                .collect();
            Bson::Document(doc! { "items": items })
        })
        .collect();
    d.insert("sets", sets);
    d
}

// ---------------------------------------------------------------------------------------------
// Decoding

pub fn decode_invoke_response(bytes: &[u8]) -> Result<InvokeResponse> {
    let d = Document::from_reader(bytes).context("response is not a valid BSON document")?;
    let sets = match d.get("sets") {
        None | Some(Bson::Null) => Vec::new(),
        Some(Bson::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(si, s)| decode_set(s).with_context(|| format!("in response set {si}")))
            .collect::<Result<_>>()?,
        Some(other) => bail!("response field `sets` is a {:?}, not an array", other.element_type()),
    };
    let timestamps = match d.get("timestamps") {
        None | Some(Bson::Null) => None,
        Some(Bson::String(s)) => Some(s.clone()),
        Some(other) => bail!("response field `timestamps` is a {:?}, not a string", other.element_type()),
    };
    Ok(InvokeResponse { sets, timestamps })
}

fn decode_set(b: &Bson) -> Result<Set> {
    let d = b.as_document().ok_or_else(|| anyhow!("set is not a document"))?;
    let items = match d.get("items") {
        None | Some(Bson::Null) => Vec::new(),
        Some(Bson::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(ii, i)| decode_item(i).with_context(|| format!("item {ii}")))
            .collect::<Result<_>>()?,
        Some(_) => bail!("`items` is not an array"),
    };
    Ok(Set { items })
}

fn decode_item(b: &Bson) -> Result<Item> {
    let d = b.as_document().ok_or_else(|| anyhow!("item is not a document"))?;
    let identifier = match d.get("identifier") {
        Some(Bson::String(s)) => s.clone(),
        None | Some(Bson::Null) => String::new(),
        Some(_) => bail!("`identifier` is not a string"),
    };
    let key = match d.get("key") {
        Some(Bson::Int32(k)) => i64::from(*k),
        Some(Bson::Int64(k)) => *k,
        None | Some(Bson::Null) => 0,
        Some(_) => bail!("`key` is not an integer"),
    };
    let data = match d.get("data") {
        Some(Bson::Binary(b)) => b.bytes.clone(),
        Some(Bson::String(s)) => s.clone().into_bytes(),
        None | Some(Bson::Null) => Vec::new(),
        Some(_) => bail!("`data` is not binary"),
    };
    Ok(Item { identifier, key, data })
}

// ---------------------------------------------------------------------------------------------
// Transport

pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
    verbose: bool,
}

impl Client {
    pub fn new(settings: &Settings) -> Result<Self> {
        reqwest::Url::parse(&settings.url)
            .with_context(|| format!("invalid server URL `{}`", settings.url))?;
        let connect_timeout = settings
            .timeout
            .map_or(Duration::from_secs(10), |t| t.min(Duration::from_secs(10)));
        let http = reqwest::blocking::Client::builder()
            // reqwest's blocking client defaults to 30s; invocations can take longer, so only
            // apply a total timeout when one is configured.
            .timeout(settings.timeout)
            .connect_timeout(connect_timeout)
            .user_agent(concat!("dandelion-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot create HTTP client")?;
        Ok(Client {
            http,
            base_url: settings.url.trim_end_matches('/').to_string(),
            verbose: settings.verbose,
        })
    }

    pub fn url_for(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// POSTs a BSON document and returns the response body of a successful request.
    pub fn post_bson(&self, path: &str, doc: &Document) -> Result<Vec<u8>> {
        let body = doc.to_vec().context("cannot encode request as BSON")?;
        let sent = body.len();
        let req = self
            .http
            .post(self.url_for(path))
            .header(reqwest::header::CONTENT_TYPE, BSON_CONTENT_TYPE)
            .body(body);
        let (status, bytes, _) = self.send(req, "POST", path, sent)?;
        check_status(status, &bytes)?;
        Ok(bytes)
    }

    /// GETs a path and returns the status and the latency. The status is not checked.
    pub fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Vec<u8>, Duration)> {
        let req = self.http.get(self.url_for(path));
        self.send(req, "GET", path, 0)
    }

    fn send(
        &self,
        req: reqwest::blocking::RequestBuilder,
        method: &str,
        path: &str,
        sent: usize,
    ) -> Result<(reqwest::StatusCode, Vec<u8>, Duration)> {
        let url = self.url_for(path);
        let start = Instant::now();
        let resp = req.send().map_err(|e| connection_error(&url, e))?;
        let status = resp.status();
        let bytes = resp.bytes().map_err(|e| connection_error(&url, e))?.to_vec();
        let elapsed = start.elapsed();
        if self.verbose {
            eprintln!(
                "{method} {url} -> {status} in {} (sent {}, received {})",
                crate::output::format_duration(elapsed),
                crate::output::format_size(sent as u64),
                crate::output::format_size(bytes.len() as u64),
            );
        }
        Ok((status, bytes, elapsed))
    }
}

fn connection_error(url: &str, e: reqwest::Error) -> anyhow::Error {
    let what = if e.is_timeout() {
        "request timed out"
    } else if e.is_connect() {
        "cannot connect"
    } else {
        "request failed"
    };
    // Include the root cause; reqwest's top-level message alone is rarely informative.
    let mut cause = String::new();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
    while let Some(s) = src {
        cause = s.to_string();
        src = s.source();
    }
    let msg = if cause.is_empty() { format!("{what}: {url}") } else { format!("{what}: {url}: {cause}") };
    anyhow::Error::new(ConnectionError(msg))
}

pub fn check_status(status: reqwest::StatusCode, body: &[u8]) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    Err(anyhow::Error::new(ApiError {
        status: status.as_u16(),
        body: String::from_utf8_lossy(body).into_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_document_shape() {
        let reg = FunctionRegistration {
            name: "f".into(),
            context_size: 1024,
            engine_type: "Kvm".into(),
            local_path: String::new(),
            binary: vec![1, 2, 3],
            input_sets: vec![
                InputSet { name: "a".into(), static_items: None },
                InputSet { name: "b".into(), static_items: Some(vec![("p".into(), vec![9])]) },
            ],
            output_sets: vec!["out".into()],
            min_set_bytes: vec![],
        };
        let d = reg.to_document().unwrap();
        let keys: Vec<&str> = d.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            ["name", "context_size", "engine_type", "local_path", "binary", "input_sets", "output_sets", "min_set_bytes"]
        );
        assert_eq!(d.get_i64("context_size").unwrap(), 1024);
        let sets = d.get_array("input_sets").unwrap();
        assert_eq!(sets[0], Bson::Array(vec!["a".into(), Bson::Null]));
        assert_eq!(
            sets[1],
            Bson::Array(vec!["b".into(), Bson::Array(vec![Bson::Array(vec!["p".into(), binary(&[9])])])])
        );
    }

    #[test]
    fn decode_response_with_i32_keys_and_timestamps() {
        let d = doc! {
            "sets": [
                { "items": [ { "identifier": "x", "key": 3_i32, "data": binary(b"hi") } ] },
                { "items": [] },
            ],
            "timestamps": "{\"start\":1}",
        };
        let r = decode_invoke_response(&d.to_vec().unwrap()).unwrap();
        assert_eq!(r.sets.len(), 2);
        assert_eq!(r.sets[0].items[0], Item { identifier: "x".into(), key: 3, data: b"hi".to_vec() });
        assert!(r.sets[1].items.is_empty());
        assert_eq!(r.timestamps.as_deref(), Some("{\"start\":1}"));
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_invoke_response(b"Hello, World\n").is_err());
    }
}
