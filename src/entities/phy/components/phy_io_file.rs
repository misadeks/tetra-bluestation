use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhyIoFileMode {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhyIoFileEofBehavior {
    Stop,
    Repeat,
}

#[derive(Debug)]
pub enum PhyIoError {
    Io(String),
    Eof,
}

impl From<io::Error> for PhyIoError {
    fn from(err: io::Error) -> Self {
        PhyIoError::Io(err.to_string())
    }
}

pub struct PhyIoFile {
    file: File,
    mode: PhyIoFileMode,
    eof_behavior: PhyIoFileEofBehavior,
    file_size: u64,
}

impl PhyIoFile {
    /// Create a new PhyIoFile instance
    /// 
    /// # Arguments
    /// * `filename` - Path to the file
    /// * `mode` - Read or Write mode
    /// * `eof_behavior` - Behavior when EOF is reached (only relevant for Read mode)
    pub fn new<P: AsRef<Path>>(filename: P, mode: PhyIoFileMode, eof_behavior: PhyIoFileEofBehavior) -> io::Result<Self> {
        let file = match mode {
            PhyIoFileMode::Read => {
                OpenOptions::new()
                    .read(true)
                    .open(&filename)?
            }
            PhyIoFileMode::Write => {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&filename)?
            }
        };

        let file_size = if mode == PhyIoFileMode::Read {
            file.metadata()?.len()
        } else {
            0
        };

        Ok(Self {
            file,
            mode,
            eof_behavior,
            file_size,
        })
    }

    /// Read a block of data from the file
    /// 
    /// # Arguments
    /// * `buffer` - Buffer to read data into (size determines block size)
    /// 
    /// # Returns
    /// * `Ok(())` - Block successfully read
    /// * `Err(PhyIoError::Eof)` - EOF reached and eof_behavior is Stop
    /// * `Err(PhyIoError::Io)` - I/O error occurred
    pub fn read_block(&mut self, buffer: &mut [u8]) -> Result<(), PhyIoError> {
        if self.mode != PhyIoFileMode::Read {
            return Err(PhyIoError::Io("File not opened for reading".to_string()));
        }

        let block_size = buffer.len();
        let mut bytes_read = 0;

        while bytes_read < block_size {
            match self.file.read(&mut buffer[bytes_read..]) {
                Ok(0) => {
                    // EOF reached
                    match self.eof_behavior {
                        PhyIoFileEofBehavior::Stop => {
                            return Err(PhyIoError::Eof);
                        }
                        PhyIoFileEofBehavior::Repeat => {
                            // Seek back to beginning and continue reading
                            self.file.seek(SeekFrom::Start(0))?;
                            
                            // If we had a partial block, it means the file doesn't contain
                            // an integer number of blocks. In this case, discard the partial
                            // block and start fresh from the beginning.
                            if bytes_read > 0 {
                                bytes_read = 0;
                                tracing::debug!("Discarding partial block at EOF, repeating from start");
                            }
                        }
                    }
                }
                Ok(n) => {
                    bytes_read += n;
                }
                Err(e) => {
                    return Err(PhyIoError::from(e));
                }
            }
        }

        Ok(())
    }

    /// Write a block of data to the file
    /// 
    /// # Arguments
    /// * `data` - Data to write
    /// 
    /// # Returns
    /// * `Ok(())` - Block successfully written
    /// * `Err(PhyIoError::Io)` - I/O error occurred or file not opened for writing
    pub fn write_block(&mut self, data: &[u8]) -> Result<(), PhyIoError> {
        if self.mode != PhyIoFileMode::Write {
            return Err(PhyIoError::Io("File not opened for writing".to_string()));
        }

        self.file.write_all(data)?;
        Ok(())
    }

    /// Flush any buffered data to disk
    pub fn flush(&mut self) -> Result<(), PhyIoError> {
        self.file.flush()?;
        Ok(())
    }

    /// Get the current file position
    pub fn position(&mut self) -> io::Result<u64> {
        self.file.stream_position()
    }

    /// Get the file size (only meaningful for read mode)
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Seek to a specific position in the file
    pub fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::env;

    fn create_temp_file(data: &[u8]) -> (String, std::path::PathBuf) {
        let mut path = env::temp_dir();
        let filename = format!("phy_io_test_{}.bin", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos());
        path.push(filename.clone());
        
        let mut file = File::create(&path).unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
        
        (filename, path)
    }

    #[test]
    fn test_write_and_read_block() {
        let mut path = env::temp_dir();
        let filename = format!("phy_io_test_write_{}.bin", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos());
        path.push(&filename);
        
        // Write some data
        {
            let mut writer = PhyIoFile::new(&path, PhyIoFileMode::Write, PhyIoFileEofBehavior::Stop).unwrap();
            let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
            writer.write_block(&data).unwrap();
            writer.flush().unwrap();
        }

        // Read it back
        {
            let mut reader = PhyIoFile::new(&path, PhyIoFileMode::Read, PhyIoFileEofBehavior::Stop).unwrap();
            let mut buffer = [0u8; 8];
            reader.read_block(&mut buffer).unwrap();
            assert_eq!(buffer, [1, 2, 3, 4, 5, 6, 7, 8]);
        }

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_eof_stop_behavior() {
        let (_filename, path) = create_temp_file(&[1u8, 2, 3, 4]);

        let mut reader = PhyIoFile::new(&path, PhyIoFileMode::Read, PhyIoFileEofBehavior::Stop).unwrap();
        let mut buffer = [0u8; 4];
        
        // First read should succeed
        assert!(reader.read_block(&mut buffer).is_ok());
        assert_eq!(buffer, [1, 2, 3, 4]);
        
        // Second read should hit EOF
        assert!(matches!(reader.read_block(&mut buffer), Err(PhyIoError::Eof)));
        
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_eof_loop_behavior() {
        let (_filename, path) = create_temp_file(&[1u8, 2, 3, 4]);

        let mut reader = PhyIoFile::new(&path, PhyIoFileMode::Read, PhyIoFileEofBehavior::Repeat).unwrap();
        let mut buffer = [0u8; 4];
        
        // First read
        assert!(reader.read_block(&mut buffer).is_ok());
        assert_eq!(buffer, [1, 2, 3, 4]);
        
        // Second read should loop back to beginning
        assert!(reader.read_block(&mut buffer).is_ok());
        assert_eq!(buffer, [1, 2, 3, 4]);
        
        // Third read should also work
        assert!(reader.read_block(&mut buffer).is_ok());
        assert_eq!(buffer, [1, 2, 3, 4]);
        
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_partial_block_loop() {
        let (_filename, path) = create_temp_file(&[1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

        let mut reader = PhyIoFile::new(&path, PhyIoFileMode::Read, PhyIoFileEofBehavior::Repeat).unwrap();
        let mut buffer = [0u8; 8];
        
        // First read gets first 8 bytes
        assert!(reader.read_block(&mut buffer).is_ok());
        assert_eq!(buffer, [1, 2, 3, 4, 5, 6, 7, 8]);
        
        // Second read should discard the 2-byte partial block and loop back
        assert!(reader.read_block(&mut buffer).is_ok());
        assert_eq!(buffer, [1, 2, 3, 4, 5, 6, 7, 8]);
        
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
