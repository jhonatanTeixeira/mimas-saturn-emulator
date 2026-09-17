with open("saturn-core/src/cs2.rs", "r") as f:
    text = f.read()

text = text.replace('''        if start_pos_type != 0 && self.disc.is_some() {
            // Track start
            let track_num = (start_fad & 0xFF) as u8;
            start_fad = self
                .disc
                .as_ref()
                .unwrap()
                .track_to_fad(track_num)
                .unwrap_or(150);
        }''', '''        if start_pos_type != 0 {
            if let Some(disc) = &self.disc {
                let track_num = (start_fad & 0xFF) as u8;
                start_fad = disc.track_to_fad(track_num).unwrap_or(150);
            }
        }''')

text = text.replace('''        if end_fad == 0 && self.disc.is_some() {
            end_fad = self.disc.as_ref().unwrap().lead_out_fad;
        }''', '''        if end_fad == 0 {
            if let Some(disc) = &self.disc {
                end_fad = disc.lead_out_fad;
            }
        }''')

text = text.replace('''        if pos_type != 0 && self.disc.is_some() {
            let track_num = (target_fad & 0xFF) as u8;
            target_fad = self
                .disc
                .as_ref()
                .unwrap()
                .track_to_fad(track_num)
                .unwrap_or(150);
        }''', '''        if pos_type != 0 {
            if let Some(disc) = &self.disc {
                let track_num = (target_fad & 0xFF) as u8;
                target_fad = disc.track_to_fad(track_num).unwrap_or(150);
            }
        }''')

with open("saturn-core/src/cs2.rs", "w") as f:
    f.write(text)
