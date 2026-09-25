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

fn parse_resource_name(name: &str) -> Result<(String, String, Vec<&str>), Box<dyn Error>> {
    let mut segments = name.split('/');
    if segments.next() != Some("projects")
        || segments.next().is_none_or(str::is_empty)
        || segments.next() != Some("databases")
        || segments.next().is_none_or(str::is_empty)
        || segments.next() != Some("documents")
    {
        return Err(format!("invalid document name: {name}").into());
    }
    let project = name
        .split('/')
        .nth(1)
        .ok_or_else(|| format!("invalid document name: {name}"))?;
    let database = name
        .split('/')
        .nth(3)
        .ok_or_else(|| format!("invalid document name: {name}"))?;
    let relative: Vec<&str> = segments.collect();
    if relative.is_empty()
        || relative.iter().any(|segment| segment.is_empty())
        || !relative.len().is_multiple_of(2)
    {
        return Err(format!("invalid document name: {name}").into());
    }
    Ok((project.to_string(), database.to_string(), relative))
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
