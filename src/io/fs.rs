use std::error::Error;
use std::io::Read;
use std::path::Path;

use super::Io;

#[derive(Debug, Clone, Default)]
pub struct StdIo;

impl Io for StdIo {
    fn is_available(&self) -> bool {
        true
    }

    fn read(&self, path: &Path, max_bytes: Option<usize>) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut file = std::fs::File::open(path)?;
        if let Ok(metadata) = file.metadata() {
            if let Some(limit) = max_bytes {
                if metadata.len() > limit as u64 {
                    return Err(
                        format!("resource limit exceeded: max file io bytes ({limit})").into(),
                    );
                }
            }
        }
        let mut buf = Vec::new();
        match max_bytes {
            Some(limit) => {
                let mut take = file.take(limit as u64 + 1);
                take.read_to_end(&mut buf)?;
                if buf.len() > limit {
                    return Err(
                        format!("resource limit exceeded: max file io bytes ({limit})").into(),
                    );
                }
            }
            None => {
                file.read_to_end(&mut buf)?;
            }
        }
        Ok(buf)
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
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, content)?;
        Ok(())
    }
}
