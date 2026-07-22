//! PDF Standard セキュリティハンドラ（R2〜R6）
//!
//! 参考: pdf.js `crypto.js`、ISO 32000 Algorithm 2 / 2.A / 2.B

use std::collections::HashMap;

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::{NoPadding, Pkcs7}};
use md5::{Digest as _, Md5};
use sha2::{Sha256, Sha384, Sha512};

use crate::error::{Error, Result};

use super::object::{Dict, Object};

/// 32バイト標準パスワードパディング
pub(crate) const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01,
    0x08, 0x2e, 0x2e, 0x00, 0xb6, 0xd0, 0x68, 0x3e, 0x80, 0x2f, 0x0c, 0xa9, 0xfe, 0x64, 0x53,
    0x69, 0x7a,
];

/// オブジェクト鍵 AES 用サフィックス "sAlT"
const SALT_SUFFIX: [u8; 4] = [0x73, 0x41, 0x6c, 0x54];

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
#[cfg(test)]
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

/// crypt filter の方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptMethod {
    /// 無変換（Identity / None）
    None,
    /// RC4
    V2,
    /// AES-128-CBC
    AesV2,
    /// AES-256-CBC
    AesV3,
}

/// ファイル鍵と暗号フィルタ設定
#[derive(Debug, Clone)]
pub(crate) struct CipherFactory {
    encryption_key: Vec<u8>,
    /// /V（1,2,4,5）
    algorithm: i64,
    encrypt_metadata: bool,
    str_method: CryptMethod,
    stm_method: CryptMethod,
    cf: HashMap<String, CryptMethod>,
}

impl CipherFactory {
    pub fn from_encrypt_dict(
        encrypt: &Dict,
        file_id: &[u8],
        password: Option<&str>,
    ) -> Result<Self> {
        let filter = match encrypt.get("Filter") {
            Some(Object::Name(n)) => n.as_ref(),
            _ => {
                return Err(Error::Reader(
                    "unknown encryption method: missing /Filter".into(),
                ));
            }
        };
        if filter != "Standard" {
            return Err(Error::Reader(format!(
                "unknown encryption method: {filter}"
            )));
        }

        let algorithm = dict_int(encrypt, "V").ok_or_else(|| {
            Error::Reader("unsupported encryption algorithm: missing /V".into())
        })?;
        if !matches!(algorithm, 1 | 2 | 4 | 5) {
            return Err(Error::Reader(format!(
                "unsupported encryption algorithm: V={algorithm}"
            )));
        }

        let revision = dict_int(encrypt, "R").unwrap_or(match algorithm {
            ..=1 => 2,
            2 => 3,
            4 => 4,
            _ => 5,
        });

        let mut key_length_bits = dict_int(encrypt, "Length").unwrap_or(0);
        if key_length_bits == 0 {
            if algorithm <= 3 {
                key_length_bits = 40;
            } else if let Some(Object::Dict(cf_dict)) = encrypt.get("CF") {
                let stm_name = match encrypt.get("StmF") {
                    Some(Object::Name(n)) => n.as_ref(),
                    _ => "Identity",
                };
                if let Some(Object::Dict(handler)) = cf_dict.get(stm_name) {
                    key_length_bits = dict_int(handler, "Length").unwrap_or(128);
                    if key_length_bits < 40 {
                        key_length_bits <<= 3;
                    }
                } else {
                    key_length_bits = 128;
                }
            } else {
                key_length_bits = 128;
            }
        }
        // MD5 経路（R2〜R4）の鍵は仕様上 40〜128 bit。超過は hash スライスの panic 防止で拒否
        if key_length_bits < 40
            || key_length_bits % 8 != 0
            || (algorithm <= 4 && key_length_bits > 128)
        {
            return Err(Error::Reader(format!(
                "invalid key length: {key_length_bits}"
            )));
        }

        let encrypt_metadata = if algorithm == 4 || algorithm == 5 {
            !matches!(encrypt.get("EncryptMetadata"), Some(Object::Bool(false)))
        } else {
            true
        };

        let filters = parse_crypt_filters(encrypt, algorithm)?;

        let o_bytes = dict_bytes(encrypt, "O").ok_or_else(|| {
            Error::Reader("encryption dictionary missing /O".into())
        })?;
        let u_bytes = dict_bytes(encrypt, "U").ok_or_else(|| {
            Error::Reader("encryption dictionary missing /U".into())
        })?;

        let flags = dict_int(encrypt, "P").unwrap_or(0);

        // 試行順は空パスワード → 指定パスワード
        let mut candidates: Vec<Vec<u8>> = vec![Vec::new()];
        if let Some(pw) = password {
            if !pw.is_empty() {
                let pw_bytes = password_to_bytes(pw, revision);
                if pw_bytes != candidates[0] {
                    candidates.push(pw_bytes);
                }
            }
        }

        let mut encryption_key = None;
        for cand in &candidates {
            let key = if algorithm != 5 {
                prepare_key_data(
                    file_id,
                    cand,
                    &o_bytes,
                    &u_bytes,
                    flags,
                    revision,
                    key_length_bits as usize,
                    encrypt_metadata,
                )
            } else {
                create_encryption_key_v5(encrypt, revision, cand, &o_bytes, &u_bytes)?
            };
            if key.is_some() {
                encryption_key = key;
                break;
            }
        }

        let mut encryption_key = encryption_key.ok_or(Error::BadPassword)?;

        // V=4 で鍵が 16 未満のときの救済
        if algorithm == 4 && encryption_key.len() < 16 {
            let mut padded = vec![0u8; 16];
            padded[..encryption_key.len()].copy_from_slice(&encryption_key);
            encryption_key = padded;
        }

        Ok(Self {
            encryption_key,
            algorithm,
            encrypt_metadata,
            str_method: filters.string,
            stm_method: filters.stream,
            cf: filters.named,
        })
    }

    pub fn encrypt_metadata(&self) -> bool {
        self.encrypt_metadata
    }

    pub fn decrypt_string(&self, data: &[u8], num: u32, generation: u16) -> Result<Vec<u8>> {
        let method = self.str_method;
        if method == CryptMethod::None {
            return Ok(data.to_vec());
        }
        let key = self.object_key(num, generation, method);
        decrypt_with_method(method, &key, data)
    }

    /// `crypt_filter_name` が Some のときは /Crypt の Name。None は既定 StmF
    pub fn decrypt_stream(
        &self,
        data: &[u8],
        num: u32,
        generation: u16,
        crypt_filter_name: Option<&str>,
    ) -> Result<Vec<u8>> {
        let method = match crypt_filter_name {
            Some(name) if name == "Identity" => CryptMethod::None,
            Some(name) => self
                .cf
                .get(name)
                .copied()
                .unwrap_or(self.stm_method),
            None => self.stm_method,
        };
        if method == CryptMethod::None {
            return Ok(data.to_vec());
        }
        let key = self.object_key(num, generation, method);
        decrypt_with_method(method, &key, data)
    }

    fn object_key(&self, num: u32, generation: u16, method: CryptMethod) -> Vec<u8> {
        if self.algorithm == 5 {
            return self.encryption_key.clone();
        }
        let is_aes = matches!(method, CryptMethod::AesV2 | CryptMethod::AesV3);
        build_object_key(&self.encryption_key, num, generation, is_aes)
    }
}

/// crypt filter の解析結果
struct CryptFilters {
    /// /CF の名前付きフィルタ
    named: HashMap<String, CryptMethod>,
    /// 既定のストリーム用（/StmF）
    stream: CryptMethod,
    /// 既定の文字列用（/StrF）
    string: CryptMethod,
}

fn parse_crypt_filters(encrypt: &Dict, algorithm: i64) -> Result<CryptFilters> {
    if algorithm < 4 {
        // V1/V2 は常に RC4
        return Ok(CryptFilters {
            named: HashMap::new(),
            stream: CryptMethod::V2,
            string: CryptMethod::V2,
        });
    }

    let stmf_name = match encrypt.get("StmF") {
        Some(Object::Name(n)) => n.as_ref(),
        _ => "Identity",
    };
    let strf_name = match encrypt.get("StrF") {
        Some(Object::Name(n)) => n.as_ref(),
        _ => "Identity",
    };

    let mut cf: HashMap<String, CryptMethod> = HashMap::new();
    if let Some(Object::Dict(cf_dict)) = encrypt.get("CF") {
        for key in cf_dict.keys() {
            if let Some(Object::Dict(handler)) = cf_dict.get(key) {
                let method = cfm_to_method(handler, algorithm)?;
                cf.insert(key.to_string(), method);
            }
        }
    }

    let stm_method = if stmf_name == "Identity" {
        CryptMethod::None
    } else {
        cf.get(stmf_name).copied().unwrap_or(CryptMethod::None)
    };
    let str_method = if strf_name == "Identity" {
        CryptMethod::None
    } else {
        cf.get(strf_name).copied().unwrap_or(CryptMethod::None)
    };

    Ok(CryptFilters {
        named: cf,
        stream: stm_method,
        string: str_method,
    })
}

fn cfm_to_method(handler: &Dict, algorithm: i64) -> Result<CryptMethod> {
    let cfm = match handler.get("CFM") {
        Some(Object::Name(n)) => n.as_ref(),
        None => return Ok(CryptMethod::None),
        _ => {
            return Err(Error::Reader("invalid /CFM value".into()));
        }
    };
    // V=5 は常に AES-256（AESV2 誤記の救済）
    if algorithm == 5 {
        return Ok(match cfm {
            "None" => CryptMethod::None,
            _ => CryptMethod::AesV3,
        });
    }
    Ok(match cfm {
        "None" => CryptMethod::None,
        "V2" => CryptMethod::V2,
        "AESV2" => CryptMethod::AesV2,
        "AESV3" => CryptMethod::AesV3,
        other => {
            return Err(Error::Reader(format!("Unknown crypto method: {other}")));
        }
    })
}

fn password_to_bytes(password: &str, revision: i64) -> Vec<u8> {
    if revision >= 5 {
        // R5/R6 は UTF-8、最大 127 バイト
        let b = password.as_bytes();
        b[..b.len().min(127)].to_vec()
    } else {
        // R2〜R4 は 1 バイト文字扱い
        password.chars().map(|c| (c as u32).min(255) as u8).collect()
    }
}

/// Algorithm 2（R2〜R4）のファイル鍵導出と /U 照合
fn prepare_key_data(
    file_id: &[u8],
    password: &[u8],
    owner_password: &[u8],
    user_password: &[u8],
    flags: i64,
    revision: i64,
    key_length_bits: usize,
    encrypt_metadata: bool,
) -> Option<Vec<u8>> {
    let o = if owner_password.len() >= 32 {
        &owner_password[..32]
    } else {
        owner_password
    };
    let u = if user_password.len() >= 32 {
        &user_password[..32]
    } else {
        user_password
    };

    let encryption_key =
        derive_file_key_r2_r4(password, o, flags, file_id, revision, key_length_bits, encrypt_metadata);

    let check = compute_u_entry(&encryption_key, file_id, revision);
    // R2 は 32 バイト全照合。R≥3 は /U 先頭 16 バイトのみ
    let ok = if revision >= 3 {
        check.len() >= 16 && u.len() >= 16 && check[..16] == u[..16]
    } else {
        check.len() == 32 && u.len() >= 32 && check[..32] == u[..32]
    };
    if ok {
        Some(encryption_key)
    } else {
        None
    }
}

/// Algorithm 2 のファイル鍵（/U 照合なし）
pub(crate) fn derive_file_key_r2_r4(
    password: &[u8],
    o_entry: &[u8],
    flags: i64,
    file_id: &[u8],
    revision: i64,
    key_length_bits: usize,
    encrypt_metadata: bool,
) -> Vec<u8> {
    let key_len = key_length_bits / 8;
    let mut hash_data = Vec::with_capacity(32 + o_entry.len() + 4 + file_id.len() + 4);

    let n = password.len().min(32);
    hash_data.extend_from_slice(&password[..n]);
    hash_data.extend_from_slice(&PASSWORD_PADDING[..32 - n]);
    hash_data.extend_from_slice(o_entry);
    let p = flags as u32;
    hash_data.extend_from_slice(&p.to_le_bytes());
    hash_data.extend_from_slice(file_id);
    if revision >= 4 && !encrypt_metadata {
        hash_data.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    }

    let mut hash = md5_digest(&hash_data);
    if revision >= 3 {
        for _ in 0..50 {
            hash = md5_digest(&hash[..key_len]);
        }
    }
    hash[..key_len].to_vec()
}

fn compute_u_entry(encryption_key: &[u8], file_id: &[u8], revision: i64) -> Vec<u8> {
    if revision >= 3 {
        let mut data = Vec::with_capacity(32 + file_id.len());
        data.extend_from_slice(&PASSWORD_PADDING);
        data.extend_from_slice(file_id);
        let mut check = rc4_process(encryption_key, &md5_digest(&data));
        let n = encryption_key.len();
        for j in 1..=19 {
            let derived: Vec<u8> = encryption_key.iter().map(|&b| b ^ (j as u8)).collect();
            check = rc4_process(&derived[..n], &check);
        }
        // 仕様上 /U は 32 バイト。先頭 16 が照合対象
        let mut out = check;
        out.resize(32, 0);
        out
    } else {
        rc4_process(encryption_key, &PASSWORD_PADDING)
    }
}

/// R5/R6 のファイル鍵取得
fn create_encryption_key_v5(
    encrypt: &Dict,
    revision: i64,
    password: &[u8],
    o_bytes: &[u8],
    u_bytes: &[u8],
) -> Result<Option<Vec<u8>>> {
    if o_bytes.len() < 48 || u_bytes.len() < 48 {
        return Err(Error::Reader(
            "invalid /O or /U length for R5/R6".into(),
        ));
    }
    let owner_validation_salt = &o_bytes[32..40];
    let owner_key_salt = &o_bytes[40..48];
    let u_for_owner = &u_bytes[..48];
    let user_validation_salt = &u_bytes[32..40];
    let user_key_salt = &u_bytes[40..48];
    let owner_password = &o_bytes[..32];
    let user_password = &u_bytes[..32];

    let oe = dict_bytes(encrypt, "OE").ok_or_else(|| {
        Error::Reader("encryption dictionary missing /OE".into())
    })?;
    let ue = dict_bytes(encrypt, "UE").ok_or_else(|| {
        Error::Reader("encryption dictionary missing /UE".into())
    })?;

    let pw = if password.len() > 127 {
        &password[..127]
    } else {
        password
    };

    if check_user_password_v5(revision, pw, user_validation_salt, user_password)? {
        let key = hash_v5(revision, pw, user_key_salt, &[])?;
        return Ok(Some(aes256_cbc_decrypt_no_pad(&key, &ue, &[0u8; 16])?));
    }
    if !pw.is_empty()
        && check_owner_password_v5(
            revision,
            pw,
            owner_validation_salt,
            u_for_owner,
            owner_password,
        )?
    {
        let key = hash_v5(revision, pw, owner_key_salt, u_for_owner)?;
        return Ok(Some(aes256_cbc_decrypt_no_pad(&key, &oe, &[0u8; 16])?));
    }
    Ok(None)
}

fn check_user_password_v5(
    revision: i64,
    password: &[u8],
    user_validation_salt: &[u8],
    user_password: &[u8],
) -> Result<bool> {
    let result = hash_v5(revision, password, user_validation_salt, &[])?;
    Ok(result.len() >= 32 && user_password.len() >= 32 && result[..32] == user_password[..32])
}

fn check_owner_password_v5(
    revision: i64,
    password: &[u8],
    owner_validation_salt: &[u8],
    u_bytes: &[u8],
    owner_password: &[u8],
) -> Result<bool> {
    let result = hash_v5(revision, password, owner_validation_salt, u_bytes)?;
    Ok(result.len() >= 32 && owner_password.len() >= 32 && result[..32] == owner_password[..32])
}

fn hash_v5(revision: i64, password: &[u8], salt: &[u8], user_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut input = Vec::with_capacity(password.len() + salt.len() + user_bytes.len());
    input.extend_from_slice(password);
    input.extend_from_slice(salt);
    input.extend_from_slice(user_bytes);
    if revision == 6 {
        algorithm_2b(password, &input, user_bytes)
    } else {
        Ok(sha256_digest(&input))
    }
}

/// Algorithm 2.B（R6）
fn algorithm_2b(password: &[u8], input: &[u8], user_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut k = sha256_digest(input);
    k.truncate(32);
    let mut i = 0usize;
    loop {
        let combined_len = password.len() + k.len() + user_bytes.len();
        let mut combined = Vec::with_capacity(combined_len);
        combined.extend_from_slice(password);
        combined.extend_from_slice(&k);
        combined.extend_from_slice(user_bytes);

        let mut k1 = Vec::with_capacity(combined_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(&combined);
        }
        let e = aes128_cbc_encrypt_no_pad(&k[..16], &k1, &k[16..32])?;
        let remainder: u32 = e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3;
        k = match remainder {
            0 => sha256_digest(&e),
            1 => sha384_digest(&e),
            _ => sha512_digest(&e),
        };
        i += 1;
        if i >= 64 && e.last().copied().unwrap_or(0) as usize <= i.saturating_sub(32) {
            break;
        }
    }
    k.truncate(32);
    Ok(k)
}

pub(crate) fn build_object_key(
    encryption_key: &[u8],
    num: u32,
    generation: u16,
    is_aes: bool,
) -> Vec<u8> {
    let n = encryption_key.len();
    let mut key = Vec::with_capacity(n + 9);
    key.extend_from_slice(encryption_key);
    key.extend_from_slice(&num.to_le_bytes()[..3]);
    key.extend_from_slice(&generation.to_le_bytes());
    if is_aes {
        key.extend_from_slice(&SALT_SUFFIX);
    }
    let hash = md5_digest(&key);
    let out_len = (n + 5).min(16);
    hash[..out_len].to_vec()
}

fn decrypt_with_method(method: CryptMethod, key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    match method {
        CryptMethod::None => Ok(data.to_vec()),
        CryptMethod::V2 => Ok(rc4_process(key, data)),
        CryptMethod::AesV2 => aes_cbc_decrypt_pkcs7(key, data, 16),
        CryptMethod::AesV3 => aes_cbc_decrypt_pkcs7(key, data, 32),
    }
}

// --- RC4 ---

/// RC4 ストリーム暗号（暗号化と復号は同一）
pub(crate) fn rc4_process(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s = [0u8; 256];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = i as u8;
    }
    let key_len = key.len().max(1);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key_len]);
        s.swap(i, j as usize);
    }
    let mut i = 0u8;
    j = 0;
    let mut out = vec![0u8; data.len()];
    for (idx, &b) in data.iter().enumerate() {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out[idx] = b ^ k;
    }
    out
}

// --- AES ---

fn aes_cbc_decrypt_pkcs7(key: &[u8], data: &[u8], key_len: usize) -> Result<Vec<u8>> {
    if data.len() < 16 {
        return Err(Error::Reader("AES ciphertext too short".into()));
    }
    if key.len() < key_len {
        return Err(Error::Reader("AES key too short".into()));
    }
    let iv = &data[..16];
    let ciphertext = &data[16..];
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(Error::Reader("AES ciphertext length invalid".into()));
    }
    let mut buf = ciphertext.to_vec();
    let key = &key[..key_len];
    let plain = if key_len == 16 {
        Aes128CbcDec::new_from_slices(key, iv)
            .map_err(|_| Error::Reader("AES-128 init failed".into()))?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| Error::Reader("AES-128 decrypt failed".into()))?
            .to_vec()
    } else {
        Aes256CbcDec::new_from_slices(key, iv)
            .map_err(|_| Error::Reader("AES-256 init failed".into()))?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| Error::Reader("AES-256 decrypt failed".into()))?
            .to_vec()
    };
    Ok(plain)
}

fn aes256_cbc_decrypt_no_pad(key: &[u8], data: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return Err(Error::Reader("AES-256 no-pad length invalid".into()));
    }
    if key.len() < 32 {
        return Err(Error::Reader("AES-256 key too short".into()));
    }
    let mut buf = data.to_vec();
    let plain = Aes256CbcDec::new_from_slices(&key[..32], iv)
        .map_err(|_| Error::Reader("AES-256 init failed".into()))?
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| Error::Reader("AES-256 decrypt failed".into()))?
        .to_vec();
    Ok(plain)
}

/// AES-128-CBC 暗号化（パディングなし。data は 16 の倍数であること）
fn aes128_cbc_encrypt_no_pad(key: &[u8], data: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    let mut buf = data.to_vec();
    let len = buf.len();
    let out = Aes128CbcEnc::new_from_slices(key, iv)
        .map_err(|_| Error::Reader("AES-128 init failed".into()))?
        .encrypt_padded_mut::<NoPadding>(&mut buf, len)
        .map_err(|_| Error::Reader("AES-128 encrypt failed".into()))?;
    Ok(out.to_vec())
}

/// AES-CBC PKCS#7 暗号化（先頭に IV を付与）。テスト用
#[cfg(test)]
pub(crate) fn aes_cbc_encrypt_pkcs7(key: &[u8], plaintext: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>> {
    let mut buf = plaintext.to_vec();
    // PKCS7 用に余裕を確保
    let pad_len = 16 - (buf.len() % 16);
    buf.resize(buf.len() + pad_len, 0);
    let data_len = plaintext.len();
    let ct = if key.len() == 16 {
        let enc = Aes128CbcEnc::new_from_slices(key, iv)
            .map_err(|_| Error::Reader("AES-128 enc init failed".into()))?;
        enc.encrypt_padded_mut::<Pkcs7>(&mut buf, data_len)
            .map_err(|_| Error::Reader("AES-128 encrypt failed".into()))?
            .to_vec()
    } else if key.len() == 32 {
        let enc = Aes256CbcEnc::new_from_slices(key, iv)
            .map_err(|_| Error::Reader("AES-256 enc init failed".into()))?;
        enc.encrypt_padded_mut::<Pkcs7>(&mut buf, data_len)
            .map_err(|_| Error::Reader("AES-256 encrypt failed".into()))?
            .to_vec()
    } else {
        return Err(Error::Reader("invalid AES key length".into()));
    };
    let mut out = Vec::with_capacity(16 + ct.len());
    out.extend_from_slice(iv);
    out.extend_from_slice(&ct);
    Ok(out)
}

#[cfg(test)]
fn aes256_cbc_encrypt_no_pad(key: &[u8], data: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>> {
    let mut buf = data.to_vec();
    let len = buf.len();
    if len % 16 != 0 {
        return Err(Error::Reader("AES-256 no-pad encrypt length invalid".into()));
    }
    let enc = Aes256CbcEnc::new_from_slices(&key[..32], iv)
        .map_err(|_| Error::Reader("AES-256 enc init failed".into()))?;
    let out = enc
        .encrypt_padded_mut::<NoPadding>(&mut buf, len)
        .map_err(|_| Error::Reader("AES-256 encrypt failed".into()))?
        .to_vec();
    Ok(out)
}

// --- ハッシュ ---

fn md5_digest(data: &[u8]) -> Vec<u8> {
    Md5::digest(data).to_vec()
}

fn sha256_digest(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

fn sha384_digest(data: &[u8]) -> Vec<u8> {
    Sha384::digest(data).to_vec()
}

fn sha512_digest(data: &[u8]) -> Vec<u8> {
    Sha512::digest(data).to_vec()
}

// --- 辞書ヘルパ ---

fn dict_int(dict: &Dict, key: &str) -> Option<i64> {
    match dict.get(key) {
        Some(Object::Int(n)) => Some(*n),
        Some(Object::Real(n)) => Some(*n as i64),
        _ => None,
    }
}

fn dict_bytes(dict: &Dict, key: &str) -> Option<Vec<u8>> {
    match dict.get(key) {
        Some(Object::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Object 木の文字列復号と Stream への crypto_ref 付与
pub(crate) fn decrypt_object_tree(
    obj: &mut Object,
    cipher: &CipherFactory,
    num: u32,
    generation: u16,
) -> Result<()> {
    match obj {
        Object::Str(s) => {
            *s = cipher.decrypt_string(s, num, generation)?;
        }
        Object::Array(arr) => {
            for item in arr.iter_mut() {
                decrypt_object_tree(item, cipher, num, generation)?;
            }
        }
        Object::Dict(d) => {
            for value in d.values_mut() {
                decrypt_object_tree(value, cipher, num, generation)?;
            }
        }
        Object::Stream(stm) => {
            for value in stm.dict.values_mut() {
                decrypt_object_tree(value, cipher, num, generation)?;
            }
            stm.crypto_ref = Some(super::object::Ref::new(num, generation));
        }
        _ => {}
    }
    Ok(())
}

// --- テスト用の暗号化組み立てヘルパ ---

/// R4 用 /U エントリ（32 バイト）
#[cfg(test)]
pub(crate) fn compute_u_r3_r4(encryption_key: &[u8], file_id: &[u8]) -> Vec<u8> {
    compute_u_entry(encryption_key, file_id, 4)
}

/// R4 用 /O エントリ（オーナーパスワードから）
#[cfg(test)]
pub(crate) fn compute_o_r3_r4(
    owner_password: &[u8],
    user_password: &[u8],
    revision: i64,
    key_length_bits: usize,
) -> Vec<u8> {
    // Algorithm 3
    let mut hash_data = [0u8; 32];
    let n = owner_password.len().min(32);
    hash_data[..n].copy_from_slice(&owner_password[..n]);
    hash_data[n..].copy_from_slice(&PASSWORD_PADDING[..32 - n]);
    let mut hash = md5_digest(&hash_data);
    let key_len = key_length_bits / 8;
    if revision >= 3 {
        for _ in 0..50 {
            hash = md5_digest(&hash);
        }
    }
    let mut user_pad = [0u8; 32];
    let un = user_password.len().min(32);
    user_pad[..un].copy_from_slice(&user_password[..un]);
    user_pad[un..].copy_from_slice(&PASSWORD_PADDING[..32 - un]);
    let mut enc = user_pad.to_vec();
    if revision >= 3 {
        for j in 0..=19 {
            let derived: Vec<u8> = hash[..key_len].iter().map(|&b| b ^ (j as u8)).collect();
            enc = rc4_process(&derived, &enc);
        }
    } else {
        enc = rc4_process(&hash[..key_len], &enc);
    }
    enc
}

/// R5/R6 用 /U /UE（ユーザーパスワードのみ）
#[cfg(test)]
pub(crate) fn compute_u_ue_r5_r6(
    revision: i64,
    user_password: &[u8],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let pw = if user_password.len() > 127 {
        &user_password[..127]
    } else {
        user_password
    };
    let u_hash = hash_v5(revision, pw, validation_salt, &[])?;
    let mut u = Vec::with_capacity(48);
    u.extend_from_slice(&u_hash[..32]);
    u.extend_from_slice(validation_salt);
    u.extend_from_slice(key_salt);

    let ue_key = hash_v5(revision, pw, key_salt, &[])?;
    let ue = aes256_cbc_encrypt_no_pad(&ue_key, file_key, &[0u8; 16])?;
    Ok((u, ue))
}

/// R5/R6 用ダミー /O /OE（空オーナー。ユーザー鍵だけ通す）
#[cfg(test)]
pub(crate) fn compute_o_oe_dummy_r5_r6(
    revision: i64,
    file_key: &[u8; 32],
    u_bytes: &[u8],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    // オーナーパスワードは空、salt はユーザーと別
    let owner_pw: &[u8] = b"";
    let o_hash = hash_v5(revision, owner_pw, validation_salt, u_bytes)?;
    let mut o = Vec::with_capacity(48);
    o.extend_from_slice(&o_hash[..32]);
    o.extend_from_slice(validation_salt);
    o.extend_from_slice(key_salt);
    let oe_key = hash_v5(revision, owner_pw, key_salt, u_bytes)?;
    let oe = aes256_cbc_encrypt_no_pad(&oe_key, file_key, &[0u8; 16])?;
    Ok((o, oe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::object::Ref;
    use super::super::xref::XRef;

    #[test]
    fn rc4_rfc6229_vector() {
        // RFC 6229 §4.1: key = 0x0102030405, plaintext = 0x00000000...
        // 先頭 16 バイトのキーストリーム
        let key = [0x01u8, 0x02, 0x03, 0x04, 0x05];
        let plaintext = [0u8; 16];
        let out = rc4_process(&key, &plaintext);
        // 公開ベクタ: 先頭 16B = b2 39 63 05 f0 3d c0 27 ...（RC4-40）
        assert_eq!(
            &out[..16],
            &[
                0xb2, 0x39, 0x63, 0x05, 0xf0, 0x3d, 0xc0, 0x27, 0xcc, 0xc3, 0x52, 0x4a, 0x0a,
                0x11, 0x18, 0xa8
            ]
        );
    }

    #[test]
    fn rc4_roundtrip() {
        let key = b"secret-key";
        let plain = b"Hello, RC4 encryption test!";
        let enc = rc4_process(key, plain);
        let dec = rc4_process(key, &enc);
        assert_eq!(dec, plain);
    }

    #[test]
    fn algorithm2_key_derivation_deterministic() {
        let password = b"";
        let o = PASSWORD_PADDING; // ダミー O
        let file_id = b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10";
        let key = derive_file_key_r2_r4(password, &o, -4, file_id, 4, 128, true);
        assert_eq!(key.len(), 16);
        // 同じ入力で同一鍵
        let key2 = derive_file_key_r2_r4(password, &o, -4, file_id, 4, 128, true);
        assert_eq!(key, key2);
        // 入力が変われば鍵も変わる
        let key3 = derive_file_key_r2_r4(b"x", &o, -4, file_id, 4, 128, true);
        assert_ne!(key, key3);
        // 固定入力の回帰値（Algorithm 2・R4・128bit・空パスワード）
        // 手計算用入力: pad‖O=PASSWORD_PADDING×2 ‖ P=0xfffffffc ‖ fileID=01..10
        let mut hash_data = Vec::new();
        hash_data.extend_from_slice(&PASSWORD_PADDING);
        hash_data.extend_from_slice(&PASSWORD_PADDING);
        hash_data.extend_from_slice(&(-4i32 as u32).to_le_bytes());
        hash_data.extend_from_slice(file_id);
        let mut expected = md5_digest(&hash_data);
        for _ in 0..50 {
            expected = md5_digest(&expected[..16]);
        }
        assert_eq!(key, expected[..16]);
    }

    fn build_encrypted_pdf_r4_rc4(user_password: &[u8]) -> Vec<u8> {
        build_encrypted_pdf_r4(user_password, false)
    }

    fn build_encrypted_pdf_r4_aes(user_password: &[u8]) -> Vec<u8> {
        build_encrypted_pdf_r4(user_password, true)
    }

    /// 最小暗号化 PDF（R4、RC4-128 または AESV2）
    fn build_encrypted_pdf_r4(user_password: &[u8], use_aes: bool) -> Vec<u8> {
        let file_id: [u8; 16] = [
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
            0xaa, 0xbb,
        ];
        let owner_password = user_password; // 同一で簡略
        let o = compute_o_r3_r4(owner_password, user_password, 4, 128);
        let file_key =
            derive_file_key_r2_r4(user_password, &o, -4, &file_id, 4, 128, true);
        let u = compute_u_r3_r4(&file_key, &file_id);

        // オブジェクト: 1=Catalog, 2=Pages, 3=Page, 4=Contents, 5=Encrypt
        // 平文コンテンツ
        let content_plain = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
        let content_obj_num = 4u32;
        let content_gen = 0u16;

        let (cfm, encrypted_content) = if use_aes {
            let obj_key = build_object_key(&file_key, content_obj_num, content_gen, true);
            let iv = [0x01u8; 16];
            let enc = aes_cbc_encrypt_pkcs7(&obj_key, content_plain, &iv).unwrap();
            ("AESV2", enc)
        } else {
            let obj_key = build_object_key(&file_key, content_obj_num, content_gen, false);
            ("V2", rc4_process(&obj_key, content_plain))
        };

        // 文字列 "Hello" を Catalog の /Title に暗号化して入れる検証用
        // ページツリーは最小構成

        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        // 1 Catalog
        let o1 = body.len();
        let cat = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        // Catalog は暗号化対象の辞書（文字列なし）— オブジェクト構造は平文、値の文字列のみ暗号化
        // 実際 PDF ではオブジェクト全体が平文で中の文字列/ストリームが暗号化
        body.extend_from_slice(cat);

        // 2 Pages
        let o2 = body.len();
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // 3 Page
        let o3 = body.len();
        body.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>\nendobj\n",
        );

        // 4 Contents stream
        let o4 = body.len();
        let stm_dict = format!(
            "4 0 obj\n<< /Length {} >>\nstream\n",
            encrypted_content.len()
        );
        body.extend_from_slice(stm_dict.as_bytes());
        body.extend_from_slice(&encrypted_content);
        body.extend_from_slice(b"\nendstream\nendobj\n");

        // 5 Encrypt
        let o5 = body.len();
        let o_hex = pdf_hex_string(&o);
        let u_hex = pdf_hex_string(&u);
        let enc_dict = format!(
            "5 0 obj\n<< /Filter /Standard /V 4 /R 4 /Length 128 /P -4 \
             /EncryptMetadata true \
             /CF << /StdCF << /CFM /{cfm} /Length 16 /AuthEvent /DocOpen >> >> \
             /StmF /StdCF /StrF /StdCF \
             /O {o_hex} /U {u_hex} >>\nendobj\n"
        );
        body.extend_from_slice(enc_dict.as_bytes());

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 6\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        for off in [o1, o2, o3, o4, o5] {
            body.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        let id_hex = pdf_hex_string(&file_id);
        body.extend_from_slice(
            format!(
                "trailer\n<< /Size 6 /Root 1 0 R /Encrypt 5 0 R /ID [{id_hex} {id_hex}] >>\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");
        let _ = (o1, o2, o3, o4, o5);
        body
    }

    /// 最小暗号化 PDF（R6 / AES-256）
    fn build_encrypted_pdf_r6(user_password: &[u8]) -> Vec<u8> {
        let file_id: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00,
        ];
        let file_key: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let u_val_salt = [0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8];
        let u_key_salt = [0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8];
        let o_val_salt = [0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8];
        let o_key_salt = [0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8];

        let (u, ue) =
            compute_u_ue_r5_r6(6, user_password, &file_key, &u_val_salt, &u_key_salt).unwrap();
        let (o, oe) =
            compute_o_oe_dummy_r5_r6(6, &file_key, &u, &o_val_salt, &o_key_salt).unwrap();

        let content_plain = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
        let content_obj_num = 4u32;
        // V=5 はファイル鍵を直接使用
        let iv = [0x02u8; 16];
        let encrypted_content =
            aes_cbc_encrypt_pkcs7(&file_key, content_plain, &iv).unwrap();

        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.7\n");

        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let o2 = body.len();
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let o3 = body.len();
        body.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>\nendobj\n",
        );
        let o4 = body.len();
        let stm_dict = format!(
            "4 0 obj\n<< /Length {} >>\nstream\n",
            encrypted_content.len()
        );
        body.extend_from_slice(stm_dict.as_bytes());
        body.extend_from_slice(&encrypted_content);
        body.extend_from_slice(b"\nendstream\nendobj\n");

        let o5 = body.len();
        let perms = [0xffu8; 16]; // ダミー（照合に未使用）
        let enc_dict = format!(
            "5 0 obj\n<< /Filter /Standard /V 5 /R 6 /Length 256 /P -4 \
             /EncryptMetadata true \
             /CF << /StdCF << /CFM /AESV3 /Length 32 /AuthEvent /DocOpen >> >> \
             /StmF /StdCF /StrF /StdCF \
             /O {o} /U {u} /OE {oe} /UE {ue} /Perms {perms} >>\nendobj\n",
            o = pdf_hex_string(&o),
            u = pdf_hex_string(&u),
            oe = pdf_hex_string(&oe),
            ue = pdf_hex_string(&ue),
            perms = pdf_hex_string(&perms),
        );
        body.extend_from_slice(enc_dict.as_bytes());
        let _ = content_obj_num;

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 6\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        for off in [o1, o2, o3, o4, o5] {
            body.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        let id_hex = pdf_hex_string(&file_id);
        body.extend_from_slice(
            format!(
                "trailer\n<< /Size 6 /Root 1 0 R /Encrypt 5 0 R /ID [{id_hex} {id_hex}] >>\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");
        body
    }

    fn pdf_hex_string(data: &[u8]) -> String {
        let mut s = String::from("<");
        for b in data {
            s.push_str(&format!("{b:02X}"));
        }
        s.push('>');
        s
    }

    fn open_and_fetch_content(data: &[u8], password: Option<&str>) -> Vec<u8> {
        let xref = XRef::parse_with_password(data, password).expect("parse encrypted pdf");
        let stream_obj = xref.fetch(Ref::new(4, 0), data).expect("fetch contents");
        let Object::Stream(stm) = stream_obj else {
            panic!("expected stream");
        };
        use super::super::filters::decode_stream;
        use super::super::parser::NullResolver;
        decode_stream(data, &stm, &NullResolver, xref.cipher(), xref.decode_limit())
            .expect("decode content stream")
    }

    #[test]
    fn roundtrip_r4_rc4_empty_password() {
        let data = build_encrypted_pdf_r4_rc4(b"");
        let content = open_and_fetch_content(&data, None);
        assert_eq!(content, b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    }

    #[test]
    fn roundtrip_r4_rc4_user_password() {
        let data = build_encrypted_pdf_r4_rc4(b"secret");
        let content = open_and_fetch_content(&data, Some("secret"));
        assert_eq!(content, b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    }

    #[test]
    fn roundtrip_r4_aesv2_empty_password() {
        let data = build_encrypted_pdf_r4_aes(b"");
        let content = open_and_fetch_content(&data, None);
        assert_eq!(content, b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    }

    #[test]
    fn roundtrip_r4_aesv2_user_password() {
        let data = build_encrypted_pdf_r4_aes(b"aespass");
        let content = open_and_fetch_content(&data, Some("aespass"));
        assert_eq!(content, b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    }

    #[test]
    fn roundtrip_r6_aes256_empty_password() {
        let data = build_encrypted_pdf_r6(b"");
        let content = open_and_fetch_content(&data, None);
        assert_eq!(content, b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    }

    #[test]
    fn roundtrip_r6_aes256_user_password() {
        let data = build_encrypted_pdf_r6(b"r6pass");
        let content = open_and_fetch_content(&data, Some("r6pass"));
        assert_eq!(content, b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
    }

    #[test]
    fn bad_password_r4() {
        let data = build_encrypted_pdf_r4_rc4(b"correct");
        let err = XRef::parse_with_password(&data, Some("wrong")).unwrap_err();
        assert!(
            matches!(err, Error::BadPassword),
            "expected BadPassword, got {err}"
        );
    }

    #[test]
    fn bad_password_r6() {
        let data = build_encrypted_pdf_r6(b"correct");
        let err = XRef::parse_with_password(&data, Some("wrong")).unwrap_err();
        assert!(
            matches!(err, Error::BadPassword),
            "expected BadPassword, got {err}"
        );
    }

    // 手動確認用の暗号化 PDF を tmp に吐き出す
    #[test]
    #[ignore]
    fn dump_encrypted_fixture() {
        let dir = std::env::temp_dir();
        let cases = [
            ("pdfni_enc_r4_aes_empty.pdf", build_encrypted_pdf_r4_aes(b"")),
            (
                "pdfni_enc_r4_aes_secret.pdf",
                build_encrypted_pdf_r4_aes(b"secret"),
            ),
            (
                "pdfni_enc_r6_secret.pdf",
                build_encrypted_pdf_r6(b"secret"),
            ),
        ];
        for (name, data) in cases {
            let path = dir.join(name);
            std::fs::write(&path, &data).unwrap();
            eprintln!("wrote {}", path.display());
        }
    }
}
