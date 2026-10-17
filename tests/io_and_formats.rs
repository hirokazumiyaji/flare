use std::fs;
use std::path::PathBuf;

use flare::{
    run_script, run_script_with_services, run_script_with_services_and_limits, CapabilityError,
    InMemoryIo, ProjectConfig, RuntimeLimits, ServiceId, Services, Value,
};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("flare_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn file_path(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    fn path_str(&self, rel: &str) -> String {
        self.file_path(rel).to_str().unwrap().to_string()
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn file_read_and_write_via_all_aliases() {
    let temp = TempDirGuard::new("aliases");
    let file1 = temp.path_str("io_file.txt");
    let file2 = temp.path_str("file_direct.txt");
    let file3 = temp.path_str("io_direct.txt");

    let script = format!(
        r#"
IO.File.write("{file1}", "hello from IO.File")
content1 = IO.File.read("{file1}")

File.write("{file2}", "hello from File")
content2 = File.read("{file2}")

IO.write("{file3}", "hello from IO")
content3 = IO.read("{file3}")

[content1, content2, content3]
"#
    );

    let res = run_script(&script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![
            Value::Str("hello from IO.File".into()),
            Value::Str("hello from File".into()),
            Value::Str("hello from IO".into()),
        ])
    );
}

#[test]
fn file_write_creates_nested_directories_and_overwrites() {
    let temp = TempDirGuard::new("nested_dir");
    let file = temp.path_str("a/b/c/nested.txt");

    let script = format!(
        r#"
File.write("{file}", "first content")
first = File.read("{file}")
File.write("{file}", "overwritten content")
second = File.read("{file}")
[first, second]
"#
    );

    let res = run_script(&script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![
            Value::Str("first content".into()),
            Value::Str("overwritten content".into()),
        ])
    );
}

#[test]
fn file_read_and_write_binary_bytes() {
    let temp = TempDirGuard::new("binary");
    let file = temp.path_str("binary.bin");

    let script = format!(
        r#"
bytes_written = File.write("{file}", bytes([0, 255, 128, 65]))
read_bytes = File.read("{file}")
[bytes_written, read_bytes]
"#
    );

    let res = run_script(&script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![Value::Int(4), Value::Bytes(vec![0, 255, 128, 65]),])
    );
}

#[test]
fn unavailable_io_mode_denies_file_access() {
    let temp = TempDirGuard::new("unavailable");
    let file = temp.path_str("test.txt");

    let services = Services::empty(ProjectConfig::default());
    let err = run_script_with_services(&format!("File.read(\"{file}\")"), services).unwrap_err();
    let cap = err.downcast::<CapabilityError>().unwrap();
    assert_eq!(cap.service, ServiceId::IO);

    let services = Services::empty(ProjectConfig::default());
    let err = run_script_with_services(&format!("File.write(\"{file}\", \"data\")"), services)
        .unwrap_err();
    let cap = err.downcast::<CapabilityError>().unwrap();
    assert_eq!(cap.service, ServiceId::IO);

    let services = Services::empty(ProjectConfig::default());
    let err = run_script_with_services(&format!("IO.File.read(\"{file}\")"), services).unwrap_err();
    let cap = err.downcast::<CapabilityError>().unwrap();
    assert_eq!(cap.service, ServiceId::IO);
}

#[test]
fn in_memory_io_supports_custom_mock_filesystem() {
    let mem_io = InMemoryIo::new();
    mem_io.insert("/mock/data.json", r#"{"status": "ok"}"#);

    let services = Services::in_memory().with_io(mem_io.clone());
    let script = r#"
content = File.read("/mock/data.json")
parsed = JSON.parse(content)
File.write("/mock/output.txt", "result: " + parsed["status"])
"#;
    run_script_with_services(script, services).unwrap();

    let output = mem_io.get("/mock/output.txt").unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "result: ok");
}

#[test]
fn max_file_io_bytes_budget_enforced() {
    let temp = TempDirGuard::new("budget");
    let file = temp.path_str("oversized.txt");
    fs::write(&file, "123456789012345").unwrap(); // 15 bytes

    let limits = RuntimeLimits::default().with_max_file_io_bytes(10); // 10 byte limit

    let err = run_script_with_services_and_limits(
        &format!("File.read(\"{file}\")"),
        Services::in_memory(),
        limits,
    )
    .unwrap_err();
    assert!(err.to_string().contains("max file io bytes (10)"));

    let err = run_script_with_services_and_limits(
        &format!("File.write(\"{file}\", \"12345678901\")"),
        Services::in_memory(),
        limits,
    )
    .unwrap_err();
    assert!(err.to_string().contains("max file io bytes (10)"));

    // Direct trait call tests
    let mut std_io = flare::StdIo;
    use flare::Io;
    let err = std_io
        .write(std::path::Path::new(&file), b"12345678901", Some(10))
        .unwrap_err();
    assert!(err.to_string().contains("max file io bytes (10)"));
}

#[test]
fn json_parse_and_generate_round_trip() {
    let script = r#"
json_str = "{\"name\": \"Alice\", \"age\": 30, \"active\": true, \"tags\": [\"admin\", \"dev\"], \"meta\": null}"
obj = JSON.parse(json_str)

generated = JSON.generate(obj)
re_parsed = JSON.parse(generated)

[
  obj["name"],
  obj["age"],
  obj["active"],
  obj["tags"],
  obj["meta"],
  re_parsed["name"],
  re_parsed["age"]
]
"#;

    let res = run_script(script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![
            Value::Str("Alice".into()),
            Value::Int(30),
            Value::Bool(true),
            Value::Array(vec![Value::Str("admin".into()), Value::Str("dev".into())]),
            Value::Nil,
            Value::Str("Alice".into()),
            Value::Int(30),
        ])
    );
}

#[test]
fn json_generate_pretty_option() {
    let script = r#"
data = {b: 2, a: 1}
compact = JSON.generate(data)
pretty = JSON.generate(data, pretty: true)
[compact, pretty]
"#;
    let res = run_script(script).unwrap();
    let Value::Array(items) = res else { panic!() };
    let Value::Str(compact_str) = &items[0] else {
        panic!()
    };
    let Value::Str(pretty_str) = &items[1] else {
        panic!()
    };

    assert!(!compact_str.contains('\n'));
    assert!(pretty_str.contains('\n'));
    assert!(pretty_str.contains("  \"a\": 1"));
}

#[test]
fn csv_parse_with_and_without_headers() {
    let script = r#"
csv_text = "id,name,role\n1,Alice,Engineer\n2,Bob,Designer\n"

with_headers = CSV.parse(csv_text, headers: true)
without_headers = CSV.parse(csv_text)

[
  with_headers.len(),
  with_headers[0]["name"],
  with_headers[0]["role"],
  with_headers[1]["name"],
  without_headers.len(),
  without_headers[0][0],
  without_headers[1][1]
]
"#;

    let res = run_script(script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![
            Value::Int(2),
            Value::Str("Alice".into()),
            Value::Str("Engineer".into()),
            Value::Str("Bob".into()),
            Value::Int(3), // header row + 2 data rows
            Value::Str("id".into()),
            Value::Str("Alice".into()),
        ])
    );
}

#[test]
fn csv_generate_from_maps_and_arrays() {
    let script = r#"
maps = [
  {id: 1, name: "Alice"},
  {id: 2, name: "Bob"}
]
csv_from_maps = CSV.generate(maps)

arrays = [
  ["x", "y"],
  [10, 20]
]
csv_from_arrays = CSV.generate(arrays)

[csv_from_maps, csv_from_arrays]
"#;

    let res = run_script(script).unwrap();
    let Value::Array(items) = res else { panic!() };
    let Value::Str(maps_csv) = &items[0] else {
        panic!()
    };
    let Value::Str(arrays_csv) = &items[1] else {
        panic!()
    };

    assert!(maps_csv.contains("id,name") || maps_csv.contains("name,id"));
    assert!(maps_csv.contains("Alice"));
    assert!(maps_csv.contains("Bob"));

    assert_eq!(arrays_csv, "x,y\n10,20\n");
}

#[test]
fn yaml_parse_and_generate() {
    let script = r#"
yaml_text = "
name: Alice
age: 30
skills:
  - rust
  - flare
active: true
"
data = YAML.parse(yaml_text)
generated = YAML.generate(data)
re_parsed = YAML.parse(generated)

[
  data["name"],
  data["age"],
  data["skills"],
  data["active"],
  re_parsed["name"]
]
"#;

    let res = run_script(script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![
            Value::Str("Alice".into()),
            Value::Int(30),
            Value::Array(vec![Value::Str("rust".into()), Value::Str("flare".into())]),
            Value::Bool(true),
            Value::Str("Alice".into()),
        ])
    );
}

#[test]
fn issue_164_usage_scenarios_import_and_export() {
    let temp = TempDirGuard::new("issue_164");
    let users_json_file = temp.path_str("users.json");
    let items_csv_file = temp.path_str("items.csv");
    let backup_json_file = temp.path_str("active_users.json");

    fs::write(
        &users_json_file,
        r#"[{"id": "u1", "name": "Alice"}, {"id": "u2", "name": "Bob"}]"#,
    )
    .unwrap();

    fs::write(
        &items_csv_file,
        "item_id,title,price\ni1,Book,1200\ni2,Pen,300\n",
    )
    .unwrap();

    let script = format!(
        r#"
# Scenario A: Import / Seed
content = IO.File.read("{users_json_file}")
users = JSON.parse(content)

Firestore.batch {{
  users.each {{ |user|
    Firestore.set("users/" + user["id"], user)
  }}
}}

csv_text = File.read("{items_csv_file}")
records = CSV.parse(csv_text, headers: true)
records.each {{ |item|
  Firestore.set("items/" + item["item_id"], item)
}}

# Scenario B: Export / Backup
rows = [
  Firestore.get("users/u1"),
  Firestore.get("users/u2")
]
IO.File.write("{backup_json_file}", JSON.generate(rows))

# Verification
backup_content = File.read("{backup_json_file}")
backup_rows = JSON.parse(backup_content)

[
  Firestore.get("users/u1")["name"],
  Firestore.get("items/i1")["title"],
  backup_rows.len(),
  backup_rows[0]["name"]
]
"#
    );

    let res = run_script(&script).unwrap();
    assert_eq!(
        res,
        Value::Array(vec![
            Value::Str("Alice".into()),
            Value::Str("Book".into()),
            Value::Int(2),
            Value::Str("Alice".into()),
        ])
    );
}

#[test]
fn data_codecs_respect_resource_limits() {
    let limits = RuntimeLimits::default().with_max_file_io_bytes(20);

    // CSV generate exceeding 20 bytes
    let script = r#"
data = [
  {"name": "very long person name exceeding twenty bytes", "age": 30}
]
CSV.generate(data)
"#;
    let err =
        run_script_with_services_and_limits(script, Services::in_memory(), limits).unwrap_err();
    assert!(err.to_string().contains("max file io bytes (20)"));

    // JSON generate exceeding 20 bytes
    let script_json = r#"
data = {"message": "this is definitely longer than twenty bytes"}
JSON.generate(data)
"#;
    let err = run_script_with_services_and_limits(script_json, Services::in_memory(), limits)
        .unwrap_err();
    assert!(err.to_string().contains("max file io bytes (20)"));

    // YAML generate exceeding 20 bytes
    let script_yaml = r#"
data = {"message": "this is definitely longer than twenty bytes"}
YAML.generate(data)
"#;
    let err = run_script_with_services_and_limits(script_yaml, Services::in_memory(), limits)
        .unwrap_err();
    assert!(err.to_string().contains("max file io bytes (20)"));

    // CSV parse exceeding max_value_nodes
    let node_limits = RuntimeLimits::default().with_max_value_nodes(5);
    let csv_text = "a,b,c\n1,2,3\n4,5,6";
    let script_csv_parse = format!("CSV.parse(\"{csv_text}\", headers: true)");
    let err =
        run_script_with_services_and_limits(&script_csv_parse, Services::in_memory(), node_limits)
            .unwrap_err();
    assert!(err.to_string().contains("max value nodes (5)"));

    // CSV parse with only headers exceeding max_value_nodes (no data rows)
    let header_node_limits = RuntimeLimits::default().with_max_value_nodes(3);
    let csv_headers_only = "col1,col2,col3,col4,col5\n";
    let script_headers = format!("CSV.parse(\"{csv_headers_only}\", headers: true)");
    let err = run_script_with_services_and_limits(
        &script_headers,
        Services::in_memory(),
        header_node_limits,
    )
    .unwrap_err();
    assert!(err.to_string().contains("max value nodes (3)"));

    // JSON parse exceeding max_value_depth during decoding
    let depth_limits = RuntimeLimits::default().with_max_value_depth(2);
    let json_nested = "[[[1]]]";
    let script_json_depth = format!("JSON.parse(\"{json_nested}\")");
    let err = run_script_with_services_and_limits(
        &script_json_depth,
        Services::in_memory(),
        depth_limits,
    )
    .unwrap_err();
    assert!(err.to_string().contains("max value depth (2)"));

    // YAML parse exceeding max_value_depth during decoding
    let yaml_nested = "a:\\n  b:\\n    c: 1";
    let script_yaml_depth = format!("YAML.parse(\"{yaml_nested}\")");
    let err = run_script_with_services_and_limits(
        &script_yaml_depth,
        Services::in_memory(),
        depth_limits,
    )
    .unwrap_err();
    assert!(err.to_string().contains("max value depth (2)"));

    // CSV generate with empty array returns empty string under limits
    let script_empty_csv = "CSV.generate([])";
    let empty_res =
        run_script_with_services_and_limits(script_empty_csv, Services::in_memory(), limits)
            .unwrap();
    assert_eq!(empty_res, Value::Str(String::new()));
}
