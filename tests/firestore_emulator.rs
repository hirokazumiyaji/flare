use std::collections::BTreeMap;
use std::error::Error;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use firelang::run_script_with_firestore;
use firelang::runtime::{Firestore, QueryFilter, Value};

struct FirestoreEmulator {
    host: String,
    project_id: String,
}

impl FirestoreEmulator {
    fn from_env() -> Option<Self> {
        let host = std::env::var("FIRESTORE_EMULATOR_HOST").ok()?;
        let project_id =
            std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
        Some(Self { host, project_id })
    }

    fn document_url(&self, path: &str) -> String {
        format!(
            "http://{}/v1/projects/{}/databases/(default)/documents/{}",
            self.host, self.project_id, path
        )
    }

    fn curl_json(
        &self,
        method: &str,
        url: &str,
        body: Option<&str>,
    ) -> Result<(u16, String), Box<dyn Error>> {
        let mut cmd = Command::new("curl");
        cmd.arg("-sS")
            .arg("-X")
            .arg(method)
            .arg(url)
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-w")
            .arg("\n%{http_code}");

        if let Some(body) = body {
            cmd.arg("-d").arg(body);
        }

        let output = cmd.output()?;
        if !output.status.success() {
            return Err(format!("curl failed: status={}", output.status).into());
        }

        let stdout = String::from_utf8(output.stdout)?;
        let (body, status) = stdout
            .rsplit_once('\n')
            .ok_or("failed to parse curl response")?;
        let status: u16 = status.trim().parse()?;
        Ok((status, body.to_string()))
    }
}

impl Firestore for FirestoreEmulator {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let (status, body) = self.curl_json("GET", &self.document_url(path), None)?;
        match status {
            200 => {
                if let Some(v) = extract_json_string_value(&body, "stringValue") {
                    let mut map = BTreeMap::new();
                    map.insert("value".to_string(), Value::Str(v));
                    Ok(Some(Value::Map(map)))
                } else {
                    Ok(None)
                }
            }
            404 => Ok(None),
            _ => Err(format!("unexpected GET status {status}: {body}").into()),
        }
    }

    fn set(
        &mut self,
        path: &str,
        value: BTreeMap<String, Value>,
        _merge: bool,
    ) -> Result<(), Box<dyn Error>> {
        let encoded = encode_fields(value)?;
        let body = format!("{{\"fields\":{encoded}}}");
        let (status, body) = self.curl_json("PATCH", &self.document_url(path), Some(&body))?;
        match status {
            200 => Ok(()),
            _ => Err(format!("unexpected PATCH status {status}: {body}").into()),
        }
    }

    fn update(&mut self, path: &str, value: BTreeMap<String, Value>) -> Result<(), Box<dyn Error>> {
        self.set(path, value, true)
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        let (status, body) = self.curl_json("DELETE", &self.document_url(path), None)?;
        match status {
            200 | 404 => Ok(()),
            _ => Err(format!("unexpected DELETE status {status}: {body}").into()),
        }
    }

    fn query_collection(
        &self,
        _path: &str,
        _filters: &[QueryFilter],
        _limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        Err("query_collection is not implemented for emulator adapter".into())
    }

    fn query_collection_group(
        &self,
        _id: &str,
        _filters: &[QueryFilter],
        _limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        Err("query_collection_group is not implemented for emulator adapter".into())
    }
}

#[test]
fn firestore_global_functions_with_emulator() {
    let Some(firestore) = FirestoreEmulator::from_env() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let doc_path = format!("users/user_{suffix}");

    let script = format!("set(doc(\"{doc_path}\"), {{value: \"ok\"}})\nget(\"{doc_path}\")");

    let out = run_script_with_firestore(&script, firestore).unwrap();
    assert!(format!("{out:?}").contains("ok"));
}

fn encode_fields(map: BTreeMap<String, Value>) -> Result<String, Box<dyn Error>> {
    let mut parts = vec![];
    for (k, v) in map {
        let encoded = match v {
            Value::Str(s) => format!("{{\"stringValue\":\"{}\"}}", escape_json_string(&s)),
            Value::Int(i) => format!("{{\"integerValue\":\"{i}\"}}"),
            Value::Bool(b) => format!("{{\"booleanValue\":{b}}}"),
            Value::Timestamp(ts) => format!("{{\"integerValue\":\"{}\"}}", ts.seconds),
            Value::ServerTimestamp => {
                return Err(
                    "server_timestamp sentinel is not implemented in emulator adapter".into(),
                );
            }
            Value::Nil => "{\"nullValue\":null}".to_string(),
            _ => return Err("unsupported field type for emulator write".into()),
        };
        parts.push(format!("\"{}\":{}", escape_json_string(&k), encoded));
    }
    Ok(format!("{{{}}}", parts.join(",")))
}

fn extract_json_string_value(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let (_, rest) = body.split_once(&needle)?;
    let (value, _) = rest.split_once('"')?;
    Some(value.to_string())
}

fn escape_json_string(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}
