//! 純粋なパス変換(接続不要)。

use std::error::Error;

/// `projects/{project_id}/databases/(default)/documents` を返す。
///
/// Firestore の reference をローカルの相対パスへ変換するときは、まず
/// この resource root が現在のクライアントと一致することを確認する。
pub(super) fn document_root(project_id: &str) -> String {
    format!("projects/{project_id}/databases/(default)/documents")
}

/// `projects/{project_id}/databases/(default)/documents/{path}` を組み立てる。
pub(super) fn document_name(project_id: &str, path: &str) -> String {
    format!("{}/{path}", document_root(project_id))
}

pub(super) fn parent_path(project_id: &str, relative_parent: Option<&str>) -> String {
    let root = document_root(project_id);
    match relative_parent {
        Some(parent) if !parent.is_empty() => format!("{root}/{parent}"),
        _ => root,
    }
}

pub(super) fn relative_document_name(name: &str) -> Result<String, Box<dyn Error>> {
    let (_, rel) = name
        .split_once("/documents/")
        .ok_or("invalid document name")?;
    Ok(rel.to_string())
}

pub(super) fn split_collection_path(path: &str) -> Result<(Option<String>, &str), Box<dyn Error>> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() || parts.len().is_multiple_of(2) {
        return Err(format!("invalid collection path: {path}").into());
    }
    let collection_id = parts[parts.len() - 1];
    let parent = if parts.len() == 1 {
        None
    } else {
        Some(parts[..parts.len() - 1].join("/"))
    };
    Ok((parent, collection_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_collection_path_supports_subcollections() {
        let (parent, collection_id) = split_collection_path("teams/t1/members").unwrap();
        assert_eq!(parent.as_deref(), Some("teams/t1"));
        assert_eq!(collection_id, "members");
    }
}
