//! Bounded readers for the original neural style vectors and database.
use crate::{Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::BTreeMap;

#[derive(Debug)]
pub(super) struct Style {
    name: String,
    vector: [f32; 256],
    reference: usize,
}
#[derive(Debug)]
struct Record {
    feature: [f32; 576],
    transferred: BTreeMap<String, [f32; 256]>,
}
#[derive(Debug)]
pub(super) struct Database {
    records: Vec<Record>,
}

fn invalid() -> Error {
    Error::InvalidMedia("invalid underwater neural style resource".into())
}
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.position.checked_add(len).ok_or_else(invalid)?;
        let result = self.bytes.get(self.position..end).ok_or_else(invalid)?;
        self.position = end;
        Ok(result)
    }
    fn integer(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("length checked")) as usize)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.integer()?;
        if n > 4096 {
            return Err(invalid());
        }
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| invalid())
    }
    fn vector<const N: usize>(&mut self) -> Result<[f32; N]> {
        if self.integer()? != N {
            return Err(invalid());
        }
        floats(self.take(N * 4)?)
    }
    fn varint(&mut self) -> Result<usize> {
        let mut value = 0_usize;
        for shift in (0..35).step_by(7) {
            let byte = self.take(1)?[0];
            value |= usize::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(invalid())
    }
}
fn floats<const N: usize>(bytes: &[u8]) -> Result<[f32; N]> {
    if bytes.len() != N * 4 {
        return Err(invalid());
    }
    let mut values = [0.0; N];
    for (value, bytes) in values.iter_mut().zip(bytes.chunks_exact(4)) {
        *value = f32::from_le_bytes(bytes.try_into().expect("four-byte chunk"));
        if !value.is_finite() {
            return Err(invalid());
        }
    }
    Ok(values)
}
impl Style {
    pub(super) fn parse(bytes: &[u8], index: u32) -> Result<Self> {
        if bytes.len() > 32_768 {
            return Err(invalid());
        }
        let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if json["version"].as_u64() != Some(2) {
            return Err(invalid());
        }
        let entries = json["result"].as_array().ok_or_else(invalid)?;
        if entries.len() != 4 {
            return Err(invalid());
        }
        let item = entries
            .iter()
            .find(|e| e["index"].as_u64() == Some(u64::from(index)))
            .ok_or_else(invalid)?;
        let bytes = STANDARD
            .decode(item["data"].as_str().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        if item["data_size"]
            .as_str()
            .and_then(|v| v.parse::<usize>().ok())
            != Some(bytes.len())
        {
            return Err(invalid());
        }
        let mut r = Reader {
            bytes: &bytes,
            position: 0,
        };
        if r.varint()? != 10 {
            return Err(invalid());
        }
        let n = r.varint()?;
        if n > 4096 {
            return Err(invalid());
        }
        let name = std::str::from_utf8(r.take(n)?)
            .map_err(|_| invalid())?
            .to_owned();
        if r.varint()? != 18 || r.varint()? != 1024 {
            return Err(invalid());
        }
        let vector = floats(r.take(1024)?)?;
        if r.varint()? != 24 {
            return Err(invalid());
        }
        let reference = r.varint()?;
        if reference >= 9 || r.position != bytes.len() {
            return Err(invalid());
        }
        Ok(Self {
            name,
            vector,
            reference,
        })
    }
}
impl Database {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 1_000_000 {
            return Err(invalid());
        }
        let mut reader = Reader { bytes, position: 0 };
        let count = reader.integer()?;
        if count != 9 {
            return Err(invalid());
        }
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            // Tag zero introduces a source-image record; its position in this
            // array, rather than this tag, is the style reference index.
            if reader.integer()? != 0 {
                return Err(invalid());
            }
            let source = reader.string()?;
            let fields = reader.integer()?;
            if fields > 256 {
                return Err(invalid());
            }
            let mut feature = None;
            let mut transferred = BTreeMap::new();
            let mut names = 0;
            let mut vectors = 0;
            for _ in 0..fields {
                match reader.integer()? {
                    1 => {
                        if feature.is_some() {
                            return Err(invalid());
                        }
                        feature = Some(reader.vector()?);
                    }
                    2 => {
                        let _: [f32; 256] = reader.vector()?;
                        vectors += 1;
                    }
                    3 => {
                        reader.string()?;
                        names += 1;
                    }
                    4 => {
                        let name = reader.string()?;
                        let original = reader.string()?;
                        if original != source
                            || transferred.insert(name, reader.vector()?).is_some()
                        {
                            return Err(invalid());
                        }
                    }
                    _ => return Err(invalid()),
                }
            }
            if names != vectors || transferred.is_empty() {
                return Err(invalid());
            }
            records.push(Record {
                feature: feature.ok_or_else(invalid)?,
                transferred,
            });
        }
        if reader.position != bytes.len() {
            return Err(invalid());
        }
        Ok(Self { records })
    }

    pub(super) fn select(&self, feature: &[f32; 576], style: &Style) -> Result<[f32; 256]> {
        let mut nearest: Vec<_> = self
            .records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let similarity = record
                    .feature
                    .iter()
                    .zip(feature)
                    .map(|(a, b)| a * b)
                    .sum::<f32>();
                (index, similarity)
            })
            .collect();
        nearest.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        // Native GetStyleVecByIndex requests top five. If the style's original
        // reference recording is among them, preserve its original vector.
        if nearest[..5]
            .iter()
            .any(|(index, _)| *index == style.reference)
        {
            return Ok(style.vector);
        }
        let key = style
            .name
            .rsplit_once('.')
            .map_or(style.name.as_str(), |(stem, _)| stem);
        self.records[nearest[0].0]
            .transferred
            .get(key)
            .copied()
            .ok_or_else(invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_five_use_reference_or_nearest_transferred_vector_without_averaging() {
        let records = (0..9)
            .map(|index| {
                let mut feature = [0.0; 576];
                feature[index] = 1.0;
                Record {
                    feature,
                    transferred: BTreeMap::from([("selected".to_owned(), [index as f32; 256])]),
                }
            })
            .collect();
        let database = Database { records };
        let style = Style {
            name: "selected.png".to_owned(),
            vector: [42.0; 256],
            reference: 6,
        };
        let mut query = [0.0; 576];
        query[0] = 1.0;
        assert_eq!(database.select(&query, &style).unwrap(), [0.0; 256]);
        query[6] = 2.0;
        assert_eq!(database.select(&query, &style).unwrap(), [42.0; 256]);
        query[6] = 0.0;
        query[8] = 2.0;
        assert_eq!(database.select(&query, &style).unwrap(), [8.0; 256]);
    }
    #[test]
    fn original_database_and_every_style_form_a_complete_group() {
        let payload = |name| {
            insta360_rs_data_underwater_resources::PAYLOADS
                .iter()
                .find(|(p, _)| *p == name)
                .unwrap()
                .1
        };
        let db = Database::parse(payload("underwater/style-database.bin")).unwrap();
        for index in 0..4 {
            let style = Style::parse(payload("underwater/styles/result.json"), index).unwrap();
            assert_eq!(style.reference, 6);
            assert_eq!(
                db.select(&db.records[6].feature, &style).unwrap(),
                style.vector
            );
            for record in &db.records {
                assert!(db.select(&record.feature, &style).is_ok());
            }
        }
    }
    #[test]
    fn readers_reject_truncation_and_nonfinite_vectors() {
        for bytes in [vec![], vec![0; 64], vec![255; 64]] {
            assert!(Database::parse(&bytes).is_err());
            assert!(Style::parse(&bytes, 0).is_err());
        }
        assert!(floats::<1>(&f32::NAN.to_le_bytes()).is_err());
    }
}
