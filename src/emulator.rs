//! Firestore REST adapter for the local emulator (`FIRESTORE_EMULATOR_HOST`).
use std::collections::BTreeMap;
use std::error::Error;

use serde_json::{json, Map, Value as Json};
use ureq::Error as UreqError;
use url::Url;

use crate::runtime::{Firestore, QueryFilter, Timestamp, Value, WriteOp};

pub struct FirestoreEmulator {
    host: String,
    project_id: String,
    agent: ureq::Agent,
    mode: Option<EmulatorMode>,
}

enum EmulatorMode {
    Batch(Vec<WriteOp>),
    Transaction {
        transaction_id: String,
        writes: Vec<WriteOp>,
    },
}

impl FirestoreEmulator {
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("FIRESTORE_EMULATOR_HOST").ok()?;
        let project_id =
            std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
        Some(Self::new(host, project_id))
    }

    pub fn new(host: String, project_id: String) -> Self {
        Self {
            host,
            project_id,
            agent: ureq::Agent::new(),
            mode: None,
        }
    }

    fn base(&self) -> String {
        format!(
            "http://{}/v1/projects/{}/databases/(default)",
            self.host, self.project_id
        )
    }

    fn document_url(&self, path: &str) -> String {
        let mut u = Url::parse(&self.base()).expect("valid base URL");
        {
            let mut segs = u.path_segments_mut().expect("base must be path-based");
            segs.push("documents");
            for s in path.split('/') {
                segs.push(s);
            }
        }
        u.to_string()
    }

    fn run_query_url(&self, parent: Option<&str>) -> String {
        let mut u = Url::parse(&self.base()).expect("valid base URL");
        {
            let mut segs = u.path_segments_mut().expect("base must be path-based");
            segs.push("documents");
            if let Some(p) = parent {
                for s in p.split('/') {
                    segs.push(s);
                }
            }
        }
        format!("{u}:runQuery")
    }

    fn commit_url(&self) -> String {
        format!("{}/documents:commit", self.base())
    }

    fn begin_transaction_url(&self) -> String {
        format!("{}/documents:beginTransaction", self.base())
    }

    fn batch_get_url(&self) -> String {
        format!("{}/documents:batchGet", self.base())
    }

    fn post_json(&self, url: &str, body: &Json) -> Result<Json, Box<dyn Error>> {
        let resp = self
            .agent
            .post(url)
            .set("Content-Type", "application/json")
            .send_json(body)?;
        parse_json_body(&resp.into_string()?)
    }

    fn patch_json(&self, url: &str, body: &Json) -> Result<Json, Box<dyn Error>> {
        let resp = self
            .agent
            .request("PATCH", url)
            .set("Content-Type", "application/json")
            .send_json(body)?;
        parse_json_body(&resp.into_string()?)
    }

    fn document_name(&self, path: &str) -> String {
        format!(
            "projects/{}/databases/(default)/documents/{}",
            self.project_id, path
        )
    }

    fn commit_single_write(&self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        let write = build_write(self, &op)?;
        self.post_json(&self.commit_url(), &json!({ "writes": [write] }))?;
        Ok(())
    }
}

fn parse_json_body(s: &str) -> Result<Json, Box<dyn Error>> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(Json::Object(Map::new()));
    }
    Ok(serde_json::from_str(t)?)
}

fn get_document_resource(agent: &ureq::Agent, url: &str) -> Result<Option<Json>, Box<dyn Error>> {
    match agent.get(url).call() {
        Ok(resp) => parse_json_body(&resp.into_string()?).map(Some),
        Err(UreqError::Status(404, _)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl Firestore for FirestoreEmulator {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: get while in batch/transaction".into());
        }
        let url = self.document_url(path);
        let Some(j) = get_document_resource(&self.agent, &url)? else {
            return Ok(None);
        };
        decode_document_for_get(&j).map(Some)
    }

    fn set(
        &mut self,
        path: &str,
        value: BTreeMap<String, Value>,
        merge: bool,
    ) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: set while in batch/transaction".into());
        }
        if value
            .values()
            .any(|v| matches!(v, Value::ArrayUnion(_) | Value::ArrayRemove(_)))
        {
            return self.commit_single_write(WriteOp::Set {
                path: path.to_string(),
                data: value,
                merge,
            });
        }
        apply_set_patch(self, path, value, merge)
    }

    fn update(&mut self, path: &str, value: BTreeMap<String, Value>) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: update while in batch/transaction".into());
        }
        let url = self.document_url(path);
        if get_document_resource(&self.agent, &url)?.is_none() {
            return Err(format!("document not found: {path}").into());
        }
        if value
            .values()
            .any(|v| matches!(v, Value::ArrayUnion(_) | Value::ArrayRemove(_)))
        {
            return self.commit_single_write(WriteOp::Update {
                path: path.to_string(),
                data: value,
            });
        }
        let mask: Vec<String> = value.keys().cloned().collect();
        let patch_url = add_update_mask_query(&url, &mask)?;
        let body = json!({ "fields": encode_fields(&value)? });
        self.patch_json(&patch_url, &body)?;
        Ok(())
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: delete while in batch/transaction".into());
        }
        let url = self.document_url(path);
        match self.agent.delete(&url).call() {
            Ok(resp) => {
                let status = resp.status();
                if status == 200 || status == 404 {
                    Ok(())
                } else {
                    Err(format!("DELETE {} -> {}", url, status).into())
                }
            }
            Err(UreqError::Status(404, _)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn query_collection(
        &self,
        path: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let (parent, collection_id) = match path.rsplit_once('/') {
            Some((p, c)) => (Some(p), c),
            None => (None, path),
        };
        let mut sq = json!({
            "structuredQuery": {
                "from": [{ "collectionId": collection_id }],
            }
        });
        if let Some(obj) = sq["structuredQuery"].as_object_mut() {
            if let Some(w) = build_where(filters)? {
                obj.insert("where".to_string(), w);
            }
        }
        run_query_sorted(self, parent, &sq, limit)
    }

    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let mut sq = json!({
            "structuredQuery": {
                "from": [{ "collectionId": id, "allDescendants": true }],
            }
        });
        if let Some(obj) = sq["structuredQuery"].as_object_mut() {
            if let Some(w) = build_where(filters)? {
                obj.insert("where".to_string(), w);
            }
        }
        run_query_sorted(self, None, &sq, limit)
    }

    fn begin_batch(&mut self) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("already in batch or transaction".into());
        }
        self.mode = Some(EmulatorMode::Batch(vec![]));
        Ok(())
    }

    fn batch_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.mode {
            Some(EmulatorMode::Batch(ops)) => {
                ops.push(op);
                Ok(())
            }
            _ => Err("not in batch".into()),
        }
    }

    fn commit_batch(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(EmulatorMode::Batch(ops)) = self.mode.take() {
            let writes = write_ops_to_json_writes(self, &ops)?;
            let body = json!({ "writes": writes });
            self.post_json(&self.commit_url(), &body)?;
            Ok(())
        } else {
            Err("not in batch".into())
        }
    }

    fn abort_batch(&mut self) {
        if matches!(self.mode, Some(EmulatorMode::Batch(_))) {
            self.mode = None;
        }
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("already in batch or transaction".into());
        }
        let resp = self.post_json(&self.begin_transaction_url(), &json!({}))?;
        let tid = resp["transaction"]
            .as_str()
            .ok_or("beginTransaction missing transaction")?
            .to_string();
        self.mode = Some(EmulatorMode::Transaction {
            transaction_id: tid,
            writes: vec![],
        });
        Ok(())
    }

    fn get_in_transaction(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let tid = match &self.mode {
            Some(EmulatorMode::Transaction { transaction_id, .. }) => transaction_id.clone(),
            _ => return Err("not in transaction".into()),
        };
        let body = json!({
            "documents": [self.document_name(path)],
            "transaction": tid,
        });
        // batchGet returns one entry per requested document: `{ "found": Document }`
        // or `{ "missing": "full/resource/name" }`.
        let resp = self.post_json(&self.batch_get_url(), &body)?;
        let row = resp
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or("batchGet response missing row")?;
        match row.get("found") {
            Some(doc) if !doc.is_null() => Ok(Some(decode_document_for_get(doc)?)),
            _ => Ok(None),
        }
    }

    fn transaction_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.mode {
            Some(EmulatorMode::Transaction { writes, .. }) => {
                writes.push(op);
                Ok(())
            }
            _ => Err("not in transaction".into()),
        }
    }

    fn commit_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(EmulatorMode::Transaction {
            transaction_id,
            writes,
        }) = self.mode.take()
        {
            let json_writes = write_ops_to_json_writes(self, &writes)?;
            let body = json!({
                "transaction": transaction_id,
                "writes": json_writes,
            });
            self.post_json(&self.commit_url(), &body)?;
            Ok(())
        } else {
            Err("not in transaction".into())
        }
    }

    fn abort_transaction(&mut self) {
        if matches!(self.mode, Some(EmulatorMode::Transaction { .. })) {
            self.mode = None;
        }
    }
}

fn run_query_sorted(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    body: &Json,
    limit: Option<usize>,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let resp = fs.post_json(&fs.run_query_url(parent), body)?;
    let rows = resp
        .as_array()
        .ok_or("runQuery response must be a JSON array")?;
    let mut docs: Vec<(String, Value)> = vec![];
    for row in rows {
        if let Some(doc) = row.get("document") {
            let name = doc["name"]
                .as_str()
                .ok_or("runQuery row missing document.name")?;
            let (_, rel) = name
                .split_once("/documents/")
                .ok_or("invalid document name")?;
            let v = decode_document_for_query(doc)?;
            docs.push((rel.to_string(), v));
        }
    }
    docs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<Value> = docs.into_iter().map(|(_, v)| v).collect();
    if let Some(l) = limit {
        out.truncate(l);
    }
    Ok(out)
}

fn build_where(filters: &[QueryFilter]) -> Result<Option<Json>, Box<dyn Error>> {
    if filters.is_empty() {
        return Ok(None);
    }
    let mut cur: Option<Json> = None;
    for f in filters {
        let op = match f.op.as_str() {
            "==" => "EQUAL",
            ">" => "GREATER_THAN",
            ">=" => "GREATER_THAN_OR_EQUAL",
            "<" => "LESS_THAN",
            "<=" => "LESS_THAN_OR_EQUAL",
            o => return Err(format!("unsupported query op: {o}").into()),
        };
        let fv = encode_field_value(&f.value)?;
        let part = json!({
            "fieldFilter": {
                "field": { "fieldPath": f.field },
                "op": op,
                "value": fv,
            }
        });
        cur = Some(match cur {
            None => part,
            Some(prev) => json!({
                "compositeFilter": {
                    "op": "AND",
                    "filters": [ prev, part ],
                }
            }),
        });
    }
    Ok(cur)
}

fn write_ops_to_json_writes(
    fs: &FirestoreEmulator,
    ops: &[WriteOp],
) -> Result<Vec<Json>, Box<dyn Error>> {
    let mut out = vec![];
    for op in ops {
        out.push(build_write(fs, op)?);
    }
    Ok(out)
}

fn build_write(fs: &FirestoreEmulator, op: &WriteOp) -> Result<Json, Box<dyn Error>> {
    match op {
        WriteOp::Set { path, data, merge } => {
            let name = fs.document_name(path);
            let (plain, transforms) = split_fields_and_transforms(data);
            let fields = encode_fields(&plain)?;
            let mut write = json!({
                "update": { "name": name, "fields": fields },
            });
            if *merge {
                let mask: Vec<String> = plain.keys().cloned().collect();
                write["updateMask"] = json!({ "fieldPaths": mask });
            }
            attach_transforms(&mut write, &transforms)?;
            Ok(write)
        }
        WriteOp::Update { path, data } => {
            let name = fs.document_name(path);
            let (plain, transforms) = split_fields_and_transforms(data);
            let fields = encode_fields(&plain)?;
            let mask: Vec<String> = plain.keys().cloned().collect();
            let mut write = json!({
                "update": { "name": name, "fields": fields },
                "updateMask": { "fieldPaths": mask },
                "currentDocument": { "exists": true },
            });
            attach_transforms(&mut write, &transforms)?;
            Ok(write)
        }
        WriteOp::Delete { path } => Ok(json!({ "delete": fs.document_name(path) })),
    }
}

fn split_fields_and_transforms(
    data: &BTreeMap<String, Value>,
) -> (BTreeMap<String, Value>, Vec<(String, Value)>) {
    let mut plain = BTreeMap::new();
    let mut transforms = vec![];
    for (k, v) in data {
        match v {
            Value::ArrayUnion(_) | Value::ArrayRemove(_) => {
                transforms.push((k.clone(), v.clone()));
            }
            _ => {
                plain.insert(k.clone(), v.clone());
            }
        }
    }
    (plain, transforms)
}

fn attach_transforms(
    write: &mut Json,
    transforms: &[(String, Value)],
) -> Result<(), Box<dyn Error>> {
    if transforms.is_empty() {
        return Ok(());
    }
    let mut out = vec![];
    for (path, v) in transforms {
        out.push(encode_field_transform(path, v)?);
    }
    write["updateTransforms"] = Json::Array(out);
    Ok(())
}

fn encode_field_transform(field_path: &str, v: &Value) -> Result<Json, Box<dyn Error>> {
    let values = match v {
        Value::ArrayUnion(items) | Value::ArrayRemove(items) => items
            .iter()
            .map(encode_field_value)
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("not a transform value".into()),
    };
    Ok(match v {
        Value::ArrayUnion(_) => {
            json!({ "fieldPath": field_path, "appendMissingElements": { "values": values } })
        }
        Value::ArrayRemove(_) => {
            json!({ "fieldPath": field_path, "removeAllFromArray": { "values": values } })
        }
        _ => unreachable!(),
    })
}

/// Firestore REST `PATCH` expects `updateMask` as **query** parameters, not JSON body fields.
fn add_update_mask_query(
    document_url: &str,
    field_paths: &[String],
) -> Result<String, Box<dyn Error>> {
    let mut u = Url::parse(document_url)?;
    {
        let mut qp = u.query_pairs_mut();
        for p in field_paths {
            qp.append_pair("updateMask.fieldPaths", p);
        }
    }
    Ok(u.to_string())
}

fn apply_set_patch(
    fs: &mut FirestoreEmulator,
    path: &str,
    value: BTreeMap<String, Value>,
    merge: bool,
) -> Result<(), Box<dyn Error>> {
    let base = fs.document_url(path);
    let fields = encode_fields(&value)?;
    let body = json!({ "fields": fields });
    let url = if merge {
        let mask: Vec<String> = value.keys().cloned().collect();
        add_update_mask_query(&base, &mask)?
    } else {
        base
    };
    fs.patch_json(&url, &body)?;
    Ok(())
}

fn encode_fields(map: &BTreeMap<String, Value>) -> Result<Map<String, Json>, Box<dyn Error>> {
    let mut out = Map::new();
    for (k, v) in map {
        out.insert(k.clone(), encode_field_value(v)?);
    }
    Ok(out)
}

fn encode_field_value(v: &Value) -> Result<Json, Box<dyn Error>> {
    Ok(match v {
        Value::Str(s) => json!({ "stringValue": s }),
        Value::Int(i) => json!({ "integerValue": i.to_string() }),
        Value::Bool(b) => json!({ "booleanValue": *b }),
        Value::Nil => json!({ "nullValue": null }),
        Value::Timestamp(ts) => json!({
            "timestampValue": ts_to_rfc3339(ts.seconds)?
        }),
        Value::ServerTimestamp => json!({
            "timestampValue": ts_to_rfc3339(crate::runtime::current_unix_seconds()?)?
        }),
        Value::Array(items) => {
            let mut vals = vec![];
            for x in items {
                vals.push(encode_field_value(x)?);
            }
            json!({ "arrayValue": { "values": vals } })
        }
        Value::Map(m) => json!({ "mapValue": { "fields": encode_fields(m)? } }),
        _ => return Err(format!("unsupported value in emulator write: {v:?}").into()),
    })
}

fn ts_to_rfc3339(sec: i64) -> Result<String, Box<dyn Error>> {
    Ok(chrono::DateTime::<chrono::Utc>::from_timestamp(sec, 0)
        .ok_or("timestamp out of range")?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn decode_document_for_get(doc: &Json) -> Result<Value, Box<dyn Error>> {
    decode_document_inner(doc, false)
}

fn decode_document_for_query(doc: &Json) -> Result<Value, Box<dyn Error>> {
    decode_document_inner(doc, true)
}

fn decode_document_inner(doc: &Json, include_doc_path: bool) -> Result<Value, Box<dyn Error>> {
    let mut out = BTreeMap::new();
    if let Some(fields) = doc.get("fields").and_then(|f| f.as_object()) {
        for (k, fv) in fields {
            out.insert(k.clone(), decode_field_value(fv)?);
        }
    }
    if include_doc_path {
        if let Some(name) = doc.get("name").and_then(|n| n.as_str()) {
            if let Some((_, rel)) = name.split_once("/documents/") {
                out.insert("__name__".to_string(), Value::Str(rel.to_string()));
            }
        }
    }
    Ok(Value::Map(out))
}

fn decode_field_value(v: &Json) -> Result<Value, Box<dyn Error>> {
    if let Some(s) = v.get("stringValue").and_then(|x| x.as_str()) {
        return Ok(Value::Str(s.to_string()));
    }
    if let Some(n) = v.get("integerValue").and_then(|x| x.as_i64()) {
        return Ok(Value::Int(n));
    }
    if let Some(s) = v.get("integerValue").and_then(|x| x.as_str()) {
        return Ok(Value::Int(s.parse()?));
    }
    if let Some(b) = v.get("booleanValue").and_then(|x| x.as_bool()) {
        return Ok(Value::Bool(b));
    }
    if v.get("nullValue").is_some() {
        return Ok(Value::Nil);
    }
    if let Some(ts) = v.get("timestampValue").and_then(|x| x.as_str()) {
        let sec = chrono::DateTime::parse_from_rfc3339(ts)?.timestamp();
        return Ok(Value::Timestamp(Timestamp { seconds: sec }));
    }
    if let Some(arr) = v
        .get("arrayValue")
        .and_then(|a| a.get("values"))
        .and_then(|x| x.as_array())
    {
        let mut out = vec![];
        for x in arr {
            out.push(decode_field_value(x)?);
        }
        return Ok(Value::Array(out));
    }
    if let Some(fields) = v
        .get("mapValue")
        .and_then(|m| m.get("fields"))
        .and_then(|f| f.as_object())
    {
        let mut out = BTreeMap::new();
        for (k, fv) in fields {
            out.insert(k.clone(), decode_field_value(fv)?);
        }
        return Ok(Value::Map(out));
    }
    Err(format!("unsupported Firestore field value: {v}").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decode_empty_document_for_get_has_no_name_field() {
        let doc = json!({
            "name": "projects/p/databases/(default)/documents/users/x",
            "fields": {}
        });
        let Value::Map(m) = decode_document_for_get(&doc).unwrap() else {
            panic!();
        };
        assert!(!m.contains_key("__name__"));
        assert!(m.is_empty());
    }

    #[test]
    fn decode_document_for_query_adds_name() {
        let doc = json!({
            "name": "projects/p/databases/(default)/documents/users/x",
            "fields": { "a": { "stringValue": "v" } }
        });
        let Value::Map(m) = decode_document_for_query(&doc).unwrap() else {
            panic!();
        };
        assert_eq!(m.get("__name__"), Some(&Value::Str("users/x".to_string())));
    }

    #[test]
    fn batch_get_response_array_with_found_document() {
        let resp = json!([
            {
                "found": {
                    "name": "projects/p/databases/(default)/documents/a/b",
                    "fields": { "n": { "integerValue": "1" } }
                }
            }
        ]);
        let rows = resp.as_array().unwrap();
        let row = &rows[0];
        let doc = row.get("found").unwrap();
        let v = decode_document_for_get(doc).unwrap();
        assert!(format!("{v:?}").contains("Int(1)"));
    }

    #[test]
    fn commit_write_update_includes_exists_precondition() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_json_writes(
            &fs,
            &[WriteOp::Update {
                path: "c/d".to_string(),
                data: BTreeMap::from([("x".to_string(), Value::Int(1))]),
            }],
        )
        .unwrap();
        assert_eq!(writes[0]["currentDocument"]["exists"], json!(true));
    }

    #[test]
    fn update_mask_excludes_transform_keys() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_json_writes(
            &fs,
            &[WriteOp::Update {
                path: "c/d".to_string(),
                data: BTreeMap::from([
                    ("plain".to_string(), Value::Int(1)),
                    (
                        "tags".to_string(),
                        Value::ArrayUnion(vec![Value::Str("x".into())]),
                    ),
                ]),
            }],
        )
        .unwrap();
        let mask = writes[0]["updateMask"]["fieldPaths"].as_array().unwrap();
        assert!(mask.iter().any(|v| v == "plain"));
        assert!(
            !mask.iter().any(|v| v == "tags"),
            "transform keys must not appear in updateMask: {mask:?}"
        );
        assert!(writes[0]["updateTransforms"].is_array());
    }

    #[test]
    fn set_merge_mask_excludes_transform_keys() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_json_writes(
            &fs,
            &[WriteOp::Set {
                path: "c/d".to_string(),
                merge: true,
                data: BTreeMap::from([
                    ("plain".to_string(), Value::Int(1)),
                    (
                        "tags".to_string(),
                        Value::ArrayRemove(vec![Value::Str("x".into())]),
                    ),
                ]),
            }],
        )
        .unwrap();
        let mask = writes[0]["updateMask"]["fieldPaths"].as_array().unwrap();
        assert_eq!(mask.len(), 1);
        assert_eq!(mask[0], "plain");
    }

    #[test]
    fn decode_document_for_query_preserves_documents_in_path() {
        let doc = json!({
            "name": "projects/p/databases/(default)/documents/teams/t1/documents/x",
            "fields": {}
        });
        let Value::Map(m) = decode_document_for_query(&doc).unwrap() else {
            panic!();
        };
        assert_eq!(
            m.get("__name__"),
            Some(&Value::Str("teams/t1/documents/x".to_string()))
        );
    }

    #[test]
    fn document_url_percent_encodes_reserved_chars() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let url = fs.document_url("users/a?b");
        assert!(url.ends_with("/documents/users/a%3Fb"), "got: {url}");
    }

    #[test]
    fn run_query_url_with_subcollection_parent() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let url = fs.run_query_url(Some("teams/t1"));
        assert!(url.ends_with("/documents/teams/t1:runQuery"), "got: {url}");
    }
}
