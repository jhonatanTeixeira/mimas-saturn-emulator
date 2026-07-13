#[derive(Default)]
pub struct Smpc {
    pub command_buffer: Vec<u8>,
}

impl Smpc {
    pub fn new() -> Self {
        Self {
            command_buffer: Vec::new(),
        }
    }

    pub fn write_command(&mut self, cmd: u8) -> Result<(), String> {
        if self.command_buffer.len() >= 8 {
            return Err("SMPC command buffer overflow".to_string());
        }
        self.command_buffer.push(cmd);
        Ok(())
    }

    pub fn execute_command(&mut self) -> u8 {
        self.command_buffer.clear();
        0x55 // SMPC status response
    }
}
