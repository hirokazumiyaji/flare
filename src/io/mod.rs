use std::error::Error;
use std::path::Path;

mod fs;
mod memory;
mod unavailable;

pub use fs::StdIo;
pub use memory::InMemoryIo;
pub use unavailable::{UnavailableIO, UnavailableIo};

pub trait Io {
    fn is_available(&self) -> bool {
        true
    }

    fn read(&self, path: &Path, max_bytes: Option<usize>) -> Result<Vec<u8>, Box<dyn Error>>;

    fn write(
        &mut self,
        path: &Path,
        content: &[u8],
        max_bytes: Option<usize>,
    ) -> Result<(), Box<dyn Error>>;
}
