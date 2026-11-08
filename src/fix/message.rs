//! FIX 4.4 wire encoding / framing / parsing.

pub const SOH: u8 = 0x01;

pub type Field = (u32, String);
pub type Msg = Vec<Field>;

fn push_field(body: &mut Vec<u8>, tag: u32, val: &str) {
    body.extend_from_slice(tag.to_string().as_bytes());
    body.push(b'=');
    body.extend_from_slice(val.as_bytes());
    body.push(SOH);
}

/// Builds a full message: header, body, BodyLength (9) and CheckSum (10).
pub fn encode(
    msg_type: &str,
    sender: &str,
    target: &str,
    seq: u64,
    time: &str,
    fields: &[Field],
) -> Vec<u8> {
    let mut body = Vec::new();
    push_field(&mut body, 35, msg_type);
    push_field(&mut body, 49, sender);
    push_field(&mut body, 56, target);
    push_field(&mut body, 34, &seq.to_string());
    push_field(&mut body, 52, time);
    for (t, v) in fields {
        push_field(&mut body, *t, v);
    }

    let mut msg = format!("8=FIX.4.4\x019={}\x01", body.len()).into_bytes();
    msg.extend_from_slice(&body);
    let sum = msg.iter().fold(0u32, |a, &b| a + b as u32) % 256;
    msg.extend_from_slice(format!("10={:03}\x01", sum).as_bytes());
    msg
}

/// Removes one complete message from the front of `buf`, if present.
pub fn extract(buf: &mut Vec<u8>) -> Option<Msg> {
    let pos = buf.windows(4).position(|w| w == b"\x0110=")?;
    let end = pos + 8; // SOH + "10=" + 3 digits + SOH
    if buf.len() < end {
        return None;
    }
    let raw: Vec<u8> = buf.drain(..end).collect();
    Some(
        raw.split(|&b| b == SOH)
            .filter(|p| !p.is_empty())
            .filter_map(|p| {
                let s = std::str::from_utf8(p).ok()?;
                let (t, v) = s.split_once('=')?;
                Some((t.parse().ok()?, v.to_string()))
            })
            .collect(),
    )
}

/// First value for `tag`.
pub fn get(msg: &[Field], tag: u32) -> Option<&str> {
    msg.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_str())
}
