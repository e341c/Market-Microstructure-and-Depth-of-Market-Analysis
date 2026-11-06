// Parse from JSON
pub trait FromPair: Sized {
    fn from_pair(first: &str, second: &str) -> Option<Self>;
}

#[derive(Debug)]
pub struct ExtractedData<T> {
    pub id: u64,
    pub primary_list: Vec<T>,
    pub secondary_list: Vec<T>,
}

pub fn parse_segmented_payload<T>(
    json: &str,
    id_key: &str,
    first_section_key: &str,
    second_section_key: &str
) -> Option<ExtractedData<T>>
    where
        T: FromPair // F: FnMut(u64, Vec<(String, String)>, Vec<(String, String)>)
{
    let first_idx = json.find(first_section_key)?;
    let second_idx = json.find(second_section_key)?;

    let id_section = &json[..first_idx];
    let first_section = &json[first_idx..second_idx];
    let second_section = &json[second_idx..];

    let id_token = id_section.split(id_key).nth(1)?;
    let id_value = id_token
        .split(':')
        .nth(1)?
        .trim_matches(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '"')
        .parse::<u64>()
        .ok()?;

    let map_section = |section: &str| -> Vec<T> {
        let mut list = Vec::new();
        if let Some(start) = section.find('[') {
            for pair_str in section[start..].split("],") {
                let clean = pair_str.trim_matches(
                    |c| c == '[' || c == ']' || c == '}' || c == ' '
                );
                if clean.is_empty() {
                    continue;
                }

                let mut parts = clean.split(',');
                if let (Some(f), Some(s)) = (parts.next(), parts.next()) {
                    if let Some(item) = T::from_pair(f.trim_matches('"'), s.trim_matches('"')) {
                        list.push(item);
                    }
                }
            }
        }
        list
    };

    Some(ExtractedData {
        id: id_value,
        primary_list: map_section(first_section),
        secondary_list: map_section(second_section),
    })
}

// Convert to JSON
#[macro_export]
macro_rules! json {
    // 1. Правило для обработки вложенных объектов { key: val, ... }
    ({ $($key:tt: $val:tt),* $(,)? }) => {
        {
            let mut s = String::new();
            s.push('{');
            let mut first = true;
            $(
                if !first {
                    s.push(',');
                }
                first = false;
                
                // Добавляем ключ в кавычках и двоеточие
                s.push_str(&format!("\"{}\":", stringify!($key)));
                
                // Рекурсивно вызываем макрос для обработки значения
                s.push_str(&$crate::json!($val));
            )*
            s.push('}');
            s
        }
    };

    // 2. Правило для обычных выражений (строки, числа, переменные, bool)
    ($val:expr) => {
        // Мы используем специальный внутренний трейт для правильного форматирования типов
        $crate::JsonStringify::stringify_val(&$val)
    };
}

// Вспомогательный трейт для автоматического добавления кавычек строкам
// и сохранения чисел/bool в исходном виде
pub trait JsonStringify {
    fn stringify_val(&self) -> String;
}

// Для строк и ссылок на строки автоматически добавляем экранированные кавычки
impl JsonStringify for &str {
    fn stringify_val(&self) -> String {
        format!("\"{}\"", self)
    }
}
impl JsonStringify for String {
    fn stringify_val(&self) -> String {
        format!("\"{}\"", self)
    }
}

// Для чисел и логических типов выводим "как есть" (без кавычек)
impl JsonStringify for i32 {
    fn stringify_val(&self) -> String {
        self.to_string()
    }
}
impl JsonStringify for u32 {
    fn stringify_val(&self) -> String {
        self.to_string()
    }
}
impl JsonStringify for f64 {
    fn stringify_val(&self) -> String {
        self.to_string()
    }
}
impl JsonStringify for bool {
    fn stringify_val(&self) -> String {
        self.to_string()
    }
}
