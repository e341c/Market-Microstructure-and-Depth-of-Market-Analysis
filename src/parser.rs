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
        T: FromPair
        // F: FnMut(u64, Vec<(String, String)>, Vec<(String, String)>)
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
