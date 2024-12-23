//!
//! pure rust pkcs12 tool
//!
//!

use cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use getrandom::getrandom;
use lazy_static::lazy_static;
use yasna::{
    models::ObjectIdentifier, tags::TAG_OCTETSTRING, ASN1Error, ASN1ErrorKind, BERReader,
    DERWriter, Tag,
};

use hmac::{Hmac, Mac};
use sha1::{Digest, Sha1};
use sha2::Sha256;

type HmacSha1 = Hmac<Sha1>;
type HmacSha256 = Hmac<Sha256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

fn as_oid(s: &'static [u64]) -> ObjectIdentifier {
    ObjectIdentifier::from_slice(s)
}

lazy_static! {
    static ref OID_DATA_CONTENT_TYPE: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 7, 1]);
    static ref OID_ENCRYPTED_DATA_CONTENT_TYPE: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 7, 6]);
    static ref OID_FRIENDLY_NAME: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 9, 20]);
    static ref OID_LOCAL_KEY_ID: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 9, 21]);
    static ref OID_CERT_TYPE_X509_CERTIFICATE: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 9, 22, 1]);
    static ref OID_CERT_TYPE_SDSI_CERTIFICATE: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 9, 22, 2]);
    static ref OID_PBE_WITH_SHA_AND3_KEY_TRIPLE_DESCBC: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 12, 1, 3]);
    static ref OID_SHA1: ObjectIdentifier = as_oid(&[1, 3, 14, 3, 2, 26]);
    static ref OID_HMAC_WITH_SHA1: ObjectIdentifier = as_oid(&[1, 2, 840, 113549, 2]);
    static ref OID_HMAC_WITH_SHA256: ObjectIdentifier = as_oid(&[1, 2, 840, 113549, 2, 9]);
    static ref OID_PBES2: ObjectIdentifier = as_oid(&[1, 2, 840, 113549, 1, 5, 13]);
    static ref OID_PBKDF2: ObjectIdentifier = as_oid(&[1, 2, 840, 113549, 1, 5, 12]);
    static ref OID_SHA2: ObjectIdentifier = as_oid(&[2, 16, 840, 1, 101, 3, 4, 2, 1]);
    static ref OID_PBE_WITH_SHA1_AND40_BIT_RC2_CBC: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 12, 1, 6]);
    static ref OID_KEY_BAG: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 1]);
    static ref OID_AES_CBC_PAD: ObjectIdentifier = as_oid(&[2, 16, 840, 1, 101, 3, 4, 1, 42]);
    static ref OID_PKCS8_SHROUDED_KEY_BAG: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 2]);
    static ref OID_CERT_BAG: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 3]);
    static ref OID_CRL_BAG: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 4]);
    static ref OID_SECRET_BAG: ObjectIdentifier = as_oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 5]);
    static ref OID_SAFE_CONTENTS_BAG: ObjectIdentifier =
        as_oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 6]);
}

const ITERATIONS: u64 = 2048;

fn sha<D: Digest>(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = D::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

#[derive(Debug, Clone)]
pub struct EncryptedContentInfo {
    pub content_encryption_algorithm: AlgorithmIdentifier,
    pub encrypted_content: Vec<u8>,
}

impl EncryptedContentInfo {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let content_type = r.next().read_oid()?;
            debug_assert_eq!(content_type, *OID_DATA_CONTENT_TYPE);
            let content_encryption_algorithm = AlgorithmIdentifier::parse(r.next())?;
            let encrypted_content = r
                .next()
                .read_tagged_implicit(Tag::context(0), |r| r.read_bytes())?;
            Ok(EncryptedContentInfo {
                content_encryption_algorithm,
                encrypted_content,
            })
        })
    }

    pub fn data(&self, password: &[u8]) -> Option<Vec<u8>> {
        self.content_encryption_algorithm
            .decrypt_pbe(&self.encrypted_content, password)
    }

    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            w.next().write_oid(&OID_DATA_CONTENT_TYPE);
            self.content_encryption_algorithm.write(w.next());
            w.next()
                .write_tagged_implicit(Tag::context(0), |w| w.write_bytes(&self.encrypted_content));
        })
    }

    pub fn to_der(&self) -> Vec<u8> {
        yasna::construct_der(|w| self.write(w))
    }

    pub fn from_safe_bags<Encryptor: DataEncryptor, KDF: KeyDeriver>(
        safe_bags: &[SafeBag],
        password: &[u8],
    ) -> Option<EncryptedContentInfo> {
        let data = yasna::construct_der(|w| {
            w.write_sequence_of(|w| {
                for sb in safe_bags {
                    sb.write(w.next());
                }
            })
        });
        let encryptor = Encryptor::new();
        encryptor.encrypt::<KDF>(&data, password)
    }
}

#[derive(Debug, Clone)]
pub struct EncryptedData {
    pub encrypted_content_info: EncryptedContentInfo,
}

impl EncryptedData {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let version = r.next().read_u8()?;
            debug_assert_eq!(version, 0);
            let encrypted_content_info = EncryptedContentInfo::parse(r.next())?;
            Ok(EncryptedData {
                encrypted_content_info,
            })
        })
    }
    pub fn data(&self, password: &[u8]) -> Option<Vec<u8>> {
        self.encrypted_content_info.data(password)
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            w.next().write_u8(0);
            self.encrypted_content_info.write(w.next());
        })
    }
    pub fn from_safe_bags<Encryptor: DataEncryptor, KDF: KeyDeriver>(
        safe_bags: &[SafeBag],
        password: &[u8],
    ) -> Option<Self> {
        let encrypted_content_info =
            EncryptedContentInfo::from_safe_bags::<Encryptor, KDF>(safe_bags, password)?;
        Some(EncryptedData {
            encrypted_content_info,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OtherContext {
    pub content_type: ObjectIdentifier,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum ContentInfo {
    Data(Vec<u8>),
    EncryptedData(EncryptedData),
    OtherContext(OtherContext),
}

impl ContentInfo {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let content_type = r.next().read_oid()?;
            if content_type == *OID_DATA_CONTENT_TYPE {
                let data = r.next().read_tagged(Tag::context(0), |r| r.read_bytes())?;
                return Ok(ContentInfo::Data(data));
            }
            if content_type == *OID_ENCRYPTED_DATA_CONTENT_TYPE {
                let result = r.next().read_tagged(Tag::context(0), |r| {
                    Ok(ContentInfo::EncryptedData(EncryptedData::parse(r)?))
                });
                return result;
            }

            let content = r.next().read_tagged(Tag::context(0), |r| r.read_der())?;
            Ok(ContentInfo::OtherContext(OtherContext {
                content_type,
                content,
            }))
        })
    }
    pub fn data(&self, password: &[u8]) -> Option<Vec<u8>> {
        match self {
            ContentInfo::Data(data) => Some(data.to_owned()),
            ContentInfo::EncryptedData(encrypted) => encrypted.data(password),
            ContentInfo::OtherContext(_) => None,
        }
    }
    pub fn oid(&self) -> ObjectIdentifier {
        match self {
            ContentInfo::Data(_) => OID_DATA_CONTENT_TYPE.clone(),
            ContentInfo::EncryptedData(_) => OID_ENCRYPTED_DATA_CONTENT_TYPE.clone(),
            ContentInfo::OtherContext(other) => other.content_type.clone(),
        }
    }
    pub fn write(&self, w: DERWriter) {
        match self {
            ContentInfo::Data(data) => w.write_sequence(|w| {
                w.next().write_oid(&OID_DATA_CONTENT_TYPE);
                w.next()
                    .write_tagged(Tag::context(0), |w| w.write_bytes(data))
            }),
            ContentInfo::EncryptedData(encrypted_data) => w.write_sequence(|w| {
                w.next().write_oid(&OID_ENCRYPTED_DATA_CONTENT_TYPE);
                w.next()
                    .write_tagged(Tag::context(0), |w| encrypted_data.write(w))
            }),
            ContentInfo::OtherContext(other) => w.write_sequence(|w| {
                w.next().write_oid(&other.content_type);
                w.next()
                    .write_tagged(Tag::context(0), |w| w.write_der(&other.content))
            }),
        }
    }
    pub fn to_der(&self) -> Vec<u8> {
        yasna::construct_der(|w| self.write(w))
    }

    pub fn from_der(der: &[u8]) -> Result<Self, ASN1Error> {
        yasna::parse_der(der, Self::parse)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkcs12PbeParams {
    pub salt: Vec<u8>,
    pub iterations: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkcs12Pbes2Params {
    pub key_derivation_function: Box<AlgorithmIdentifier>,
    pub encryption_scheme: Box<AlgorithmIdentifier>,
}
impl Pkcs12Pbes2Params {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let key_derivation_function: AlgorithmIdentifier =
                AlgorithmIdentifier::parse(r.next())?;
            let encryption_scheme = AlgorithmIdentifier::parse(r.next())?;
            Ok(Self {
                key_derivation_function: Box::new(key_derivation_function),
                encryption_scheme: Box::new(encryption_scheme),
            })
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            self.key_derivation_function.write(w.next());
            self.encryption_scheme.write(w.next());
        })
    }
}

impl Pkcs12PbeParams {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let salt = r.next().read_bytes()?;
            let iterations = r.next().read_u64()?;
            Ok(Pkcs12PbeParams { salt, iterations })
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            w.next().write_bytes(&self.salt);
            w.next().write_u64(self.iterations);
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pbkdf2Params {
    pub salt: Pbkdf2Salt,
    pub iteration_count: u64,
    pub key_length: Option<u64>,
    pub prf: Box<AlgorithmIdentifier>,
}
impl Pbkdf2Params {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let salt = Pbkdf2Salt::parse(r.next())?;
            let iteration_count = r.next().read_u64()?;
            let key_length = r.read_optional(|r| r.read_u64())?;
            let prf = r.read_default(AlgorithmIdentifier::HmacWithSha1(None), |r| {
                AlgorithmIdentifier::parse(r)
            })?;
            Ok(Self {
                salt,
                iteration_count,
                key_length,
                prf: Box::new(prf),
            })
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            self.salt.write(w.next());
            w.next().write_u64(self.iteration_count);
            if let Some(key_length) = self.key_length {
                w.next().write_u64(key_length);
            }
            self.prf.write(w.next());
        });
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pbkdf2Salt {
    Specified(Vec<u8>),
    OtherSource(Box<AlgorithmIdentifier>),
}
impl Pbkdf2Salt {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        let tag = r.lookahead_tag()?;
        if tag == TAG_OCTETSTRING {
            Ok(Self::Specified(r.read_bytes()?))
        } else {
            let src = AlgorithmIdentifier::parse(r)?;
            Ok(Self::OtherSource(Box::new(src)))
        }
    }
    pub fn write(&self, w: DERWriter) {
        match self {
            Pbkdf2Salt::Specified(vec) => w.write_bytes(vec),
            Pbkdf2Salt::OtherSource(algorithm_identifier) => algorithm_identifier.write(w),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherAlgorithmIdentifier {
    pub algorithm_type: ObjectIdentifier,
    pub params: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlgorithmIdentifier {
    Sha1,
    Sha2,
    HmacWithSha1(Option<Vec<u8>>),
    HmacWithSha256(Option<Vec<u8>>),
    PbewithSHAAnd40BitRC2CBC(Pkcs12PbeParams),
    PbeWithSHAAnd3KeyTripleDESCBC(Pkcs12PbeParams),
    Pbes2(Pkcs12Pbes2Params),
    Pbkdf2(Pbkdf2Params),
    AesCbcPad(Vec<u8>),
    OtherAlg(OtherAlgorithmIdentifier),
}

impl AlgorithmIdentifier {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let algorithm_type = r.next().read_oid()?;
            if algorithm_type == *OID_SHA1 {
                r.read_optional(|r| r.read_null())?;
                return Ok(AlgorithmIdentifier::Sha1);
            }
            if algorithm_type == *OID_SHA2 {
                r.read_optional(|r| r.read_null())?;
                return Ok(AlgorithmIdentifier::Sha2);
            }
            if algorithm_type == *OID_PBE_WITH_SHA1_AND40_BIT_RC2_CBC {
                let params = Pkcs12PbeParams::parse(r.next())?;
                return Ok(AlgorithmIdentifier::PbewithSHAAnd40BitRC2CBC(params));
            }
            if algorithm_type == *OID_PBE_WITH_SHA_AND3_KEY_TRIPLE_DESCBC {
                let params = Pkcs12PbeParams::parse(r.next())?;
                return Ok(AlgorithmIdentifier::PbeWithSHAAnd3KeyTripleDESCBC(params));
            }
            if algorithm_type == *OID_PBES2 {
                let params = Pkcs12Pbes2Params::parse(r.next())?;
                return Ok(AlgorithmIdentifier::Pbes2(params));
            }
            if algorithm_type == *OID_PBKDF2 {
                let params = Pbkdf2Params::parse(r.next())?;
                return Ok(AlgorithmIdentifier::Pbkdf2(params));
            }
            if algorithm_type == *OID_HMAC_WITH_SHA1 {
                let r = r.read_optional(|r| r.read_der())?;
                return Ok(AlgorithmIdentifier::HmacWithSha1(r));
            }
            if algorithm_type == *OID_HMAC_WITH_SHA256 {
                let r = r.read_optional(|r| r.read_der())?;
                return Ok(AlgorithmIdentifier::HmacWithSha256(r));
            }
            if algorithm_type == *OID_AES_CBC_PAD {
                let iv = r.next().read_bytes()?;
                return Ok(AlgorithmIdentifier::AesCbcPad(iv));
            }
            let params = r.read_optional(|r| r.read_der())?;
            Ok(AlgorithmIdentifier::OtherAlg(OtherAlgorithmIdentifier {
                algorithm_type,
                params,
            }))
        })
    }
    pub fn decrypt_pbe(&self, ciphertext: &[u8], password: &[u8]) -> Option<Vec<u8>> {
        match self {
            AlgorithmIdentifier::Sha1 => None,
            AlgorithmIdentifier::Sha2 => None,
            AlgorithmIdentifier::HmacWithSha1(_) => None,
            AlgorithmIdentifier::HmacWithSha256(_) => None,
            AlgorithmIdentifier::Pbkdf2(_) => None,
            AlgorithmIdentifier::AesCbcPad(_) => None,

            AlgorithmIdentifier::Pbes2(Pkcs12Pbes2Params {
                key_derivation_function,
                encryption_scheme,
            }) => pbes2_decrypt(
                key_derivation_function,
                encryption_scheme,
                ciphertext,
                password,
            ),
            AlgorithmIdentifier::PbewithSHAAnd40BitRC2CBC(param) => {
                let Ok(str) = std::str::from_utf8(password) else {
                    return None;
                };
                let password = &bmp_string(str);
                pbe_with_sha1_and40_bit_rc2_cbc(ciphertext, password, &param.salt, param.iterations)
            }
            AlgorithmIdentifier::PbeWithSHAAnd3KeyTripleDESCBC(param) => {
                let Ok(str) = std::str::from_utf8(password) else {
                    return None;
                };
                let password = &bmp_string(str);
                pbe_with_sha_and3_key_triple_des_cbc(
                    ciphertext,
                    password,
                    &param.salt,
                    param.iterations,
                )
            }
            AlgorithmIdentifier::OtherAlg(id) => {
                debug_assert!(false, "{id:?}");
                None
            }
        }
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| match self {
            AlgorithmIdentifier::Sha1 => {
                w.next().write_oid(&OID_SHA1);
                w.next().write_null();
            }
            AlgorithmIdentifier::Sha2 => {
                w.next().write_oid(&OID_SHA2);
                w.next().write_null();
            }
            AlgorithmIdentifier::PbewithSHAAnd40BitRC2CBC(p) => {
                w.next().write_oid(&OID_PBE_WITH_SHA1_AND40_BIT_RC2_CBC);
                p.write(w.next());
            }
            AlgorithmIdentifier::PbeWithSHAAnd3KeyTripleDESCBC(p) => {
                w.next().write_oid(&OID_PBE_WITH_SHA_AND3_KEY_TRIPLE_DESCBC);
                p.write(w.next());
            }
            AlgorithmIdentifier::Pbes2(p) => {
                w.next().write_oid(&OID_PBES2);
                p.write(w.next());
            }
            AlgorithmIdentifier::OtherAlg(other) => {
                w.next().write_oid(&other.algorithm_type);
                if let Some(der) = &other.params {
                    w.next().write_der(der);
                }
            }
            AlgorithmIdentifier::AesCbcPad(iv) => {
                w.next().write_oid(&OID_AES_CBC_PAD);
                w.next().write_bytes(iv);
            }
            AlgorithmIdentifier::HmacWithSha1(r) => {
                w.next().write_oid(&OID_HMAC_WITH_SHA1);
                if let Some(r) = r {
                    w.next().write_bytes(r);
                }
            }
            AlgorithmIdentifier::HmacWithSha256(r) => {
                w.next().write_oid(&OID_HMAC_WITH_SHA256);
                if let Some(r) = r {
                    w.next().write_bytes(r);
                }
            }
            AlgorithmIdentifier::Pbkdf2(pbkdf2_params) => {
                w.next().write_oid(&OID_PBKDF2);
                pbkdf2_params.write(w.next());
            }
        })
    }
}

fn pbes2_decrypt(
    key_derivation_function: &AlgorithmIdentifier,
    encryption_scheme: &AlgorithmIdentifier,
    cipher_text: &[u8],
    password: &[u8],
) -> Option<Vec<u8>> {
    let AlgorithmIdentifier::Pbkdf2(params) = key_derivation_function else {
        return None;
    };
    let Pbkdf2Salt::Specified(salt) = &params.salt else {
        return None;
    };
    let mut key = vec![0; params.key_length.unwrap_or(32) as usize];
    match params.prf.as_ref() {
        AlgorithmIdentifier::HmacWithSha1(_) => {
            pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, params.iteration_count as u32, &mut key)
        }
        AlgorithmIdentifier::HmacWithSha256(_) => {
            pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, params.iteration_count as u32, &mut key)
        }
        _ => return None,
    }

    let AlgorithmIdentifier::AesCbcPad(iv) = encryption_scheme else {
        return None;
    };
    let decryptor = Aes256CbcDec::new(key.as_slice().into(), iv.as_slice().into());
    let result = decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(cipher_text)
        .expect("failed");
    Some(result)
}

#[derive(Debug)]
pub struct DigestInfo {
    pub digest_algorithm: AlgorithmIdentifier,
    pub digest: Vec<u8>,
}

impl DigestInfo {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let digest_algorithm = AlgorithmIdentifier::parse(r.next())?;
            let digest = r.next().read_bytes()?;
            Ok(DigestInfo {
                digest_algorithm,
                digest,
            })
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            self.digest_algorithm.write(w.next());
            w.next().write_bytes(&self.digest);
        })
    }
}

#[derive(Debug)]
pub struct MacData {
    pub mac: DigestInfo,
    pub salt: Vec<u8>,
    pub iterations: u32,
}

impl MacData {
    pub fn parse(r: BERReader) -> Result<MacData, ASN1Error> {
        r.read_sequence(|r| {
            let mac = DigestInfo::parse(r.next())?;
            let salt = r.next().read_bytes()?;
            let iterations = r.next().read_u32()?;
            Ok(MacData {
                mac,
                salt,
                iterations,
            })
        })
    }

    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            self.mac.write(w.next());
            w.next().write_bytes(&self.salt);
            w.next().write_u32(self.iterations);
        })
    }

    pub fn verify_mac(&self, data: &[u8], password: &[u8]) -> bool {
        match self.mac.digest_algorithm {
            AlgorithmIdentifier::Sha1 => {
                let key = pbepkcs12sha::<Sha1>(password, &self.salt, self.iterations as u64, 3, 20);
                let mut mac = HmacSha1::new_from_slice(&key).unwrap();
                mac.update(data);
                mac.verify_slice(&self.mac.digest).is_ok()
            }
            AlgorithmIdentifier::Sha2 => {
                let key =
                    pbepkcs12sha::<Sha256>(password, &self.salt, self.iterations as u64, 3, 32);
                let mut mac = HmacSha256::new_from_slice(&key).unwrap();
                mac.update(data);
                mac.verify_slice(&self.mac.digest).is_ok()
            }
            _ => {
                debug_assert!(false, "digest should be sha1 or sha2");
                false
            }
        }
    }

    pub fn new(data: &[u8], password: &[u8]) -> MacData {
        let salt = rand::<8>().unwrap();
        let password = std::str::from_utf8(password).unwrap();
        let password = &bmp_string(password);
        let key = pbepkcs12sha::<Sha1>(password, &salt, ITERATIONS, 3, 20);
        let mut mac = HmacSha1::new_from_slice(&key).unwrap();
        mac.update(data);
        let digest = mac.finalize().into_bytes().to_vec();
        MacData {
            mac: DigestInfo {
                digest_algorithm: AlgorithmIdentifier::Sha1,
                digest,
            },
            salt: salt.to_vec(),
            iterations: ITERATIONS as u32,
        }
    }
}

fn rand<const IV_SIZE: usize>() -> Option<[u8; IV_SIZE]> {
    let mut buf = [0u8; IV_SIZE];
    if getrandom(&mut buf).is_ok() {
        Some(buf)
    } else {
        None
    }
}

pub trait DataEncryptor {
    fn encrypt_keybag<KDF: KeyDeriver>(&self, data: &[u8], password: &[u8]) -> Option<SafeBagKind> {
        self.encrypt_keybag_key_deriver(data, password, &KDF::default())
    }
    fn encrypt_keybag_key_deriver(
        &self,
        data: &[u8],
        password: &[u8],
        key_deriver: &impl KeyDeriver,
    ) -> Option<SafeBagKind>;
    fn encrypt<KDF: KeyDeriver>(
        &self,
        data: &[u8],
        password: &[u8],
    ) -> Option<EncryptedContentInfo> {
        self.encrypt_key_deriver(data, password, &KDF::default())
    }
    fn encrypt_key_deriver(
        &self,
        data: &[u8],
        password: &[u8],
        key_deriver: &impl KeyDeriver,
    ) -> Option<EncryptedContentInfo>;

    fn new() -> impl DataEncryptor;
}
pub trait KeyDeriver: Default {
    fn derive_key(&self, password: &[u8]) -> Option<Vec<u8>>;
    fn get_algorithm(&self) -> AlgorithmIdentifier;
    fn new(alg: AlgorithmIdentifier) -> impl KeyDeriver;
}

pub struct AesCbcDataEncryptor {
    iv: Vec<u8>,
}
pub struct Pbkdf2(AlgorithmIdentifier);

impl Default for Pbkdf2 {
    fn default() -> Self {
        Self(AlgorithmIdentifier::Pbkdf2(Pbkdf2Params {
            salt: Pbkdf2Salt::Specified(rand::<16>().unwrap().to_vec()),
            iteration_count: 2048,
            key_length: None,
            prf: Box::new(AlgorithmIdentifier::HmacWithSha256(None)),
        }))
    }
}

impl KeyDeriver for Pbkdf2 {
    fn derive_key(&self, password: &[u8]) -> Option<Vec<u8>> {
        let AlgorithmIdentifier::Pbkdf2(params) = &self.0 else {
            return None;
        };
        let Pbkdf2Salt::Specified(salt) = &params.salt else {
            return None;
        };
        let mut key = vec![0; params.key_length.unwrap_or(32) as usize];
        match params.prf.as_ref() {
            AlgorithmIdentifier::HmacWithSha1(_) => {
                pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, params.iteration_count as u32, &mut key)
            }
            AlgorithmIdentifier::HmacWithSha256(_) => pbkdf2::pbkdf2_hmac::<Sha256>(
                password,
                salt,
                params.iteration_count as u32,
                &mut key,
            ),
            _ => return None,
        }
        Some(key)
    }

    fn new(alg: AlgorithmIdentifier) -> impl KeyDeriver {
        Self(alg)
    }

    fn get_algorithm(&self) -> AlgorithmIdentifier {
        self.0.clone()
    }
}
impl DataEncryptor for AesCbcDataEncryptor {
    fn new() -> impl DataEncryptor {
        let salt = rand::<16>().unwrap().to_vec();
        Self { iv: salt }
    }
    fn encrypt_keybag_key_deriver(
        &self,
        data: &[u8],
        password: &[u8],
        key_deriver: &impl KeyDeriver,
    ) -> Option<SafeBagKind> {
        let key = key_deriver.derive_key(password)?;
        let cbc = Aes256CbcEnc::new(key.as_slice().into(), self.iv.as_slice().into());
        let encrypted_data = cbc.encrypt_padded_vec_mut::<Pkcs7>(data);
        Some(SafeBagKind::Pkcs8ShroudedKeyBag(EncryptedPrivateKeyInfo {
            encryption_algorithm: AlgorithmIdentifier::Pbes2(Pkcs12Pbes2Params {
                key_derivation_function: Box::new(key_deriver.get_algorithm()),
                encryption_scheme: Box::new(AlgorithmIdentifier::AesCbcPad(self.iv.clone())),
            }),
            encrypted_data,
        }))
    }

    fn encrypt_key_deriver(
        &self,
        data: &[u8],
        password: &[u8],
        key_deriver: &impl KeyDeriver,
    ) -> Option<EncryptedContentInfo> {
        let key = key_deriver.derive_key(password)?;
        let cbc = Aes256CbcEnc::new(key.as_slice().into(), self.iv.as_slice().into());
        let encrypted_content = cbc.encrypt_padded_vec_mut::<Pkcs7>(data);
        Some(EncryptedContentInfo {
            content_encryption_algorithm: AlgorithmIdentifier::Pbes2(Pkcs12Pbes2Params {
                key_derivation_function: Box::new(key_deriver.get_algorithm()),
                encryption_scheme: Box::new(AlgorithmIdentifier::AesCbcPad(self.iv.clone())),
            }),
            encrypted_content,
        })
    }
}

struct PbeWithShaAnd40BitRc2CbcEncryptKeyDeriver(AlgorithmIdentifier);
impl Default for PbeWithShaAnd40BitRc2CbcEncryptKeyDeriver {
    fn default() -> Self {
        Self(AlgorithmIdentifier::PbewithSHAAnd40BitRC2CBC(
            Pkcs12PbeParams {
                salt: rand::<8>().unwrap().to_vec(),
                iterations: ITERATIONS,
            },
        ))
    }
}
struct PbeWithShaAnd40BitRc2CbcEncryptor;

impl KeyDeriver for PbeWithShaAnd40BitRc2CbcEncryptKeyDeriver {
    fn derive_key(&self, _password: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn get_algorithm(&self) -> AlgorithmIdentifier {
        self.0.clone()
    }

    fn new(alg: AlgorithmIdentifier) -> impl KeyDeriver {
        Self(alg)
    }
}
impl DataEncryptor for PbeWithShaAnd40BitRc2CbcEncryptor {
    fn encrypt_keybag_key_deriver(
        &self,
        data: &[u8],
        password: &[u8],
        _key_deriver: &impl KeyDeriver,
    ) -> Option<SafeBagKind> {
        let password = std::str::from_utf8(password).ok()?;
        let password = bmp_string(password);
        let salt = rand::<8>()?.to_vec();
        let encrypted_data =
            pbe_with_sha_and3_key_triple_des_cbc_encrypt(data, &password, &salt, ITERATIONS)?;
        let param = Pkcs12PbeParams {
            salt,
            iterations: ITERATIONS,
        };
        let key_bag_inner = SafeBagKind::Pkcs8ShroudedKeyBag(EncryptedPrivateKeyInfo {
            encryption_algorithm: AlgorithmIdentifier::PbeWithSHAAnd3KeyTripleDESCBC(param),
            encrypted_data,
        });
        Some(key_bag_inner)
    }

    fn encrypt_key_deriver(
        &self,
        data: &[u8],
        password: &[u8],
        _key_deriver: &impl KeyDeriver,
    ) -> Option<EncryptedContentInfo> {
        let password = std::str::from_utf8(password).ok()?;
        let password = bmp_string(password);
        let salt = rand::<8>()?.to_vec();
        let encrypted_content =
            pbe_with_sha_and40_bit_rc2_cbc_encrypt::<Sha1>(data, &password, &salt, ITERATIONS)?;
        let content_encryption_algorithm =
            AlgorithmIdentifier::PbewithSHAAnd40BitRC2CBC(Pkcs12PbeParams {
                salt,
                iterations: ITERATIONS,
            });
        Some(EncryptedContentInfo {
            content_encryption_algorithm,
            encrypted_content,
        })
    }

    fn new() -> impl DataEncryptor {
        Self {}
    }
}

#[derive(Debug)]
pub struct PFX {
    pub version: u8,
    pub auth_safe: ContentInfo,
    pub mac_data: Option<MacData>,
}

impl PFX {
    pub fn new<Encryptor: DataEncryptor, KDF: KeyDeriver>(
        cert_der: &[u8],
        key_der: &[u8],
        ca_der: Option<&[u8]>,
        password: &str,
        name: &str,
    ) -> Option<PFX> {
        let mut cas = vec![];
        if let Some(ca) = ca_der {
            cas.push(ca);
        }
        Self::new_with_cas::<Encryptor, KDF>(cert_der, key_der, &cas, password, name)
    }
    pub fn new_with_cas<Encryptor: DataEncryptor, KDF: KeyDeriver>(
        cert_der: &[u8],
        key_der: &[u8],
        ca_der_list: &[&[u8]],
        password: &str,
        name: &str,
    ) -> Option<PFX> {
        let data_encryptor = Encryptor::new();
        let key_bag_inner = data_encryptor.encrypt_keybag::<KDF>(key_der, password.as_bytes())?;
        let friendly_name = PKCS12Attribute::FriendlyName(name.to_owned());
        let local_key_id = PKCS12Attribute::LocalKeyId(sha::<Sha1>(cert_der));
        let key_bag = SafeBag {
            bag: key_bag_inner,
            attributes: vec![friendly_name.clone(), local_key_id.clone()],
        };
        let cert_bag_inner = SafeBagKind::CertBag(CertBag::X509(cert_der.to_owned()));
        let cert_bag = SafeBag {
            bag: cert_bag_inner,
            attributes: vec![friendly_name, local_key_id],
        };
        let mut cert_bags = vec![cert_bag];
        for ca in ca_der_list {
            cert_bags.push(SafeBag {
                bag: SafeBagKind::CertBag(CertBag::X509((*ca).to_owned())),
                attributes: vec![],
            });
        }
        let contents = yasna::construct_der(|w| {
            w.write_sequence_of(|w| {
                ContentInfo::EncryptedData(
                    EncryptedData::from_safe_bags::<Encryptor, KDF>(
                        &cert_bags,
                        password.as_bytes(),
                    )
                    .ok_or_else(|| ASN1Error::new(ASN1ErrorKind::Invalid))
                    .unwrap(),
                )
                .write(w.next());
                ContentInfo::Data(yasna::construct_der(|w| {
                    w.write_sequence_of(|w| {
                        key_bag.write(w.next());
                    })
                }))
                .write(w.next());
            });
        });
        let mac_data = MacData::new(&contents, password.as_bytes());
        Some(PFX {
            version: 3,
            auth_safe: ContentInfo::Data(contents),
            mac_data: Some(mac_data),
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<PFX, ASN1Error> {
        yasna::parse_ber(bytes, |r| {
            r.read_sequence(|r| {
                let version = r.next().read_u8()?;
                let auth_safe = ContentInfo::parse(r.next())?;
                let mac_data = r.read_optional(MacData::parse)?;
                Ok(PFX {
                    version,
                    auth_safe,
                    mac_data,
                })
            })
        })
    }

    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            w.next().write_u8(self.version);
            self.auth_safe.write(w.next());
            if let Some(mac_data) = &self.mac_data {
                mac_data.write(w.next())
            }
        })
    }

    pub fn to_der(&self) -> Vec<u8> {
        yasna::construct_der(|w| self.write(w))
    }
    pub fn bags(&self, password: &str) -> Result<Vec<SafeBag>, ASN1Error> {
        let password = password.as_bytes();

        let data = self
            .auth_safe
            .data(password)
            .ok_or_else(|| ASN1Error::new(ASN1ErrorKind::Invalid))?;
        let contents = yasna::parse_ber(&data, |r| r.collect_sequence_of(ContentInfo::parse))?;

        let mut result = vec![];
        for content in contents.iter() {
            let data = content
                .data(password)
                .ok_or_else(|| ASN1Error::new(ASN1ErrorKind::Invalid))?;

            let safe_bags = yasna::parse_ber(&data, |r| r.collect_sequence_of(SafeBag::parse))?;

            for safe_bag in safe_bags.iter() {
                result.push(safe_bag.to_owned())
            }
        }
        Ok(result)
    }
    //DER-encoded X.509 certificate
    pub fn cert_bags(&self, password: &str) -> Result<Vec<Vec<u8>>, ASN1Error> {
        self.cert_x509_bags(password)
    }
    //DER-encoded X.509 certificate
    pub fn cert_x509_bags(&self, password: &str) -> Result<Vec<Vec<u8>>, ASN1Error> {
        let mut result = vec![];
        for safe_bag in self.bags(password)? {
            if let Some(cert) = safe_bag.bag.get_x509_cert() {
                result.push(cert);
            }
        }
        Ok(result)
    }
    pub fn cert_sdsi_bags(&self, password: &str) -> Result<Vec<String>, ASN1Error> {
        let mut result = vec![];
        for safe_bag in self.bags(password)? {
            if let Some(cert) = safe_bag.bag.get_sdsi_cert() {
                result.push(cert);
            }
        }
        Ok(result)
    }
    pub fn key_bags(&self, password: &str) -> Result<Vec<Vec<u8>>, ASN1Error> {
        let bmp_password = password.as_bytes();
        let mut result = vec![];
        for safe_bag in self.bags(password)? {
            if let Some(key) = safe_bag.bag.get_key(bmp_password) {
                result.push(key);
            }
        }
        Ok(result)
    }

    pub fn verify_mac(&self, password: &str) -> bool {
        let bmp_password = bmp_string(password);
        if let Some(mac_data) = &self.mac_data {
            return match self.auth_safe.data(&bmp_password) {
                Some(data) => mac_data.verify_mac(&data, &bmp_password),
                None => false,
            };
        }
        true
    }
}

#[inline(always)]
fn pbepkcs12shacore<D: Digest>(d: &[u8], i: &[u8], a: &mut Vec<u8>, iterations: u64) -> Vec<u8> {
    let mut ai: Vec<u8> = d.iter().chain(i.iter()).cloned().collect();
    for _ in 0..iterations {
        ai = sha::<D>(&ai);
    }
    a.append(&mut ai.clone());
    ai
}

#[allow(clippy::many_single_char_names)]
fn pbepkcs12sha<D: Digest>(
    pass: &[u8],
    salt: &[u8],
    iterations: u64,
    id: u8,
    size: u64,
) -> Vec<u8> {
    const U: u64 = 160 / 8;
    const V: u64 = 512 / 8;
    let r: u64 = iterations;
    let d = [id; V as usize];
    fn get_len(s: usize) -> usize {
        let s = s as u64;
        (V * ((s + V - 1) / V)) as usize
    }
    let s = salt.iter().cycle().take(get_len(salt.len()));
    let p = pass.iter().cycle().take(get_len(pass.len()));
    let mut i: Vec<u8> = s.chain(p).cloned().collect();
    let c = (size + U - 1) / U;
    let mut a: Vec<u8> = vec![];
    for _ in 1..c {
        let ai = pbepkcs12shacore::<D>(&d, &i, &mut a, r);

        let b: Vec<u8> = ai.iter().cycle().take(V as usize).cloned().collect();

        let b_iter = b.iter().rev().cycle().take(i.len());
        let i_b_iter = i.iter_mut().rev().zip(b_iter);
        let mut inc = 1u8;
        for (i3, (ii, bi)) in i_b_iter.enumerate() {
            if ((i3 as u64) % V) == 0 {
                inc = 1;
            }
            let (ii2, inc2) = ii.overflowing_add(*bi);
            let (ii3, inc3) = ii2.overflowing_add(inc);
            inc = (inc2 || inc3) as u8;
            *ii = ii3;
        }
    }

    pbepkcs12shacore::<D>(&d, &i, &mut a, r);

    a.iter().take(size as usize).cloned().collect()
}

fn pbe_with_sha1_and40_bit_rc2_cbc(
    data: &[u8],
    password: &[u8],
    salt: &[u8],
    iterations: u64,
) -> Option<Vec<u8>> {
    use cbc::Decryptor;
    use rc2::Rc2;
    type Rc2Cbc = Decryptor<Rc2>;

    let dk = pbepkcs12sha::<Sha1>(password, salt, iterations, 1, 5);
    let iv = pbepkcs12sha::<Sha1>(password, salt, iterations, 2, 8);

    let rc2 = Rc2Cbc::new_from_slices(&dk, &iv).ok()?;
    rc2.decrypt_padded_vec_mut::<Pkcs7>(data).ok()
}

fn pbe_with_sha_and40_bit_rc2_cbc_encrypt<D: Digest>(
    data: &[u8],
    password: &[u8],
    salt: &[u8],
    iterations: u64,
) -> Option<Vec<u8>> {
    use cbc::Encryptor;
    use rc2::Rc2;
    type Rc2Cbc = Encryptor<Rc2>;

    let dk = pbepkcs12sha::<D>(password, salt, iterations, 1, 5);
    let iv = pbepkcs12sha::<D>(password, salt, iterations, 2, 8);

    let rc2 = Rc2Cbc::new_from_slices(&dk, &iv).ok()?;
    Some(rc2.encrypt_padded_vec_mut::<Pkcs7>(data))
}

fn pbe_with_sha_and3_key_triple_des_cbc(
    data: &[u8],
    password: &[u8],
    salt: &[u8],
    iterations: u64,
) -> Option<Vec<u8>> {
    use cbc::Decryptor;
    use des::TdesEde3;
    type TDesCbc = Decryptor<TdesEde3>;

    let dk = pbepkcs12sha::<Sha1>(password, salt, iterations, 1, 24);
    let iv = pbepkcs12sha::<Sha1>(password, salt, iterations, 2, 8);

    let tdes = TDesCbc::new_from_slices(&dk, &iv).ok()?;
    tdes.decrypt_padded_vec_mut::<Pkcs7>(data).ok()
}

fn pbe_with_sha_and3_key_triple_des_cbc_encrypt(
    data: &[u8],
    password: &[u8],
    salt: &[u8],
    iterations: u64,
) -> Option<Vec<u8>> {
    use cbc::Encryptor;
    use des::TdesEde3;
    type TDesCbc = Encryptor<TdesEde3>;

    let dk = pbepkcs12sha::<Sha1>(password, salt, iterations, 1, 24);
    let iv = pbepkcs12sha::<Sha1>(password, salt, iterations, 2, 8);

    let tdes = TDesCbc::new_from_slices(&dk, &iv).ok()?;
    Some(tdes.encrypt_padded_vec_mut::<Pkcs7>(data))
}

fn bmp_string(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().collect();

    let mut bytes = Vec::with_capacity(utf16.len() * 2 + 2);
    for c in utf16 {
        bytes.push((c / 256) as u8);
        bytes.push((c % 256) as u8);
    }
    bytes.push(0x00);
    bytes.push(0x00);
    bytes
}

#[derive(Debug, Clone)]
pub enum CertBag {
    X509(Vec<u8>),
    SDSI(String),
}

impl CertBag {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let oid = r.next().read_oid()?;
            if oid == *OID_CERT_TYPE_X509_CERTIFICATE {
                let x509 = r.next().read_tagged(Tag::context(0), |r| r.read_bytes())?;
                return Ok(CertBag::X509(x509));
            };
            if oid == *OID_CERT_TYPE_SDSI_CERTIFICATE {
                let sdsi = r
                    .next()
                    .read_tagged(Tag::context(0), |r| r.read_ia5_string())?;
                return Ok(CertBag::SDSI(sdsi));
            }
            Err(ASN1Error::new(ASN1ErrorKind::Invalid))
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| match self {
            CertBag::X509(x509) => {
                w.next().write_oid(&OID_CERT_TYPE_X509_CERTIFICATE);
                w.next()
                    .write_tagged(Tag::context(0), |w| w.write_bytes(x509));
            }
            CertBag::SDSI(sdsi) => {
                w.next().write_oid(&OID_CERT_TYPE_SDSI_CERTIFICATE);
                w.next()
                    .write_tagged(Tag::context(0), |w| w.write_ia5_string(sdsi));
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncryptedPrivateKeyInfo {
    pub encryption_algorithm: AlgorithmIdentifier,
    pub encrypted_data: Vec<u8>,
}

impl EncryptedPrivateKeyInfo {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let encryption_algorithm = AlgorithmIdentifier::parse(r.next())?;

            let encrypted_data = r.next().read_bytes()?;

            Ok(EncryptedPrivateKeyInfo {
                encryption_algorithm,
                encrypted_data,
            })
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            self.encryption_algorithm.write(w.next());
            w.next().write_bytes(&self.encrypted_data);
        })
    }
    pub fn decrypt(&self, password: &[u8]) -> Option<Vec<u8>> {
        self.encryption_algorithm
            .decrypt_pbe(&self.encrypted_data, password)
    }
}

#[test]
fn test_encrypted_private_key_info() {
    let epki = EncryptedPrivateKeyInfo {
        encryption_algorithm: AlgorithmIdentifier::Sha1,
        encrypted_data: b"foo".to_vec(),
    };
    let der = yasna::construct_der(|w| {
        epki.write(w);
    });
    let epki2 = yasna::parse_ber(&der, EncryptedPrivateKeyInfo::parse).unwrap();
    assert_eq!(epki2, epki);
}

#[derive(Debug, Clone)]
pub struct OtherBag {
    pub bag_id: ObjectIdentifier,
    pub bag_value: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum SafeBagKind {
    //KeyBag(),
    Pkcs8ShroudedKeyBag(EncryptedPrivateKeyInfo),
    CertBag(CertBag),
    //CRLBag(),
    //SecretBag(),
    //SafeContents(Vec<SafeBag>),
    OtherBagKind(OtherBag),
}

impl SafeBagKind {
    pub fn parse(r: BERReader, bag_id: ObjectIdentifier) -> Result<Self, ASN1Error> {
        if bag_id == *OID_CERT_BAG {
            return Ok(SafeBagKind::CertBag(CertBag::parse(r)?));
        }
        if bag_id == *OID_PKCS8_SHROUDED_KEY_BAG {
            return Ok(SafeBagKind::Pkcs8ShroudedKeyBag(
                EncryptedPrivateKeyInfo::parse(r)?,
            ));
        }
        let bag_value = r.read_der()?;
        Ok(SafeBagKind::OtherBagKind(OtherBag { bag_id, bag_value }))
    }
    pub fn write(&self, w: DERWriter) {
        match self {
            SafeBagKind::Pkcs8ShroudedKeyBag(epk) => epk.write(w),
            SafeBagKind::CertBag(cb) => cb.write(w),
            SafeBagKind::OtherBagKind(other) => w.write_der(&other.bag_value),
        }
    }
    pub fn oid(&self) -> ObjectIdentifier {
        match self {
            SafeBagKind::Pkcs8ShroudedKeyBag(_) => OID_PKCS8_SHROUDED_KEY_BAG.clone(),
            SafeBagKind::CertBag(_) => OID_CERT_BAG.clone(),
            SafeBagKind::OtherBagKind(other) => other.bag_id.clone(),
        }
    }
    pub fn get_x509_cert(&self) -> Option<Vec<u8>> {
        if let SafeBagKind::CertBag(CertBag::X509(x509)) = self {
            return Some(x509.to_owned());
        }
        None
    }

    pub fn get_sdsi_cert(&self) -> Option<String> {
        if let SafeBagKind::CertBag(CertBag::SDSI(sdsi)) = self {
            return Some(sdsi.to_owned());
        }
        None
    }

    pub fn get_key(&self, password: &[u8]) -> Option<Vec<u8>> {
        if let SafeBagKind::Pkcs8ShroudedKeyBag(kb) = self {
            return kb.decrypt(password);
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct OtherAttribute {
    pub oid: ObjectIdentifier,
    pub data: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum PKCS12Attribute {
    FriendlyName(String),
    LocalKeyId(Vec<u8>),
    Other(OtherAttribute),
}

impl PKCS12Attribute {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let oid = r.next().read_oid()?;
            if oid == *OID_FRIENDLY_NAME {
                let name = r
                    .next()
                    .collect_set_of(|s| s.read_bmp_string())?
                    .pop()
                    .ok_or_else(|| ASN1Error::new(ASN1ErrorKind::Invalid))?;
                return Ok(PKCS12Attribute::FriendlyName(name));
            }
            if oid == *OID_LOCAL_KEY_ID {
                let local_key_id = r
                    .next()
                    .collect_set_of(|s| s.read_bytes())?
                    .pop()
                    .ok_or_else(|| ASN1Error::new(ASN1ErrorKind::Invalid))?;
                return Ok(PKCS12Attribute::LocalKeyId(local_key_id));
            }

            let data = r.next().collect_set_of(|s| s.read_der())?;
            let other = OtherAttribute { oid, data };
            Ok(PKCS12Attribute::Other(other))
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| match self {
            PKCS12Attribute::FriendlyName(name) => {
                w.next().write_oid(&OID_FRIENDLY_NAME);
                w.next().write_set_of(|w| {
                    w.next().write_bmp_string(name);
                })
            }
            PKCS12Attribute::LocalKeyId(id) => {
                w.next().write_oid(&OID_LOCAL_KEY_ID);
                w.next().write_set_of(|w| w.next().write_bytes(id))
            }
            PKCS12Attribute::Other(other) => {
                w.next().write_oid(&other.oid);
                w.next().write_set_of(|w| {
                    for bytes in other.data.iter() {
                        w.next().write_der(bytes);
                    }
                })
            }
        })
    }
}
#[derive(Debug, Clone)]
pub struct SafeBag {
    pub bag: SafeBagKind,
    pub attributes: Vec<PKCS12Attribute>,
}

impl SafeBag {
    pub fn parse(r: BERReader) -> Result<Self, ASN1Error> {
        r.read_sequence(|r| {
            let oid = r.next().read_oid()?;

            let bag = r
                .next()
                .read_tagged(Tag::context(0), |r| SafeBagKind::parse(r, oid))?;

            let attributes = r
                .read_optional(|r| r.collect_set_of(PKCS12Attribute::parse))?
                .unwrap_or_else(Vec::new);

            Ok(SafeBag { bag, attributes })
        })
    }
    pub fn write(&self, w: DERWriter) {
        w.write_sequence(|w| {
            w.next().write_oid(&self.bag.oid());
            w.next()
                .write_tagged(Tag::context(0), |w| self.bag.write(w));
            if !self.attributes.is_empty() {
                w.next().write_set_of(|w| {
                    for attr in &self.attributes {
                        attr.write(w.next());
                    }
                })
            }
        })
    }
    pub fn friendly_name(&self) -> Option<String> {
        for attr in self.attributes.iter() {
            if let PKCS12Attribute::FriendlyName(name) = attr {
                return Some(name.to_owned());
            }
        }
        None
    }
    pub fn local_key_id(&self) -> Option<Vec<u8>> {
        for attr in self.attributes.iter() {
            if let PKCS12Attribute::LocalKeyId(id) = attr {
                return Some(id.to_owned());
            }
        }
        None
    }
}

#[test]
fn test_create_p12_pbes2() {
    use std::fs::File;
    use std::io::{Read, Write};
    let mut cafile = File::open("ca.der").unwrap();
    let mut ca = vec![];
    cafile.read_to_end(&mut ca).unwrap();
    let mut fcert = File::open("clientcert.der").unwrap();
    let mut fkey = File::open("clientkey.der").unwrap();
    let mut cert = vec![];
    fcert.read_to_end(&mut cert).unwrap();
    let mut key = vec![];
    fkey.read_to_end(&mut key).unwrap();
    let p12 = PFX::new::<AesCbcDataEncryptor, Pbkdf2>(&cert, &key, Some(&ca), "changeit", "look")
        .unwrap()
        .to_der();

    let pfx = PFX::parse(&p12).unwrap();

    let keys = pfx.key_bags("changeit").unwrap();
    assert_eq!(keys[0], key);

    let certs = pfx.cert_x509_bags("changeit").unwrap();
    assert_eq!(certs[0], cert);
    assert_eq!(certs[1], ca);
    assert!(pfx.verify_mac("changeit"));

    let mut fp12 = File::create("test.p12").unwrap();
    fp12.write_all(&p12).unwrap();
}
#[test]
fn test_create_p12_pbes2_without_password() {
    use std::fs::File;
    use std::io::{Read, Write};
    let mut cafile = File::open("ca.der").unwrap();
    let mut ca = vec![];
    cafile.read_to_end(&mut ca).unwrap();
    let mut fcert = File::open("clientcert.der").unwrap();

    let mut cert = vec![];
    fcert.read_to_end(&mut cert).unwrap();

    let p12 = PFX::new::<AesCbcDataEncryptor, Pbkdf2>(&cert, &[], Some(&ca), "", "look")
        .expect("failed to generate")
        .to_der();

    let pfx = PFX::parse(&p12).unwrap();

    let certs = pfx.cert_x509_bags("").unwrap();
    assert_eq!(certs[0], cert);
    assert_eq!(certs[1], ca);
    assert!(pfx.verify_mac(""));

    let mut fp12 = File::create("test.p12").unwrap();
    fp12.write_all(&p12).unwrap();
}

#[test]
fn test_create_p12_legacy() {
    use std::fs::File;
    use std::io::{Read, Write};
    let mut cafile = File::open("ca.der").unwrap();
    let mut ca = vec![];
    cafile.read_to_end(&mut ca).unwrap();
    let mut fcert = File::open("clientcert.der").unwrap();
    let mut fkey = File::open("clientkey.der").unwrap();
    let mut cert = vec![];
    fcert.read_to_end(&mut cert).unwrap();
    let mut key = vec![];
    fkey.read_to_end(&mut key).unwrap();
    let p12 = PFX::new::<
        PbeWithShaAnd40BitRc2CbcEncryptor,
        PbeWithShaAnd40BitRc2CbcEncryptKeyDeriver,
    >(&cert, &key, Some(&ca), "changeit", "look")
    .unwrap()
    .to_der();

    let pfx = PFX::parse(&p12).unwrap();

    let keys = pfx.key_bags("changeit").unwrap();
    assert_eq!(keys[0], key);

    let certs = pfx.cert_x509_bags("changeit").unwrap();
    assert_eq!(certs[0], cert);
    assert_eq!(certs[1], ca);
    assert!(pfx.verify_mac("changeit"));

    let mut fp12 = File::create("test.p12").unwrap();
    fp12.write_all(&p12).unwrap();
}
#[test]
fn test_create_p12_legacy_without_password() {
    use std::fs::File;
    use std::io::{Read, Write};
    let mut cafile = File::open("ca.der").unwrap();
    let mut ca = vec![];
    cafile.read_to_end(&mut ca).unwrap();
    let mut fcert = File::open("clientcert.der").unwrap();

    let mut cert = vec![];
    fcert.read_to_end(&mut cert).unwrap();

    let p12 = PFX::new::<
        PbeWithShaAnd40BitRc2CbcEncryptor,
        PbeWithShaAnd40BitRc2CbcEncryptKeyDeriver,
    >(&cert, &[], Some(&ca), "", "look")
    .expect("failed to generate")
    .to_der();

    let pfx = PFX::parse(&p12).unwrap();

    let certs = pfx.cert_x509_bags("").unwrap();
    assert_eq!(certs[0], cert);
    assert_eq!(certs[1], ca);
    assert!(pfx.verify_mac(""));

    let mut fp12 = File::create("test.p12").unwrap();
    fp12.write_all(&p12).unwrap();
}

#[test]
fn test_bmp_string() {
    let value = bmp_string("Beavis");
    assert!(
        value
            == [0x00, 0x42, 0x00, 0x65, 0x00, 0x61, 0x00, 0x76, 0x00, 0x69, 0x00, 0x73, 0x00, 0x00]
    )
}

#[test]
fn test_pbepkcs12sha1() {
    use hex_literal::hex;
    let pass = bmp_string("");
    assert_eq!(pass, vec![0, 0]);
    let salt = hex!("9af4702958a8e95c");
    let iterations = 2048;
    let id = 1;
    let size = 24;
    let result = pbepkcs12sha::<Sha1>(&pass, &salt, iterations, id, size);
    let res = hex!("c2294aa6d02930eb5ce9c329eccb9aee1cb136baea746557");
    assert_eq!(result, res);
}

#[test]
fn test_pbepkcs12sha1_2() {
    use hex_literal::hex;
    let pass = bmp_string("");
    assert_eq!(pass, vec![0, 0]);
    let salt = hex!("9af4702958a8e95c");
    let iterations = 2048;
    let id = 2;
    let size = 8;
    let result = pbepkcs12sha::<Sha1>(&pass, &salt, iterations, id, size);
    let res = hex!("8e9f8fc7664378bc");
    assert_eq!(result, res);
}
