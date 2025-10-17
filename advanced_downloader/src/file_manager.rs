use std::io::SeekFrom;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};


#[derive(Debug)]
pub struct FileManager {
    file: File,
}

impl FileManager {
    
    pub async fn new(path: &str, total_size: u64) -> Result<Self, std::io::Error> {
   
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .read(true)
            .open(path)
            .await?;
        file.set_len(total_size).await?;

        Ok(FileManager { file })
    }

    pub async fn write_chunk(&mut self, offset: u64, data: &[u8]) -> Result<(), std::io::Error> {
 
        self.file.seek(SeekFrom::Start(offset)).await?;
        
        self.file.write_all(data).await?;

        Ok(())
    }
}

