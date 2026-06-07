use crate::store::RocksStorage;
use std::{path::Path, sync::Arc};

pub fn open_rocks_store<P: AsRef<Path>>(path: Option<P>) -> Result<Arc<RocksStorage>, Box<dyn std::error::Error>> {
    match path {
        Some(pth) => Ok(Arc::new(RocksStorage::open(pth)?)),
        None => {
            let dir = tempfile::tempdir()?;
            Ok(Arc::new(RocksStorage::open(dir.path())?))
        }
    }
}
