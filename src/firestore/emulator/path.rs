//! 純粋なパス変換(接続不要)。

use std::error::Error;

/// `projects/{project_id}/databases/(default)/documents/{path}` を組み立てる。
pub(super) fn document_name(project_id: &str, path: &str) -> String {
    format!("projects/{project_id}/databases/(default)/documents/{path}")
}

pub(super) fn parent_path(project_id: &str, relative_parent: Option<&str>) -> String {
    match relative_parent {
        Some(parent) if !parent.is_empty() => {
            format!("projects/{project_id}/databases/(default)/documents/{parent}")
        }
        _ => format!("projects/{project_id}/databases/(default)/documents"),
    }
}

pub(super) fn relative_document_name(name: &str) -> Result<String, Box<dyn Error>> {
    let (_, _, relative) = parse_resource_name(name)?;
    Ok(relative.join("/"))
}

pub(super) fn relative_document_name_for(
    name: &str,
    project_id: &str,
    database_id: &str,
) -> Result<String, Box<dyn Error>> {
    let (project, database, relative) = parse_resource_name(name)?;
    if project != project_id || database != database_id {
        return Err(format!("document belongs to another project/database: {name}").into());
    }
    Ok(relative.join("/"))
}

/// `projects/{p}/databases/{db}/documents/{relative}` を 1 回だけ走査して分解する。
///
/// `project` / `database` は `name` からの借用で返す(この関数は decode / query の
/// ホットパスで呼ばれるため、`String` の確保を避ける)。
fn parse_resource_name(
    name: &str,
) -> Result<(&str, &str, Vec<&str>), Box<dyn Error>> {
    let invalid = || format!("invalid document name: {name}");
    let mut segments = name.split('/');
    // 先頭 5 セグメントを 1 回のパスで検証しつつ取り出す。
    let Some("projects") = segments.next() else {
        return Err(invalid().into());
    };
    let Some(project) = segments.next().filter(|s| !s.is_empty()) else {
        return Err(invalid().into());
    };
    let Some("databases") = segments.next() else {
        return Err(invalid().into());
    };
    let Some(database) = segments.next().filter(|s| !s.is_empty()) else {
        return Err(invalid().into());
    };
    let Some("documents") = segments.next() else {
        return Err(invalid().into());
    };
    let relative: Vec<&str> = segments.collect();
    if relative.is_empty()
        || relative.iter().any(|segment| segment.is_empty())
        || !relative.len().is_multiple_of(2)
    {
        return Err(invalid().into());
    }
    Ok((project, database, relative))
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

    #[test]
    fn parse_resource_name_borrows_project_and_database() {
        let (project, database, relative) =
            parse_resource_name("projects/p1/databases/(default)/documents/teams/t1").unwrap();
        assert_eq!(project, "p1");
        assert_eq!(database, "(default)");
        assert_eq!(relative, vec!["teams", "t1"]);
    }

    #[test]
    fn parse_resource_name_accepts_nested_collection_documents() {
        // `documents` はパス中に再度現れても正常なサブコレクション。
        let (_, _, relative) =
            parse_resource_name("projects/p1/databases/(default)/documents/teams/t1/documents/x1")
                .unwrap();
        assert_eq!(relative, vec!["teams", "t1", "documents", "x1"]);
    }

    #[test]
    fn parse_resource_name_rejects_malformed_prefixes() {
        for name in [
            "",
            "projects",
            "projects//databases/(default)/documents/a/b",
            "projects/p1/database/(default)/documents/a/b",
            "projects/p1/databases//documents/a/b",
            "projects/p1/databases/(default)",
            "projects/p1/databases/(default)/doc",
        ] {
            assert!(
                parse_resource_name(name).is_err(),
                "expected error for {name:?}"
            );
        }
    }

    #[test]
    fn parse_resource_name_rejects_odd_or_empty_relative_paths() {
        for name in [
            // 空の相対パス
            "projects/p1/databases/(default)/documents",
            // 奇数セグメント(ドキュメントが閉じられていない)
            "projects/p1/databases/(default)/documents/teams",
            // 空セグメント
            "projects/p1/databases/(default)/documents/teams//t1",
        ] {
            assert!(
                parse_resource_name(name).is_err(),
                "expected error for {name:?}"
            );
        }
    }

    #[test]
    fn relative_document_name_for_rejects_foreign_project() {
        let err = relative_document_name_for(
            "projects/other/databases/(default)/documents/a/b",
            "p1",
            "(default)",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("another project/database"),
            "{err}"
        );
    }
}
