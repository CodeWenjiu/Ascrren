//! 设备自描述 SFR 表解析。
//!
//! 固件在虚拟 SFR 地址 `0x0100` 提供 256 字节描述表，格式为：
//! `name\0 unit\0 family\0 data...`。
//! `family` 的高 3 位是类型，低 5 位是长度。

use crate::error::Error;

/// 一个 SFR 数据项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfrEntry {
    pub name: String,
    pub unit: String,
    pub family: u8,
    pub kind: SfrKind,
}

/// SFR 数据项的编码类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfrKind {
    /// u8 数据，地址为 16 位。
    U8 { address: u16, len: u8 },
    /// u16 数据，地址为 8 位。
    U16 { address: u8, len: u8 },
    /// u32 数据，地址为 16 位。
    U32 { address: u16, len: u8 },
    /// 内联字符串。
    String { data: Vec<u8> },
    /// 内联 u8 数组。
    Array { data: Vec<u8> },
}

impl SfrKind {
    pub fn description(&self) -> String {
        match self {
            Self::U8 { address, len } => format!("u8[{len}] @0x{address:04X}"),
            Self::U16 { address, len } => format!("u16[{len}] @0x{address:02X}"),
            Self::U32 { address, len } => format!("u32[{len}] @0x{address:04X}"),
            Self::String { data } => format!("string[{}]", data.len()),
            Self::Array { data } => format!("u8-array[{}]", data.len()),
        }
    }
}

/// 从设备读取到的 SFR 值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfrValue {
    U16(u16),
    Bytes(Vec<u8>),
    Text(String),
}

/// 解析后的 SFR 描述表。
#[derive(Debug, Clone, Default)]
pub struct SfrTable {
    pub entries: Vec<SfrEntry>,
}

impl SfrTable {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut cursor = 0usize;
        let mut entries = Vec::new();

        while cursor < bytes.len() {
            let name = read_cstr(bytes, &mut cursor)?;
            if name.is_empty() {
                break;
            }
            let unit = read_cstr(bytes, &mut cursor)?;
            let family = read_cstr(bytes, &mut cursor)?;
            let family_byte = *family
                .first()
                .ok_or_else(|| Error::SfrParse("empty family field".to_owned()))?;
            let len = family_byte % 32;

            let kind = match family_byte / 32 {
                0 => {
                    let address = read_be_u16(bytes, &mut cursor)?;
                    SfrKind::U8 { address, len }
                }
                1 => {
                    let address = read_be_u8(bytes, &mut cursor)?;
                    SfrKind::U16 { address, len }
                }
                2 => {
                    let address = read_be_u16(bytes, &mut cursor)?;
                    SfrKind::U32 { address, len }
                }
                3 => {
                    let data = read_bytes(bytes, &mut cursor, usize::from(len))?.to_vec();
                    SfrKind::String { data }
                }
                4 => {
                    let data = read_bytes(bytes, &mut cursor, usize::from(len))?.to_vec();
                    SfrKind::Array { data }
                }
                other => {
                    return Err(Error::SfrParse(format!(
                        "unsupported family type {other} (byte 0x{family_byte:02X})"
                    )));
                }
            };

            entries.push(SfrEntry {
                name: String::from_utf8_lossy(name).into_owned(),
                unit: String::from_utf8_lossy(unit).into_owned(),
                family: family_byte,
                kind,
            });
        }

        Ok(Self { entries })
    }

    pub fn get(&self, name: &str) -> Option<&SfrEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

fn read_cstr<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], Error> {
    let start = *cursor;
    while *cursor < bytes.len() && bytes[*cursor] != 0 {
        *cursor += 1;
    }
    if *cursor >= bytes.len() {
        return Err(Error::SfrParse("unterminated string field".to_owned()));
    }
    let value = &bytes[start..*cursor];
    *cursor += 1;
    Ok(value)
}

fn read_bytes<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], Error> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| Error::SfrParse("data length overflow".to_owned()))?;
    if end > bytes.len() {
        return Err(Error::SfrParse(format!(
            "truncated data: need {len} bytes at offset {cursor}"
        )));
    }
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn read_be_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, Error> {
    Ok(read_bytes(bytes, cursor, 1)?[0])
}

fn read_be_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, Error> {
    let value = read_bytes(bytes, cursor, 2)?;
    Ok(u16::from_be_bytes([value[0], value[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stock_table() {
        let raw = b"Flash_Info\0 \0!\0\x02MSN_UID\0 \0\x1e\0\x00\x00MSN_Status\0 \0\x01\0\x00\x1f";
        let table = SfrTable::parse(raw).unwrap();

        assert_eq!(table.entries.len(), 3);
        assert_eq!(table.entries[0].name, "Flash_Info");
        assert_eq!(table.entries[0].unit, " ");
        assert_eq!(
            table.entries[0].kind,
            SfrKind::U16 {
                address: 0x02,
                len: 1
            }
        );
        assert_eq!(table.entries[1].name, "MSN_UID");
        assert_eq!(
            table.entries[1].kind,
            SfrKind::U8 {
                address: 0x0000,
                len: 30
            }
        );
        assert_eq!(table.entries[2].name, "MSN_Status");
        assert_eq!(
            table.entries[2].kind,
            SfrKind::U8 {
                address: 0x001F,
                len: 1
            }
        );
    }

    #[test]
    fn parses_inline_data() {
        let raw = b"A\0 \0\x63\0ABCB\0 \0\x83\0XYZ\0";
        let table = SfrTable::parse(raw).unwrap();
        assert_eq!(
            table.entries[0].kind,
            SfrKind::String {
                data: b"ABC".to_vec()
            }
        );
        assert_eq!(
            table.entries[1].kind,
            SfrKind::Array {
                data: b"XYZ".to_vec()
            }
        );
    }

    #[test]
    fn rejects_truncated_data() {
        assert!(SfrTable::parse(b"A\0 \0!\0").is_err());
    }
}
