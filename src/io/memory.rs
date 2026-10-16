use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::Io;

#[derive(Debug, Clone, Default)]
pub struct InMemoryIo {
    files: Rc<RefCell<BTreeMap<PathBuf, Vec<u8>>>>,
}

impl InMemoryIo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, path: impl AsRef<Path>, content: impl AsRef<[u8]>) {
        self.files
            .borrow_mut()
            .insert(path.as_ref().to_path_buf(), content.as_ref().to_vec());
    }

    pub fn get(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        self.files.borrow().get(path.as_ref()).cloned()
    }
}

impl Io for InMemoryIo {
    fn is_available(&self) -> bool {
        true
    }

    fn read(&self, path: &Path, max_bytes: Option<usize>) -> Result<Vec<u8>, Box<dyn Error>> {
        let files = self.files.borrow();
        let content = files.get(path).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{}: No such file or directory", path.display()),
            )
        })?;
        if let Some(limit) = max_bytes {
            if content.len() > limit {
                return Err(format!("resource limit exceeded: max file io bytes ({limit})").into());
            }
        }
        Ok(content.clone())
    }

    fn write(
        &mut self,
        path: &Path,
        content: &[u8],
        max_bytes: Option<usize>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(limit) = max_bytes {
            if content.len() > limit {
                return Err(format!("resource limit exceeded: max file io bytes ({limit})").into());
            }
        }
        self.files
            .borrow_mut()
            .insert(path.to_path_buf(), content.to_vec());
        Ok(())
    }
}
