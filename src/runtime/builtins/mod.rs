pub mod auth;
pub mod codecs;
pub mod core;
pub mod firestore;
pub mod io;
pub mod storage;

use std::collections::BTreeMap;
use std::error::Error;

use crate::ast::Block;
use crate::error::ServiceId;
use crate::runtime::contract::CallContract;
use crate::runtime::evaluator::{Evaluator, ExecFlow};
use crate::value::{BuiltinModule, Value};

pub type Builtin = for<'a> fn(
    &mut Evaluator<'a>,
    Vec<Value>,
    BTreeMap<String, Value>,
    Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>>;

#[derive(Clone, Copy)]
pub struct BuiltinSpec {
    pub module: Option<BuiltinModule>,
    pub method: &'static str,
    pub global_name: &'static str,
    pub contract: CallContract,
    pub required_service: Option<ServiceId>,
    pub handler: Builtin,
}

const fn spec(
    module: Option<BuiltinModule>,
    method: &'static str,
    global_name: &'static str,
    contract: CallContract,
    required_service: Option<ServiceId>,
    handler: Builtin,
) -> BuiltinSpec {
    BuiltinSpec {
        module,
        method,
        global_name,
        contract,
        required_service,
        handler,
    }
}

pub const BUILTIN_SPECS: [BuiltinSpec; 39] = {
    use BuiltinModule::{Auth, CSV, File, Firestore, IO, JSON, Storage, YAML};
    use ServiceId::{
        Auth as AuthService, Firestore as FirestoreService, IO as IOService,
        Storage as StorageService,
    };

    [
        spec(
            None,
            "puts",
            "puts",
            CallContract::any(),
            None,
            core::builtin_puts,
        ),
        spec(
            Some(Firestore),
            "doc",
            "doc",
            CallContract::exact(1),
            None,
            firestore::builtin_doc,
        ),
        spec(
            Some(Firestore),
            "col",
            "col",
            CallContract::exact(1),
            None,
            firestore::builtin_col,
        ),
        spec(
            Some(Firestore),
            "col_group",
            "col_group",
            CallContract::exact(1),
            None,
            firestore::builtin_col_group,
        ),
        spec(
            None,
            "array",
            "array",
            CallContract::any(),
            None,
            core::builtin_array,
        ),
        spec(
            None,
            "len",
            "len",
            CallContract::exact(1),
            None,
            core::builtin_len,
        ),
        spec(
            None,
            "now",
            "now",
            CallContract::exact(0),
            None,
            core::builtin_now,
        ),
        spec(
            None,
            "timestamp",
            "timestamp",
            CallContract::exact(1),
            None,
            core::builtin_timestamp,
        ),
        spec(
            None,
            "bytes",
            "bytes",
            CallContract::exact(1),
            None,
            core::builtin_bytes,
        ),
        spec(
            Some(Firestore),
            "server_timestamp",
            "server_timestamp",
            CallContract::exact(0),
            None,
            core::builtin_server_timestamp,
        ),
        spec(
            Some(Firestore),
            "array_union",
            "array_union",
            CallContract::exact(1),
            None,
            core::builtin_array_union,
        ),
        spec(
            Some(Firestore),
            "array_remove",
            "array_remove",
            CallContract::exact(1),
            None,
            core::builtin_array_remove,
        ),
        spec(
            Some(Firestore),
            "delete_field",
            "delete_field",
            CallContract::exact(0),
            None,
            core::builtin_delete_field,
        ),
        spec(
            Some(Firestore),
            "increment",
            "increment",
            CallContract::exact(1).with_missing_hint("expected int argument"),
            None,
            core::builtin_increment,
        ),
        spec(
            Some(Firestore),
            "get",
            "get",
            CallContract::exact(1),
            Some(FirestoreService),
            firestore::builtin_get,
        ),
        spec(
            Some(Firestore),
            "set",
            "set",
            CallContract::range(2, 3)
                .with_keywords(&["merge"])
                .with_positional_keyword_conflicts(&[("merge", 2)]),
            Some(FirestoreService),
            firestore::builtin_set,
        ),
        spec(
            Some(Firestore),
            "update",
            "update",
            CallContract::exact(2),
            Some(FirestoreService),
            firestore::builtin_update,
        ),
        spec(
            Some(Firestore),
            "delete",
            "delete",
            CallContract::exact(1),
            Some(FirestoreService),
            firestore::builtin_delete,
        ),
        spec(
            Some(Firestore),
            "batch",
            "batch",
            CallContract::exact(0).requiring_block(),
            Some(FirestoreService),
            firestore::builtin_batch,
        ),
        spec(
            Some(Firestore),
            "transaction",
            "transaction",
            CallContract::exact(0).requiring_block(),
            Some(FirestoreService),
            firestore::builtin_transaction,
        ),
        spec(
            Some(Storage),
            "upload",
            "storage_upload",
            CallContract::range(2, 3)
                .with_keywords(&["content_type"])
                .with_positional_keyword_conflicts(&[("content_type", 2)]),
            Some(StorageService),
            storage::builtin_storage_upload,
        ),
        spec(
            Some(Storage),
            "download",
            "storage_download",
            CallContract::exact(1),
            Some(StorageService),
            storage::builtin_storage_download,
        ),
        spec(
            Some(Storage),
            "metadata",
            "storage_metadata",
            CallContract::exact(1),
            Some(StorageService),
            storage::builtin_storage_metadata,
        ),
        spec(
            Some(Storage),
            "delete",
            "storage_delete",
            CallContract::exact(1),
            Some(StorageService),
            storage::builtin_storage_delete,
        ),
        spec(
            Some(Auth),
            "sign_up",
            "auth_sign_up",
            CallContract::exact(2),
            Some(AuthService),
            auth::builtin_auth_sign_up,
        ),
        spec(
            Some(Auth),
            "sign_in",
            "auth_sign_in",
            CallContract::exact(2),
            Some(AuthService),
            auth::builtin_auth_sign_in,
        ),
        spec(
            Some(Auth),
            "current_user",
            "auth_current_user",
            CallContract::exact(0),
            Some(AuthService),
            auth::builtin_auth_current_user,
        ),
        spec(
            Some(Auth),
            "sign_out",
            "auth_sign_out",
            CallContract::exact(0),
            Some(AuthService),
            auth::builtin_auth_sign_out,
        ),
        spec(
            Some(IO),
            "File",
            "io_file",
            CallContract::exact(0),
            None,
            io::builtin_io_file,
        ),
        spec(
            Some(IO),
            "read",
            "io_read",
            CallContract::exact(1),
            Some(IOService),
            io::builtin_file_read,
        ),
        spec(
            Some(IO),
            "write",
            "io_write",
            CallContract::exact(2),
            Some(IOService),
            io::builtin_file_write,
        ),
        spec(
            Some(File),
            "read",
            "file_read",
            CallContract::exact(1),
            Some(IOService),
            io::builtin_file_read,
        ),
        spec(
            Some(File),
            "write",
            "file_write",
            CallContract::exact(2),
            Some(IOService),
            io::builtin_file_write,
        ),
        spec(
            Some(JSON),
            "parse",
            "json_parse",
            CallContract::exact(1),
            None,
            codecs::builtin_json_parse,
        ),
        spec(
            Some(JSON),
            "generate",
            "json_generate",
            CallContract::range(1, 2)
                .with_keywords(&["pretty"])
                .with_positional_keyword_conflicts(&[("pretty", 1)]),
            None,
            codecs::builtin_json_generate,
        ),
        spec(
            Some(CSV),
            "parse",
            "csv_parse",
            CallContract::range(1, 2)
                .with_keywords(&["headers"])
                .with_positional_keyword_conflicts(&[("headers", 1)]),
            None,
            codecs::builtin_csv_parse,
        ),
        spec(
            Some(CSV),
            "generate",
            "csv_generate",
            CallContract::exact(1),
            None,
            codecs::builtin_csv_generate,
        ),
        spec(
            Some(YAML),
            "parse",
            "yaml_parse",
            CallContract::exact(1),
            None,
            codecs::builtin_yaml_parse,
        ),
        spec(
            Some(YAML),
            "generate",
            "yaml_generate",
            CallContract::exact(1),
            None,
            codecs::builtin_yaml_generate,
        ),
    ]
};

pub fn builtin_specs() -> &'static [BuiltinSpec] {
    &BUILTIN_SPECS
}

pub fn lookup_global_builtin(name: &str) -> Option<BuiltinSpec> {
    builtin_specs()
        .iter()
        .copied()
        .find(|spec| spec.global_name == name)
}

pub fn lookup_module_builtin(module: BuiltinModule, method: &str) -> Option<BuiltinSpec> {
    builtin_specs()
        .iter()
        .copied()
        .find(|spec| spec.module == Some(module) && spec.method == method)
}

pub fn is_builtin_name(name: &str) -> bool {
    lookup_global_builtin(name).is_some()
}
