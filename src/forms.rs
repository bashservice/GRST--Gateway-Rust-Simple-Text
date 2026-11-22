#[derive(Clone)]
pub struct FormField {
    pub name: String,
    pub value: String,
}

pub struct FormState {
    pub fields: Vec<FormField>,
    pub active_idx: usize,
}

impl FormState {
    pub fn new() -> Self {
        Self {
            fields: vec![
                FormField {
                    name: "login / szukaj".to_string(),
                    value: String::new(),
                },
                FormField {
                    name: "haslo / tekst".to_string(),
                    value: String::new(),
                },
            ],
            active_idx: 0,
        }
    }

    pub fn active_field(&self) -> Option<&FormField> {
        self.fields.get(self.active_idx)
    }

    pub fn set_active_value(&mut self, val: String) {
        if let Some(f) = self.fields.get_mut(self.active_idx) {
            f.value = val;
        }
    }

    pub fn next_field(&mut self) {
        if !self.fields.is_empty() {
            self.active_idx = (self.active_idx + 1) % self.fields.len();
        }
    }

    pub fn summary(&self) -> String {
        self.fields
            .iter()
            .filter(|f| !f.value.is_empty())
            .map(|f| format!("{}: {}", f.name, f.value))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}