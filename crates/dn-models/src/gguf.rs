//! Pure-Rust GGUF header reader (metadata + tensor table, no weights). Enough for the fit
//! estimator to size a model without loading llama.cpp. Spec: ggml `docs/gguf.md` (v2/v3).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum GgufError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("not a GGUF file")]
    BadMagic,
    #[error("unsupported GGUF version {0}")]
    Version(u32),
    #[error("malformed GGUF: {0}")]
    Malformed(String),
}

/// A scalar metadata value (arrays are kept as their length only, except small numeric ones).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetaValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    /// Integer or bool array (e.g. per-layer `head_count_kv`, `sliding_window_pattern`),
    /// at most 4096 items; bools are 0/1.
    IntArray(Vec<i64>),
    /// Any other array: only its length is kept (e.g. the tokenizer vocabulary).
    ArrayLen(u64),
}

impl MetaValue {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Int(i) if *i >= 0 => Some(*i as u64),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TensorInfo {
    pub name: String,
    pub dims: Vec<u64>,
    pub ggml_type: u32,
    pub offset: u64,
    /// Bytes in the file (from consecutive offsets).
    pub size: u64,
}

impl TensorInfo {
    pub fn n_elements(&self) -> u64 {
        self.dims.iter().product()
    }

    /// Mixture-of-experts FFN tensors (`blk.N.ffn_{up,down,gate}_exps`, also `_chexps`).
    pub fn is_expert(&self) -> bool {
        let n = &self.name;
        n.contains(".ffn_") && (n.ends_with("_exps.weight") || n.ends_with("_chexps.weight"))
    }

    /// Block index for `blk.N.*` tensors.
    pub fn layer(&self) -> Option<u32> {
        self.name
            .strip_prefix("blk.")?
            .split('.')
            .next()?
            .parse()
            .ok()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gguf {
    pub version: u32,
    pub metadata: BTreeMap<String, MetaValue>,
    pub tensors: Vec<TensorInfo>,
    pub file_size: u64,
}

const MAX_STR: u64 = 16 << 20;
const MAX_ITEMS: u64 = 1 << 26;

struct R<T> {
    inner: T,
    pos: u64,
}

impl<T: Read> R<T> {
    fn bytes<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.inner.read_exact(&mut b)?;
        self.pos += N as u64;
        Ok(b)
    }
    fn u32(&mut self) -> io::Result<u32> {
        self.bytes::<4>().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> io::Result<u64> {
        self.bytes::<8>().map(u64::from_le_bytes)
    }
    fn skip(&mut self, n: u64) -> io::Result<()> {
        let copied = io::copy(&mut (&mut self.inner).take(n), &mut io::sink())?;
        self.pos += copied;
        if copied != n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }
    fn string(&mut self) -> Result<String, GgufError> {
        let n = self.u64()?;
        if n > MAX_STR {
            return Err(GgufError::Malformed(format!("string of {n} bytes")));
        }
        let mut b = vec![0u8; n as usize];
        self.inner.read_exact(&mut b)?;
        self.pos += n;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    fn scalar(&mut self, ty: u32) -> Result<MetaValue, GgufError> {
        Ok(match ty {
            0 => MetaValue::Int(self.bytes::<1>()?[0] as i64),
            1 => MetaValue::Int(self.bytes::<1>()?[0] as i8 as i64),
            2 => MetaValue::Int(u16::from_le_bytes(self.bytes()?) as i64),
            3 => MetaValue::Int(i16::from_le_bytes(self.bytes()?) as i64),
            4 => MetaValue::Int(self.u32()? as i64),
            5 => MetaValue::Int(i32::from_le_bytes(self.bytes()?) as i64),
            6 => MetaValue::Float(f32::from_le_bytes(self.bytes()?) as f64),
            7 => MetaValue::Bool(self.bytes::<1>()?[0] != 0),
            8 => MetaValue::Str(self.string()?),
            10 => MetaValue::Int(self.u64()? as i64),
            11 => MetaValue::Int(i64::from_le_bytes(self.bytes()?)),
            12 => MetaValue::Float(f64::from_le_bytes(self.bytes()?)),
            t => return Err(GgufError::Malformed(format!("value type {t}"))),
        })
    }

    fn value(&mut self, ty: u32) -> Result<MetaValue, GgufError> {
        if ty != 9 {
            return self.scalar(ty);
        }
        let ity = self.u32()?;
        let n = self.u64()?;
        if n > MAX_ITEMS {
            return Err(GgufError::Malformed(format!("array of {n} items")));
        }
        let width = match ity {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        };
        match (ity, width) {
            (6 | 12, Some(w)) => {
                self.skip(n * w)?;
                Ok(MetaValue::ArrayLen(n))
            }
            (_, Some(_)) if n <= 4096 => {
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    match self.scalar(ity)? {
                        MetaValue::Int(i) => v.push(i),
                        MetaValue::Bool(b) => v.push(b as i64),
                        _ => unreachable!("integer and bool types only"),
                    }
                }
                Ok(MetaValue::IntArray(v))
            }
            (_, Some(w)) => {
                self.skip(n * w)?;
                Ok(MetaValue::ArrayLen(n))
            }
            (8, None) => {
                for _ in 0..n {
                    let len = self.u64()?;
                    if len > MAX_STR {
                        return Err(GgufError::Malformed("array string too long".into()));
                    }
                    self.skip(len)?;
                }
                Ok(MetaValue::ArrayLen(n))
            }
            _ => Err(GgufError::Malformed(format!("array item type {ity}"))),
        }
    }
}

impl Gguf {
    pub fn open(path: &Path) -> Result<Self, GgufError> {
        let f = std::fs::File::open(path)?;
        let size = f.metadata()?.len();
        Self::read(BufReader::with_capacity(1 << 16, f), size)
    }

    pub fn read<T: Read + Seek>(mut inner: T, file_size: u64) -> Result<Self, GgufError> {
        inner.seek(SeekFrom::Start(0))?;
        let mut r = R { inner, pos: 0 };
        if &r.bytes::<4>()? != b"GGUF" {
            return Err(GgufError::BadMagic);
        }
        let version = r.u32()?;
        if !(2..=3).contains(&version) {
            return Err(GgufError::Version(version));
        }
        let n_tensors = r.u64()?;
        let n_kv = r.u64()?;
        if n_tensors > MAX_ITEMS || n_kv > MAX_ITEMS {
            return Err(GgufError::Malformed("too many entries".into()));
        }
        let mut metadata = BTreeMap::new();
        for _ in 0..n_kv {
            let key = r.string()?;
            let ty = r.u32()?;
            let v = r.value(ty)?;
            metadata.insert(key, v);
        }
        let mut tensors = Vec::with_capacity(n_tensors as usize);
        for _ in 0..n_tensors {
            let name = r.string()?;
            let nd = r.u32()?;
            if nd > 8 {
                return Err(GgufError::Malformed(format!("{name}: {nd} dims")));
            }
            let dims = (0..nd).map(|_| r.u64()).collect::<io::Result<Vec<_>>>()?;
            let ggml_type = r.u32()?;
            let offset = r.u64()?;
            tensors.push(TensorInfo {
                name,
                dims,
                ggml_type,
                offset,
                size: 0,
            });
        }
        let align = metadata
            .get("general.alignment")
            .and_then(MetaValue::as_u64)
            .filter(|a| *a > 0)
            .unwrap_or(32);
        let data_start = r.pos.div_ceil(align) * align;
        let data_len = file_size.saturating_sub(data_start);
        let mut order: Vec<usize> = (0..tensors.len()).collect();
        order.sort_by_key(|&i| tensors[i].offset);
        for (k, &i) in order.iter().enumerate() {
            let end = order
                .get(k + 1)
                .map(|&j| tensors[j].offset)
                .unwrap_or(data_len);
            tensors[i].size = end.saturating_sub(tensors[i].offset);
        }
        Ok(Self {
            version,
            metadata,
            tensors,
            file_size,
        })
    }

    pub fn get(&self, key: &str) -> Option<&MetaValue> {
        self.metadata.get(key)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            MetaValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.get(key)?.as_u64()
    }

    pub fn arch(&self) -> &str {
        self.str("general.architecture").unwrap_or("unknown")
    }

    /// `{arch}.{suffix}` as an integer.
    pub fn arch_u64(&self, suffix: &str) -> Option<u64> {
        self.u64(&format!("{}.{suffix}", self.arch()))
    }

    /// Quantisation name from `general.file_type` (llama.cpp `llama_ftype`).
    pub fn quant_name(&self) -> Option<&'static str> {
        ftype_name(self.u64("general.file_type")?)
    }
}

/// llama.cpp `llama_ftype` → conventional name.
pub fn ftype_name(t: u64) -> Option<&'static str> {
    Some(match t {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4_MOE",
        _ => return None,
    })
}

/// Test helper and fixture writer: builds a GGUF v3 header in memory.
#[doc(hidden)]
pub mod write {
    pub enum V<'a> {
        U32(u32),
        Str(&'a str),
        I32Arr(&'a [i32]),
        BoolArr(&'a [bool]),
        StrArr(&'a [&'a str]),
    }

    fn s(out: &mut Vec<u8>, x: &str) {
        out.extend((x.len() as u64).to_le_bytes());
        out.extend(x.as_bytes());
    }

    /// `tensors`: (name, dims, ggml_type, byte size). Data is zero-filled.
    pub fn gguf(kv: &[(&str, V<'_>)], tensors: &[(&str, &[u64], u32, u64)]) -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend((tensors.len() as u64).to_le_bytes());
        out.extend((kv.len() as u64).to_le_bytes());
        for (k, v) in kv {
            s(&mut out, k);
            match v {
                V::U32(x) => {
                    out.extend(4u32.to_le_bytes());
                    out.extend(x.to_le_bytes());
                }
                V::Str(x) => {
                    out.extend(8u32.to_le_bytes());
                    s(&mut out, x);
                }
                V::I32Arr(a) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(5u32.to_le_bytes());
                    out.extend((a.len() as u64).to_le_bytes());
                    a.iter().for_each(|x| out.extend(x.to_le_bytes()));
                }
                V::BoolArr(a) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(7u32.to_le_bytes());
                    out.extend((a.len() as u64).to_le_bytes());
                    a.iter().for_each(|&x| out.push(x as u8));
                }
                V::StrArr(a) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(8u32.to_le_bytes());
                    out.extend((a.len() as u64).to_le_bytes());
                    a.iter().for_each(|x| s(&mut out, x));
                }
            }
        }
        let mut off = 0u64;
        for (name, dims, ty, size) in tensors {
            s(&mut out, name);
            out.extend((dims.len() as u32).to_le_bytes());
            dims.iter().for_each(|d| out.extend(d.to_le_bytes()));
            out.extend(ty.to_le_bytes());
            out.extend(off.to_le_bytes());
            off += size.div_ceil(32) * 32;
        }
        let pad = out.len().div_ceil(32) * 32 - out.len();
        out.extend(std::iter::repeat_n(0u8, pad + off as usize));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::write::{V, gguf};
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_metadata_and_tensor_sizes() {
        let bytes = gguf(
            &[
                ("general.architecture", V::Str("qwen3moe")),
                ("general.file_type", V::U32(15)),
                ("qwen3moe.block_count", V::U32(2)),
                ("qwen3moe.attention.head_count_kv", V::I32Arr(&[4, 4])),
                ("tokenizer.ggml.tokens", V::StrArr(&["a", "bb", "ccc"])),
            ],
            &[
                ("token_embd.weight", &[8, 4], 0, 128),
                ("blk.0.attn_q.weight", &[8, 8], 0, 256),
                ("blk.0.ffn_up_exps.weight", &[8, 8, 4], 12, 640),
                ("blk.1.ffn_down_exps.weight", &[8, 8, 4], 12, 640),
            ],
        );
        let len = bytes.len() as u64;
        let g = Gguf::read(Cursor::new(bytes), len).unwrap();
        assert_eq!(g.arch(), "qwen3moe");
        assert_eq!(g.quant_name(), Some("Q4_K_M"));
        assert_eq!(g.arch_u64("block_count"), Some(2));
        assert_eq!(
            g.get("qwen3moe.attention.head_count_kv"),
            Some(&MetaValue::IntArray(vec![4, 4]))
        );
        assert_eq!(
            g.get("tokenizer.ggml.tokens"),
            Some(&MetaValue::ArrayLen(3))
        );
        let sizes: Vec<u64> = g.tensors.iter().map(|t| t.size).collect();
        assert_eq!(sizes, [128, 256, 640, 640]);
        let experts: Vec<bool> = g.tensors.iter().map(|t| t.is_expert()).collect();
        assert_eq!(experts, [false, false, true, true]);
        assert_eq!(g.tensors[3].layer(), Some(1));
        assert_eq!(g.tensors[2].n_elements(), 256);
    }

    #[test]
    fn rejects_non_gguf() {
        let e = Gguf::read(Cursor::new(b"GGML0000".to_vec()), 8).unwrap_err();
        assert!(matches!(e, GgufError::BadMagic));
    }

    #[test]
    fn rejects_truncated() {
        let mut bytes = gguf(&[("general.architecture", V::Str("llama"))], &[]);
        bytes.truncate(20);
        assert!(Gguf::read(Cursor::new(bytes), 20).is_err());
    }
}
