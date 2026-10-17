use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::runtime::expect_string;
use crate::value::{validate_value_limits, Value};

use super::io::BoundedWriter;

fn map_csv_error(e: csv::Error) -> String {
    match e.kind() {
        csv::ErrorKind::Io(io_err) => io_err.to_string(),
        _ => format!("CSV.generate error: {e}"),
    }
}

pub fn builtin_json_parse(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let text = expect_string(args.first())?;
    let json_val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("JSON.parse error: {e}"))?;
    let mut nodes = 0usize;
    let val = json_to_value(
        &json_val,
        0,
        rt.state.limits.max_value_depth,
        &mut nodes,
        rt.state.limits.max_value_nodes,
    )?;
    Ok(ExecFlow::Value(val))
}

pub fn builtin_json_generate(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected value argument")?;
    let pretty = if let Some(p) = kwargs.get("pretty") {
        p.truthy()
    } else if let Some(p) = args.get(1) {
        p.truthy()
    } else {
        false
    };
    let json_val = value_to_json(val)?;
    let max_bytes = rt.state.limits.max_file_io_bytes;
    let mut writer = BoundedWriter::new(Vec::new(), max_bytes);
    if pretty {
        serde_json::to_writer_pretty(&mut writer, &json_val).map_err(|e| {
            if e.is_io() {
                e.to_string()
            } else {
                format!("JSON.generate error: {e}")
            }
        })?;
    } else {
        serde_json::to_writer(&mut writer, &json_val).map_err(|e| {
            if e.is_io() {
                e.to_string()
            } else {
                format!("JSON.generate error: {e}")
            }
        })?;
    };
    let bytes = writer.into_inner();
    let text = String::from_utf8(bytes).map_err(|e| format!("JSON.generate error: {e}"))?;
    let result = Value::Str(text);
    validate_value_limits(
        &result,
        rt.state.limits.max_value_depth,
        rt.state.limits.max_value_nodes,
    )?;
    Ok(ExecFlow::Value(result))
}

pub fn builtin_csv_parse(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let text = expect_string(args.first())?;
    let headers_enabled = if let Some(h) = kwargs.get("headers") {
        h.truthy()
    } else if let Some(h) = args.get(1) {
        h.truthy()
    } else {
        false
    };

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(headers_enabled)
        .from_reader(text.as_bytes());

    let max_nodes = rt.state.limits.max_value_nodes;
    let mut total_nodes: usize = 1;

    let val = if headers_enabled {
        let raw_headers = reader
            .headers()
            .map_err(|e| format!("CSV.parse headers error: {e}"))?;
        let mut headers = Vec::new();
        for h in raw_headers.iter() {
            total_nodes = total_nodes.saturating_add(1);
            if let Some(limit) = max_nodes {
                if total_nodes > limit {
                    return Err(
                        format!("resource limit exceeded: max value nodes ({limit})").into(),
                    );
                }
            }
            headers.push(h.to_string());
        }
        let mut rows = Vec::new();
        for record_res in reader.records() {
            let record = record_res.map_err(|e| format!("CSV.parse record error: {e}"))?;
            total_nodes = total_nodes.saturating_add(1);
            let mut map = BTreeMap::new();
            for (i, header) in headers.iter().enumerate() {
                let field_str = record.get(i).unwrap_or("");
                total_nodes = total_nodes.saturating_add(1);
                if let Some(limit) = max_nodes {
                    if total_nodes > limit {
                        return Err(
                            format!("resource limit exceeded: max value nodes ({limit})").into(),
                        );
                    }
                }
                map.insert(header.clone(), Value::Str(field_str.to_string()));
            }
            if let Some(limit) = max_nodes {
                if total_nodes > limit {
                    return Err(
                        format!("resource limit exceeded: max value nodes ({limit})").into(),
                    );
                }
            }
            rows.push(Value::Map(map));
        }
        Value::Array(rows)
    } else {
        let mut rows = Vec::new();
        for record_res in reader.records() {
            let record = record_res.map_err(|e| format!("CSV.parse record error: {e}"))?;
            total_nodes = total_nodes.saturating_add(1);
            let mut row = Vec::new();
            for f in record.iter() {
                total_nodes = total_nodes.saturating_add(1);
                if let Some(limit) = max_nodes {
                    if total_nodes > limit {
                        return Err(
                            format!("resource limit exceeded: max value nodes ({limit})").into(),
                        );
                    }
                }
                row.push(Value::Str(f.to_string()));
            }
            if let Some(limit) = max_nodes {
                if total_nodes > limit {
                    return Err(
                        format!("resource limit exceeded: max value nodes ({limit})").into(),
                    );
                }
            }
            rows.push(Value::Array(row));
        }
        Value::Array(rows)
    };

    validate_value_limits(
        &val,
        rt.state.limits.max_value_depth,
        rt.state.limits.max_value_nodes,
    )?;
    Ok(ExecFlow::Value(val))
}

pub fn builtin_csv_generate(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected array argument")?;
    let Value::Array(rows) = val else {
        return Err("CSV.generate expects an array".into());
    };
    let max_bytes = rt.state.limits.max_file_io_bytes;
    let mut wtr = csv::Writer::from_writer(BoundedWriter::new(Vec::new(), max_bytes));

    if !rows.is_empty() {
        if let Value::Map(_) = &rows[0] {
            let mut headers = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for row in rows {
                let Value::Map(map) = row else {
                    return Err(
                        "CSV.generate with map rows requires all elements to be maps".into(),
                    );
                };
                for k in map.keys() {
                    if seen.insert(k.clone()) {
                        headers.push(k.clone());
                    }
                }
            }
            wtr.write_record(&headers).map_err(map_csv_error)?;
            for row in rows {
                let Value::Map(map) = row else { unreachable!() };
                let record: Vec<String> = headers
                    .iter()
                    .map(|h| match map.get(h) {
                        Some(Value::Str(s)) => s.clone(),
                        Some(Value::Nil) | None => String::new(),
                        Some(other) => other.to_string(),
                    })
                    .collect();
                wtr.write_record(&record).map_err(map_csv_error)?;
            }
        } else if let Value::Array(_) = &rows[0] {
            for row in rows {
                let Value::Array(cols) = row else {
                    return Err(
                        "CSV.generate with array rows requires all elements to be arrays".into(),
                    );
                };
                let record: Vec<String> = cols
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => s.clone(),
                        Value::Nil => String::new(),
                        other => other.to_string(),
                    })
                    .collect();
                wtr.write_record(&record).map_err(map_csv_error)?;
            }
        } else {
            return Err("CSV.generate expects an array of maps or array of arrays".into());
        }
    }

    let bounded = wtr.into_inner().map_err(|e| e.error().to_string())?;
    let bytes = bounded.into_inner();
    let text = String::from_utf8(bytes).map_err(|e| format!("CSV.generate error: {e}"))?;
    let result = Value::Str(text);
    validate_value_limits(
        &result,
        rt.state.limits.max_value_depth,
        rt.state.limits.max_value_nodes,
    )?;
    Ok(ExecFlow::Value(result))
}

pub fn builtin_yaml_parse(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let text = expect_string(args.first())?;
    let yaml_val: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("YAML.parse error: {e}"))?;
    let mut nodes = 0usize;
    let val = yaml_to_value(
        &yaml_val,
        0,
        rt.state.limits.max_value_depth,
        &mut nodes,
        rt.state.limits.max_value_nodes,
    )?;
    Ok(ExecFlow::Value(val))
}

pub fn builtin_yaml_generate(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let val = args.first().ok_or("expected value argument")?;
    let yaml_val = value_to_yaml(val)?;
    let max_bytes = rt.state.limits.max_file_io_bytes;
    let mut writer = BoundedWriter::new(Vec::new(), max_bytes);
    serde_yaml::to_writer(&mut writer, &yaml_val)
        .map_err(|e| format!("YAML.generate error: {e}"))?;
    let bytes = writer.into_inner();
    let text = String::from_utf8(bytes).map_err(|e| format!("YAML.generate error: {e}"))?;
    let result = Value::Str(text);
    validate_value_limits(
        &result,
        rt.state.limits.max_value_depth,
        rt.state.limits.max_value_nodes,
    )?;
    Ok(ExecFlow::Value(result))
}

fn json_to_value(
    val: &serde_json::Value,
    depth: usize,
    max_depth: Option<usize>,
    nodes: &mut usize,
    max_nodes: Option<usize>,
) -> Result<Value, Box<dyn Error>> {
    if let Some(limit) = max_depth {
        if depth > limit {
            return Err(format!("resource limit exceeded: max value depth ({limit})").into());
        }
    }
    *nodes = nodes.saturating_add(1);
    if let Some(limit) = max_nodes {
        if *nodes > limit {
            return Err(format!("resource limit exceeded: max value nodes ({limit})").into());
        }
    }
    Ok(match val {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                if u <= i64::MAX as u64 {
                    Value::Int(u as i64)
                } else {
                    Value::Opaque(n.to_string())
                }
            } else {
                Value::Opaque(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => {
            let next_depth = depth.saturating_add(1);
            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                items.push(json_to_value(
                    item, next_depth, max_depth, nodes, max_nodes,
                )?);
            }
            Value::Array(items)
        }
        serde_json::Value::Object(obj) => {
            let next_depth = depth.saturating_add(1);
            let mut map = BTreeMap::new();
            for (k, v) in obj {
                map.insert(
                    k.clone(),
                    json_to_value(v, next_depth, max_depth, nodes, max_nodes)?,
                );
            }
            Value::Map(map)
        }
    })
}

fn value_to_json(val: &Value) -> Result<serde_json::Value, Box<dyn Error>> {
    Ok(match val {
        Value::Nil => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(b) => {
            let nums = b
                .iter()
                .map(|&byte| serde_json::Value::Number((byte as i64).into()))
                .collect();
            serde_json::Value::Array(nums)
        }
        Value::Array(arr) | Value::ArrayUnion(arr) | Value::ArrayRemove(arr) => {
            let items = arr
                .iter()
                .map(value_to_json)
                .collect::<Result<Vec<_>, _>>()?;
            serde_json::Value::Array(items)
        }
        Value::Map(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                obj.insert(k.clone(), value_to_json(v)?);
            }
            serde_json::Value::Object(obj)
        }
        Value::Timestamp(ts) => match ts.to_rfc3339() {
            Ok(rfc) => serde_json::Value::String(rfc),
            Err(_) => serde_json::Value::Number(ts.seconds.into()),
        },
        Value::Opaque(s) => {
            if let Ok(i) = s.parse::<i64>() {
                serde_json::Value::Number(i.into())
            } else if let Ok(n) = s.parse::<serde_json::Number>() {
                serde_json::Value::Number(n)
            } else {
                serde_json::Value::String(s.clone())
            }
        }
        Value::DocRef(d) => serde_json::Value::String(d.path.clone()),
        Value::CollectionRef(c) => serde_json::Value::String(c.path.clone()),
        Value::CollectionGroupRef(cg) => serde_json::Value::String(cg.id.clone()),
        Value::AuthSession(session) => value_to_json(&session.user_value())?,
        Value::RedactedToken(tok) => serde_json::Value::String(tok.to_string()),
        other => return Err(format!("cannot convert {other:?} to JSON").into()),
    })
}

fn yaml_to_value(
    val: &serde_yaml::Value,
    depth: usize,
    max_depth: Option<usize>,
    nodes: &mut usize,
    max_nodes: Option<usize>,
) -> Result<Value, Box<dyn Error>> {
    if let Some(limit) = max_depth {
        if depth > limit {
            return Err(format!("resource limit exceeded: max value depth ({limit})").into());
        }
    }
    *nodes = nodes.saturating_add(1);
    if let Some(limit) = max_nodes {
        if *nodes > limit {
            return Err(format!("resource limit exceeded: max value nodes ({limit})").into());
        }
    }
    Ok(match val {
        serde_yaml::Value::Null => Value::Nil,
        serde_yaml::Value::Bool(b) => Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                if u <= i64::MAX as u64 {
                    Value::Int(u as i64)
                } else {
                    Value::Opaque(n.to_string())
                }
            } else {
                Value::Opaque(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => Value::Str(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            let next_depth = depth.saturating_add(1);
            let mut items = Vec::with_capacity(seq.len());
            for item in seq {
                items.push(yaml_to_value(
                    item, next_depth, max_depth, nodes, max_nodes,
                )?);
            }
            Value::Array(items)
        }
        serde_yaml::Value::Mapping(map) => {
            let next_depth = depth.saturating_add(1);
            let mut m = BTreeMap::new();
            for (k, v) in map {
                let key_str = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    other => format!("{other:?}"),
                };
                m.insert(
                    key_str,
                    yaml_to_value(v, next_depth, max_depth, nodes, max_nodes)?,
                );
            }
            Value::Map(m)
        }
        serde_yaml::Value::Tagged(tagged) => {
            yaml_to_value(&tagged.value, depth, max_depth, nodes, max_nodes)?
        }
    })
}

fn value_to_yaml(val: &Value) -> Result<serde_yaml::Value, Box<dyn Error>> {
    Ok(match val {
        Value::Nil => serde_yaml::Value::Null,
        Value::Bool(b) => serde_yaml::Value::Bool(*b),
        Value::Int(i) => serde_yaml::Value::Number((*i).into()),
        Value::Str(s) => serde_yaml::Value::String(s.clone()),
        Value::Bytes(b) => {
            let nums = b
                .iter()
                .map(|&byte| serde_yaml::Value::Number((byte as i64).into()))
                .collect();
            serde_yaml::Value::Sequence(nums)
        }
        Value::Array(arr) | Value::ArrayUnion(arr) | Value::ArrayRemove(arr) => {
            let items = arr
                .iter()
                .map(value_to_yaml)
                .collect::<Result<Vec<_>, _>>()?;
            serde_yaml::Value::Sequence(items)
        }
        Value::Map(m) => {
            let mut map = serde_yaml::Mapping::new();
            for (k, v) in m {
                map.insert(serde_yaml::Value::String(k.clone()), value_to_yaml(v)?);
            }
            serde_yaml::Value::Mapping(map)
        }
        Value::Timestamp(ts) => match ts.to_rfc3339() {
            Ok(rfc) => serde_yaml::Value::String(rfc),
            Err(_) => serde_yaml::Value::Number(ts.seconds.into()),
        },
        Value::Opaque(s) => {
            if let Ok(i) = s.parse::<i64>() {
                serde_yaml::Value::Number(i.into())
            } else if let Ok(n) = s.parse::<f64>() {
                serde_yaml::Value::Number(n.into())
            } else {
                serde_yaml::Value::String(s.clone())
            }
        }
        Value::DocRef(d) => serde_yaml::Value::String(d.path.clone()),
        Value::CollectionRef(c) => serde_yaml::Value::String(c.path.clone()),
        Value::CollectionGroupRef(cg) => serde_yaml::Value::String(cg.id.clone()),
        Value::AuthSession(session) => value_to_yaml(&session.user_value())?,
        Value::RedactedToken(tok) => serde_yaml::Value::String(tok.to_string()),
        other => return Err(format!("cannot convert {other:?} to YAML").into()),
    })
}
